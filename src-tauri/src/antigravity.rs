use crate::{
    protobuf as pb, store,
    usage::{namespace, Settings, SourceConfig, Usage},
};
use rusqlite::{params, Connection, OpenFlags};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const MAX_BLOB: usize = 16 * 1024 * 1024;
const MAX_RECORDS: usize = 4096;
type Stamp = Vec<Option<(u64, SystemTime)>>;

pub fn open_readonly(path: &Path) -> Result<Connection, String> {
    let c = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| e.to_string())?;
    c.busy_timeout(Duration::from_millis(100))
        .map_err(|e| e.to_string())?;
    Ok(c)
}
fn stamp(paths: &[PathBuf]) -> Stamp {
    paths
        .iter()
        .map(|p| {
            fs::metadata(p)
                .ok()
                .and_then(|m| Some((m.len(), m.modified().ok()?)))
        })
        .collect()
}
fn wal(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}-wal", path.display()))
}
fn scalar(m: &pb::Message<'_>, k: u32) -> Result<i64, String> {
    i64::try_from(pb::number(m, k)?).map_err(|_| "Token counter exceeds SQLite INTEGER".into())
}

// Values verified against the descriptor embedded in the installed language_server.exe.
fn terminal_state(status: i64) -> Option<&'static str> {
    match status {
        3 => Some("DONE"),
        4 => Some("INVALID"),
        5 => Some("CLEARED"),
        6 => Some("CANCELLED"),
        7 => Some("ERROR"),
        12 => Some("INTERRUPTED"),
        _ => None,
    }
}

#[derive(Default, Clone, PartialEq)]
struct SessionInfo {
    title: String,
    project: String,
    parent: String,
    role: String,
}
#[derive(Clone)]
struct Model {
    id: String,
    enum_id: u64,
}

pub struct Scanner {
    started_ms: i64,
    stamps: HashMap<PathBuf, Stamp>,
    progress: HashMap<PathBuf, i64>,
    progress_stamps: HashMap<PathBuf, Stamp>,
    progress_fingerprints: HashMap<PathBuf, Vec<u8>>,
    rescan_after: HashSet<PathBuf>,
    info: BTreeMap<String, SessionInfo>,
    dirty_info: HashSet<String>,
    metadata_stamp: Stamp,
    metadata_ready: bool,
    pub files: usize,
    pub importing: bool,
    pub errors: Vec<String>,
}

impl Default for Scanner {
    fn default() -> Self {
        Self {
            started_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64,
            stamps: HashMap::new(),
            progress: HashMap::new(),
            progress_stamps: HashMap::new(),
            progress_fingerprints: HashMap::new(),
            rescan_after: HashSet::new(),
            info: BTreeMap::new(),
            dirty_info: HashSet::new(),
            metadata_stamp: Vec::new(),
            metadata_ready: false,
            files: 0,
            importing: false,
            errors: Vec::new(),
        }
    }
}

