//! Connector for Miniharness JSONL session logs.
//!
//! Miniharness keeps one summon in one JSONL file. The default session root is
//! `~/.local/share/miniharness/sessions`; callers may override it with
//! `MINIHARNESS_SESSION_DIR` or provide explicit [`ScanRoot`] values. A root
//! may be either a directory or one session JSONL file.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Value, json};
use walkdir::WalkDir;

use super::scan::{DiscoveredSourceFile, DiscoveredSourceRole, ScanContext, ScanRoot};
use super::{
    Connector, file_modified_since, flatten_content, franken_detection_for_connector,
    parse_timestamp,
};
use crate::types::{DetectionResult, NormalizedConversation, NormalizedMessage, reindex_messages};

const TITLE_LIMIT: usize = 120;

pub struct MiniharnessConnector;

impl Default for MiniharnessConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl MiniharnessConnector {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    fn sessions_root() -> Option<PathBuf> {
        dotenvy::var("MINIHARNESS_SESSION_DIR")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home| home.join(".local/share/miniharness/sessions")))
    }

    fn source_roots(ctx: &ScanContext) -> Vec<ScanRoot> {
        let mut roots = Vec::new();
        if ctx.use_default_detection() {
            // A local-default context may still be used by deterministic
            // callers to point directly at a fixture root. Only use it when it
            // actually contains a candidate, otherwise retain the connector's
            // canonical default-root behavior.
            if (ctx.data_dir.is_file() && is_jsonl(&ctx.data_dir))
                || (!ctx.data_dir.is_file() && !Self::session_files(&ctx.data_dir).is_empty())
            {
                roots.push(ScanRoot::local(ctx.data_dir.clone()));
            } else if let Some(root) = Self::sessions_root() {
                roots.push(ScanRoot::local(root));
            }
        } else {
            roots.extend(ctx.scan_roots.iter().cloned());
        }

        roots.sort_by(|left, right| left.path.cmp(&right.path));
        roots.dedup_by(|left, right| left.path == right.path);
        roots
    }

    fn session_files(root: &Path) -> Vec<PathBuf> {
        if root.is_file() {
            return is_jsonl(root)
                .then(|| root.to_path_buf())
                .into_iter()
                .collect();
        }
        if !root.is_dir() {
            return Vec::new();
        }

        let mut files = WalkDir::new(root)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file() && is_jsonl(entry.path()))
            .map(|entry| entry.path().to_path_buf())
            .collect::<Vec<_>>();
        files.sort();
        files.dedup();
        files
    }

    fn discover_sources(ctx: &ScanContext) -> Vec<DiscoveredSourceFile> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for root in Self::source_roots(ctx) {
            for path in Self::session_files(&root.path) {
                if !seen.insert(path.clone()) || !file_modified_since(&path, ctx.since_ts) {
                    continue;
                }
                out.push(
                    DiscoveredSourceFile::new(
                        "miniharness",
                        &root,
                        path,
                        DiscoveredSourceRole::PrimarySessionLog,
                        true,
                    )
                    .with_fs_metadata(),
                );
            }
        }
        out.sort_by(|left, right| left.source_path.cmp(&right.source_path));
        out
    }

    fn parse_file(path: &Path) -> Result<Option<NormalizedConversation>> {
        let raw = fs::read_to_string(path)?;
        Ok(parse_jsonl(&raw, path))
    }
}

impl Connector for MiniharnessConnector {
    fn detect(&self) -> DetectionResult {
        franken_detection_for_connector("miniharness").unwrap_or_else(DetectionResult::not_found)
    }

    fn scan(&self, ctx: &ScanContext) -> Result<Vec<NormalizedConversation>> {
        let mut out = Vec::new();
        for source in Self::discover_sources(ctx) {
            match Self::parse_file(&source.source_path) {
                Ok(Some(conversation)) => out.push(conversation),
                Ok(None) => {}
                Err(error) => tracing::debug!(
                    path = %source.source_path.display(),
                    error = %error,
                    "miniharness: skipping unreadable session"
                ),
            }
        }
        Ok(out)
    }

    fn discover_source_files(&self, ctx: &ScanContext) -> Result<Vec<DiscoveredSourceFile>> {
        Ok(Self::discover_sources(ctx))
    }
}

fn is_jsonl(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"))
}

