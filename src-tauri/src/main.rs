#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod monitor;
mod pricing;
mod statistics;
mod store;
mod titles;
mod usage;

use monitor::{Scanner, Status};
use parking_lot::FairMutex;
use rusqlite::Connection;
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{
    image::Image,
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    Manager, State, WindowEvent,
};
use usage::Settings;
use winrt_notification::{Duration as ToastDuration, Toast};

struct Shared {
    db: FairMutex<Connection>,
    status: Mutex<Status>,
    data_path: PathBuf,
}

#[derive(Serialize)]
struct Snapshot {
    settings: Settings,
    status: Status,
    history: store::History,
}

#[tauri::command]
async fn snapshot(
    state: State<'_, Arc<Shared>>,
    search: String,
    alerts_only: bool,
    page: u32,
) -> Result<Snapshot, String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let db = store::open_reader(&shared.data_path.join("history.sqlite3"))
            .map_err(|e| e.to_string())?;
        let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
        let result = Snapshot {
            settings: store::settings(&tx)?,
            status: shared.status.lock().map_err(|e| e.to_string())?.clone(),
            history: store::history(&tx, &search, alerts_only, page).map_err(|e| e.to_string())?,
        };
        tx.commit().map_err(|e| e.to_string())?;
        Ok(result)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn save_settings(state: State<'_, Arc<Shared>>, settings: Settings) -> Result<(), String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        if settings.threshold <= 0 || settings.threshold > 1_000_000_000 {
            return Err("Threshold must be between 1 and 1,000,000,000 tokens".into());
        }
        if !std::path::Path::new(&settings.source).is_absolute()
            || !std::path::Path::new(&settings.source).is_dir()
        {
            return Err("Choose an existing absolute Claude projects folder".into());
        }
        let db = shared.db.lock();
        store::save_settings(&db, &settings)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn notify(title: &str, body: &str) -> Result<(), String> {
    // The built-in PowerShell AUMID supports WinRT toasts without an installer or shortcut registration.
    Toast::new(Toast::POWERSHELL_APP_ID)
        .title(title)
        .text1(body)
        .duration(ToastDuration::Long)
        .show()
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn test_notification() -> Result<(), String> {
    notify("Claude Cache Watch - test", "Desktop alerts are connected. Windows notification settings control whether a banner appears.")
}

#[tauri::command]
async fn pricing_catalog(state: State<'_, Arc<Shared>>) -> Result<Vec<store::ModelRate>, String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let db = store::open_reader(&shared.data_path.join("history.sqlite3"))
            .map_err(|e| e.to_string())?;
        let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
        let result = store::pricing_catalog(&tx).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(result)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn set_model_rates(
    state: State<'_, Arc<Shared>>,
    model: String,
    rates: Option<pricing::Rates>,
) -> Result<(), String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let db = shared.db.lock();
        store::set_model_rates(&db, &model, rates)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn statistics(
    state: State<'_, Arc<Shared>>,
    start_ms: Option<i64>,
    end_ms: i64,
) -> Result<statistics::Statistics, String> {
    if start_ms.is_some_and(|start| start > end_ms) {
        return Err("The interval start must be before its end".into());
    }
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let db = store::open_reader(&shared.data_path.join("history.sqlite3"))
            .map_err(|e| e.to_string())?;
        let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
        let settings = store::settings(&tx)?;
        let result =
            statistics::calculate(&tx, start_ms, end_ms, &settings).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(result)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn open_data_folder(state: State<Arc<Shared>>) -> Result<(), String> {
    std::process::Command::new("explorer.exe")
        .arg(&state.data_path)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn export_history_page(
    state: State<'_, Arc<Shared>>,
    search: String,
    alerts_only: bool,
    page: u32,
) -> Result<String, String> {
    let shared = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let rows = {
            let db = store::open_reader(&shared.data_path.join("history.sqlite3"))
                .map_err(|e| e.to_string())?;
            let tx = db.unchecked_transaction().map_err(|e| e.to_string())?;
            let rows = store::history(&tx, &search, alerts_only, page)
                .map_err(|e| e.to_string())?
                .rows;
            tx.commit().map_err(|e| e.to_string())?;
            rows
        };
        let folder = dirs::download_dir()
            .ok_or("Windows Downloads folder is unavailable")?
            .join("Claude Cache Watch");
        std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let path = folder.join(format!(
            "claude-calls-{timestamp}-page-{}.json",
            u64::from(page) + 1
        ));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| e.to_string())?;
        serde_json::to_writer_pretty(&mut file, &rows).map_err(|e| e.to_string())?;
        Ok(path.to_string_lossy().into_owned())
    })
    .await
    .map_err(|e| e.to_string())?
}

fn deliver_pending(shared: &Shared) -> Result<(), String> {
    let rows = {
        let db = shared.db.lock();
        let mut query=db.prepare("SELECT id,input+creation,read,model FROM calls WHERE alert='pending' ORDER BY timestamp LIMIT 10").map_err(|e| e.to_string())?;
        let rows = query
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| e.to_string())?;
        rows
    };
    for (id, uncached, read, model) in rows {
        let result = notify(
            "Claude Code - high uncached input",
            &format!("{uncached} uncached tokens | {read} cache-read tokens | {model}"),
        );
        let (status, error) = match result {
            Ok(()) => ("sent", String::new()),
            Err(error) => ("failed", error),
        };
        shared
            .db
            .lock()
            .execute(
                "UPDATE calls SET alert=?1,alert_error=?2 WHERE id=?3",
                rusqlite::params![status, error, id],
            )
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn run_monitor(shared: Arc<Shared>) {
    let mut scanner = Scanner::new();
    loop {
        let mut files = 0;
        let mut importing = false;
        let result = (|| -> Result<(), String> {
            let settings = {
                let db = shared.db.lock();
                store::settings(&db)?
            };
            if settings.paused {
                return Ok(());
            }
            let paths = scanner.discover(&settings.source)?;
            files = paths.len();
            let mut errors = Vec::new();
            for path in paths {
                let result = {
                    let mut db = shared.db.lock();
                    scanner.scan_file(&mut db, &path, &settings)
                };
                match result {
                    Ok(pending) => importing |= pending,
                    Err(error) => {
                        if errors.len() < 3 {
                            errors.push(error);
                        }
                    }
                }
            }
            deliver_pending(&shared)?;
            if !errors.is_empty() {
                return Err(errors.join("\n"));
            }
            Ok(())
        })();
        if let Ok(mut status) = shared.status.lock() {
            status.ready = true;
            status.files = files;
            status.importing = importing;
            status.error = result.err().unwrap_or_default();
            status.last_scan = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
        }
        std::thread::sleep(Duration::from_secs(3));
    }
}

fn show(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| show(app)))
        .invoke_handler(tauri::generate_handler![
            snapshot,
            save_settings,
            test_notification,
            pricing_catalog,
            set_model_rates,
            statistics,
            open_data_folder,
            export_history_page
        ])
        .setup(|app| {
            let data_path = app.path().app_local_data_dir()?;
            std::fs::create_dir_all(&data_path)?;
            let db = store::open(&data_path.join("history.sqlite3"))?;
            let shared = Arc::new(Shared {
                db: FairMutex::new(db),
                status: Mutex::new(Status {
                    data_path: data_path.to_string_lossy().into_owned(),
                    ..Status::default()
                }),
                data_path,
            });
            app.manage(shared.clone());
            let open = MenuItem::with_id(app, "open", "Open history", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit monitor", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;
            let mut rgba = vec![0u8; 32 * 32 * 4];
            for y in 0..32 {
                for x in 0..32 {
                    let pixel = (y * 32 + x) * 4;
                    let color = if (7..25).contains(&x) && (7..25).contains(&y) {
                        [131, 230, 188, 255]
                    } else {
                        [25, 34, 45, 255]
                    };
                    rgba[pixel..pixel + 4].copy_from_slice(&color);
                }
            }
            TrayIconBuilder::new()
                .icon(Image::new_owned(rgba, 32, 32))
                .tooltip("Claude Cache Watch - monitoring")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;
            std::thread::spawn(move || run_monitor(shared));
            if !std::env::args().any(|arg| arg == "--background") {
                show(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("Could not start Claude Cache Watch");
}
