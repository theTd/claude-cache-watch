use crate::{pricing::Metadata, usage::Usage};
use serde_json::Value;
pub fn parse(v: &Value) -> Option<Usage> {
    if v.get("type")?.as_str()? != "assistant" {
        return None;
    }
    let m = v.get("message")?;
    let usage = m.get("usage")?;
    let input = usage.get("input_tokens")?.as_u64()?;
    let creation = usage
        .get("cache_creation_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let read = usage
        .get("cache_read_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    // SQLite INTEGER is signed. Reject corrupt/out-of-range usage instead of wrapping.
    if input.checked_add(creation)?.checked_add(read)? > i64::MAX as u64 {
        return None;
    }
    let text = |obj: &Value, key| {
        obj.get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let id = v
        .get("requestId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            m.get("id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })?;
    Some(Usage {
        source_kind: "claude".into(),
        raw_session: text(v, "sessionId"),
        parent_session: String::new(),
        kind: "response".into(),
        state: "DONE".into(),
        usage_complete: true,
        cache_known: true,
        creation_known: true,
        metadata_source: "claude_transcript".into(),
        id: id.to_owned(),
        timestamp: text(v, "timestamp"),
        session: text(v, "sessionId"),
        project: text(v, "cwd"),
        model: text(m, "model"),
        input: input as i64,
        creation: creation as i64,
        read: read as i64,
        output: usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(i64::MAX as u64) as i64,
        agent_id: text(v, "agentId"),
        billing: Metadata {
            cache_1h: usage
                .get("cache_creation")
                .and_then(|c| c.get("ephemeral_1h_input_tokens"))
                .and_then(Value::as_i64)
                .filter(|n| *n >= 0 && *n <= creation as i64),
            speed: text(usage, "speed"),
            geo: text(usage, "inference_geo"),
            searches: usage
                .get("server_tool_use")
                .and_then(|s| s.get("web_search_requests"))
                .and_then(Value::as_i64)
                .unwrap_or(0)
                .max(0),
        },
    })
}
