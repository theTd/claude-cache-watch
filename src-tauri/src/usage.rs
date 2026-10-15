use crate::pricing::Metadata;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub source: String,
    pub threshold: i64,
    pub zero_cache_only: bool,
    pub paused: bool,
}

impl Default for Settings {
    fn default() -> Self {
        let base = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".claude"));
        Self {
            source: base.join("projects").to_string_lossy().into_owned(),
            threshold: 100_000,
            zero_cache_only: false,
            paused: false,
        }
    }
}

#[derive(Debug)]
pub struct Usage {
    pub id: String,
    pub timestamp: String,
    pub session: String,
    pub project: String,
    pub model: String,
    pub input: i64,
    pub creation: i64,
    pub read: i64,
    pub output: i64,
    pub billing: Metadata,
    pub agent_id: String,
}

impl Usage {
    #[cfg(test)]
    pub fn parse(line: &[u8]) -> Option<Self> {
        let v: Value = serde_json::from_slice(line).ok()?;
        Self::from_value(&v)
    }

    pub fn from_value(v: &Value) -> Option<Self> {
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
        Some(Self {
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

    pub fn matches(&self, settings: &Settings) -> bool {
        self.input + self.creation > settings.threshold
            && (!settings.zero_cache_only || self.read == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn call(input: i64, creation: i64, read: i64) -> Usage {
        Usage::parse(
            serde_json::json!({"type":"assistant","requestId":"req-1","message":{
            "id":"msg-1","usage":{"input_tokens":input,"cache_creation_input_tokens":creation,
            "cache_read_input_tokens":read}}})
            .to_string()
            .as_bytes(),
        )
        .unwrap()
    }
    #[test]
    fn strict_threshold_and_cache_writes() {
        let mut settings = Settings::default();
        assert!(!call(1, 99_999, 0).matches(&settings));
        assert!(call(1, 100_000, 50).matches(&settings));
        assert!(!call(1, 0, 200_000).matches(&settings));
        settings.zero_cache_only = true;
        assert!(!call(1, 100_000, 50).matches(&settings));
        assert!(call(1, 100_000, 0).matches(&settings));
    }
    #[test]
    fn invalid_or_non_usage_lines_are_ignored() {
        assert!(Usage::parse(b"not json").is_none());
        assert!(
            Usage::parse(br#"{"type":"user","message":{"usage":{"input_tokens":200000}}}"#)
                .is_none()
        );
        assert!(
            Usage::parse(br#"{"type":"assistant","message":{"usage":{"input_tokens":-1}}}"#)
                .is_none()
        );
    }
}
