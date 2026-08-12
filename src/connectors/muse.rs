//! Connector for Muse Code session event logs.
//!
//! Muse Code 0.1.0 stores append-only JSONL logs at
//! `~/.local/share/muse/sessions/YYYY/MM/DD/<session-id>/session.jsonl`.
//! Nested agents use `subagent/<subagent-id>/session.jsonl` below the same
//! session directory. The event schema is documented in upstream issue #15.
//!
//! Only the four conversation-bearing `runtime.session` run events are
//! normalized. Runtime diagnostics and model/token telemetry are not messages;
//! the observed Muse schema does not correlate model completion usage reliably
//! enough for per-message token attribution.

use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Value, json};
use walkdir::WalkDir;

use super::scan::{DiscoveredSourceFile, DiscoveredSourceRole, ScanContext, ScanRoot};
use super::{Connector, file_modified_since, franken_detection_for_connector};
use crate::types::{
    DetectionResult, NormalizedConversation, NormalizedInvocation, NormalizedMessage,
    reindex_messages,
};

const MICROS_PER_MILLI: i64 = 1_000;
const TITLE_LIMIT: usize = 120;

pub struct MuseConnector;

impl Default for MuseConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl MuseConnector {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    fn sessions_root() -> Option<PathBuf> {
        dirs::home_dir().map(|home| home.join(".local/share/muse/sessions"))
    }

    fn looks_like_sessions_root(path: &Path) -> bool {
        path.file_name().is_some_and(|name| name == "sessions")
            && path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|name| name == "muse")
    }

    fn append_explicit_roots(out: &mut Vec<PathBuf>, base: &Path) {
        if base.is_file() && base.file_name().is_some_and(|name| name == "session.jsonl") {
            if let Some(parent) = base.parent() {
                out.push(parent.to_path_buf());
            }
            return;
        }

        if Self::looks_like_sessions_root(base) {
            out.push(base.to_path_buf());
        }
        if base.file_name().is_some_and(|name| name == "muse") {
            out.push(base.join("sessions"));
        }
        let candidate = base.join(".local/share/muse/sessions");
        if candidate.exists() {
            out.push(candidate);
        }
    }

    fn source_roots(ctx: &ScanContext) -> Vec<ScanRoot> {
        let mut roots = Vec::new();
        if ctx.use_default_detection() {
            let mut explicit = Vec::new();
            Self::append_explicit_roots(&mut explicit, &ctx.data_dir);
            if explicit.is_empty()
                && ctx.data_dir.exists()
                && ctx
                    .data_dir
                    .file_name()
                    .is_some_and(|name| name == "sessions")
            {
                roots.push(ScanRoot::local(ctx.data_dir.clone()));
            } else if !explicit.is_empty() {
                roots.extend(explicit.into_iter().map(ScanRoot::local));
            } else if !Self::session_files(&ctx.data_dir).is_empty() {
                roots.push(ScanRoot::local(ctx.data_dir.clone()));
            }
            if roots.is_empty()
                && let Some(root) = Self::sessions_root()
                && root.exists()
            {
                roots.push(ScanRoot::local(root));
            }
        } else {
            for scan_root in &ctx.scan_roots {
                let mut explicit = Vec::new();
                Self::append_explicit_roots(&mut explicit, &scan_root.path);
                if explicit.is_empty() && !Self::session_files(&scan_root.path).is_empty() {
                    explicit.push(scan_root.path.clone());
                }
                roots.extend(explicit.into_iter().map(|path| scan_root.with_path(path)));
            }
            let mut explicit = Vec::new();
            Self::append_explicit_roots(&mut explicit, &ctx.data_dir);
            if explicit.is_empty() && !Self::session_files(&ctx.data_dir).is_empty() {
                explicit.push(ctx.data_dir.clone());
            }
            roots.extend(explicit.into_iter().map(ScanRoot::local));
        }

        roots.sort_by(|a, b| a.path.cmp(&b.path));
        roots.dedup_by(|a, b| a.path == b.path);
        roots
    }

    fn session_files(root: &Path) -> Vec<PathBuf> {
        if !root.exists() {
            return Vec::new();
        }
        let mut files = WalkDir::new(root)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file() && entry.file_name() == "session.jsonl")
            .map(|entry| entry.path().to_path_buf())
            .collect::<Vec<_>>();
        files.sort();
        files
    }

    fn discover_sources(ctx: &ScanContext) -> Vec<DiscoveredSourceFile> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for root in Self::source_roots(ctx) {
            for file in Self::session_files(&root.path) {
                if !seen.insert(file.clone()) || !file_modified_since(&file, ctx.since_ts) {
                    continue;
                }
                out.push(
                    DiscoveredSourceFile::new(
                        "muse",
                        &root,
                        file,
                        DiscoveredSourceRole::PrimarySessionLog,
                        true,
                    )
                    .with_fs_metadata(),
                );
            }
        }
        out
    }

    fn parent_session_log(path: &Path) -> Option<PathBuf> {
        let subagent_id = path.parent()?;
        let subagent = subagent_id.parent()?;
        if subagent.file_name()? != "subagent" {
            return None;
        }
        Some(subagent.parent()?.join("session.jsonl"))
    }

    fn parse_file(path: &Path) -> Result<Option<NormalizedConversation>> {
        let file = fs::File::open(path)?;
        let mut conversation = parse_reader(BufReader::new(file), path);
        if let Some(ref mut conversation) = conversation
            && conversation.workspace.is_none()
            && let Some(parent) = Self::parent_session_log(path)
            && let Ok(parent_file) = fs::File::open(parent)
        {
            conversation.workspace = workspace_root_from_reader(BufReader::new(parent_file));
        }
        Ok(conversation)
    }
}

