use crate::{
    store, titles,
    usage::{Settings, Usage},
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

const MAX_LINE: u64 = 16 * 1024 * 1024;
const BATCH_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Default, Clone, Serialize)]
pub struct Status {
    pub ready: bool,
    pub files: usize,
    pub importing: bool,
    pub error: String,
    pub last_scan: u64,
    pub data_path: String,
}

pub struct Scanner {
    source: String,
    baseline: HashMap<PathBuf, Option<Baseline>>,
    initialized: bool,
}

struct Baseline {
    length: u64,
    prefix: Vec<u8>,
}

impl Scanner {
    pub fn new() -> Self {
        Self {
            source: String::new(),
            baseline: HashMap::new(),
            initialized: false,
        }
    }

    pub fn discover(&mut self, source: &str) -> Result<Vec<PathBuf>, String> {
        if self.source != source {
            self.source = source.to_owned();
            self.initialized = false;
            self.baseline.clear();
        }
        if !Path::new(source).is_dir() {
            return Err(format!("Log folder unavailable: {source}"));
        }
        let mut paths = Vec::new();
        for entry in WalkDir::new(source).follow_links(false) {
            let entry = entry.map_err(|e| format!("Cannot discover logs: {e}"))?;
            if entry.file_type().is_file() && entry.path().extension().is_some_and(|v| v == "jsonl")
            {
                paths.push(entry.into_path());
            }
        }
        if !self.initialized {
            for path in &paths {
                let mut file = File::open(path).map_err(|e| e.to_string())?;
                let length = file.metadata().map_err(|e| e.to_string())?.len();
                let mut prefix = vec![0; length.min(256) as usize];
                file.read_exact(&mut prefix).map_err(|e| e.to_string())?;
                self.baseline
                    .insert(path.clone(), Some(Baseline { length, prefix }));
            }
            self.initialized = true;
        }
        // Favor active sessions while progressively importing older logs.
        paths.sort_by_cached_key(|p| {
            std::cmp::Reverse(std::fs::metadata(p).and_then(|m| m.modified()).ok())
        });
        Ok(paths)
    }

    pub fn scan_file(
        &mut self,
        conn: &mut Connection,
        path: &Path,
        settings: &Settings,
    ) -> Result<bool, String> {
        scan_file(
            conn,
            path,
            settings,
            self.baseline.entry(path.to_path_buf()).or_default(),
        )
    }
}