fn parse_jsonl(raw: &str, source_path: &Path) -> Option<NormalizedConversation> {
    let mut header_id = None;
    let mut header_created = None;
    let mut workspace = None;
    let mut started_at = None;
    let mut ended_at = None;
    let mut messages = Vec::new();

    for line in raw.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };

        if value.get("kind").and_then(Value::as_str) == Some("header") {
            header_id = value
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_owned);
            header_created = value.get("createdAt").and_then(parse_timestamp);
            workspace = value.get("cwd").and_then(Value::as_str).map(PathBuf::from);
        }

        let Some(message) = message_value(&value) else {
            continue;
        };
        let Some(role) = message.get("role").and_then(Value::as_str) else {
            continue;
        };
        if !matches!(role, "user" | "assistant") {
            continue;
        }

        let created_at = value
            .get("timestamp")
            .and_then(parse_timestamp)
            .or_else(|| message.get("timestamp").and_then(parse_timestamp));
        if let Some(timestamp) = created_at {
            started_at = Some(started_at.map_or(timestamp, |current: i64| current.min(timestamp)));
            ended_at = Some(ended_at.map_or(timestamp, |current: i64| current.max(timestamp)));
        }

        messages.push(NormalizedMessage {
            idx: i64::try_from(messages.len()).unwrap_or(i64::MAX),
            role: role.to_owned(),
            author: None,
            created_at,
            content: message
                .get("content")
                .map(flatten_content)
                .unwrap_or_default(),
            // Retain the complete source record so consumers can inspect
            // provider/model/usage/cost fields without FAD inventing policy.
            extra: value,
            snippets: Vec::new(),
            invocations: Vec::new(),
        });
    }

    if messages.is_empty() || !messages.iter().any(|message| message.role == "assistant") {
        return None;
    }

    reindex_messages(&mut messages);
    let title = messages
        .iter()
        .find(|message| message.role == "user" && !message.content.trim().is_empty())
        .map(|message| truncate(message.content.trim()));
    let started_at = header_created.or(started_at);
    let ended_at = ended_at.or(started_at);
    Some(NormalizedConversation {
        agent_slug: "miniharness".to_owned(),
        external_id: header_id.or_else(|| file_session_id(source_path)),
        title,
        workspace,
        source_path: source_path.to_path_buf(),
        started_at,
        ended_at,
        metadata: json!({}),
        messages,
    })
}

fn message_value(value: &Value) -> Option<&Value> {
    value
        .get("message")
        .or_else(|| value.pointer("/entry/message"))
        .or_else(|| {
            (value.get("type").and_then(Value::as_str) == Some("message"))
                .then(|| value.get("message"))
                .flatten()
        })
        .or_else(|| value.get("role").is_some().then_some(value))
}

fn file_session_id(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    Some(stem.split_once('_').map_or(stem, |(_, id)| id).to_owned())
}

