use rusqlite::{params, Connection};
use serde_json::Value;
use std::path::Path;

pub fn identity(path: &Path) -> (String, String) {
    if path
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|s| s == "subagents")
    {
        (
            path.parent()
                .and_then(Path::parent)
                .and_then(Path::file_name)
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .trim_start_matches("agent-")
                .to_owned(),
        )
    } else {
        (
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            String::new(),
        )
    }
}

pub fn record_line(conn: &Connection, value: &Value, path: &Path) -> rusqlite::Result<()> {
    let (fallback, agent) = identity(path);
    let session = value
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(&fallback);
    if !agent.is_empty() {
        return Ok(());
    }
    let text = |key| value.get(key).and_then(Value::as_str).unwrap_or("");
    let (custom, ai, summary) = match text("type") {
        "custom-title" => (text("customTitle"), "", ""),
        "ai-title" => ("", text("aiTitle"), ""),
        "summary" => ("", "", text("summary")),
        _ => ("", "", ""),
    };
    let slug = text("slug");
    if ![custom, ai, summary, slug].iter().any(|s| !s.is_empty()) {
        return Ok(());
    }
    conn.execute("INSERT INTO sessions(id,custom_title,ai_title,summary,slug) VALUES(?1,?2,?3,?4,?5)
        ON CONFLICT(id) DO UPDATE SET
        custom_title=COALESCE(NULLIF(excluded.custom_title,''),custom_title),
        ai_title=COALESCE(NULLIF(excluded.ai_title,''),ai_title),
        summary=COALESCE(NULLIF(excluded.summary,''),summary),slug=COALESCE(NULLIF(excluded.slug,''),slug)",
        params![session,custom,ai,summary,slug])?;
    Ok(())
}

pub fn record_agent_sidecar(conn: &Connection, path: &Path) -> Result<(), String> {
    let (session, agent) = identity(path);
    if agent.is_empty() {
        return Ok(());
    }
    let sidecar = path.with_extension("meta.json");
    let file = match std::fs::File::open(&sidecar) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("{}: {error}", sidecar.display())),
    };
    let value: Value =
        serde_json::from_reader(file).map_err(|e| format!("{}: {e}", sidecar.display()))?;
    let title = value
        .get("description")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            value
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        });
    if let Some(title) = title {
        conn.execute(
            "INSERT INTO agents(session,id,title) VALUES(?1,?2,?3)
            ON CONFLICT(session,id) DO UPDATE SET title=excluded.title WHERE title!=excluded.title",
            params![session, agent, title],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store;
    #[test]
    fn renamed_parent_and_subagent_titles_are_independent() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("session1.jsonl");
        let child = dir.path().join("session1/subagents/agent-a1.jsonl");
        std::fs::create_dir_all(child.parent().unwrap()).unwrap();
        let db = store::open(Path::new(":memory:")).unwrap();
        record_line(
            &db,
            &serde_json::json!({"type":"custom-title","customTitle":"My renamed session"}),
            &main,
        )
        .unwrap();
        record_line(
            &db,
            &serde_json::json!({"type":"ai-title","aiTitle":"Generated title"}),
            &main,
        )
        .unwrap();
        std::fs::write(
            child.with_extension("meta.json"),
            r#"{"description":"Review cache billing"}"#,
        )
        .unwrap();
        record_agent_sidecar(&db, &child).unwrap();
        let title: String = db
            .query_row(
                "SELECT custom_title FROM sessions WHERE id='session1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(title, "My renamed session");
        let title: String = db
            .query_row(
                "SELECT title FROM agents WHERE session='session1' AND id='a1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(title, "Review cache billing");
    }
}