impl Scanner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn scan(
        &mut self,
        target: &mut Connection,
        source: &SourceConfig,
        settings: &Settings,
    ) -> Result<bool, String> {
        self.files = 0;
        self.importing = false;
        self.errors.clear();
        let root = Path::new(&source.path);
        if !root.is_dir() {
            return Err(format!(
                "{} folder unavailable: {}",
                source.kind, source.path
            ));
        }
        let summary = root.join("conversation_summaries.db");
        let mut metadata_paths = vec![summary.clone(), wal(&summary), root.join("brain")];
        if root.join("brain").is_dir() {
            for entry in fs::read_dir(root.join("brain")).map_err(|e| e.to_string())? {
                let descriptors = entry
                    .map_err(|e| e.to_string())?
                    .path()
                    .join(".system_generated/subagents");
                metadata_paths.push(descriptors.clone());
                if descriptors.is_dir() {
                    for child in fs::read_dir(descriptors).map_err(|e| e.to_string())? {
                        metadata_paths.push(child.map_err(|e| e.to_string())?.path());
                    }
                }
            }
        }
        metadata_paths.sort();
        let current_meta = stamp(&metadata_paths);
        let refresh_meta = !self.metadata_ready || self.metadata_stamp != current_meta;
        if refresh_meta {
            match load_info(root) {
                Ok(info) => {
                    for sid in info.keys().chain(self.info.keys()) {
                        if info.get(sid) != self.info.get(sid) {
                            self.dirty_info.insert(sid.clone());
                        }
                    }
                    self.info = info;
                    self.metadata_stamp = current_meta;
                    self.metadata_ready = true;
                }
                Err(e) => self.errors.push(format!("Session metadata: {e}")),
            }
        }
        // Discovery is limited to the two documented directories, never the scratch/tool output tree.
        let mut sessions: BTreeMap<String, (Option<PathBuf>, Option<PathBuf>)> = BTreeMap::new();
        if root.join("conversations").is_dir() {
            for entry in fs::read_dir(root.join("conversations")).map_err(|e| e.to_string())? {
                let p = entry.map_err(|e| e.to_string())?.path();
                if p.extension().is_some_and(|v| v == "db") {
                    if let Some(id) = p.file_stem().and_then(|v| v.to_str()) {
                        sessions.entry(id.into()).or_default().0 = Some(p.clone());
                    }
                }
            }
        }
        if root.join("brain").is_dir() {
            for entry in fs::read_dir(root.join("brain")).map_err(|e| e.to_string())? {
                let p = entry.map_err(|e| e.to_string())?.path();
                let transcript = p.join(".system_generated/logs/transcript.jsonl");
                if transcript.is_file() {
                    if let Some(id) = p.file_name().and_then(|v| v.to_str()) {
                        sessions.entry(id.into()).or_default().1 = Some(transcript);
                    }
                }
            }
        }
        let mut changed = false;
        for (sid, (db, transcript)) in sessions {
            self.files += 1;
            let path = db
                .as_ref()
                .or(transcript.as_ref())
                .expect("discovered session");
            let descriptors = root
                .join("brain")
                .join(&sid)
                .join(".system_generated/subagents");
            let current = stamp(&[path.clone(), wal(path), descriptors]);
            if self
                .progress_stamps
                .get(path)
                .is_some_and(|old| old != &current)
            {
                if db.is_some() {
                    // Finish this bounded pass so continuous WAL appends cannot starve later steps.
                    // Then revisit older steps that may have changed during the pass.
                    self.rescan_after.insert(path.clone());
                } else {
                    let offset = self.progress.get(path).copied().unwrap_or(0).max(0) as u64;
                    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
                    let same = file.metadata().map_err(|e| e.to_string())?.len() >= offset
                        && self.progress_fingerprints.get(path)
                            == Some(
                                &crate::file_fingerprint::processed(&mut file, offset)
                                    .map_err(|e| e.to_string())?,
                            );
                    if !same {
                        self.progress.remove(path);
                    }
                }
            }
            if self.stamps.get(path) == Some(&current)
                && !self.progress.contains_key(path)
                && !self.dirty_info.contains(&sid)
            {
                continue;
            }
            let info = self.info.get(&sid).cloned().unwrap_or_default();
            // A broken/locked native DB is retried, never silently replaced with a weaker source.
            let result = if let Some(db) = db.as_ref() {
                self.scan_database(target, source, &sid, db, &info, settings)
            } else {
                self.scan_transcript(target, source, &sid, path, &info, settings)
            };
            match result {
                Ok(done) => {
                    changed = true;
                    if done {
                        if self.rescan_after.remove(path) {
                            self.importing = true;
                        } else {
                            self.stamps.insert(path.clone(), current);
                        }
                        self.progress_stamps.remove(path);
                        self.dirty_info.remove(&sid);
                    } else {
                        self.progress_stamps.insert(path.clone(), current);
                        self.importing = true;
                    }
                }
                Err(e) => {
                    if self.errors.len() < 3 {
                        self.errors.push(format!("{sid}: {e}"));
                    }
                }
            }
        }
        if !self.errors.is_empty() {
            return Err(self.errors.join("\n"));
        }
        Ok(changed || refresh_meta)
    }

    fn scan_database(
        &mut self,
        target: &mut Connection,
        source: &SourceConfig,
        sid: &str,
        path: &Path,
        info: &SessionInfo,
        settings: &Settings,
    ) -> Result<bool, String> {
        let native = open_readonly(path)?;
        let read = native.unchecked_transaction().map_err(|e| e.to_string())?;
        let mut models: HashMap<i64, Model> = HashMap::new();
        let mut metadata_error = None;
        // Fetch only the model-related portion; data blobs themselves are never persisted.
        let mut q = read
            .prepare("SELECT data,length(data) FROM gen_metadata")
            .map_err(|e| e.to_string())?;
        let mut rows = q.query([]).map_err(|e| e.to_string())?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            if row.get::<_, i64>(1).map_err(|e| e.to_string())? > MAX_BLOB as i64 {
                return Err("Generation metadata exceeds 16 MiB".into());
            }
            let blob: Vec<u8> = row.get(0).map_err(|e| e.to_string())?;
            let decoded = (|| -> Result<(), String> {
                let generation = pb::decode(&blob)?;
                let chat =
                    pb::decode(pb::bytes(&generation, 1).ok_or("Generation has no ChatModel")?)?;
                let usage = pb::decode(pb::bytes(&chat, 4).unwrap_or_default())?;
                let model = Model {
                    id: pb::text(&chat, 19).unwrap_or_default(),
                    enum_id: pb::number(&usage, 1)?,
                };
                for idx in pb::packed(&generation, 2)? {
                    models
                        .entry(i64::try_from(idx).map_err(|_| "Step index overflow")?)
                        .and_modify(|previous| {
                            if previous.id != model.id || previous.enum_id != model.enum_id {
                                previous.id.clear();
                            }
                        })
                        .or_insert_with(|| model.clone());
                }
                Ok(())
            })();
            if let Err(e) = decoded {
                metadata_error = Some(e);
            }
        }
        drop(rows);
        drop(q);
        let after = self.progress.get(path).copied().unwrap_or(-1);
        let tx = target.transaction().map_err(|e| e.to_string())?;
        save_info(&tx, source, sid, info).map_err(|e| e.to_string())?;
        let mut q=read.prepare("SELECT idx,step_type,status,metadata,length(metadata) FROM steps WHERE idx>?1 ORDER BY idx LIMIT ?2").map_err(|e|e.to_string())?;
        let mut rows = q
            .query(params![after, MAX_RECORDS as i64])
            .map_err(|e| e.to_string())?;
        let mut count = 0;
        let mut last = after;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let idx: i64 = row.get(0).map_err(|e| e.to_string())?;
            last = idx;
            count += 1;
            let typ: i64 = row.get(1).map_err(|e| e.to_string())?;
            let status: i64 = row.get(2).map_err(|e| e.to_string())?;
            // Status 5 retains completed historical steps after rewind; local transcripts say DONE.
            // ACTIVE and unconfirmed terminal states never enter alert evaluation.
            let Some(state) = terminal_state(status) else {
                continue;
            };
            if row
                .get::<_, Option<i64>>(4)
                .map_err(|e| e.to_string())?
                .unwrap_or(0)
                > MAX_BLOB as i64
            {
                return Err("Step metadata exceeds 16 MiB".into());
            }
            let blob: Option<Vec<u8>> = row.get(3).map_err(|e| e.to_string())?;
            let metadata = pb::decode(blob.as_deref().unwrap_or_default())?;
            let usage = pb::decode(pb::bytes(&metadata, 9).unwrap_or_default())?;
            // Field 1 is a numeric model enum; require at least a recorded counter field.
            let recognized = [2, 3, 4, 5].iter().any(|k| usage.contains_key(k));
            if !recognized && !matches!(typ, 15 | 23) {
                continue;
            }
            let input = scalar(&usage, 2)?;
            let output = scalar(&usage, 3)?;
            let creation = scalar(&usage, 4)?;
            let cached = scalar(&usage, 5)?;
            input
                .checked_add(creation)
                .and_then(|n| n.checked_add(cached))
                .ok_or("Token sum overflow")?;
            let enum_id = pb::number(&usage, 1)?;
            let direct = models.get(&idx).filter(|m| {
                !m.id.is_empty() && (!recognized || m.enum_id == enum_id || m.enum_id == 0)
            });
            let model = direct.map(|m| m.id.clone()).unwrap_or_default();
            let time = pb::decode(pb::bytes(&metadata, 1).unwrap_or_default())?;
            let seconds = pb::number(&time, 1)?;
            let nanos = pb::number(&time, 2)?;
            if nanos >= 1_000_000_000 {
                return Err("Invalid protobuf timestamp nanoseconds".into());
            }
            let timestamp = if seconds == 0 {
                String::new()
            } else {
                read.query_row(
                    "SELECT strftime('%Y-%m-%dT%H:%M:%fZ',?1,'unixepoch')",
                    [seconds as f64 + nanos as f64 / 1e9],
                    |r| r.get::<_, Option<String>>(0),
                )
                .map_err(|e| e.to_string())?
                .unwrap_or_default()
            };
            let mut u = base_usage(source, sid, idx, info);
            u.timestamp = timestamp;
            u.model = model;
            u.input = input;
            u.creation = creation;
            u.read = cached;
            u.output = output;
            u.kind = match typ {
                15 => "response",
                23 => "checkpoint",
                _ => "other",
            }
            .into();
            u.state = state.into();
            u.usage_complete = recognized;
            u.cache_known = recognized;
            u.creation_known = recognized;
            u.metadata_source = "antigravity_sqlite".into();
            if u.model.is_empty() {
                u.model = format!("antigravity-enum-{enum_id}");
            }
            let imported = seconds == 0
                || (seconds as i128 * 1000 + nanos as i128 / 1_000_000)
                    <= i128::from(self.started_ms);
            store::record(&tx, &u, &path.to_string_lossy(), imported, settings)
                .map_err(|e| e.to_string())?;
        }
        drop(rows);
        drop(q);
        read.commit().map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        // Re-read changed sessions from the beginning after a full pass: existing steps can be updated.
        if count == MAX_RECORDS {
            self.progress.insert(path.to_owned(), last);
        } else {
            self.progress.remove(path);
        }
        if let Some(e) = metadata_error {
            return Err(format!("Model metadata unsupported; usage retained: {e}"));
        }
        Ok(count < MAX_RECORDS)
    }

    fn scan_transcript(
        &mut self,
        target: &mut Connection,
        source: &SourceConfig,
        sid: &str,
        path: &Path,
        info: &SessionInfo,
        settings: &Settings,
    ) -> Result<bool, String> {
        use std::io::{Seek, SeekFrom};
        let offset = self.progress.get(path).copied().unwrap_or(0).max(0) as u64;
        let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| e.to_string())?;
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        let mut bytes = offset;
        let mut count = 0;
        let tx = target.transaction().map_err(|e| e.to_string())?;
        save_info(&tx, source, sid, info).map_err(|e| e.to_string())?;
        loop {
            line.clear();
            let n = reader
                .by_ref()
                .take(MAX_BLOB as u64 + 1)
                .read_until(b'\n', &mut line)
                .map_err(|e| e.to_string())?;
            if n > MAX_BLOB {
                return Err("Transcript line exceeds 16 MiB".into());
            }
            if n == 0 || line.last() != Some(&b'\n') {
                break;
            }
            bytes += n as u64;
            count += 1;
            if let Ok(v) = serde_json::from_slice(&line) {
                if let Some(u) = parse_transcript(source, sid, &v, info) {
                    let ms: Option<f64> = tx
                        .query_row(
                            "SELECT (julianday(?1)-2440587.5)*86400000",
                            [&u.timestamp],
                            |r| r.get(0),
                        )
                        .map_err(|e| e.to_string())?;
                    let imported = ms.is_none_or(|ms| ms <= self.started_ms as f64);
                    store::record(&tx, &u, &path.to_string_lossy(), imported, settings)
                        .map_err(|e| e.to_string())?;
                }
            }
            if count >= MAX_RECORDS {
                break;
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        if count >= MAX_RECORDS {
            self.progress.insert(path.to_owned(), bytes as i64);
            self.progress_fingerprints.insert(
                path.to_owned(),
                crate::file_fingerprint::processed(reader.get_mut(), bytes)
                    .map_err(|e| e.to_string())?,
            );
        } else {
            self.progress.remove(path);
            self.progress_fingerprints.remove(path);
        }
        Ok(count < MAX_RECORDS)
    }
}