fn truncate(text: &str) -> String {
    if text.chars().count() <= TITLE_LIMIT {
        return text.to_owned();
    }
    text.chars().take(TITLE_LIMIT).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::{
        Connector, ScanContext, ScanRoot, assert_discovery_covers_scan_sources,
    };
    use tempfile::TempDir;

    fn source_line(message: Value, timestamp: i64) -> String {
        json!({
            "kind": "entry",
            "type": "message",
            "message": message,
            "timestamp": timestamp,
        })
        .to_string()
    }

    fn header(id: &str, cwd: Option<&str>) -> String {
        let mut value = json!({
            "kind": "header",
            "version": 4,
            "id": id,
            "createdAt": 1_700_000_000_000_i64,
        });
        if let Some(cwd) = cwd {
            value["cwd"] = Value::String(cwd.to_owned());
        }
        value.to_string()
    }

    fn write_session(root: &Path, name: &str, lines: &[String]) -> PathBuf {
        fs::create_dir_all(root).unwrap();
        let path = root.join(name);
        fs::write(&path, lines.join("\n") + "\n").unwrap();
        path
    }

    #[test]
    fn parses_current_nested_shape_and_preserves_usage_evidence() {
        let input = [
            header("mh-1", Some("/work")),
            source_line(
                json!({
                    "role": "user",
                    "content": [{"type": "text", "text": "summarize"}],
                    "timestamp": 1_700_000_000_100_i64,
                }),
                1_700_000_000_100,
            ),
            source_line(
                json!({
                    "role": "assistant",
                    "provider": "commandcode",
                    "model": "deepseek/deepseek-v4-flash",
                    "content": [{"type": "text", "text": "done"}],
                    "usage": {
                        "input": 100,
                        "output": 20,
                        "cacheRead": 30,
                        "cacheWrite": 4,
                        "reasoning": 5,
                        "cost": {"total": 0.012345},
                    },
                }),
                1_700_000_000_200,
            ),
        ]
        .join("\n");

        let conversation = parse_jsonl(&input, Path::new("/tmp/2026-01-01_mh-1.jsonl")).unwrap();
        assert_eq!(conversation.agent_slug, "miniharness");
        assert_eq!(conversation.external_id.as_deref(), Some("mh-1"));
        assert_eq!(conversation.workspace, Some(PathBuf::from("/work")));
        assert_eq!(conversation.messages.len(), 2);
        assert_eq!(conversation.messages[0].content, "summarize");
        assert_eq!(
            conversation.messages[1]
                .extra
                .pointer("/message/usage/cost/total")
                .and_then(Value::as_f64),
            Some(0.012345)
        );
        assert_eq!(
            conversation.messages[1]
                .extra
                .pointer("/message/model")
                .and_then(Value::as_str),
            Some("deepseek/deepseek-v4-flash")
        );
    }

    #[test]
    fn parses_direct_and_role_bearing_shapes() {
        let input = [
            header("mh-2", None),
            json!({"message": {"role": "user", "content": "hello"}, "timestamp": 1_700_000_000_100_i64}).to_string(),
            json!({"role": "assistant", "content": "world", "timestamp": 1_700_000_000_200_i64}).to_string(),
        ]
        .join("\n");
        let conversation = parse_jsonl(&input, Path::new("/tmp/session.jsonl")).unwrap();
        assert_eq!(conversation.workspace, None);
        assert_eq!(
            conversation
                .messages
                .iter()
                .map(|m| m.role.as_str())
                .collect::<Vec<_>>(),
            ["user", "assistant"]
        );
    }

    #[test]
    fn skips_malformed_and_incomplete_files() {
        let input = [
            "not json".to_owned(),
            header("mh-3", None),
            "{\"kind\":\"entry\",\"type\":\"progress\"}".to_owned(),
        ]
        .join("\n");
        assert!(parse_jsonl(&input, Path::new("/tmp/session.jsonl")).is_none());
        assert!(parse_jsonl("not json\n", Path::new("/tmp/session.jsonl")).is_none());
    }

    #[test]
    fn keeps_all_usage_bearing_assistant_messages() {
        let input = [
            header("mh-4", None),
            source_line(json!({"role":"user","content":"one"}), 1_700_000_000_100),
            source_line(
                json!({"role":"assistant","content":"a","usage":{"input":1}}),
                1_700_000_000_200,
            ),
            source_line(
                json!({"role":"assistant","content":"b","usage":{"input":2}}),
                1_700_000_000_300,
            ),
        ]
        .join("\n");
        let conversation = parse_jsonl(&input, Path::new("/tmp/session.jsonl")).unwrap();
        assert_eq!(
            conversation
                .messages
                .iter()
                .filter(|message| message.role == "assistant"
                    && message.extra.pointer("/message/usage").is_some())
                .count(),
            2
        );
    }

    #[test]
    fn falls_back_to_filename_id() {
        let input = [
            header("", None),
            source_line(json!({"role":"user","content":"hello"}), 1_700_000_000_100),
            source_line(
                json!({"role":"assistant","content":"done"}),
                1_700_000_000_200,
            ),
        ]
        .join("\n");
        let conversation = parse_jsonl(&input, Path::new("/tmp/2026-01-01_abc-123.jsonl")).unwrap();
        assert_eq!(conversation.external_id.as_deref(), Some("abc-123"));

        let input = [
            header("mh-5", None),
            source_line(json!({"role":"user","content":"hello"}), 1_700_000_000_100),
            source_line(
                json!({"role":"assistant","content":"done"}),
                1_700_000_000_200,
            ),
        ]
        .join("\n");
        let conversation = parse_jsonl(&input, Path::new("/tmp/2026-01-01_abc-123.jsonl")).unwrap();
        assert_eq!(conversation.external_id.as_deref(), Some("mh-5"));
    }

    #[test]
    fn explicit_directory_and_direct_file_roots_share_discovery() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("sessions");
        let first = write_session(
            &root,
            "2026-01-01_mh-a.jsonl",
            &[
                header("mh-a", None),
                source_line(json!({"role":"user","content":"a"}), 1_700_000_000_100),
                source_line(
                    json!({"role":"assistant","content":"done"}),
                    1_700_000_000_200,
                ),
            ],
        );
        let second = write_session(
            &root,
            "2026-01-02_mh-b.jsonl",
            &[
                header("mh-b", None),
                source_line(json!({"role":"user","content":"b"}), 1_700_000_000_200),
                source_line(
                    json!({"role":"assistant","content":"done"}),
                    1_700_000_000_300,
                ),
            ],
        );
        let connector = MiniharnessConnector::new();

        let directory_ctx =
            ScanContext::with_roots(root.clone(), vec![ScanRoot::local(root.clone())], None);
        assert_discovery_covers_scan_sources(&connector, &directory_ctx);
        assert_eq!(connector.scan(&directory_ctx).unwrap().len(), 2);

        let file_ctx = ScanContext::with_roots(first.clone(), vec![ScanRoot::local(first)], None);
        assert_discovery_covers_scan_sources(&connector, &file_ctx);
        assert_eq!(connector.scan(&file_ctx).unwrap().len(), 1);
        assert_eq!(
            connector.scan(&file_ctx).unwrap()[0].external_id.as_deref(),
            Some("mh-a")
        );
        assert!(second.exists());
    }
}