impl Connector for MuseConnector {
    fn detect(&self) -> DetectionResult {
        franken_detection_for_connector("muse").unwrap_or_else(DetectionResult::not_found)
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
                    "muse: skipping unreadable session"
                ),
            }
        }
        Ok(out)
    }

    fn discover_source_files(&self, ctx: &ScanContext) -> Result<Vec<DiscoveredSourceFile>> {
        Ok(Self::discover_sources(ctx))
    }
}

#[allow(clippy::too_many_lines)]
fn parse_reader<R: BufRead>(reader: R, source_path: &Path) -> Option<NormalizedConversation> {
    let mut records = reader
        .lines()
        .map_while(Result::ok)
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .collect::<Vec<_>>();
    records.sort_by_key(|record| record.get("sequence").and_then(Value::as_i64).unwrap_or(0));

    let external_id = records
        .iter()
        .find_map(|record| record.pointer("/stream/id").and_then(Value::as_str))
        .map(str::to_owned);
    let workspace = workspace_root(records.iter());
    let mut messages = Vec::new();

    for record in &records {
        let Some(event) = run_event(record) else {
            continue;
        };
        let created_at = recorded_at_millis(record);
        match event.get("kind").and_then(Value::as_str) {
            Some("started") => {
                let Some(prompt) = event.get("prompt").and_then(Value::as_str) else {
                    continue;
                };
                push_message(
                    &mut messages,
                    "user",
                    prompt.to_owned(),
                    created_at,
                    Vec::new(),
                    record,
                );
            }
            Some("assistant_message_committed") => {
                let text = event
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                push_message(
                    &mut messages,
                    "assistant",
                    text.to_owned(),
                    created_at,
                    Vec::new(),
                    record,
                );
            }
            Some("assistant_tool_calls_committed") => {
                let invocations = tool_calls(event);
                if !invocations.is_empty() {
                    push_message(
                        &mut messages,
                        "assistant",
                        String::new(),
                        created_at,
                        invocations,
                        record,
                    );
                }
            }
            Some("tool_result_batch_committed") => {
                let text = tool_results(event);
                if !text.is_empty() {
                    push_message(&mut messages, "tool", text, created_at, Vec::new(), record);
                }
            }
            _ => {}
        }
    }
    if messages.is_empty() {
        return None;
    }

    reindex_messages(&mut messages);
    let title = messages
        .iter()
        .find(|message| message.role == "user")
        .map(|message| truncate(&message.content));
    let started_at = records.iter().filter_map(recorded_at_millis).min();
    let ended_at = records.iter().filter_map(recorded_at_millis).max();
    let is_subagent = SelfPath::is_subagent(source_path);
    let metadata = json!({
        "is_subagent": is_subagent,
        "subagent_id": if is_subagent { source_path.parent().and_then(Path::file_name).and_then(|name| name.to_str()) } else { None },
        "token_coverage": "model completion events are retained only in raw source; per-message attribution is unsupported",
    });

    Some(NormalizedConversation {
        agent_slug: "muse".to_owned(),
        external_id,
        title,
        workspace,
        source_path: source_path.to_path_buf(),
        started_at,
        ended_at,
        metadata,
        messages,
    })
}