fn base_usage(source: &SourceConfig, sid: &str, idx: i64, info: &SessionInfo) -> Usage {
    let session = namespace(&source.kind, &source.path, sid);
    let parent = if info.parent.is_empty() {
        String::new()
    } else {
        namespace(&source.kind, &source.path, &info.parent)
    };
    Usage {
        id: format!("{session}:step:{idx}"),
        source_kind: source.kind.clone(),
        raw_session: sid.into(),
        session,
        parent_session: parent,
        agent_id: if info.parent.is_empty() {
            String::new()
        } else {
            sid.into()
        },
        project: info.project.clone(),
        timestamp: String::new(),
        model: String::new(),
        input: 0,
        creation: 0,
        read: 0,
        output: 0,
        billing: Default::default(),
        kind: "response".into(),
        state: "DONE".into(),
        usage_complete: false,
        cache_known: false,
        creation_known: false,
        metadata_source: "antigravity_transcript".into(),
    }
}

fn parse_transcript(
    source: &SourceConfig,
    sid: &str,
    v: &Value,
    info: &SessionInfo,
) -> Option<Usage> {
    let typ = v.get("type")?.as_str()?;
    if !matches!(typ, "PLANNER_RESPONSE" | "CHECKPOINT") {
        return None;
    }
    let state = v.get("status")?.as_str()?;
    if !matches!(
        state,
        "DONE" | "CLEARED" | "CANCELED" | "CANCELLED" | "ERROR" | "INTERRUPTED" | "INVALID"
    ) {
        return None;
    }
    let idx = v.get("step_index")?.as_i64()?;
    if idx < 0 {
        return None;
    }
    let mut u = base_usage(source, sid, idx, info);
    u.timestamp = v
        .get("created_at")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .into();
    u.state = if state == "CANCELED" {
        "CANCELLED"
    } else {
        state
    }
    .into();
    u.kind = if typ == "CHECKPOINT" {
        "checkpoint"
    } else {
        "response"
    }
    .into();
    for key in ["input_tokens", "cache_read_tokens", "output_tokens"] {
        if v.get(key).and_then(Value::as_i64).is_some_and(|n| n < 0) {
            return None;
        }
    }
    let input = v
        .get("input_tokens")
        .and_then(Value::as_i64)
        .filter(|n| *n >= 0);
    u.input = input.unwrap_or(0);
    u.output = v
        .get("output_tokens")
        .and_then(Value::as_i64)
        .filter(|n| *n >= 0)
        .unwrap_or(0);
    u.cache_known = v
        .get("cache_read_tokens")
        .and_then(Value::as_i64)
        .is_some_and(|n| n >= 0);
    u.read = v
        .get("cache_read_tokens")
        .and_then(Value::as_i64)
        .filter(|n| *n >= 0)
        .unwrap_or(0);
    u.usage_complete = input.is_some()
        && v.get("output_tokens")
            .and_then(Value::as_i64)
            .is_some_and(|n| n >= 0)
        && u.cache_known;
    u.input.checked_add(u.read)?;
    Some(u)
}

