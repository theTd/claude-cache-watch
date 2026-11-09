use crate::{store, titles, usage::Settings};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::SystemTime,
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
    unchanged: HashMap<PathBuf, (FileStamp, Option<FileStamp>)>,
    discovered: HashMap<PathBuf, (FileStamp, Option<FileStamp>)>,
    initialized: bool,
}

struct Baseline {
    length: u64,
    prefix: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    length: u64,
    modified: SystemTime,
}

impl FileStamp {
    fn from_metadata(metadata: &std::fs::Metadata) -> std::io::Result<Self> {
        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified()?,
        })
    }

    fn read(path: &Path) -> std::io::Result<Self> {
        let metadata = std::fs::metadata(path)?;
        Self::from_metadata(&metadata)
    }
}

fn file_stamps(path: &Path) -> Option<(FileStamp, Option<FileStamp>)> {
    let stamp = FileStamp::read(path).ok()?;
    let sidecar = if titles::identity(path).1.is_empty() {
        None
    } else {
        match FileStamp::read(&path.with_extension("meta.json")) {
            Ok(stamp) => Some(stamp),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            // Metadata is only an optimization; let the normal scan handle errors.
            Err(_) => return None,
        }
    };
    Some((stamp, sidecar))
}

struct ScanProgress {
    importing: bool,
    waiting_for_change: bool,
}

impl Scanner {
    pub fn new() -> Self {
        Self {
            source: String::new(),
            baseline: HashMap::new(),
            unchanged: HashMap::new(),
            discovered: HashMap::new(),
            initialized: false,
        }
    }

    pub fn discover(&mut self, source: &str) -> Result<Vec<PathBuf>, String> {
        if self.source != source {
            self.source = source.to_owned();
            self.initialized = false;
            self.baseline.clear();
            self.unchanged.clear();
        }
        self.discovered.clear();
        if !Path::new(source).is_dir() {
            return Err(format!("Log folder unavailable: {source}"));
        }
        let mut jsonl_entries: Vec<(PathBuf, Option<FileStamp>)> = Vec::new();
        let mut sidecar_stamps: HashMap<PathBuf, Option<FileStamp>> = HashMap::new();
        for entry in WalkDir::new(source).follow_links(false) {
            let entry = entry.map_err(|e| format!("Cannot discover logs: {e}"))?;
            if !entry.file_type().is_file() {
                continue;
            }
            let stamp = entry
                .metadata()
                .ok()
                .and_then(|m| FileStamp::from_metadata(&m).ok());
            let path = entry.into_path();
            if path.extension().is_some_and(|v| v == "jsonl") {
                jsonl_entries.push((path, stamp));
            } else if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".meta.json"))
            {
                sidecar_stamps.insert(path, stamp);
            }
        }
        for (path, stamp) in &jsonl_entries {
            let Some(stamp) = *stamp else {
                continue;
            };
            let sidecar = if titles::identity(path).1.is_empty() {
                None
            } else {
                match sidecar_stamps.get(&path.with_extension("meta.json")) {
                    Some(Some(sidecar)) => Some(*sidecar),
                    None => None,
                    Some(None) => continue,
                }
            };
            self.discovered.insert(path.clone(), (stamp, sidecar));
        }
        // Favor active sessions while progressively importing older logs.
        jsonl_entries.sort_by_key(|(_, stamp)| std::cmp::Reverse(stamp.map(|s| s.modified)));
        let paths: Vec<PathBuf> = jsonl_entries.into_iter().map(|(path, _)| path).collect();
        let present: HashSet<_> = paths.iter().collect();
        self.unchanged.retain(|path, _| present.contains(path));
        self.baseline.retain(|path, _| present.contains(path));
        if !self.initialized {
            for path in &paths {
                let mut file = File::open(path).map_err(|e| e.to_string())?;
                let length = match self.discovered.get(path) {
                    Some((s, _)) => s.length,
                    None => file.metadata().map_err(|e| e.to_string())?.len(),
                };
                let mut prefix = vec![0; length.min(256) as usize];
                file.read_exact(&mut prefix).map_err(|e| e.to_string())?;
                self.baseline
                    .insert(path.clone(), Some(Baseline { length, prefix }));
            }
            self.initialized = true;
        }
        Ok(paths)
    }

    pub fn is_unchanged(&mut self, path: &Path) -> bool {
        if let Some(stamps) = self.discovered.get(path) {
            if self.unchanged.get(path) == Some(stamps) {
                self.discovered.remove(path);
                return true;
            }
        }
        false
    }

    pub fn scan_file(
        &mut self,
        conn: &mut Connection,
        path: &Path,
        settings: &Settings,
    ) -> Result<bool, String> {
        // Inspect cheap metadata before opening transcripts, reading sidecars or querying SQLite.
        // Cache only successful scans that reached EOF (or an unfinished trailing line).
        let stamps = self.discovered.remove(path).or_else(|| file_stamps(path));
        if stamps
            .as_ref()
            .is_some_and(|stamps| self.unchanged.get(path) == Some(stamps))
        {
            return Ok(false);
        }
        self.unchanged.remove(path);
        let progress = scan_file(
            conn,
            path,
            settings,
            self.baseline.entry(path.to_path_buf()).or_default(),
        )?;
        if progress.waiting_for_change {
            if let Some(stamps) = stamps {
                self.unchanged.insert(path.to_path_buf(), stamps);
            }
        }
        Ok(progress.importing)
    }
}

