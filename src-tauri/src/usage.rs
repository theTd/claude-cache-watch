use crate::pricing::Metadata;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub source: String,
    #[serde(default)]
    pub sources: Vec<SourceConfig>,
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
            sources: Vec::new(),
            threshold: 100_000,
            zero_cache_only: false,
            paused: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceConfig {
    pub kind: String,
    pub path: String,
    pub enabled: bool,
}
impl Settings {
    pub fn normalize(&mut self) {
        if self.sources.is_empty() {
            self.sources.push(SourceConfig {
                kind: "claude".into(),
                path: self.source.clone(),
                enabled: true,
            });
            let home = dirs::home_dir().unwrap_or_default();
            for (kind, folder) in [
                ("antigravity_ide", "antigravity"),
                ("antigravity_cli", "antigravity-cli"),
            ] {
                let path = home.join(".gemini").join(folder);
                self.sources.push(SourceConfig {
                    kind: kind.into(),
                    enabled: path.is_dir(),
                    path: path.to_string_lossy().into_owned(),
                });
            }
        }
        if let Some(claude) = self.sources.iter().find(|s| s.kind == "claude") {
            self.source = claude.path.clone();
        }
    }
}

/// Include the source instance path so custom roots cannot collide.
pub fn namespace(kind: &str, root: &str, session: &str) -> String {
    let path = std::fs::canonicalize(root).unwrap_or_else(|_| PathBuf::from(root));
    let path = path.to_string_lossy().replace('\\', "/").to_lowercase();
    format!("{kind}:{}:{path}:{session}", path.len())
}

#[derive(Debug)]
pub struct Usage {
    pub source_kind: String,
    pub raw_session: String,
    pub parent_session: String,
    pub kind: String,
    pub state: String,
    pub usage_complete: bool,
    pub cache_known: bool,
    pub creation_known: bool,
    pub metadata_source: String,
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
        let v: serde_json::Value = serde_json::from_slice(line).ok()?;
        crate::claude::parse(&v)
    }

    pub fn matches(&self, settings: &Settings) -> bool {
        self.usage_complete
            && self.cache_known
            && matches!(
                self.state.as_str(),
                "DONE" | "CANCELLED" | "ERROR" | "INTERRUPTED"
            )
            && self.input + self.creation > settings.threshold
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