fn load_info(root: &Path) -> Result<BTreeMap<String, SessionInfo>, String> {
    let mut info = BTreeMap::new();
    let path = root.join("conversation_summaries.db");
    if path.is_file() {
        let c = open_readonly(&path)?;
        let mut q=c.prepare("SELECT conversation_id,title,workspace_uris,parent_conversation_id,agent_name FROM conversation_summaries").map_err(|e|e.to_string())?;
        let rows = q
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            let (sid, title, workspace, parent, role) = row.map_err(|e| e.to_string())?;
            let project = serde_json::from_str::<Vec<String>>(&workspace)
                .ok()
                .and_then(|v| v.into_iter().next())
                .unwrap_or(workspace);
            info.insert(
                sid,
                SessionInfo {
                    title,
                    project,
                    parent,
                    role,
                },
            );
        }
    }
    // Explicit descriptors also work when no summary DB exists (e.g. transcript-only sessions).
    if root.join("brain").is_dir() {
        for entry in fs::read_dir(root.join("brain")).map_err(|e| e.to_string())? {
            let parent = entry.map_err(|e| e.to_string())?.path();
            let descriptors = parent.join(".system_generated/subagents");
            if !descriptors.is_dir() {
                continue;
            }
            for entry in fs::read_dir(descriptors).map_err(|e| e.to_string())? {
                let path = entry.map_err(|e| e.to_string())?.path();
                if path.extension().is_none_or(|v| v != "json") {
                    continue;
                }
                if fs::metadata(&path).map_err(|e| e.to_string())?.len() > 1024 * 1024 {
                    return Err("Subagent descriptor exceeds 1 MiB".into());
                }
                let v: Value = serde_json::from_slice(&fs::read(&path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
                let Some(child) = v
                    .get("conversationId")
                    .and_then(Value::as_str)
                    .or_else(|| path.file_stem().and_then(|s| s.to_str()))
                else {
                    continue;
                };
                let item = info
                    .entry(child.into())
                    .or_insert_with(SessionInfo::default);
                item.parent = parent
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let descriptor = v.get("subagentDescriptor").unwrap_or(&Value::Null);
                item.role = descriptor
                    .get("role")
                    .and_then(Value::as_str)
                    .or_else(|| descriptor.get("typeName").and_then(Value::as_str))
                    .unwrap_or_default()
                    .into();
                if item.project.is_empty() {
                    item.project = v
                        .get("workspaceUris")
                        .and_then(Value::as_array)
                        .and_then(|a| a.first())
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .into();
                }
            }
        }
    }
    Ok(info)
}

fn save_info(
    c: &Connection,
    source: &SourceConfig,
    sid: &str,
    info: &SessionInfo,
) -> rusqlite::Result<()> {
    let session = namespace(&source.kind, &source.path, sid);
    c.execute("INSERT INTO sessions(id,ai_title) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET ai_title=excluded.ai_title",params![session,info.title])?;
    if !info.parent.is_empty() {
        c.execute("INSERT INTO agents(session,id,title) VALUES(?1,?2,?3) ON CONFLICT(session,id) DO UPDATE SET title=excluded.title",params![session,sid,info.role])?;
    }
    Ok(())
}

/// Verify native usage against the independent JSONL representation without storing prompt content.
pub fn crosscheck(root: &Path) -> Result<serde_json::Value, String> {
    let mut sessions = 0;
    let mut calls = 0;
    let mut incomplete = 0;
    let mut missing = 0;
    let mut mismatches = 0;
    let mut input = 0i64;
    let mut cached = 0i64;
    let mut output = 0i64;
    for entry in fs::read_dir(root.join("conversations")).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().is_none_or(|s| s != "db") {
            continue;
        }
        let sid = path.file_stem().unwrap_or_default().to_string_lossy();
        let transcript = root
            .join("brain")
            .join(sid.as_ref())
            .join(".system_generated/logs/transcript.jsonl");
        let mut expected = HashMap::new();
        if transcript.is_file() {
            for line in
                BufReader::new(fs::File::open(transcript).map_err(|e| e.to_string())?).lines()
            {
                let line = line.map_err(|e| e.to_string())?;
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if let (Some(idx), Some(i), Some(r), Some(o)) = (
                        v["step_index"].as_i64(),
                        v["input_tokens"].as_i64(),
                        v["cache_read_tokens"].as_i64(),
                        v["output_tokens"].as_i64(),
                    ) {
                        expected.insert(idx, (i, r, o));
                    }
                }
            }
        }
        let c = open_readonly(&path)?;
        let tx = c.unchecked_transaction().map_err(|e| e.to_string())?;
        let mut q = tx
            .prepare("SELECT idx,metadata,step_type FROM steps WHERE status IN (3,4,5,6,7,12)")
            .map_err(|e| e.to_string())?;
        let mut rows = q.query([]).map_err(|e| e.to_string())?;
        while let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let idx: i64 = row.get(0).map_err(|e| e.to_string())?;
            let b: Option<Vec<u8>> = row.get(1).map_err(|e| e.to_string())?;
            let m = pb::decode(b.as_deref().unwrap_or_default())?;
            let u = pb::decode(pb::bytes(&m, 9).unwrap_or_default())?;
            if ![2, 3, 4, 5].iter().any(|k| u.contains_key(k)) {
                if matches!(row.get::<_, i64>(2).map_err(|e| e.to_string())?, 15 | 23) {
                    incomplete += 1;
                }
                continue;
            }
            let actual = (scalar(&u, 2)?, scalar(&u, 5)?, scalar(&u, 3)?);
            calls += 1;
            input += actual.0;
            cached += actual.1;
            output += actual.2;
            match expected.remove(&idx) {
                Some(e) if e != actual => mismatches += 1,
                None => missing += 1,
                _ => {}
            }
        }
        mismatches += expected.len();
        sessions += 1;
    }
    Ok(
        serde_json::json!({"sessions":sessions,"native_calls":calls,"native_incomplete_calls":incomplete,"input_tokens":input,"cache_read_tokens":cached,"output_tokens":output,"native_only_calls":missing,"mismatched_or_transcript_only_calls":mismatches}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn captured_native_usage_matches_history_statistics_and_export() {
        let fixture: Value =
            serde_json::from_str(include_str!("../fixtures/antigravity-native.json")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let source = source(dir.path());
        let c = database(dir.path(), "example");
        for row in fixture["steps"].as_array().unwrap() {
            let hex = row["metadata_hex"].as_str().unwrap();
            let blob = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect::<Vec<_>>();
            c.execute(
                "INSERT INTO steps VALUES(?1,?2,?3,?4)",
                params![
                    row["idx"].as_i64().unwrap(),
                    row["step_type"].as_i64().unwrap(),
                    row["status"].as_i64().unwrap(),
                    blob
                ],
            )
            .unwrap();
        }
        let mut target = store::open(Path::new(":memory:")).unwrap();
        Scanner::new()
            .scan(&mut target, &source, &Settings::default())
            .unwrap();
        let h = store::history(&target, "", false, 0).unwrap();
        assert_eq!(h.calls, 4);
        for (idx, expected) in [(421, 183271), (423, 0), (425, 186011)] {
            assert_eq!(
                h.rows
                    .iter()
                    .find(|r| r.id.ends_with(&format!(":{idx}")))
                    .unwrap()
                    .read,
                expected
            );
        }
        let filter = store::Filter {
            source: source.kind,
            kind: "checkpoint".into(),
        };
        let exported =
            serde_json::to_value(store::history_filtered(&target, "", false, 0, &filter).unwrap())
                .unwrap();
        assert_eq!(exported["rows"][0]["input"], 262543);
        assert_eq!(exported["rows"][0]["kind"], "checkpoint");
        assert_eq!(exported["rows"][0]["metadata_source"], "antigravity_sqlite");
        let stats = crate::statistics::calculate_filtered(
            &target,
            None,
            1_800_000_000_000,
            &Settings::default(),
            &filter,
        )
        .unwrap();
        assert_eq!(stats.metrics.calls, 1);
        assert_eq!(stats.metrics.ordinary_input, 262543);
        assert_eq!(stats.metrics.output, 6341);
        assert_eq!(
            stats
                .trend
                .points
                .iter()
                .map(|p| p.metrics.ordinary_input)
                .sum::<i64>(),
            262543
        );
        assert!(h
            .rows
            .iter()
            .all(|r| r.model.starts_with("antigravity-enum-") && r.estimated_billing.is_none()));
    }
    fn var(mut n: u64) -> Vec<u8> {
        let mut b = Vec::new();
        loop {
            let v = (n & 127) as u8;
            n >>= 7;
            b.push(v | if n > 0 { 128 } else { 0 });
            if n == 0 {
                return b;
            }
        }
    }
    fn n(k: u32, v: u64) -> Vec<u8> {
        let mut b = var(u64::from(k) << 3);
        b.extend(var(v));
        b
    }
    fn b(k: u32, v: &[u8]) -> Vec<u8> {
        let mut b = var((u64::from(k) << 3) | 2);
        b.extend(var(v.len() as u64));
        b.extend(v);
        b
    }
    fn metadata(i: u64, r: u64, o: u64) -> Vec<u8> {
        let time = [n(1, 1_794_189_634), n(2, 123_000_000)].concat();
        // The captured schema omits default-valued cache reads instead of writing field 5=0.
        let mut usage = [n(1, 1417), n(2, i), n(3, o)].concat();
        if r > 0 {
            usage.extend(n(5, r));
        }
        [b(1, &time), b(9, &usage)].concat()
    }
    fn source(dir: &Path) -> SourceConfig {
        SourceConfig {
            kind: "antigravity_ide".into(),
            path: dir.to_string_lossy().into_owned(),
            enabled: true,
        }
    }
    fn database(root: &Path, sid: &str) -> Connection {
        fs::create_dir_all(root.join("conversations")).unwrap();
        let c = Connection::open(root.join("conversations").join(format!("{sid}.db"))).unwrap();
        c.execute_batch("PRAGMA journal_mode=WAL;CREATE TABLE gen_metadata(idx INTEGER PRIMARY KEY,data BLOB);CREATE TABLE steps(idx INTEGER PRIMARY KEY,step_type INTEGER,status INTEGER,metadata BLOB);").unwrap();
        c
    }
    fn generation(c: &Connection, idx: i64, steps: &[u64], model: &str) {
        let usage = n(1, 1417);
        let chat = [b(4, &usage), b(19, model.as_bytes())].concat();
        let packed: Vec<u8> = steps.iter().flat_map(|s| var(*s)).collect();
        c.execute(
            "INSERT INTO gen_metadata VALUES(?1,?2)",
            params![idx, [b(1, &chat), b(2, &packed)].concat()],
        )
        .unwrap();
    }
    fn step(c: &Connection, idx: i64, typ: i64, state: i64, i: u64, r: u64, o: u64) {
        c.execute(
            "INSERT OR REPLACE INTO steps VALUES(?1,?2,?3,?4)",
            params![idx, typ, state, metadata(i, r, o)],
        )
        .unwrap();
    }
    #[test]
    fn native_wal_compaction_cancelled_and_duplicate_generation_links_count_once() {
        let dir = tempfile::tempdir().unwrap();
        let source = source(dir.path());
        let c = database(dir.path(), "session");
        generation(&c, 0, &[252, 421, 423, 425, 430], "gemini-4-argon");
        generation(&c, 1, &[423], "gemini-4-argon");
        step(&c, 421, 15, 3, 2310, 183271, 356);
        step(&c, 423, 15, 3, 186010, 0, 271);
        step(&c, 425, 15, 3, 4167, 186011, 2749);
        step(&c, 252, 23, 3, 262543, 0, 6341);
        step(&c, 430, 15, 6, 100005, 0, 12);
        step(&c, 431, 15, 2, 900000, 0, 9);
        let mut target = store::open(Path::new(":memory:")).unwrap();
        let mut scanner = Scanner::new();
        scanner
            .scan(&mut target, &source, &Settings::default())
            .unwrap();
        let h = store::history(&target, "", false, 0).unwrap();
        assert_eq!(h.calls, 5);
        assert_eq!(h.alerts, 0);
        assert!(h
            .rows
            .iter()
            .all(|r| r.imported && r.cache_known && r.model == "gemini-4-argon"));
        assert_eq!(h.rows.iter().filter(|r| r.kind == "checkpoint").count(), 1);
        assert_eq!(h.rows.iter().filter(|r| r.state == "CANCELLED").count(), 1);
        scanner.stamps.clear();
        scanner
            .scan(&mut target, &source, &Settings::default())
            .unwrap();
        assert_eq!(store::history(&target, "", false, 0).unwrap().calls, 5);
        // The input DB remains open in WAL mode and has not checkpointed the live step away.
        assert!(wal(&dir.path().join("conversations/session.db")).is_file());
        assert_eq!(
            c.query_row("SELECT COUNT(*) FROM steps", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            6
        );
    }
    #[test]
    fn transcript_promotes_to_native_without_duplicate_or_realert_and_unknown_is_not_zero() {
        let dir = tempfile::tempdir().unwrap();
        let source = source(dir.path());
        let logs = dir.path().join("brain/s/.system_generated/logs");
        fs::create_dir_all(&logs).unwrap();
        let records = [
            serde_json::json!({"type":"PLANNER_RESPONSE","status":"DONE","step_index":1,"created_at":"2026-11-09T02:00:34Z","input_tokens":186010,"cache_read_tokens":0,"output_tokens":271}),
            serde_json::json!({"type":"PLANNER_RESPONSE","status":"DONE","step_index":3,"created_at":"2026-11-09T02:00:34Z","input_tokens":200000,"output_tokens":1}),
        ];
        let lines = records.iter().map(|v| format!("{v}\n")).collect::<String>();
        fs::write(logs.join("transcript.jsonl"), &lines).unwrap();
        fs::write(logs.join("transcript_full.jsonl"), &lines).unwrap();
        let mut target = store::open(Path::new(":memory:")).unwrap();
        let mut scanner = Scanner::new();
        scanner.started_ms = 0;
        scanner
            .scan(&mut target, &source, &Settings::default())
            .unwrap();
        let h = store::history(&target, "", false, 0).unwrap();
        assert_eq!(h.calls, 2);
        assert_eq!(h.alerts, 1);
        assert_eq!(h.rows.iter().filter(|r| !r.cache_known).count(), 1);
        target
            .execute("UPDATE calls SET alert='sent' WHERE alert='pending'", [])
            .unwrap();
        let c = database(dir.path(), "s");
        generation(&c, 0, &[1, 3], "gemini-4-argon");
        step(&c, 1, 15, 3, 185999, 0, 271);
        step(&c, 3, 15, 3, 200000, 1, 1);
        scanner
            .scan(&mut target, &source, &Settings::default())
            .unwrap();
        let h = store::history(&target, "", false, 0).unwrap();
        assert_eq!(h.calls, 2);
        assert_eq!(h.rows.iter().filter(|r| r.alert == "sent").count(), 1);
        assert_eq!(
            h.rows.iter().find(|r| r.id.ends_with(":1")).unwrap().input,
            185999
        );
        assert!(h
            .rows
            .iter()
            .all(|r| r.metadata_source == "antigravity_sqlite"));
        let mut restarted = Scanner::new();
        restarted
            .scan(&mut target, &source, &Settings::default())
            .unwrap();
        assert_eq!(store::history(&target, "", false, 0).unwrap().alerts, 1);
    }
    #[test]
    fn models_switch_and_source_ids_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let a = source(dir.path());
        let mut other = a.clone();
        other.kind = "antigravity_cli".into();
        let c = database(dir.path(), "s");
        generation(&c, 0, &[1], "gemini-4-argon");
        generation(&c, 1, &[3], "claude-sonnet-4-6");
        step(&c, 1, 15, 3, 123, 0, 1);
        step(&c, 3, 15, 3, 345, 123, 1);
        let mut target = store::open(Path::new(":memory:")).unwrap();
        let mut scanner = Scanner::new();
        scanner.scan(&mut target, &a, &Settings::default()).unwrap();
        Scanner::new()
            .scan(&mut target, &other, &Settings::default())
            .unwrap();
        let h = store::history(&target, "", false, 0).unwrap();
        assert_eq!(h.calls, 4);
        assert_eq!(
            h.rows
                .iter()
                .filter(|r| r.model == "claude-sonnet-4-6")
                .count(),
            2
        );
        let filter = store::Filter {
            source: a.kind,
            kind: "response".into(),
        };
        assert_eq!(
            store::history_filtered(&target, "", false, 0, &filter)
                .unwrap()
                .calls,
            2
        );
    }
    #[test]
    fn later_descriptor_updates_existing_subagent_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let source = source(dir.path());
        let c = database(dir.path(), "child");
        generation(&c, 0, &[1], "gemini-4-argon");
        step(&c, 1, 15, 3, 1, 0, 1);
        let mut target = store::open(Path::new(":memory:")).unwrap();
        let mut scanner = Scanner::new();
        scanner
            .scan(&mut target, &source, &Settings::default())
            .unwrap();
        let desc = dir.path().join("brain/parent/.system_generated/subagents");
        fs::create_dir_all(&desc).unwrap();
        fs::write(desc.join("child.json"),serde_json::json!({"conversationId":"child","subagentDescriptor":{"role":"reviewer","typeName":"oracle"},"workspaceUris":["file:///example"]}).to_string()).unwrap();
        scanner
            .scan(&mut target, &source, &Settings::default())
            .unwrap();
        let h = store::history(&target, "reviewer", false, 0).unwrap();
        assert_eq!(h.total, 1);
        assert!(!h.rows[0].parent_session.is_empty());
        assert_eq!(h.rows[0].project, "file:///example");
        fs::write(desc.join("child.json"),serde_json::json!({"conversationId":"child","subagentDescriptor":{"role":"new role longer"}}).to_string()).unwrap();
        scanner
            .scan(&mut target, &source, &Settings::default())
            .unwrap();
        assert_eq!(
            store::history(&target, "new role longer", false, 0)
                .unwrap()
                .total,
            1
        );
    }
    #[test]
    fn cancelled_and_incomplete_json_handling_is_explicit() {
        let s = SourceConfig {
            kind: "antigravity_cli".into(),
            path: "/example".into(),
            enabled: true,
        };
        let mut row = serde_json::json!({"type":"CHECKPOINT","status":"CANCELLED","step_index":7,"input_tokens":262543,"cache_read_tokens":0,"output_tokens":6341});
        let u = parse_transcript(&s, "s", &row, &SessionInfo::default()).unwrap();
        assert!(u.matches(&Settings::default()));
        assert_eq!(u.kind, "checkpoint");
        row["cache_read_tokens"] = Value::Null;
        let u = parse_transcript(&s, "s", &row, &SessionInfo::default()).unwrap();
        assert!(!u.cache_known);
        assert!(!u.matches(&Settings::default()));
        row["status"] = "ACTIVE".into();
        assert!(parse_transcript(&s, "s", &row, &SessionInfo::default()).is_none());
    }
    #[test]
    fn cleared_errors_missing_usage_and_snapshot_corrections_are_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let source = source(dir.path());
        let c = database(dir.path(), "s");
        generation(&c, 0, &[1, 2, 3], "gemini-4-argon");
        step(&c, 1, 15, 5, 100001, 0, 50);
        step(&c, 2, 15, 7, 100002, 0, 50);
        c.execute(
            "INSERT INTO steps VALUES(3,23,3,?1)",
            [b(1, &n(1, 1_794_189_634))],
        )
        .unwrap();
        let mut target = store::open(Path::new(":memory:")).unwrap();
        let mut scanner = Scanner::new();
        scanner.started_ms = 0;
        scanner
            .scan(&mut target, &source, &Settings::default())
            .unwrap();
        let h = store::history(&target, "", false, 0).unwrap();
        assert_eq!(h.calls, 3);
        assert_eq!(h.alerts, 1);
        assert_eq!(h.rows.iter().filter(|r| !r.usage_complete).count(), 1);
        assert_eq!(
            h.rows.iter().find(|r| r.id.ends_with(":1")).unwrap().state,
            "CLEARED"
        );
        step(&c, 2, 15, 7, 2, 30, 1);
        scanner
            .scan(&mut target, &source, &Settings::default())
            .unwrap();
        let h = store::history(&target, "", false, 0).unwrap();
        let row = h.rows.iter().find(|r| r.id.ends_with(":2")).unwrap();
        assert_eq!((row.input, row.read, row.output), (2, 30, 1));
        assert_eq!(h.alerts, 0);
        step(&c, 2, 15, 7, 100002, 0, 50);
        scanner
            .scan(&mut target, &source, &Settings::default())
            .unwrap();
        assert_eq!(store::history(&target, "", false, 0).unwrap().alerts, 0);
    }
    #[test]
    fn native_lock_retries_and_rewrites_existing_steps_without_changing_identity() {
        let dir = tempfile::tempdir().unwrap();
        let source = source(dir.path());
        let c = database(dir.path(), "s");
        step(&c, 1, 15, 3, 100, 10, 2);
        c.execute_batch(
            "PRAGMA wal_checkpoint(TRUNCATE);PRAGMA journal_mode=DELETE;BEGIN EXCLUSIVE;",
        )
        .unwrap();
        let mut target = store::open(Path::new(":memory:")).unwrap();
        let mut scanner = Scanner::new();
        assert!(scanner
            .scan(&mut target, &source, &Settings::default())
            .is_err());
        assert_eq!(store::history(&target, "", false, 0).unwrap().calls, 0);
        c.execute_batch("COMMIT;PRAGMA journal_mode=WAL;").unwrap();
        scanner
            .scan(&mut target, &source, &Settings::default())
            .unwrap();
        step(&c, 1, 15, 3, 90, 20, 1);
        scanner
            .scan(&mut target, &source, &Settings::default())
            .unwrap();
        let h = store::history(&target, "", false, 0).unwrap();
        assert_eq!(h.calls, 1);
        assert_eq!((h.rows[0].input, h.rows[0].read), (90, 20));
    }
    #[test]
    fn transcript_batches_partial_lines_and_rewrites_restart_at_the_right_offset() {
        let dir = tempfile::tempdir().unwrap();
        let source = source(dir.path());
        let logs = dir.path().join("brain/s/.system_generated/logs");
        fs::create_dir_all(&logs).unwrap();
        let path = logs.join("transcript.jsonl");
        let row = |idx, input| {
            format!(
                "{}\n",
                serde_json::json!({"type":"PLANNER_RESPONSE","status":"DONE","step_index":idx,"input_tokens":input,"cache_read_tokens":0,"output_tokens":1})
            )
        };
        let mut text = (0..MAX_RECORDS + 1).map(|i| row(i, 1)).collect::<String>();
        text.push_str("{\"type\":");
        fs::write(&path, &text).unwrap();
        let mut target = store::open(Path::new(":memory:")).unwrap();
        let mut scanner = Scanner::new();
        scanner
            .scan(&mut target, &source, &Settings::default())
            .unwrap();
        assert!(scanner.importing);
        scanner
            .scan(&mut target, &source, &Settings::default())
            .unwrap();
        assert!(!scanner.importing);
        assert_eq!(
            store::history(&target, "", false, 0).unwrap().calls,
            (MAX_RECORDS + 1) as i64
        );
        // Rewrite in place, including data far beyond the unchanged first line.
        text = text.replace("\"input_tokens\":1,", "\"input_tokens\":2,");
        fs::write(&path, text).unwrap();
        scanner
            .scan(&mut target, &source, &Settings::default())
            .unwrap();
        // Replace during an unfinished batch: progress must not skip the rewritten prefix.
        fs::write(&path, row(0, 9)).unwrap();
        scanner
            .scan(&mut target, &source, &Settings::default())
            .unwrap();
        let h = store::history(&target, "", false, 0).unwrap();
        assert_eq!(
            h.rows.iter().find(|r| r.id.ends_with(":0")).unwrap().input,
            9
        );
        assert_eq!(h.alerts, 0);
    }

    #[test]
    fn continuous_appends_do_not_restart_a_native_import_batch() {
        let dir = tempfile::tempdir().unwrap();
        let source = source(dir.path());
        let c = database(dir.path(), "s");
        c.execute_batch("BEGIN").unwrap();
        for idx in 0..MAX_RECORDS as i64 + 2 {
            step(&c, idx, 15, 3, 1, 0, 1);
        }
        c.execute_batch("COMMIT").unwrap();
        let mut target = store::open(Path::new(":memory:")).unwrap();
        let mut scanner = Scanner::new();
        scanner
            .scan(&mut target, &source, &Settings::default())
            .unwrap();
        assert!(scanner.importing);
        step(&c, MAX_RECORDS as i64 + 2, 15, 3, 2, 0, 1);
        scanner
            .scan(&mut target, &source, &Settings::default())
            .unwrap();
        assert_eq!(
            store::history(&target, "", false, 0).unwrap().calls,
            MAX_RECORDS as i64 + 3
        );
        // Changed earlier steps receive a subsequent complete pass too.
        for _ in 0..3 {
            scanner
                .scan(&mut target, &source, &Settings::default())
                .unwrap();
        }
        assert!(!scanner.importing);
    }
}