fn scan_file(
    conn: &mut Connection,
    path: &Path,
    settings: &Settings,
    baseline: &mut Option<Baseline>,
) -> Result<bool, String> {
    // Title metadata is optional: report failures after processing the actual transcript.
    let metadata_error = titles::record_agent_sidecar(conn, path).err();
    let result = (|| -> Result<bool, Box<dyn std::error::Error>> {
        let path_text = path.to_string_lossy();
        let mut file = File::open(path)?;
        let length = file.metadata()?.len();
        let cursor: Option<(u64, Vec<u8>)> = conn
            .query_row(
                "SELECT offset,signature FROM cursors WHERE path=?1",
                [path_text.as_ref()],
                |r| Ok((r.get::<_, i64>(0)? as u64, r.get(1)?)),
            )
            .optional()?;
        let (mut offset, old_signature) = cursor.unwrap_or_default();
        let mut prefix = vec![0; length.min(256) as usize];
        file.read_exact(&mut prefix)?;
        // A baseline belongs to the file observed at discovery, not every future file at this path.
        if baseline
            .as_ref()
            .is_some_and(|b| length < b.length || !prefix.starts_with(&b.prefix))
        {
            *baseline = None;
        }
        let import_until = baseline.as_ref().map_or(0, |b| b.length);
        let replaced = offset > length || !prefix.starts_with(&old_signature);
        if replaced {
            offset = 0;
        }
        if offset == length && !old_signature.is_empty() && !replaced {
            return Ok(false);
        }
        file.seek(SeekFrom::Start(offset))?;
        let mut reader = BufReader::new(file);
        let tx = conn.transaction()?;
        let start = offset;
        let mut line = Vec::new();
        while offset < length && offset - start < BATCH_BYTES {
            line.clear();
            // Bound memory even if a malformed transcript lacks newlines.
            let read = reader
                .by_ref()
                .take(MAX_LINE + 1)
                .read_until(b'\n', &mut line)?;
            if read as u64 > MAX_LINE {
                return Err(
                    "Transcript line exceeds 16 MiB; monitoring this file is suspended".into(),
                );
            }
            if read == 0 || line.last() != Some(&b'\n') {
                break;
            }
            if let Ok(value) = serde_json::from_slice(&line) {
                titles::record_line(&tx, &value, path)?;
                if let Some(mut usage) = Usage::from_value(&value) {
                    let (session, agent) = titles::identity(path);
                    if usage.session.is_empty() {
                        usage.session = session;
                    }
                    if usage.agent_id.is_empty() {
                        usage.agent_id = agent;
                    }
                    store::record(
                        &tx,
                        &usage,
                        path_text.as_ref(),
                        offset < import_until,
                        settings,
                    )?;
                }
            }
            offset += read as u64;
        }
        tx.execute("INSERT INTO cursors VALUES(?1,?2,?3) ON CONFLICT(path) DO UPDATE SET offset=excluded.offset,signature=excluded.signature",
            params![path_text.as_ref(), i64::try_from(offset)?, prefix])?;
        tx.commit()?;
        Ok(offset < import_until && offset < length)
    })();
    match result {
        Ok(importing) => match metadata_error {
            Some(error) => Err(error),
            None => Ok(importing),
        },
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    fn line(id: &str) -> String {
        format!("{{\"type\":\"assistant\",\"requestId\":\"{id}\",\"message\":{{\"usage\":{{\"input_tokens\":100001}}}}}}\n")
    }

    #[test]
    fn malformed_title_sidecar_does_not_block_calls_and_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1/subagents/agent-a1.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, line("live-call")).unwrap();
        std::fs::write(path.with_extension("meta.json"), "partial JSON").unwrap();
        let mut db = store::open(Path::new(":memory:")).unwrap();
        assert!(scan_file(&mut db, &path, &Settings::default(), &mut None).is_err());
        let h = store::history(&db, "", false, 0).unwrap();
        assert_eq!(h.calls, 1);
        assert_eq!(h.rows[0].alert, "pending");
        assert_eq!(
            db.query_row("SELECT offset FROM cursors", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            std::fs::metadata(&path).unwrap().len() as i64
        );
        std::fs::write(
            path.with_extension("meta.json"),
            r#"{"description":"Recovered title"}"#,
        )
        .unwrap();
        scan_file(&mut db, &path, &Settings::default(), &mut None).unwrap();
        assert_eq!(
            store::history(&db, "Recovered title", false, 0)
                .unwrap()
                .total,
            1
        );
    }
    #[test]
    fn backfill_partial_append_restart_and_subagent_detection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        std::fs::write(&path, line("old")).unwrap();
        let mut db = store::open(&dir.path().join("history.db")).unwrap();
        let settings = Settings {
            source: dir.path().to_string_lossy().into_owned(),
            ..Settings::default()
        };
        let mut scanner = Scanner::new();
        let paths = scanner.discover(&settings.source).unwrap();
        scanner.scan_file(&mut db, &paths[0], &settings).unwrap();
        assert_eq!(store::history(&db, "", true, 0).unwrap().total, 0);
        let next = line("live");
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(&next.as_bytes()[..20]).unwrap();
        scanner.scan_file(&mut db, &path, &settings).unwrap();
        assert_eq!(store::history(&db, "", false, 0).unwrap().calls, 1);
        f.write_all(&next.as_bytes()[20..]).unwrap();
        scanner.scan_file(&mut db, &path, &settings).unwrap();
        assert_eq!(store::history(&db, "", true, 0).unwrap().total, 1);
        drop(db);
        let mut db = store::open(&dir.path().join("history.db")).unwrap();
        let mut restarted = Scanner::new();
        restarted.discover(&settings.source).unwrap();
        restarted.scan_file(&mut db, &path, &settings).unwrap();
        assert_eq!(store::history(&db, "", false, 0).unwrap().calls, 2);
        let child = dir.path().join("session/subagents");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(child.join("agent.jsonl"), line("agent")).unwrap();
        for path in restarted.discover(&settings.source).unwrap() {
            restarted.scan_file(&mut db, &path, &settings).unwrap();
        }
        assert_eq!(store::history(&db, "", true, 0).unwrap().total, 2);
    }
    #[test]
    fn truncated_and_replaced_logs_are_replayed_without_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut db = store::open(Path::new(":memory:")).unwrap();
        let settings = Settings::default();
        std::fs::write(&path, line("long-original-id")).unwrap();
        scan_file(&mut db, &path, &settings, &mut None).unwrap();
        std::fs::write(&path, line("new")).unwrap();
        scan_file(&mut db, &path, &settings, &mut None).unwrap();
        scan_file(&mut db, &path, &settings, &mut None).unwrap();
        assert_eq!(store::history(&db, "", false, 0).unwrap().calls, 2);
    }

    #[test]
    fn replacement_before_restart_is_silent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut db = store::open(Path::new(":memory:")).unwrap();
        let settings = Settings {
            source: dir.path().to_string_lossy().into_owned(),
            ..Settings::default()
        };
        std::fs::write(&path, line("original-long-id")).unwrap();
        scan_file(&mut db, &path, &settings, &mut None).unwrap();
        std::fs::write(&path, line("offline")).unwrap();
        let mut restarted = Scanner::new();
        restarted.discover(&settings.source).unwrap();
        restarted.scan_file(&mut db, &path, &settings).unwrap();
        let history = store::history(&db, "offline", false, 0).unwrap();
        assert_eq!(history.total, 1);
        assert!(history.rows[0].imported);
        assert!(history.rows[0].alert.is_empty());
    }

    #[test]
    fn live_replacement_stays_live_across_batches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let mut db = store::open(Path::new(":memory:")).unwrap();
        let settings = Settings {
            source: dir.path().to_string_lossy().into_owned(),
            ..Settings::default()
        };
        std::fs::write(
            &path,
            format!("{}\n", " ".repeat((BATCH_BYTES * 3) as usize)),
        )
        .unwrap();
        let mut scanner = Scanner::new();
        scanner.discover(&settings.source).unwrap();
        scanner.scan_file(&mut db, &path, &settings).unwrap();
        let padding = format!("{{\"padding\":\"{}\"}}\n", "x".repeat(BATCH_BYTES as usize));
        std::fs::write(
            &path,
            format!("{}{}{}", line("live-first"), padding, line("live-second")),
        )
        .unwrap();
        scanner.scan_file(&mut db, &path, &settings).unwrap();
        scanner.scan_file(&mut db, &path, &settings).unwrap();
        assert_eq!(store::history(&db, "live-", true, 0).unwrap().total, 2);
    }
}