use crate::file_fingerprint::processed as fingerprint;

fn scan_file(
    conn: &mut Connection,
    path: &Path,
    settings: &Settings,
    baseline: &mut Option<Baseline>,
) -> Result<ScanProgress, String> {
    // Title metadata is optional: report failures after processing the actual transcript.
    let metadata_error = titles::record_agent_sidecar(conn, path).err();
    let result = (|| -> Result<ScanProgress, Box<dyn std::error::Error>> {
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
        let replaced = offset > length
            || if old_signature.starts_with(b"CCW2") {
                fingerprint(&mut file, offset)? != old_signature
            } else {
                !prefix.starts_with(&old_signature)
            };
        if replaced {
            offset = 0;
        }
        if offset == length && !old_signature.is_empty() && !replaced {
            return Ok(ScanProgress {
                importing: false,
                waiting_for_change: true,
            });
        }
        file.seek(SeekFrom::Start(offset))?;
        let mut reader = BufReader::new(file);
        let tx = conn.transaction()?;
        let start = offset;
        let mut line = Vec::new();
        let mut partial_line = false;
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
                partial_line = true;
                break;
            }
            if let Ok(value) = serde_json::from_slice(&line) {
                titles::record_line(&tx, &value, path)?;
                if let Some(mut usage) = crate::claude::parse(&value) {
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
            params![path_text.as_ref(), i64::try_from(offset)?, fingerprint(reader.get_mut(),offset)?])?;
        tx.commit()?;
        Ok(ScanProgress {
            importing: offset < import_until && offset < length,
            waiting_for_change: offset == length || partial_line,
        })
    })();
    match result {
        Ok(progress) => match metadata_error {
            Some(error) => Err(error),
            None => Ok(progress),
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
    fn unchanged_files_do_not_query_sqlite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, line("original")).unwrap();
        let mut db = store::open(Path::new(":memory:")).unwrap();
        let mut scanner = Scanner::new();
        let settings = Settings {
            source: dir.path().to_string_lossy().into_owned(),
            ..Settings::default()
        };
        let discovered = scanner.discover(&settings.source).unwrap();
        assert_eq!(discovered.len(), 1);
        assert!(!scanner.is_unchanged(&path));
        scanner.scan_file(&mut db, &path, &settings).unwrap();
        // Any cursor lookup would fail: an unchanged file must not touch SQLite at all.
        db.execute("DROP TABLE cursors", []).unwrap();
        let discovered = scanner.discover(&settings.source).unwrap();
        assert_eq!(discovered.len(), 1);
        assert!(scanner.is_unchanged(&path));
        scanner.scan_file(&mut db, &path, &settings).unwrap();
        std::fs::write(&path, line("changed-content")).unwrap();
        let _ = scanner.discover(&settings.source).unwrap();
        assert!(!scanner.is_unchanged(&path));
        assert!(scanner.scan_file(&mut db, &path, &settings).is_err());
    }

    #[test]
    fn sidecars_are_refreshed_without_transcript_changes_and_errors_retry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1/subagents/agent-a1.jsonl");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, line("call")).unwrap();
        let mut db = store::open(Path::new(":memory:")).unwrap();
        let mut scanner = Scanner::new();
        let settings = Settings::default();
        scanner.scan_file(&mut db, &path, &settings).unwrap();
        std::fs::write(path.with_extension("meta.json"), "partial JSON").unwrap();
        for _ in 0..2 {
            assert!(scanner.scan_file(&mut db, &path, &settings).is_err());
        }
        std::fs::write(
            path.with_extension("meta.json"),
            r#"{"description":"Recovered title"}"#,
        )
        .unwrap();
        scanner.scan_file(&mut db, &path, &settings).unwrap();
        assert_eq!(
            store::history(&db, "Recovered title", false, 0)
                .unwrap()
                .total,
            1
        );
        std::fs::write(
            path.with_extension("meta.json"),
            r#"{"description":"New agent title"}"#,
        )
        .unwrap();
        scanner.scan_file(&mut db, &path, &settings).unwrap();
        assert_eq!(
            store::history(&db, "New agent title", false, 0)
                .unwrap()
                .total,
            1
        );
    }

    #[test]
    fn same_length_rewrites_are_detected_by_modification_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, line("old")).unwrap();
        let old_time = std::fs::metadata(&path).unwrap().modified().unwrap();
        let mut db = store::open(Path::new(":memory:")).unwrap();
        let mut scanner = Scanner::new();
        scanner
            .scan_file(&mut db, &path, &Settings::default())
            .unwrap();
        std::fs::write(&path, line("new")).unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(old_time + std::time::Duration::from_secs(1)),
            )
            .unwrap();
        scanner
            .scan_file(&mut db, &path, &Settings::default())
            .unwrap();
        assert_eq!(store::history(&db, "", false, 0).unwrap().calls, 2);
    }

    #[test]
    fn edits_after_an_unchanged_prefix_are_replayed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let prefix = format!("{{\"padding\":\"{}\"}}\n", "x".repeat(1024));
        std::fs::write(&path, format!("{prefix}{}", line("old"))).unwrap();
        let mut db = store::open(Path::new(":memory:")).unwrap();
        let mut scanner = Scanner::new();
        scanner
            .scan_file(&mut db, &path, &Settings::default())
            .unwrap();
        std::fs::write(&path, format!("{prefix}{}", line("new"))).unwrap();
        scanner
            .scan_file(&mut db, &path, &Settings::default())
            .unwrap();
        assert_eq!(store::history(&db, "", false, 0).unwrap().calls, 2);
    }

    #[test]
    fn historical_batches_continue_even_when_file_metadata_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        let padding = format!("{{\"padding\":\"{}\"}}\n", "x".repeat(BATCH_BYTES as usize));
        std::fs::write(
            &path,
            format!("{}{}{}", line("first"), padding, line("last")),
        )
        .unwrap();
        let settings = Settings {
            source: dir.path().to_string_lossy().into_owned(),
            ..Settings::default()
        };
        let mut db = store::open(Path::new(":memory:")).unwrap();
        let mut scanner = Scanner::new();
        scanner.discover(&settings.source).unwrap();
        assert!(scanner.scan_file(&mut db, &path, &settings).unwrap());
        assert!(!scanner.scan_file(&mut db, &path, &settings).unwrap());
        let history = store::history(&db, "", false, 0).unwrap();
        assert_eq!(history.calls, 2);
        assert!(history
            .rows
            .iter()
            .all(|row| row.imported && row.alert.is_empty()));
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