struct SelfPath;

impl SelfPath {
    fn is_subagent(path: &Path) -> bool {
        path.parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .is_some_and(|name| name == "subagent")
    }
}

fn workspace_root<'a, I>(records: I) -> Option<PathBuf>
where
    I: IntoIterator<Item = &'a Value>,
{
    records
        .into_iter()
        .find(|record| {
            record.get("payload_type").and_then(Value::as_str) == Some("runtime.session.metadata")
        })
        .and_then(|record| {
            record
                .pointer("/payload/record/workspace_root")
                .and_then(Value::as_str)
        })
        .map(PathBuf::from)
}

fn workspace_root_from_reader<R: BufRead>(reader: R) -> Option<PathBuf> {
    let records = reader
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .collect::<Vec<_>>();
    workspace_root(records.iter())
}

fn run_event(record: &Value) -> Option<&Value> {
    if record.get("payload_type").and_then(Value::as_str) != Some("runtime.session") {
        return None;
    }
    let payload = record.get("payload")?;
    if payload.get("kind").and_then(Value::as_str) != Some("run") {
        return None;
    }
    payload.get("event")
}

fn recorded_at_millis(record: &Value) -> Option<i64> {
    record
        .get("recorded_at")
        .and_then(Value::as_i64)
        .map(|micros| micros / MICROS_PER_MILLI)
}

fn tool_calls(event: &Value) -> Vec<NormalizedInvocation> {
    event
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|call| {
            let name = call.get("name").and_then(Value::as_str)?.trim();
            if name.is_empty() {
                return None;
            }
            let arguments = call.get("args").map(|args| match args {
                Value::String(text) => serde_json::from_str(text).unwrap_or_else(|_| json!(text)),
                other => other.clone(),
            });
            Some(NormalizedInvocation {
                kind: "tool".to_owned(),
                name: name.to_owned(),
                raw_name: None,
                call_id: call
                    .get("call_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                arguments,
            })
        })
        .collect()
}

