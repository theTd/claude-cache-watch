use crate::pricing::{self, Estimate, Metadata, Rates, Tokens};
use crate::usage::{Settings, Usage};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub fn open_reader(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(conn)
}

pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    let mut conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch("PRAGMA journal_mode=WAL;
        CREATE TABLE IF NOT EXISTS settings (id INTEGER PRIMARY KEY CHECK(id=1), json TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS cursors (path TEXT PRIMARY KEY, offset INTEGER NOT NULL, signature BLOB NOT NULL);
        CREATE TABLE IF NOT EXISTS calls (
            id TEXT PRIMARY KEY, timestamp TEXT NOT NULL, session TEXT NOT NULL,
            project TEXT NOT NULL, model TEXT NOT NULL, input INTEGER NOT NULL,
            creation INTEGER NOT NULL, read INTEGER NOT NULL, output INTEGER NOT NULL,
            source TEXT NOT NULL, alert TEXT NOT NULL DEFAULT '', alert_error TEXT NOT NULL DEFAULT '',
            imported INTEGER NOT NULL, seen_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')));
        CREATE INDEX IF NOT EXISTS calls_time ON calls(timestamp DESC);")?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version == 0 {
        let tx = conn.transaction()?;
        tx.execute_batch("ALTER TABLE calls ADD COLUMN cache_1h INTEGER;
            ALTER TABLE calls ADD COLUMN speed TEXT NOT NULL DEFAULT '';
            ALTER TABLE calls ADD COLUMN geo TEXT NOT NULL DEFAULT '';
            ALTER TABLE calls ADD COLUMN searches INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE calls ADD COLUMN agent_id TEXT NOT NULL DEFAULT '';
            CREATE TABLE sessions(id TEXT PRIMARY KEY,custom_title TEXT NOT NULL DEFAULT '',ai_title TEXT NOT NULL DEFAULT '',summary TEXT NOT NULL DEFAULT '',slug TEXT NOT NULL DEFAULT '');
            CREATE TABLE agents(session TEXT NOT NULL,id TEXT NOT NULL,title TEXT NOT NULL,PRIMARY KEY(session,id));
            CREATE TABLE model_rates(model TEXT PRIMARY KEY,json TEXT NOT NULL);
            DELETE FROM cursors;
            PRAGMA user_version=1;")?;
        tx.commit()?;
    } else if version != 1 {
        return Err(rusqlite::Error::InvalidQuery);
    }
    conn.execute_batch("CREATE INDEX IF NOT EXISTS calls_date ON calls(julianday(timestamp));")?;
    Ok(conn)
}

pub fn settings(conn: &Connection) -> Result<Settings, String> {
    let json: Option<String> = conn
        .query_row("SELECT json FROM settings WHERE id=1", [], |r| r.get(0))
        .optional()
        .map_err(|e| e.to_string())?;
    match json {
        Some(json) => {
            serde_json::from_str(&json).map_err(|e| format!("Invalid saved settings: {e}"))
        }
        None => Ok(Settings::default()),
    }
}

pub fn save_settings(conn: &Connection, settings: &Settings) -> Result<(), String> {
    let json = serde_json::to_string(settings).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO settings VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET json=excluded.json",
        [json],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn record(
    conn: &Connection,
    usage: &Usage,
    source: &str,
    imported: bool,
    settings: &Settings,
) -> rusqlite::Result<()> {
    let alert = if !imported && usage.matches(settings) {
        "pending"
    } else {
        ""
    };
    conn.execute("INSERT INTO calls(id,timestamp,session,project,model,input,creation,read,output,source,alert,imported,cache_1h,speed,geo,searches,agent_id)
        VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)
        ON CONFLICT(id) DO UPDATE SET input=MAX(input,excluded.input), creation=MAX(creation,excluded.creation),
        read=MAX(read,excluded.read), output=MAX(output,excluded.output),
        cache_1h=CASE WHEN excluded.cache_1h IS NULL THEN cache_1h ELSE MAX(COALESCE(cache_1h,0),excluded.cache_1h) END,
        speed=COALESCE(NULLIF(excluded.speed,''),speed),geo=COALESCE(NULLIF(excluded.geo,''),geo),
        searches=MAX(searches,excluded.searches),agent_id=COALESCE(NULLIF(excluded.agent_id,''),agent_id),
        model=COALESCE(NULLIF(excluded.model,''),model),session=COALESCE(NULLIF(excluded.session,''),session)",
        params![usage.id, usage.timestamp, usage.session, usage.project, usage.model, usage.input,
            usage.creation, usage.read, usage.output, source, alert, imported,usage.billing.cache_1h,
            usage.billing.speed,usage.billing.geo,usage.billing.searches,usage.agent_id])?;
    // Imported final usage can also invalidate a pending alert persisted before a crash.
    conn.execute(
        "UPDATE calls SET alert='' WHERE id=?1 AND alert='pending' AND ?2=1 AND read>0",
        params![usage.id, settings.zero_cache_only],
    )?;
    // Some transcripts initially publish zero usage, then the final counters. Evaluate the merged row.
    if !imported {
        conn.execute(
            "UPDATE calls SET alert='pending' WHERE id=?1 AND alert='' AND imported=0
            AND input+creation>?2 AND (?3=0 OR read=0)",
            params![usage.id, settings.threshold, settings.zero_cache_only],
        )?;
    }
    Ok(())
}

#[derive(Serialize)]
pub struct Call {
    pub id: String,
    pub timestamp: String,
    pub session: String,
    pub project: String,
    pub model: String,
    pub input: i64,
    pub creation: i64,
    pub read: i64,
    pub output: i64,
    pub alert: String,
    pub alert_error: String,
    pub imported: bool,
    pub session_title: String,
    pub agent_id: String,
    pub subagent_title: Option<String>,
    pub estimated_billing: Option<Estimate>,
}

#[derive(Serialize)]
pub struct History {
    pub rows: Vec<Call>,
    pub total: i64,
    pub calls: i64,
    pub alerts: i64,
    pub uncached: i64,
}

pub fn history(
    conn: &Connection,
    search: &str,
    alerts_only: bool,
    page: u32,
) -> rusqlite::Result<History> {
    let filter = format!(
        "%{}%",
        search
            .replace('!', "!!")
            .replace('%', "!%")
            .replace('_', "!_")
    );
    let joins=" FROM calls c LEFT JOIN sessions s ON c.session=s.id LEFT JOIN agents a ON c.session=a.session AND c.agent_id=a.id";
    let session_title="COALESCE(NULLIF(s.custom_title,''),NULLIF(s.ai_title,''),NULLIF(s.summary,''),NULLIF(s.slug,''),NULLIF(c.session,''),'Untitled session')";
    let predicate = " WHERE (?1=0 OR c.alert!='') AND
        (c.project LIKE ?2 ESCAPE '!' OR c.model LIKE ?2 ESCAPE '!' OR c.session LIKE ?2 ESCAPE '!' OR c.id LIKE ?2 ESCAPE '!'
        OR s.custom_title LIKE ?2 ESCAPE '!' OR s.ai_title LIKE ?2 ESCAPE '!' OR s.summary LIKE ?2 ESCAPE '!' OR s.slug LIKE ?2 ESCAPE '!'
        OR a.title LIKE ?2 ESCAPE '!' OR c.agent_id LIKE ?2 ESCAPE '!')";
    let total = conn.query_row(
        &format!("SELECT COUNT(*){joins}{predicate}"),
        params![alerts_only, filter],
        |r| r.get(0),
    )?;
    let rates = load_rates(conn)?;
    let mut statement = conn.prepare(&format!("SELECT c.id,c.timestamp,c.session,c.project,c.model,c.input,c.creation,c.read,c.output,c.alert,c.alert_error,c.imported,
        c.agent_id,c.cache_1h,c.speed,c.geo,c.searches,{session_title},a.title
        {joins}{predicate} ORDER BY c.timestamp DESC,c.id LIMIT 100 OFFSET ?3"))?;
    let rows = statement
        .query_map(params![alerts_only, filter, i64::from(page) * 100], |r| {
            let model: String = r.get(4)?;
            let tokens = Tokens {
                input: r.get(5)?,
                creation: r.get(6)?,
                read: r.get(7)?,
                output: r.get(8)?,
            };
            let metadata = Metadata {
                cache_1h: r.get(13)?,
                speed: r.get(14)?,
                geo: r.get(15)?,
                searches: r.get(16)?,
            };
            let estimated_billing = pricing::estimate(&model, tokens, &metadata, &rates);
            Ok(Call {
                id: r.get(0)?,
                timestamp: r.get(1)?,
                session: r.get(2)?,
                project: r.get(3)?,
                model: r.get(4)?,
                input: r.get(5)?,
                creation: r.get(6)?,
                read: r.get(7)?,
                output: r.get(8)?,
                alert: r.get(9)?,
                alert_error: r.get(10)?,
                imported: r.get(11)?,
                agent_id: r.get(12)?,
                session_title: r.get(17)?,
                subagent_title: r.get(18)?,
                estimated_billing,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let (calls, alerts, uncached) = conn.query_row(
        "SELECT COUNT(*),COALESCE(SUM(alert!=''),0),COALESCE(SUM(input+creation),0) FROM calls",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    Ok(History {
        rows,
        total,
        calls,
        alerts,
        uncached,
    })
}

pub(crate) fn load_rates(conn: &Connection) -> rusqlite::Result<BTreeMap<String, Rates>> {
    let mut query = conn.prepare("SELECT model,json FROM model_rates")?;
    let result = query
        .query_map([], |r| {
            let json: String = r.get(1)?;
            let rates: Rates = serde_json::from_str(&json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            rates.validate().map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::other(e)),
                )
            })?;
            Ok((r.get(0)?, rates))
        })?
        .collect();
    result
}

#[derive(Serialize)]
pub struct ModelRate {
    pub model: String,
    pub rates: Option<Rates>,
    pub custom: bool,
}

pub fn pricing_catalog(conn: &Connection) -> rusqlite::Result<Vec<ModelRate>> {
    let rates = load_rates(conn)?;
    let mut query =
        conn.prepare("SELECT DISTINCT model FROM calls WHERE model!='' ORDER BY model")?;
    let mut models = query
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<BTreeSet<_>>>()?;
    models.extend(rates.keys().cloned());
    Ok(models
        .into_iter()
        .map(|model| ModelRate {
            rates: rates
                .get(&model)
                .cloned()
                .or_else(|| pricing::builtin(&model)),
            custom: rates.contains_key(&model),
            model,
        })
        .collect())
}

pub fn set_model_rates(conn: &Connection, model: &str, rates: Option<Rates>) -> Result<(), String> {
    if model.trim().is_empty() || model.len() > 200 {
        return Err("Enter a model ID of 1 to 200 characters".into());
    }
    if let Some(rates) = rates {
        rates.validate()?;
        let json = serde_json::to_string(&rates).map_err(|e| e.to_string())?;
        conn.execute("INSERT INTO model_rates VALUES(?1,?2) ON CONFLICT(model) DO UPDATE SET json=excluded.json",params![model,json]).map_err(|e|e.to_string())?;
    } else {
        conn.execute("DELETE FROM model_rates WHERE model=?1", [model])
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dashboard_readers_stay_available_during_import_transactions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite");
        let mut writer = open(&path).unwrap();
        let usage=Usage::parse(br#"{"type":"assistant","requestId":"r1","message":{"model":"claude-opus-5-5","usage":{"input_tokens":1}}}"#).unwrap();
        record(&writer, &usage, "test", true, &Settings::default()).unwrap();
        let tx = writer.transaction().unwrap();
        tx.execute("UPDATE calls SET input=9999", []).unwrap();
        let reader = open_reader(&path).unwrap();
        assert_eq!(history(&reader, "", false, 0).unwrap().rows[0].input, 1);
        assert!(reader.execute("DELETE FROM calls", []).is_err());
        tx.commit().unwrap();
        assert_eq!(history(&reader, "", false, 0).unwrap().rows[0].input, 9999);
    }

    #[test]
    fn reader_transaction_pins_rates_and_usage_to_one_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.sqlite");
        let mut writer = open(&path).unwrap();
        let usage=Usage::parse(br#"{"type":"assistant","requestId":"r1","message":{"model":"claude-opus-5-5","usage":{"input_tokens":1000}}}"#).unwrap();
        record(&writer, &usage, "test", true, &Settings::default()).unwrap();
        let reader = open_reader(&path).unwrap();
        let read_tx = reader.unchecked_transaction().unwrap();
        let before = history(&read_tx, "", false, 0).unwrap();
        let write_tx = writer.transaction().unwrap();
        write_tx.execute("UPDATE calls SET input=2000", []).unwrap();
        set_model_rates(
            &write_tx,
            "claude-opus-5-5",
            Some(Rates {
                input: 1.,
                cache_5m: 0.,
                cache_1h: 0.,
                read: 0.,
                output: 0.,
            }),
        )
        .unwrap();
        write_tx.commit().unwrap();
        let pinned = history(&read_tx, "", false, 0).unwrap();
        assert_eq!(pinned.rows[0].input, 1000);
        assert_eq!(
            pinned.rows[0].estimated_billing.as_ref().unwrap().total_usd,
            before.rows[0].estimated_billing.as_ref().unwrap().total_usd
        );
        read_tx.commit().unwrap();
        let after = history(&reader, "", false, 0).unwrap();
        assert_eq!(after.rows[0].input, 2000);
        assert!(
            (after.rows[0].estimated_billing.as_ref().unwrap().total_usd - 0.002).abs() < 1e-10
        );
    }
    #[test]
    fn billing_titles_and_custom_rates_update_existing_history() {
        let db = open(Path::new(":memory:")).unwrap();
        let settings = Settings::default();
        let mut usage=Usage::parse(br#"{"type":"assistant","requestId":"billing-r1","sessionId":"s1","agentId":"a1","message":{"model":"claude-opus-5-5","usage":{"input_tokens":1000,"cache_creation_input_tokens":3000,"cache_read_input_tokens":10000,"output_tokens":500,"cache_creation":{"ephemeral_1h_input_tokens":1000},"speed":"standard","inference_geo":"global"}}}"#).unwrap();
        record(&db, &usage, "test", true, &settings).unwrap();
        crate::titles::record_line(
            &db,
            &serde_json::json!({"type":"ai-title","aiTitle":"Original session","sessionId":"s1"}),
            Path::new("s1.jsonl"),
        )
        .unwrap();
        crate::titles::record_line(&db,&serde_json::json!({"type":"custom-title","customTitle":"Renamed session","sessionId":"s1"}),Path::new("s1.jsonl")).unwrap();
        db.execute("INSERT INTO agents VALUES('s1','a1','Audit billing')", [])
            .unwrap();
        let h = history(&db, "Audit billing", false, 0).unwrap();
        assert_eq!(h.total, 1);
        assert_eq!(h.rows[0].session_title, "Renamed session");
        assert_eq!(h.rows[0].subagent_title.as_deref(), Some("Audit billing"));
        assert!((h.rows[0].estimated_billing.as_ref().unwrap().total_usd - 0.034).abs() < 1e-10);
        usage.billing = Metadata::default(); // Later sparse streaming entries must preserve cache duration.
        usage.output = 1000;
        record(&db, &usage, "test", true, &settings).unwrap();
        let h = history(&db, "Renamed session", false, 0).unwrap();
        assert!((h.rows[0].estimated_billing.as_ref().unwrap().total_usd - 0.044).abs() < 1e-10);
        set_model_rates(
            &db,
            "claude-opus-5-5",
            Some(Rates {
                input: 0.,
                cache_5m: 0.,
                cache_1h: 0.,
                read: 0.,
                output: 0.,
            }),
        )
        .unwrap();
        assert_eq!(
            history(&db, "", false, 0).unwrap().rows[0]
                .estimated_billing
                .as_ref()
                .unwrap()
                .total_usd,
            0.
        );
        set_model_rates(&db, "claude-opus-5-5", None).unwrap();
        assert!(
            history(&db, "", false, 0).unwrap().rows[0]
                .estimated_billing
                .as_ref()
                .unwrap()
                .total_usd
                > 0.
        );
    }

    #[test]
    fn v0_database_migrates_once_preserving_history_and_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.sqlite");
        {
            let db = Connection::open(&path).unwrap();
            db.execute_batch("CREATE TABLE calls(id TEXT PRIMARY KEY,timestamp TEXT NOT NULL,session TEXT NOT NULL,project TEXT NOT NULL,model TEXT NOT NULL,input INTEGER NOT NULL,creation INTEGER NOT NULL,read INTEGER NOT NULL,output INTEGER NOT NULL,source TEXT NOT NULL,alert TEXT NOT NULL DEFAULT '',alert_error TEXT NOT NULL DEFAULT '',imported INTEGER NOT NULL,seen_at TEXT NOT NULL DEFAULT '');
                CREATE TABLE cursors(path TEXT PRIMARY KEY,offset INTEGER NOT NULL,signature BLOB NOT NULL);
                INSERT INTO calls VALUES('old','2026-10-01','s1','project','claude-opus-5-5',1,2,3,4,'test','sent','',0,'');
                INSERT INTO cursors VALUES('old.jsonl',100,X'01');").unwrap();
            db.execute_batch(
                "CREATE TABLE settings(id INTEGER PRIMARY KEY CHECK(id=1),json TEXT NOT NULL);",
            )
            .unwrap();
            save_settings(
                &db,
                &Settings {
                    threshold: 234567,
                    ..Settings::default()
                },
            )
            .unwrap();
        }
        let db = open(&path).unwrap();
        assert_eq!(history(&db, "", false, 0).unwrap().rows[0].alert, "sent");
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM cursors", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(super::settings(&db).unwrap().threshold, 234567);
        db.execute("INSERT INTO cursors VALUES('new.jsonl',100,X'01')", [])
            .unwrap();
        drop(db);
        let db = open(&path).unwrap();
        assert_eq!(super::settings(&db).unwrap().threshold, 234567);
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM cursors", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    #[test]
    fn duplicates_merge_and_only_one_alert_is_queued() {
        let db = open(Path::new(":memory:")).unwrap();
        let settings = Settings::default();
        let mut usage = Usage::parse(br#"{"type":"assistant","requestId":"r1","message":{"usage":{"input_tokens":1,"cache_creation_input_tokens":100001}}}"#).unwrap();
        record(&db, &usage, "test", false, &settings).unwrap();
        db.execute("UPDATE calls SET alert='sent'", []).unwrap();
        usage.output = 500;
        record(&db, &usage, "test", false, &settings).unwrap();
        let h = history(&db, "", false, 0).unwrap();
        assert_eq!(h.calls, 1);
        assert_eq!(h.alerts, 1);
        assert_eq!(h.rows[0].output, 500);
        assert_eq!(h.rows[0].alert, "sent");
    }
    #[test]
    fn imported_calls_never_queue_and_search_is_literal() {
        let db = open(Path::new(":memory:")).unwrap();
        let u = Usage::parse(br#"{"type":"assistant","requestId":"r%1","message":{"usage":{"input_tokens":200000}}}"#).unwrap();
        record(&db, &u, "test", true, &Settings::default()).unwrap();
        record(&db, &u, "test", false, &Settings::default()).unwrap();
        assert_eq!(history(&db, "%", false, 0).unwrap().total, 1);
        assert_eq!(history(&db, "_", false, 0).unwrap().total, 0);
        assert_eq!(history(&db, "", true, 0).unwrap().total, 0);
    }

    #[test]
    fn later_cache_reads_revoke_only_unsent_zero_cache_alerts() {
        let db = open(Path::new(":memory:")).unwrap();
        let settings = Settings {
            zero_cache_only: true,
            ..Settings::default()
        };
        let mut usage = Usage::parse(
            br#"{"type":"assistant","requestId":"r1","message":{"usage":{"input_tokens":200000}}}"#,
        )
        .unwrap();
        record(&db, &usage, "test", false, &settings).unwrap();
        assert_eq!(history(&db, "", true, 0).unwrap().total, 1);
        usage.read = 100;
        record(&db, &usage, "test", false, &settings).unwrap();
        assert_eq!(history(&db, "", true, 0).unwrap().total, 0);
        db.execute("UPDATE calls SET alert='sent'", []).unwrap();
        record(&db, &usage, "test", false, &settings).unwrap();
        assert_eq!(history(&db, "", false, 0).unwrap().rows[0].alert, "sent");
    }

    #[test]
    fn imported_final_cache_reads_revoke_persisted_pending_alert() {
        let db = open(Path::new(":memory:")).unwrap();
        let settings = Settings {
            zero_cache_only: true,
            ..Settings::default()
        };
        let mut usage = Usage::parse(br#"{"type":"assistant","requestId":"pending-before-crash","message":{"usage":{"input_tokens":200000}}}"#).unwrap();
        record(&db, &usage, "test", false, &settings).unwrap();
        assert_eq!(history(&db, "", true, 0).unwrap().total, 1);
        usage.read = 100;
        record(&db, &usage, "test", true, &settings).unwrap();
        assert_eq!(history(&db, "", true, 0).unwrap().total, 0);
        db.execute("UPDATE calls SET alert='failed'", []).unwrap();
        record(&db, &usage, "test", true, &settings).unwrap();
        assert_eq!(history(&db, "", false, 0).unwrap().rows[0].alert, "failed");
    }
}