fn tool_results(event: &Value) -> String {
    event
        .get("results")
        .and_then(Value::as_array)
        .map(|results| {
            results
                .iter()
                .filter_map(|result| result.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn push_message(
    messages: &mut Vec<NormalizedMessage>,
    role: &str,
    content: String,
    created_at: Option<i64>,
    invocations: Vec<NormalizedInvocation>,
    record: &Value,
) {
    messages.push(NormalizedMessage {
        idx: 0,
        role: role.to_owned(),
        author: None,
        created_at,
        content,
        extra: record.clone(),
        snippets: Vec::new(),
        invocations,
    });
}

fn truncate(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= TITLE_LIMIT {
        return trimmed.to_owned();
    }
    trimmed.chars().take(TITLE_LIMIT).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::{Connector, assert_discovery_covers_scan_sources};
    use tempfile::TempDir;

    fn line(sequence: i64, recorded_at: i64, payload_type: &str, payload: &Value) -> String {
        json!({
            "stream": {"kind": "session", "id": "muse-session"},
            "sequence": sequence,
            "recorded_at": recorded_at,
            "payload_type": payload_type,
            "payload": payload,
        })
        .to_string()
    }

    fn fixture() -> String {
        [
            line(4, 1_700_000_003_000_999, "runtime.session", &json!({"kind":"run","event":{"kind":"assistant_message_committed","text":"done"}})),
            line(1, 1_700_000_000_000_123, "runtime.session.metadata", &json!({"kind":"metadata","record":{"workspace_root":"/tmp/muse-workspace"}})),
            line(2, 1_700_000_001_000_123, "runtime.session", &json!({"kind":"run","event":{"kind":"started","prompt":"inspect the tree"}})),
            line(3, 1_700_000_002_000_123, "runtime.session", &json!({"kind":"run","event":{"kind":"assistant_tool_calls_committed","tool_calls":[{"name":"bash","call_id":"call-1","args":"{\"command\":\"ls\"}"}]}})),
            line(5, 1_700_000_004_000_123, "runtime.session", &json!({"kind":"run","event":{"kind":"tool_result_batch_committed","results":[{"tool_call_id":"call-1","text":"file.rs"}]}})),
            line(6, 1_700_000_005_000_123, "runtime.session", &json!({"kind":"run","event":{"kind":"model_completed","model":"muse-spark-1.2"}})),
        ]
        .join("\n")
    }

    #[test]
    fn root_normalization_orders_by_sequence_and_normalizes_micros() {
        let conversation =
            parse_reader(fixture().as_bytes(), Path::new("/muse/session.jsonl")).unwrap();
        assert_eq!(conversation.external_id.as_deref(), Some("muse-session"));
        assert_eq!(
            conversation.workspace,
            Some(PathBuf::from("/tmp/muse-workspace"))
        );
        assert_eq!(conversation.started_at, Some(1_700_000_000_000));
        assert_eq!(conversation.messages[0].role, "user");
        assert_eq!(conversation.messages[0].created_at, Some(1_700_000_001_000));
        assert_eq!(
            conversation.messages[1].invocations[0].arguments,
            Some(json!({"command":"ls"}))
        );
        assert_eq!(conversation.messages[2].content, "done");
        assert_eq!(conversation.messages[3].role, "tool");
        assert!(
            conversation.messages[1]
                .extra
                .pointer("/payload/event")
                .is_some()
        );
        assert!(
            conversation.messages[1]
                .extra
                .pointer("/muse_telemetry")
                .is_none()
        );
        assert_eq!(
            conversation.metadata["token_coverage"],
            "model completion events are retained only in raw source; per-message attribution is unsupported"
        );
    }

    #[test]
    fn telemetry_only_and_malformed_files_are_ignored() {
        let input = "not json\n".to_owned()
            + &line(
                1,
                1,
                "runtime.session",
                &json!({"kind":"run","event":{"kind":"context_block_diagnostic"}}),
            );
        assert!(parse_reader(input.as_bytes(), Path::new("/muse/session.jsonl")).is_none());
        assert!(parse_reader(&b""[..], Path::new("/muse/session.jsonl")).is_none());
    }

    #[test]
    fn nested_subagent_inherits_workspace_and_has_metadata() {
        let temp = TempDir::new().unwrap();
        let session = temp.path().join("2026/08/13/session-1");
        let child = session.join("subagent/child-1");
        fs::create_dir_all(&child).unwrap();
        fs::write(session.join("session.jsonl"), fixture()).unwrap();
        let child_fixture = line(
            1,
            1_700_000_010_000_123,
            "runtime.session",
            &json!({"kind":"run","event":{"kind":"started","prompt":"child"}}),
        );
        fs::write(child.join("session.jsonl"), child_fixture).unwrap();
        let conversation = MuseConnector::parse_file(&child.join("session.jsonl"))
            .unwrap()
            .unwrap();
        assert_eq!(
            conversation.workspace,
            Some(PathBuf::from("/tmp/muse-workspace"))
        );
        assert_eq!(conversation.metadata["is_subagent"], true);
        assert_eq!(conversation.metadata["subagent_id"], "child-1");
    }

    #[test]
    fn discovery_covers_root_and_nested_scan_sources() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("sessions");
        let session = root.join("2026/08/13/session-1");
        fs::create_dir_all(session.join("subagent/child-1")).unwrap();
        fs::write(session.join("session.jsonl"), fixture()).unwrap();
        fs::write(session.join("subagent/child-1/session.jsonl"), fixture()).unwrap();
        let connector = MuseConnector::new();
        let ctx = ScanContext::local_default(root, None);
        assert_discovery_covers_scan_sources(&connector, &ctx);
        assert_eq!(connector.discover_source_files(&ctx).unwrap().len(), 2);
    }
}
