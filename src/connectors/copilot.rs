//! Connector for GitHub Copilot Chat session logs.
//!
//! VS Code has used three native persistence generations for Copilot-backed
//! chat sessions:
//! - `state.vscdb`, key `interactive.sessions` (through March 2025)
//! - `workspaceStorage/<id>/chatSessions/*.json` (March 2025 through 1.108)
//! - the same session directory with append-only `.jsonl` logs (1.109 onward)
//!
//! Empty-window sessions live under User-level global storage. The connector
//! scans Code, Code - Insiders, and `VSCodium` roots on Linux, macOS, and Windows,
//! and filters the shared native store to Copilot-owned sessions. Converted
//! exports under `globalStorage/github.copilot-chat` remain supported too.
//!
//! Additionally, the `gh copilot` CLI may store history at:
//! - ~/.config/gh-copilot/
//!
//! ## Copilot CLI event logs
//!
//! GitHub Copilot CLI (the `gh copilot` or standalone `copilot` binary) stores
//! session history as JSONL event logs:
//! - ~/.copilot/session-state/{session-id}/events.jsonl  (v2, since 0.0.342)
//! - ~/.copilot/history-session-state/{session-id}.json  (v1, legacy)
//! - ~/.copilot/command-history-state.json
//!
//! Each line in `events.jsonl` is a JSON object with a `type` field identifying
//! the event kind. Conversation events use `user.message` and `assistant.message`
//! types with `content`, `role`, and `timestamp` fields.
//!
//! ## VS Code Copilot Chat JSON format
//!
//! The primary storage file is `conversations.json` (or individual `.json` files),
//! containing an array of conversation objects:
//!
//! ```json
//! [
//!   {
//!     "id": "uuid",
//!     "requester": "user",
//!     "workspaceFolder": "/path/to/project",
//!     "turns": [
//!       {
//!         "request": { "message": "...", "timestamp": 1700000000000 },
//!         "response": { "message": "...", "timestamp": 1700000001000 }
//!       }
//!     ]
//!   }
//! ]
//! ```

use std::collections::HashSet;
use std::fs;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
#[cfg(feature = "copilot-sqlite")]
use rusqlite::{OpenFlags, params};
use serde_json::Value;
use walkdir::WalkDir;

use super::scan::{DiscoveredSourceFile, DiscoveredSourceRole, ScanContext, ScanRoot};
#[cfg(feature = "copilot-sqlite")]
use super::sqlite_sync::{ConnectionExt, open_with_flags};
use super::vscode_chat::replay_operation_log;
use super::{
    Connector, extract_invocations_from_content_blocks, file_modified_since, flatten_content,
    franken_detection_for_connector, parse_timestamp,
};
use crate::types::{
    DetectionResult, NormalizedConversation, NormalizedInvocation, NormalizedMessage,
};

pub struct CopilotConnector;

fn min_timestamp(current: Option<i64>, candidate: Option<i64>) -> Option<i64> {
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(current.min(candidate)),
        (None, candidate) => candidate,
        (current, None) => current,
    }
}

fn max_timestamp(current: Option<i64>, candidate: Option<i64>) -> Option<i64> {
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(current.max(candidate)),
        (None, candidate) => candidate,
        (current, None) => current,
    }
}

impl Default for CopilotConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl CopilotConnector {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// VS Code User roots containing workspaceStorage and globalStorage.
    ///
    /// Keep the roots explicit for all supported product variants.  A User
    /// root is scanned once, which covers both workspace chat files and the
    /// application-scoped global database without duplicate physical files.
    fn vscode_linux_user_paths() -> Vec<PathBuf> {
        let Some(home) = dirs::home_dir() else {
            return Vec::new();
        };
        vec![
            home.join(".config/Code/User"),
            home.join(".config/Code - Insiders/User"),
            home.join(".config/VSCodium/User"),
        ]
    }

    /// Known VS Code User roots on macOS.
    fn vscode_macos_user_paths() -> Vec<PathBuf> {
        let Some(home) = dirs::home_dir() else {
            return Vec::new();
        };
        vec![
            home.join("Library/Application Support/Code/User"),
            home.join("Library/Application Support/Code - Insiders/User"),
            home.join("Library/Application Support/VSCodium/User"),
        ]
    }

    /// Known VS Code User roots on Windows.
    fn vscode_windows_user_paths() -> Vec<PathBuf> {
        let Some(appdata) = dirs::config_dir() else {
            return Vec::new();
        };
        vec![
            appdata.join("Code/User"),
            appdata.join("Code - Insiders/User"),
            appdata.join("VSCodium/User"),
        ]
    }

    fn vscode_user_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();
        paths.extend(Self::vscode_linux_user_paths());
        paths.extend(Self::vscode_macos_user_paths());
        paths.extend(Self::vscode_windows_user_paths());
        paths.sort();
        paths.dedup();
        paths
    }

    /// Known VS Code globalStorage paths for Copilot Chat on Linux.
    fn vscode_linux_paths() -> Vec<PathBuf> {
        Self::vscode_linux_user_paths()
            .into_iter()
            .map(|path| path.join("globalStorage/github.copilot-chat"))
            .collect()
    }

    /// Known VS Code globalStorage paths for Copilot Chat on macOS.
    fn vscode_macos_paths() -> Vec<PathBuf> {
        Self::vscode_macos_user_paths()
            .into_iter()
            .map(|path| path.join("globalStorage/github.copilot-chat"))
            .collect()
    }

    /// gh copilot CLI config path and Copilot CLI session-state paths.
    fn gh_copilot_paths() -> Vec<PathBuf> {
        let Some(home) = dirs::home_dir() else {
            return Vec::new();
        };
        vec![
            home.join(".config/gh-copilot"),
            home.join(".config/gh/copilot"),
            // Copilot CLI v2 session storage (since 0.0.342)
            home.join(".copilot/session-state"),
            // Copilot CLI v1 legacy session storage
            home.join(".copilot/history-session-state"),
        ]
    }

    /// Known VS Code globalStorage paths for Copilot Chat on Windows.
    ///
    /// Uses `%APPDATA%` (typically `C:\Users\<name>\AppData\Roaming`).
    fn vscode_windows_paths() -> Vec<PathBuf> {
        Self::vscode_windows_user_paths()
            .into_iter()
            .map(|path| path.join("globalStorage/github.copilot-chat"))
            .collect()
    }

    /// All candidate paths for this platform.
    fn all_candidate_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();
        paths.extend(Self::vscode_user_paths());
        paths.extend(Self::vscode_linux_paths());
        paths.extend(Self::vscode_macos_paths());
        paths.extend(Self::vscode_windows_paths());
        paths.extend(Self::gh_copilot_paths());
        paths.sort();
        paths.dedup();
        paths
    }

    /// Check if a path looks like Copilot Chat or Copilot CLI storage.
    fn looks_like_copilot_storage(path: &Path) -> bool {
        let segments: Vec<String> = path
            .components()
            .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
            .collect();

        if segments.iter().any(|segment| {
            segment == "github.copilot-chat" || segment == "copilot-chat" || segment == "gh-copilot"
        }) {
            return true;
        }

        // Copilot CLI session-state directories:
        // ~/.copilot/session-state/ or ~/.copilot/history-session-state/
        if segments.windows(2).any(|pair| {
            pair[0] == ".copilot"
                && (pair[1] == "session-state" || pair[1] == "history-session-state")
        }) {
            return true;
        }

        // Support nested CLI config path: ~/.config/gh/copilot
        segments
            .windows(2)
            .any(|pair| pair[0] == "gh" && pair[1] == "copilot")
    }

    /// Return true for a native VS Code storage root or one of its children.
    fn looks_like_vscode_storage(path: &Path) -> bool {
        let segments: Vec<String> = path
            .components()
            .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
            .collect();
        segments.iter().any(|segment| {
            matches!(
                segment.as_str(),
                "workspacestorage" | "chatsessions" | "emptywindowchatsessions" | "state.vscdb"
            )
        }) || (path.file_name().is_some_and(|name| name == "User")
            && (path.join("workspaceStorage").exists() || path.join("globalStorage").exists()))
    }

    #[allow(clippy::too_many_lines)]
    fn append_explicit_roots(roots: &mut Vec<PathBuf>, base: &Path) {
        let file_name = base.file_name().and_then(|n| n.to_str());
        let is_config = file_name.is_some_and(|n| n == ".config");
        let is_app_support = file_name.is_some_and(|n| n == "Application Support");
        let is_appdata_roaming = file_name.is_some_and(|n| n == "Roaming")
            && base
                .parent()
                .is_some_and(|p| p.file_name().is_some_and(|n| n == "AppData"));
        let is_code_variant =
            file_name.is_some_and(|n| n == "Code" || n == "Code - Insiders" || n == "VSCodium");
        let is_user = file_name.is_some_and(|n| n == "User");
        let is_global_storage = file_name.is_some_and(|n| n == "globalStorage");
        let is_workspace_storage = file_name.is_some_and(|n| n == "workspaceStorage");
        let is_chat_sessions = file_name.is_some_and(|n| n == "chatSessions");
        let is_workspace_id = base
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|n| n == "workspaceStorage");
        let is_state_db = base.is_file() && file_name.is_some_and(|n| n == "state.vscdb");

        if base.exists()
            && (Self::looks_like_copilot_storage(base)
                || Self::looks_like_vscode_storage(base)
                || is_workspace_id)
        {
            roots.push(base.to_path_buf());
        }
        if is_user {
            roots.push(base.to_path_buf());
        }

        // Explicit roots may point directly at any native storage layer.
        // Keeping the physical root lets discovery and scanning share exactly
        // the same traversal and preserves remote-root provenance.
        if is_state_db || is_workspace_storage || is_chat_sessions || is_global_storage {
            roots.push(base.to_path_buf());
        }
        if is_workspace_id {
            let sessions = base.join("chatSessions");
            if sessions.exists() {
                roots.push(sessions);
            }
        }

        if file_name.is_some_and(|n| n == ".copilot") {
            let session_state = base.join("session-state");
            if session_state.exists() {
                roots.push(session_state);
            }
            let history_state = base.join("history-session-state");
            if history_state.exists() {
                roots.push(history_state);
            }
        }

        if is_global_storage {
            let copilot_chat = base.join("github.copilot-chat");
            if copilot_chat.exists() {
                roots.push(copilot_chat);
            }
            let empty_window = base.join("emptyWindowChatSessions");
            if empty_window.exists() {
                roots.push(empty_window);
            }
        }

        if file_name.is_some_and(|n| n == "gh") {
            let gh_copilot = base.join("copilot");
            if gh_copilot.exists() {
                roots.push(gh_copilot);
            }
        }

        let mut candidates: Vec<PathBuf> = Vec::new();

        if is_config {
            candidates.push(base.join("Code/User"));
            candidates.push(base.join("Code - Insiders/User"));
            candidates.push(base.join("VSCodium/User"));
            candidates.push(base.join("Code/User/globalStorage/github.copilot-chat"));
            candidates.push(base.join("Code - Insiders/User/globalStorage/github.copilot-chat"));
            candidates.push(base.join("VSCodium/User/globalStorage/github.copilot-chat"));
            candidates.push(base.join("gh-copilot"));
            candidates.push(base.join("gh/copilot"));
        }

        if is_app_support {
            candidates.push(base.join("Code/User"));
            candidates.push(base.join("Code - Insiders/User"));
            candidates.push(base.join("VSCodium/User"));
            candidates.push(base.join("Code/User/globalStorage/github.copilot-chat"));
            candidates.push(base.join("Code - Insiders/User/globalStorage/github.copilot-chat"));
            candidates.push(base.join("VSCodium/User/globalStorage/github.copilot-chat"));
        }
        if is_appdata_roaming {
            candidates.push(base.join("Code/User"));
            candidates.push(base.join("Code - Insiders/User"));
            candidates.push(base.join("VSCodium/User"));
            candidates.push(base.join("Code/User/globalStorage/github.copilot-chat"));
            candidates.push(base.join("Code - Insiders/User/globalStorage/github.copilot-chat"));
            candidates.push(base.join("VSCodium/User/globalStorage/github.copilot-chat"));
        }

        if is_code_variant {
            candidates.push(base.join("User"));
            candidates.push(base.join("User/globalStorage/github.copilot-chat"));
        }
        if is_user {
            candidates.push(base.join("globalStorage/github.copilot-chat"));
            candidates.push(base.join("workspaceStorage"));
            candidates.push(base.join("globalStorage"));
        }

        if !(is_config
            || is_app_support
            || is_appdata_roaming
            || is_code_variant
            || is_user
            || is_global_storage)
        {
            candidates.push(base.join(".config/Code/User"));
            candidates.push(base.join(".config/Code - Insiders/User"));
            candidates.push(base.join(".config/VSCodium/User"));
            candidates.push(base.join("Library/Application Support/Code/User"));
            candidates.push(base.join("Library/Application Support/Code - Insiders/User"));
            candidates.push(base.join("Library/Application Support/VSCodium/User"));
            candidates.push(base.join("AppData/Roaming/Code/User"));
            candidates.push(base.join("AppData/Roaming/Code - Insiders/User"));
            candidates.push(base.join("AppData/Roaming/VSCodium/User"));
            candidates.push(base.join(".config/Code/User/globalStorage/github.copilot-chat"));
            candidates
                .push(base.join(".config/Code - Insiders/User/globalStorage/github.copilot-chat"));
            candidates.push(base.join(".config/VSCodium/User/globalStorage/github.copilot-chat"));
            candidates.push(
                base.join(
                    "Library/Application Support/Code/User/globalStorage/github.copilot-chat",
                ),
            );
            candidates.push(base.join(
                "Library/Application Support/Code - Insiders/User/globalStorage/github.copilot-chat",
            ));
            candidates.push(base.join(
                "Library/Application Support/VSCodium/User/globalStorage/github.copilot-chat",
            ));
            candidates
                .push(base.join("AppData/Roaming/Code/User/globalStorage/github.copilot-chat"));
            candidates.push(
                base.join("AppData/Roaming/Code - Insiders/User/globalStorage/github.copilot-chat"),
            );
            candidates
                .push(base.join("AppData/Roaming/VSCodium/User/globalStorage/github.copilot-chat"));
            candidates.push(base.join(".config/gh-copilot"));
            candidates.push(base.join(".config/gh/copilot"));
            candidates.push(base.join(".copilot/session-state"));
            candidates.push(base.join(".copilot/history-session-state"));
            candidates.push(base.join("Code/User"));
            candidates.push(base.join("Code - Insiders/User"));
            candidates.push(base.join("VSCodium/User"));
        }

        for candidate in candidates {
            if candidate.exists()
                && (Self::looks_like_copilot_storage(&candidate)
                    || Self::looks_like_vscode_storage(&candidate)
                    || candidate.file_name().is_some_and(|n| n == "User"))
            {
                roots.push(candidate);
            }
        }
    }

    /// Find JSON and JSONL files that may contain conversation data.
    fn find_conversation_files(root: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        if !root.exists() {
            return files;
        }

        // If root is a file, check it directly.
        if root.is_file() {
            if root
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e == "json" || e == "jsonl")
                && (Self::native_session_file(root) || Self::looks_like_copilot_storage(root))
            {
                files.push(root.to_path_buf());
            }
            return files;
        }

        // Walk the directory for JSON/JSONL files. A User root reaches
        // workspaceStorage/{id}/chatSessions/{session}.jsonl at depth four.
        for entry in WalkDir::new(root)
            .max_depth(5)
            .into_iter()
            .flatten()
            .filter(|e| e.file_type().is_file())
        {
            let name = entry.file_name().to_string_lossy();
            let is_json = name.ends_with(".json") || name.ends_with(".jsonl");
            let is_session = Self::native_session_file(entry.path())
                || Self::looks_like_copilot_storage(entry.path());
            if is_json && is_session {
                files.push(entry.path().to_path_buf());
            }
        }

        // Keep connector traversal deterministic across filesystems/runs.
        files.sort();
        files
    }

    /// Find native VS Code state databases beneath a User/storage root.
    #[cfg(feature = "copilot-sqlite")]
    fn find_db_files(root: &Path) -> Vec<PathBuf> {
        let mut dbs = Vec::new();
        if root.is_file() {
            if root.file_name().is_some_and(|name| name == "state.vscdb") {
                dbs.push(root.to_path_buf());
            }
            return dbs;
        }
        let direct = root.join("state.vscdb");
        if direct.is_file() {
            dbs.push(direct);
        }
        if root.file_name().is_some_and(|name| name == "User") {
            let global = root.join("globalStorage/state.vscdb");
            if global.is_file() {
                dbs.push(global);
            }
            let workspaces = root.join("workspaceStorage");
            if let Ok(entries) = fs::read_dir(workspaces) {
                for entry in entries.flatten() {
                    let db = entry.path().join("state.vscdb");
                    if db.is_file() {
                        dbs.push(db);
                    }
                }
            }
        } else if root
            .file_name()
            .is_some_and(|name| name == "workspaceStorage")
        {
            if let Ok(entries) = fs::read_dir(root) {
                for entry in entries.flatten() {
                    let db = entry.path().join("state.vscdb");
                    if db.is_file() {
                        dbs.push(db);
                    }
                }
            }
        }
        dbs.sort();
        dbs.dedup();
        dbs
    }

    fn source_roots(ctx: &ScanContext) -> Vec<ScanRoot> {
        let mut roots: Vec<ScanRoot> = Vec::new();

        if ctx.use_default_detection() {
            if (Self::looks_like_copilot_storage(&ctx.data_dir)
                || Self::looks_like_vscode_storage(&ctx.data_dir))
                && ctx.data_dir.exists()
            {
                roots.push(ScanRoot::local(ctx.data_dir.clone()));
            } else {
                roots.extend(
                    Self::all_candidate_paths()
                        .into_iter()
                        .filter(|path| path.exists())
                        .map(ScanRoot::local),
                );
            }
        } else {
            for scan_root in &ctx.scan_roots {
                let mut candidates = Vec::new();
                Self::append_explicit_roots(&mut candidates, &scan_root.path);
                roots.extend(candidates.into_iter().map(|path| scan_root.with_path(path)));
            }
        }

        roots.sort_by(|a, b| a.path.cmp(&b.path));
        roots.dedup_by(|a, b| a.path == b.path);
        roots
    }

    fn discover_sources(ctx: &ScanContext) -> Vec<DiscoveredSourceFile> {
        let mut out = Vec::new();
        for root in Self::source_roots(ctx) {
            let files = Self::find_conversation_files(&root.path);
            for file in files {
                if !file_modified_since(&file, ctx.since_ts) {
                    continue;
                }
                out.push(
                    DiscoveredSourceFile::new(
                        "copilot",
                        &root,
                        file,
                        DiscoveredSourceRole::PrimarySessionLog,
                        true,
                    )
                    .with_fs_metadata(),
                );
            }
            #[cfg(feature = "copilot-sqlite")]
            for db_path in Self::find_db_files(&root.path) {
                if !file_modified_since(&db_path, ctx.since_ts) {
                    continue;
                }
                out.push(
                    DiscoveredSourceFile::new(
                        "copilot",
                        &root,
                        db_path,
                        DiscoveredSourceRole::SqliteDatabase,
                        true,
                    )
                    .with_fs_metadata(),
                );
            }
        }
        out.sort_by(|a, b| a.source_path.cmp(&b.source_path));
        out.dedup_by(|a, b| a.source_path == b.source_path);
        out
    }

    fn native_session_file(path: &Path) -> bool {
        let is_session_file = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                ext.eq_ignore_ascii_case("json") || ext.eq_ignore_ascii_case("jsonl")
            });
        is_session_file
            && path.components().any(|component| {
                matches!(
                    component.as_os_str().to_str(),
                    Some("chatSessions" | "emptyWindowChatSessions" | "transferredChatSessions")
                )
            })
    }

    fn workspace_for_session(path: &Path) -> Option<PathBuf> {
        let workspace_json = path.parent()?.parent()?.join("workspace.json");
        let value: Value = serde_json::from_str(&fs::read_to_string(workspace_json).ok()?).ok()?;
        value
            .get("folder")
            .or_else(|| value.get("workspaceFolder"))
            .or_else(|| value.get("workspace"))
            .and_then(Value::as_str)
            .and_then(Self::path_from_uri)
    }

    fn path_from_uri(value: &str) -> Option<PathBuf> {
        if let Some(remote) = value.strip_prefix("vscode-remote://") {
            // VS Code's workspace.json stores remote folders as
            // `vscode-remote://<authority>/<path>`. The authority identifies
            // the SSH/WSL/container target and is not part of the workspace
            // path that consumers should use for project matching.
            let path = remote.get(remote.find('/')?..)?;
            let decoded = Self::percent_decode(path)?;
            return Some(PathBuf::from(decoded));
        }
        let path = value.strip_prefix("file://")?;
        let decoded = Self::percent_decode(path)?;
        // VS Code records Windows file URIs as file:///c%3A/... . Preserve a
        // Windows drive path when scanning a copied Windows workspace on Unix.
        if decoded.len() > 2 && decoded.starts_with('/') && decoded.as_bytes().get(2) == Some(&b':')
        {
            return Some(PathBuf::from(&decoded[1..]));
        }
        Some(PathBuf::from(decoded))
    }

    fn percent_decode(value: &str) -> Option<String> {
        let bytes = value.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] == b'%' {
                let high = Self::hex_value(*bytes.get(index + 1)?)?;
                let low = Self::hex_value(*bytes.get(index + 2)?)?;
                out.push(high * 16 + low);
                index += 3;
            } else {
                out.push(bytes[index]);
                index += 1;
            }
        }
        String::from_utf8(out).ok()
    }

    const fn hex_value(value: u8) -> Option<u8> {
        match value {
            b'0'..=b'9' => Some(value - b'0'),
            b'a'..=b'f' => Some(value - b'a' + 10),
            b'A'..=b'F' => Some(value - b'A' + 10),
            _ => None,
        }
    }

    fn copilot_text(value: &Value) -> Option<String> {
        if let Some(text) = value.as_str() {
            return Some(text.to_string());
        }
        if let Some(text) = value.get("text").and_then(Value::as_str) {
            return Some(text.to_string());
        }
        if let Some(parts) = value.get("parts") {
            let text = flatten_content(parts);
            if !text.is_empty() {
                return Some(text);
            }
        }
        None
    }

    fn copilot_agent_value(value: &Value) -> bool {
        if let Some(value) = value.get("value") {
            return Self::copilot_agent_value(value);
        }
        let Some(text) = value.as_str() else {
            return false;
        };
        let normalized = text.to_ascii_lowercase();
        normalized == "github.copilot-chat"
            || normalized == "github copilot"
            || normalized == "github-copilot"
            || normalized.contains("github.copilot-chat")
            || normalized.contains("github copilot")
    }

    fn has_copilot_evidence(session: &Value) -> bool {
        if session
            .get("responderUsername")
            .is_some_and(Self::copilot_agent_value)
        {
            return true;
        }
        session
            .get("requests")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|request| request.get("agent"))
            .any(|agent| {
                if Self::copilot_agent_value(agent) {
                    return true;
                }
                ["id", "name", "displayName", "extensionId", "publisher"]
                    .iter()
                    .filter_map(|key| agent.get(*key))
                    .any(Self::copilot_agent_value)
            })
    }

    fn response_parts(value: &Value) -> (String, Vec<NormalizedInvocation>) {
        let parts: Vec<&Value> = value
            .as_array()
            .map_or_else(|| vec![value], |parts| parts.iter().collect());
        let mut text = String::new();
        let mut invocations = extract_invocations_from_content_blocks(value);
        for part in &parts {
            let part_text = if part.get("kind").and_then(Value::as_str) == Some("markdownContent") {
                part.get("content").and_then(Self::copilot_text)
            } else {
                Self::copilot_text(part)
            };
            if let Some(part_text) = part_text.filter(|text| !text.trim().is_empty()) {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&part_text);
            }

            if part.get("kind").and_then(Value::as_str) == Some("toolInvocationSerialized") {
                let name = part
                    .get("toolId")
                    .or_else(|| part.get("toolName"))
                    .or_else(|| part.get("name"))
                    .and_then(Value::as_str);
                if let Some(name) = name {
                    invocations.push(NormalizedInvocation {
                        kind: "tool".to_string(),
                        name: name.to_string(),
                        raw_name: None,
                        call_id: part
                            .get("toolCallId")
                            .or_else(|| part.get("callId"))
                            .and_then(Value::as_str)
                            .map(String::from),
                        arguments: part
                            .get("arguments")
                            .or_else(|| part.get("parameters"))
                            .cloned(),
                    });
                }
            }
        }
        (text, invocations)
    }

    #[allow(clippy::too_many_lines)]
    fn parse_native_session_value(
        session: &Value,
        source_path: &Path,
        workspace_hint: Option<PathBuf>,
        storage: &str,
    ) -> Option<NormalizedConversation> {
        if !session.is_object() || !Self::has_copilot_evidence(session) {
            return None;
        }
        let requests = session.get("requests").and_then(Value::as_array)?;
        let external_id = session
            .get("sessionId")
            .or_else(|| session.get("id"))
            .and_then(Value::as_str)
            .map(String::from)
            .or_else(|| {
                source_path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .map(String::from)
            });
        let workspace = session
            .get("workingDirectory")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .or(workspace_hint);
        let mut messages = Vec::new();
        let mut started_at = session.get("creationDate").and_then(parse_timestamp);
        let mut ended_at = started_at;

        for request in requests {
            let Some(request_value) = request.get("message") else {
                continue;
            };
            let user_content = Self::copilot_text(request_value)
                .unwrap_or_else(|| Self::extract_message_content(request));
            let user_ts = request.get("timestamp").and_then(parse_timestamp);
            if !user_content.trim().is_empty() {
                started_at = min_timestamp(started_at, user_ts);
                ended_at = max_timestamp(ended_at, user_ts);
                messages.push(NormalizedMessage {
                    idx: i64::try_from(messages.len()).unwrap_or(i64::MAX),
                    role: "user".to_string(),
                    author: Some("user".to_string()),
                    created_at: user_ts,
                    content: user_content,
                    extra: request.clone(),
                    invocations: Vec::new(),
                    snippets: Vec::new(),
                });
            }

            if let Some(response) = request.get("response") {
                let (content, invocations) = Self::response_parts(response);
                let response_ts = request
                    .get("responseTimestamp")
                    .or_else(|| response.get("timestamp"))
                    .and_then(parse_timestamp);
                started_at = min_timestamp(started_at, response_ts);
                ended_at = max_timestamp(ended_at, response_ts);
                if !content.trim().is_empty() || !invocations.is_empty() {
                    messages.push(NormalizedMessage {
                        idx: i64::try_from(messages.len()).unwrap_or(i64::MAX),
                        role: "assistant".to_string(),
                        author: Some("copilot".to_string()),
                        created_at: response_ts,
                        content,
                        extra: response.clone(),
                        invocations,
                        snippets: Vec::new(),
                    });
                }
            }
        }
        if messages.is_empty() {
            return None;
        }
        let title = session
            .get("customTitle")
            .or_else(|| session.get("computedTitle"))
            .and_then(Value::as_str)
            .filter(|title| !title.is_empty())
            .map(String::from)
            .or_else(|| {
                messages
                    .iter()
                    .find(|message| message.role == "user")
                    .map(|message| {
                        message
                            .content
                            .lines()
                            .next()
                            .unwrap_or(&message.content)
                            .chars()
                            .take(120)
                            .collect()
                    })
            });
        Some(NormalizedConversation {
            agent_slug: "copilot".to_string(),
            external_id,
            title,
            workspace,
            source_path: source_path.to_path_buf(),
            started_at,
            ended_at,
            metadata: serde_json::json!({"source": "copilot", "storage": storage}),
            messages,
        })
    }

    fn parse_native_session_file(path: &Path) -> Result<Vec<NormalizedConversation>> {
        let content = fs::read_to_string(path)?;
        let value = if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
        {
            replay_operation_log(&content)?
        } else {
            serde_json::from_str(&content)?
        };
        Ok(Self::parse_native_session_value(
            &value,
            path,
            Self::workspace_for_session(path),
            if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
            {
                "vscode-workspace-jsonl"
            } else {
                "vscode-workspace-json"
            },
        )
        .into_iter()
        .collect())
    }

    #[cfg(feature = "copilot-sqlite")]
    fn workspace_for_database(path: &Path) -> Option<PathBuf> {
        let workspace_root = path.parent()?.join("workspace.json");
        if workspace_root.is_file() {
            let value: Value =
                serde_json::from_str(&fs::read_to_string(workspace_root).ok()?).ok()?;
            if let Some(folder) = value
                .get("folder")
                .or_else(|| value.get("workspaceFolder"))
                .and_then(Value::as_str)
                .and_then(Self::path_from_uri)
            {
                return Some(folder);
            }
        }
        None
    }

    /// Read VS Code's pre-filesystem `interactive.sessions` storage key.
    ///
    /// The key contains one JSON array of real serializable chat sessions.
    /// `chat.ChatSessionStore.index` is intentionally ignored: it contains
    /// titles/timing only and cannot reconstruct transcript bodies.
    #[cfg(feature = "copilot-sqlite")]
    fn parse_interactive_sessions_db(
        path: &Path,
        since_ts: Option<i64>,
    ) -> Result<Vec<NormalizedConversation>> {
        let conn = open_with_flags(
            path.to_string_lossy().as_ref(),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .with_context(|| format!("failed to open VS Code state database {}", path.display()))?;
        let _ = conn.execute("PRAGMA busy_timeout = 5000;");
        let rows = conn
            .query_map_collect(
                "SELECT value FROM ItemTable WHERE key = 'interactive.sessions' AND value IS NOT NULL",
                params![],
                |row| row.get::<_, String>(0),
            )
            .unwrap_or_default();
        let workspace = Self::workspace_for_database(path);
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for raw in rows {
            let Ok(Value::Array(sessions)) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            for session in sessions {
                let Some(session_id) = session
                    .get("sessionId")
                    .or_else(|| session.get("id"))
                    .and_then(Value::as_str)
                else {
                    continue;
                };
                if !seen.insert(session_id.to_string()) {
                    continue;
                }
                let source_path = path.join(format!("interactive.sessions#{session_id}"));
                if let Some(conversation) = Self::parse_native_session_value(
                    &session,
                    &source_path,
                    workspace.clone(),
                    "vscode-interactive-sessions",
                ) && since_ts
                    .is_none_or(|since| conversation.ended_at.is_none_or(|end| end >= since))
                {
                    out.push(conversation);
                }
            }
        }
        Ok(out)
    }

    /// Parse a single JSON file that may contain one or more conversations.
    ///
    /// Handles multiple formats:
    /// 1. Array of conversation objects at top level
    /// 2. Single conversation object
    /// 3. Object with a "conversations" key containing an array
    fn parse_conversation_file(path: &Path) -> Result<Vec<NormalizedConversation>> {
        let content = fs::read_to_string(path)?;
        let val: Value = serde_json::from_str(&content)?;
        let mut conversations = Vec::new();

        // Strategy: try multiple known shapes of the JSON.
        let conv_array = if let Some(arr) = val.as_array() {
            // Top-level array of conversations
            arr.clone()
        } else if let Some(arr) = val.get("conversations").and_then(|v| v.as_array()) {
            // Object with "conversations" key
            arr.clone()
        } else if val.get("id").is_some() || val.get("turns").is_some() {
            // Single conversation object
            vec![val]
        } else {
            // Unknown format — skip
            tracing::debug!(
                path = %path.display(),
                "copilot: skipping file with unrecognized JSON structure"
            );
            return Ok(Vec::new());
        };

        for conv_val in &conv_array {
            if let Some(parsed) = Self::parse_single_conversation(conv_val, path) {
                conversations.push(parsed);
            }
        }

        Ok(conversations)
    }

    /// Parse a single conversation object from Copilot Chat JSON.
    #[allow(clippy::too_many_lines)]
    fn parse_single_conversation(
        conv: &Value,
        source_path: &Path,
    ) -> Option<NormalizedConversation> {
        let external_id = conv
            .get("id")
            .or_else(|| conv.get("conversationId"))
            .and_then(|v| v.as_str())
            .map(String::from);

        let title = conv
            .get("title")
            .or_else(|| conv.get("chatTitle"))
            .and_then(|v| v.as_str())
            .map(String::from);

        // Workspace/project path.
        let workspace = conv
            .get("workspaceFolder")
            .or_else(|| conv.get("workspace"))
            .or_else(|| conv.get("workspacePath"))
            .and_then(|v| v.as_str())
            .map(PathBuf::from);

        // Parse messages from "turns" array (VS Code Copilot Chat format).
        let mut messages = Vec::new();
        let mut started_at: Option<i64> = None;
        let mut ended_at: Option<i64> = None;

        if let Some(turns) = conv.get("turns").and_then(|v| v.as_array()) {
            for turn in turns {
                // Each turn typically has a "request" and "response".
                if let Some(request) = turn.get("request") {
                    let content = Self::extract_message_content(request);
                    if !content.trim().is_empty() {
                        let ts = Self::extract_turn_timestamp(request);
                        started_at = match (started_at, ts) {
                            (Some(curr), Some(t)) => Some(curr.min(t)),
                            (None, Some(t)) => Some(t),
                            (other, None) => other,
                        };
                        ended_at = match (ended_at, ts) {
                            (Some(curr), Some(t)) => Some(curr.max(t)),
                            (None, Some(t)) => Some(t),
                            (other, None) => other,
                        };

                        messages.push(NormalizedMessage {
                            idx: i64::try_from(messages.len()).unwrap_or(i64::MAX),
                            role: "user".to_string(),
                            author: Some("user".to_string()),
                            created_at: ts,
                            content,
                            extra: request.clone(),
                            invocations: Vec::new(),
                            snippets: Vec::new(),
                        });
                    }
                }

                if let Some(response) = turn.get("response") {
                    let content = Self::extract_message_content(response);
                    if !content.trim().is_empty() {
                        let ts = Self::extract_turn_timestamp(response);
                        started_at = match (started_at, ts) {
                            (Some(curr), Some(t)) => Some(curr.min(t)),
                            (None, Some(t)) => Some(t),
                            (other, None) => other,
                        };
                        ended_at = match (ended_at, ts) {
                            (Some(curr), Some(t)) => Some(curr.max(t)),
                            (None, Some(t)) => Some(t),
                            (other, None) => other,
                        };

                        messages.push(NormalizedMessage {
                            idx: i64::try_from(messages.len()).unwrap_or(i64::MAX),
                            role: "assistant".to_string(),
                            author: Some("copilot".to_string()),
                            created_at: ts,
                            content,
                            extra: response.clone(),
                            invocations: Vec::new(),
                            snippets: Vec::new(),
                        });
                    }
                }
            }
        }

        // Alternative format: "messages" array with role/content objects.
        if messages.is_empty()
            && let Some(msgs) = conv.get("messages").and_then(|v| v.as_array())
        {
            for msg in msgs {
                let role = msg
                    .get("role")
                    .and_then(|v| v.as_str())
                    .unwrap_or("assistant")
                    .to_string();

                let content = Self::extract_message_content(msg);
                if content.trim().is_empty() {
                    continue;
                }

                let ts = Self::extract_turn_timestamp(msg);
                started_at = match (started_at, ts) {
                    (Some(curr), Some(t)) => Some(curr.min(t)),
                    (None, Some(t)) => Some(t),
                    (other, None) => other,
                };
                ended_at = match (ended_at, ts) {
                    (Some(curr), Some(t)) => Some(curr.max(t)),
                    (None, Some(t)) => Some(t),
                    (other, None) => other,
                };

                messages.push(NormalizedMessage {
                    idx: i64::try_from(messages.len()).unwrap_or(i64::MAX),
                    role: role.clone(),
                    author: Some(if role == "user" {
                        "user".to_string()
                    } else {
                        "copilot".to_string()
                    }),
                    created_at: ts,
                    content,
                    extra: msg.clone(),
                    invocations: Vec::new(),
                    snippets: Vec::new(),
                });
            }
        }

        // Also check top-level timestamp if per-message timestamps missing.
        if started_at.is_none() {
            started_at = conv
                .get("createdAt")
                .or_else(|| conv.get("created_at"))
                .or_else(|| conv.get("timestamp"))
                .and_then(parse_timestamp);
        }
        if ended_at.is_none() {
            ended_at = conv
                .get("updatedAt")
                .or_else(|| conv.get("updated_at"))
                .and_then(parse_timestamp);
        }
        // If only one boundary is available, mirror it so timeline consumers
        // still get a consistent non-empty range.
        if started_at.is_none() {
            started_at = ended_at;
        }
        if ended_at.is_none() {
            ended_at = started_at;
        }

        if messages.is_empty() {
            return None;
        }

        // Derive title from first user message if not explicitly set.
        let title = title.or_else(|| {
            messages.iter().find(|m| m.role == "user").map(|m| {
                m.content
                    .lines()
                    .next()
                    .unwrap_or(&m.content)
                    .chars()
                    .take(120)
                    .collect::<String>()
            })
        });

        let metadata = serde_json::json!({
            "source": "copilot",
        });

        Some(NormalizedConversation {
            agent_slug: "copilot".to_string(),
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

    /// Check if a file path looks like a Copilot CLI event log (JSONL format).
    fn is_cli_event_log(path: &Path) -> bool {
        // Explicit .jsonl extension
        if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e == "jsonl")
        {
            return true;
        }

        // Files named events.jsonl inside session-state directories
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == "events.jsonl" {
            return true;
        }

        // JSON files inside session-state or history-session-state directories
        // are CLI format (one session state JSON per session-id).
        let path_str = path.to_string_lossy().to_lowercase();
        if path_str.contains("session-state") || path_str.contains("history-session-state") {
            return true;
        }

        false
    }

    /// Parse a Copilot CLI event log file (JSONL format).
    ///
    /// Each line is a JSON object representing an event. We extract events
    /// with message-like types (`user.message`, `assistant.message`, or
    /// events containing `role`+`content` fields) and assemble them into
    /// a single conversation per session file.
    #[allow(clippy::too_many_lines)]
    fn parse_cli_event_log(path: &Path) -> Result<Vec<NormalizedConversation>> {
        let content = fs::read_to_string(path)?;

        // If it looks like a single JSON document (not JSONL), try the legacy
        // CLI session-state format: a JSON object with a messages/conversation array.
        let is_jsonl = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("jsonl"));
        let trimmed = content.trim_start();
        if !is_jsonl && (trimmed.starts_with('{') || trimmed.starts_with('[')) {
            if let Ok(val) = serde_json::from_str::<Value>(&content) {
                return Ok(Self::parse_cli_session_json(&val, path));
            }
        }

        // JSONL: each line is a separate JSON event.
        let reader = std::io::BufReader::new(content.as_bytes());
        let mut messages = Vec::new();
        let mut started_at: Option<i64> = None;
        let mut ended_at: Option<i64> = None;
        let mut session_id: Option<String> = None;
        let mut workspace: Option<PathBuf> = None;

        for line in reader.lines() {
            let Ok(line) = line else {
                continue;
            };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            let Ok(event) = serde_json::from_str::<Value>(line) else {
                continue;
            };

            // Extract session ID from any event if we haven't found one yet.
            if session_id.is_none() {
                session_id = event
                    .get("session_id")
                    .or_else(|| event.get("sessionId"))
                    .and_then(|v| v.as_str())
                    .map(String::from);
            }

            // Extract workspace/cwd from session start events.
            if workspace.is_none() {
                workspace = event
                    .get("cwd")
                    .or_else(|| event.get("workingDirectory"))
                    .or_else(|| event.get("workspace"))
                    .and_then(|v| v.as_str())
                    .map(PathBuf::from);
            }

            // Extract the event type if present.
            let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");

            let ts = Self::extract_turn_timestamp(&event);

            // Update time bounds.
            started_at = match (started_at, ts) {
                (Some(curr), Some(t)) => Some(curr.min(t)),
                (None, Some(t)) => Some(t),
                (other, None) => other,
            };
            ended_at = match (ended_at, ts) {
                (Some(curr), Some(t)) => Some(curr.max(t)),
                (None, Some(t)) => Some(t),
                (other, None) => other,
            };

            // Determine role and extract content from the event.
            let (role, content) = Self::extract_cli_event_message(&event, event_type);
            if role.is_empty() || content.trim().is_empty() {
                continue;
            }

            messages.push(NormalizedMessage {
                idx: i64::try_from(messages.len()).unwrap_or(i64::MAX),
                role: role.clone(),
                author: Some(if role == "user" {
                    "user".to_string()
                } else {
                    "copilot".to_string()
                }),
                created_at: ts,
                content,
                extra: event,
                invocations: Vec::new(),
                snippets: Vec::new(),
            });
        }

        if messages.is_empty() {
            return Ok(Vec::new());
        }

        // Use session directory name as session ID if not found in events.
        if session_id.is_none() {
            session_id = path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .map(String::from);
        }

        // Mirror timestamps if only one boundary is available.
        if started_at.is_none() {
            started_at = ended_at;
        }
        if ended_at.is_none() {
            ended_at = started_at;
        }

        let title = messages.iter().find(|m| m.role == "user").map(|m| {
            m.content
                .lines()
                .next()
                .unwrap_or(&m.content)
                .chars()
                .take(120)
                .collect::<String>()
        });

        let metadata = serde_json::json!({
            "source": "copilot-cli",
        });

        Ok(vec![NormalizedConversation {
            agent_slug: "copilot".to_string(),
            external_id: session_id,
            title,
            workspace,
            source_path: path.to_path_buf(),
            started_at,
            ended_at,
            metadata,
            messages,
        }])
    }

    /// Parse a legacy CLI session-state JSON file (single JSON document).
    ///
    /// These are used by Copilot CLI v1 (`history-session-state/{id}.json`)
    /// and checkpoint files. The format is a JSON object containing conversation
    /// data, potentially with `messages`, `conversation`, or `events` arrays.
    fn parse_cli_session_json(val: &Value, path: &Path) -> Vec<NormalizedConversation> {
        // If the document has a top-level "messages" or "conversation" array,
        // treat it as a chat-style conversation and delegate to the existing parser.
        if val.get("turns").is_some()
            || val.get("messages").is_some()
            || val.get("conversations").is_some()
        {
            return Self::parse_conversation_file_from_value(val, path);
        }

        // Try extracting messages from "events" array (session-state checkpoint format).
        let events = val
            .get("events")
            .and_then(|v| v.as_array())
            .or_else(|| val.get("history").and_then(|v| v.as_array()));

        let Some(events) = events else {
            // Fall back to treating the entire JSON as a single-conversation document.
            return Self::parse_conversation_file_from_value(val, path);
        };

        let mut messages = Vec::new();
        let mut started_at: Option<i64> = None;
        let mut ended_at: Option<i64> = None;

        for event in events {
            let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let ts = Self::extract_turn_timestamp(event);

            started_at = match (started_at, ts) {
                (Some(curr), Some(t)) => Some(curr.min(t)),
                (None, Some(t)) => Some(t),
                (other, None) => other,
            };
            ended_at = match (ended_at, ts) {
                (Some(curr), Some(t)) => Some(curr.max(t)),
                (None, Some(t)) => Some(t),
                (other, None) => other,
            };

            let (role, content) = Self::extract_cli_event_message(event, event_type);
            if role.is_empty() || content.trim().is_empty() {
                continue;
            }

            messages.push(NormalizedMessage {
                idx: i64::try_from(messages.len()).unwrap_or(i64::MAX),
                role: role.clone(),
                author: Some(if role == "user" {
                    "user".to_string()
                } else {
                    "copilot".to_string()
                }),
                created_at: ts,
                content,
                extra: event.clone(),
                invocations: Vec::new(),
                snippets: Vec::new(),
            });
        }

        if messages.is_empty() {
            return Vec::new();
        }

        let session_id = val
            .get("session_id")
            .or_else(|| val.get("sessionId"))
            .or_else(|| val.get("id"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| path.file_stem().and_then(|n| n.to_str()).map(String::from));

        let workspace = val
            .get("cwd")
            .or_else(|| val.get("workingDirectory"))
            .or_else(|| val.get("workspace"))
            .or_else(|| val.get("workspacePath"))
            .and_then(|v| v.as_str())
            .map(PathBuf::from);

        if started_at.is_none() {
            started_at = ended_at;
        }
        if ended_at.is_none() {
            ended_at = started_at;
        }

        let title = messages.iter().find(|m| m.role == "user").map(|m| {
            m.content
                .lines()
                .next()
                .unwrap_or(&m.content)
                .chars()
                .take(120)
                .collect::<String>()
        });

        let metadata = serde_json::json!({
            "source": "copilot-cli",
        });

        vec![NormalizedConversation {
            agent_slug: "copilot".to_string(),
            external_id: session_id,
            title,
            workspace,
            source_path: path.to_path_buf(),
            started_at,
            ended_at,
            metadata,
            messages,
        }]
    }

    /// Parse a JSON value through the existing VS Code conversation parser.
    fn parse_conversation_file_from_value(val: &Value, path: &Path) -> Vec<NormalizedConversation> {
        let mut conversations = Vec::new();

        let conv_array = if let Some(arr) = val.as_array() {
            arr.clone()
        } else if let Some(arr) = val.get("conversations").and_then(|v| v.as_array()) {
            arr.clone()
        } else if val.get("id").is_some()
            || val.get("turns").is_some()
            || val.get("messages").is_some()
        {
            vec![val.clone()]
        } else {
            return Vec::new();
        };

        for conv_val in &conv_array {
            if let Some(parsed) = Self::parse_single_conversation(conv_val, path) {
                conversations.push(parsed);
            }
        }

        conversations
    }

    /// Extract role and content from a CLI event log entry.
    ///
    /// Recognizes multiple event type naming conventions:
    /// - `user.message` / `assistant.message` (documented Copilot CLI format)
    /// - `userPromptSubmitted` / `assistantResponse` (hook event names)
    /// - Events with explicit `role` field
    fn extract_cli_event_message(event: &Value, event_type: &str) -> (String, String) {
        let type_lower = event_type.to_lowercase();

        // Determine role from event type.
        let role_from_type = if type_lower.contains("user")
            || type_lower == "userpromptsubmitted"
            || type_lower == "prompt"
        {
            Some("user".to_string())
        } else if type_lower.contains("assistant")
            || type_lower == "assistantresponse"
            || type_lower == "response"
            || type_lower == "completion"
        {
            Some("assistant".to_string())
        } else {
            None
        };

        // Explicit role field takes precedence.
        let role = event
            .get("role")
            .and_then(|v| v.as_str())
            .map(|r| {
                if r == "user" || r == "human" {
                    "user".to_string()
                } else {
                    "assistant".to_string()
                }
            })
            .or(role_from_type);

        let Some(role) = role else {
            return (String::new(), String::new());
        };

        // Extract content from various fields.
        let content = Self::extract_message_content(event);

        // If standard extraction failed, try event-specific fields.
        if content.trim().is_empty() {
            // Try "prompt" field for user messages.
            if let Some(prompt) = event.get("prompt").or_else(|| event.get("initialPrompt")) {
                let text = flatten_content(prompt);
                if !text.is_empty() {
                    return (role, text);
                }
            }
            // Try "output" / "result" for assistant messages.
            if let Some(output) = event.get("output").or_else(|| event.get("result")) {
                let text = flatten_content(output);
                if !text.is_empty() {
                    return (role, text);
                }
            }
        }

        (role, content)
    }

    /// Extract message content from various possible field names/shapes.
    fn extract_message_content(val: &Value) -> String {
        // Try "message" field (Copilot Chat turns format)
        if let Some(msg) = val.get("message") {
            let text = flatten_content(msg);
            if !text.is_empty() {
                return text;
            }
        }

        // Try "content" field (standard chat format)
        if let Some(content) = val.get("content") {
            let text = flatten_content(content);
            if !text.is_empty() {
                return text;
            }
        }

        // Try "text" field
        if let Some(text) = val.get("text") {
            let text = flatten_content(text);
            if !text.is_empty() {
                return text;
            }
        }

        // Try "value" field
        if let Some(value) = val.get("value") {
            let text = flatten_content(value);
            if !text.is_empty() {
                return text;
            }
        }

        String::new()
    }

    /// Extract timestamp from a turn/message object.
    fn extract_turn_timestamp(val: &Value) -> Option<i64> {
        let candidates = ["timestamp", "createdAt", "created_at", "time", "ts", "date"];
        for key in candidates {
            if let Some(ts) = val.get(key).and_then(parse_timestamp) {
                return Some(ts);
            }
        }
        None
    }
}

impl Connector for CopilotConnector {
    fn detect(&self) -> DetectionResult {
        franken_detection_for_connector("copilot").unwrap_or_else(DetectionResult::not_found)
    }

    fn scan(&self, ctx: &ScanContext) -> Result<Vec<NormalizedConversation>> {
        let roots: Vec<PathBuf> = Self::source_roots(ctx)
            .into_iter()
            .map(|root| root.path)
            .collect();

        if roots.is_empty() {
            return Ok(Vec::new());
        }

        let mut all_conversations = Vec::new();
        let mut seen_external_ids = HashSet::new();

        for root in roots {
            let files = Self::find_conversation_files(&root);
            tracing::debug!(
                root = %root.display(),
                file_count = files.len(),
                "copilot: scanning conversation files"
            );

            for file in files {
                if !file_modified_since(&file, ctx.since_ts) {
                    continue;
                }

                // Dispatch to the appropriate parser based on file type.
                let result = if Self::native_session_file(&file) {
                    Self::parse_native_session_file(&file)
                } else if !Self::looks_like_copilot_storage(&file) {
                    // A User root also contains every other extension's
                    // globalStorage. Copilot's legacy/synthetic parser is
                    // restricted to high-signal Copilot/CLI paths so a shared
                    // root cannot leak another provider's transcripts.
                    Ok(Vec::new())
                } else if Self::is_cli_event_log(&file) {
                    Self::parse_cli_event_log(&file)
                } else {
                    Self::parse_conversation_file(&file)
                };

                match result {
                    Ok(convs) => {
                        tracing::debug!(
                            file = %file.display(),
                            conversations = convs.len(),
                            "copilot: parsed conversation file"
                        );
                        for conversation in convs {
                            if conversation
                                .external_id
                                .as_ref()
                                .is_none_or(|id| seen_external_ids.insert(id.clone()))
                            {
                                all_conversations.push(conversation);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::debug!(
                            file = %file.display(),
                            error = %e,
                            "copilot: skipping unparseable file"
                        );
                    }
                }
            }

            #[cfg(feature = "copilot-sqlite")]
            for db_path in Self::find_db_files(&root) {
                if !file_modified_since(&db_path, ctx.since_ts) {
                    continue;
                }
                match Self::parse_interactive_sessions_db(&db_path, ctx.since_ts) {
                    Ok(convs) => {
                        tracing::debug!(
                            file = %db_path.display(),
                            conversations = convs.len(),
                            "copilot: parsed VS Code interactive.sessions database"
                        );
                        for conversation in convs {
                            if conversation
                                .external_id
                                .as_ref()
                                .is_none_or(|id| seen_external_ids.insert(id.clone()))
                            {
                                all_conversations.push(conversation);
                            }
                        }
                    }
                    Err(error) => tracing::debug!(
                        file = %db_path.display(),
                        error = %error,
                        "copilot: skipping unreadable VS Code state database"
                    ),
                }
            }
        }

        Ok(all_conversations)
    }

    fn discover_source_files(&self, ctx: &ScanContext) -> Result<Vec<DiscoveredSourceFile>> {
        Ok(Self::discover_sources(ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::scan::ScanRoot;
    #[cfg(feature = "copilot-sqlite")]
    use crate::connectors::sqlite_sync::ConnectionExt;
    #[cfg(feature = "copilot-sqlite")]
    use rusqlite::params;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Helper to write a JSON file into a temp directory.
    fn write_json(dir: &Path, filename: &str, content: &str) -> PathBuf {
        let path = dir.join(filename);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn detect_returns_not_found_when_no_dirs_exist() {
        let connector = CopilotConnector::new();
        // On most test systems Copilot dirs won't exist.
        // This test just ensures detect() doesn't panic.
        let result = connector.detect();
        // Result depends on system — franken detection includes positive and
        // negative probe evidence. Just assert basic structural invariants.
        assert!(!result.evidence.is_empty());
        if result.detected {
            assert!(!result.root_paths.is_empty());
        }
    }

    #[test]
    fn scan_empty_dir_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("copilot-chat");
        fs::create_dir_all(&root).unwrap();

        let connector = CopilotConnector::new();
        let ctx = ScanContext::local_default(root, None);
        let convs = connector.scan(&ctx).unwrap();
        assert!(convs.is_empty());
    }

    #[test]
    fn scan_native_workspace_jsonl_replays_operations_and_resolves_workspace() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("Code/User/workspaceStorage/workspace-a");
        let sessions = workspace.join("chatSessions");
        fs::create_dir_all(&sessions).unwrap();
        write_json(
            &workspace,
            "workspace.json",
            r#"{"folder":"file:///workspaces/demo%20project"}"#,
        );
        let initial = serde_json::json!({
            "version": 3,
            "creationDate": 1_700_000_000_000_i64,
            "customTitle": null,
            "sessionId": "native-jsonl-001",
            "responderUsername": "",
            "requests": [{
                "requestId": "r1",
                "timestamp": 1_700_000_000_100_i64,
                "message": {"text": "first question", "parts": [{"text": "first question"}]},
                "agent": {"extensionId": {"value": "github.copilot-chat"}},
                "response": [{"kind": "markdownContent", "content": "first answer"}],
                "responseTimestamp": 1_700_000_000_200_i64
            }]
        });
        let append_request = serde_json::json!({
            "requestId": "r2",
            "timestamp": 1_700_000_000_300_i64,
            "message": {"text": "second question"},
            "agent": {"id": "github.copilot-chat"},
            "response": [{
                "kind": "toolInvocationSerialized",
                "toolId": "terminal",
                "toolCallId": "call-2"
            }],
            "responseTimestamp": 1_700_000_000_400_i64
        });
        let content = format!(
            "{}\n{}\n{}\n",
            serde_json::json!({"kind": 0, "v": initial}),
            serde_json::json!({"kind": 1, "k": ["customTitle"], "v": "Native chat"}),
            serde_json::json!({"kind": 2, "k": ["requests"], "v": [append_request]})
        );
        write_json(&sessions, "native-jsonl-001.jsonl", &content);

        let connector = CopilotConnector::new();
        let ctx = ScanContext::with_roots(
            tmp.path().join("cass"),
            vec![ScanRoot::local(tmp.path().join("Code/User"))],
            None,
        );
        let convs = connector.scan(&ctx).unwrap();

        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].external_id.as_deref(), Some("native-jsonl-001"));
        assert_eq!(convs[0].title.as_deref(), Some("Native chat"));
        assert_eq!(
            convs[0].workspace,
            Some(PathBuf::from("/workspaces/demo project"))
        );
        assert_eq!(convs[0].messages.len(), 4);
        assert_eq!(convs[0].messages[0].content, "first question");
        assert_eq!(convs[0].messages[1].content, "first answer");
        assert_eq!(convs[0].messages[3].invocations[0].name, "terminal");
        assert_eq!(
            convs[0].messages[3].invocations[0].call_id.as_deref(),
            Some("call-2")
        );
        crate::connectors::assert_discovery_covers_scan_sources(&connector, &ctx);
    }

    #[test]
    fn path_from_uri_supports_file_and_vscode_remote_forms() {
        assert_eq!(
            CopilotConnector::path_from_uri("file:///workspaces/demo%20project"),
            Some(PathBuf::from("/workspaces/demo project"))
        );
        assert_eq!(
            CopilotConnector::path_from_uri(
                "vscode-remote://ssh-remote%2Byeowoolmac.local/Users/yeowool/Documents/Eastself"
            ),
            Some(PathBuf::from("/Users/yeowool/Documents/Eastself"))
        );
        assert_eq!(
            CopilotConnector::path_from_uri(
                "vscode-remote://ssh-remote%2B10.0.0.14/Users/yeowool/Documents/Eastself"
            ),
            Some(PathBuf::from("/Users/yeowool/Documents/Eastself"))
        );
        assert_eq!(
            CopilotConnector::path_from_uri("vscode-remote://ssh-remote%2Bhost/not%20encoded"),
            Some(PathBuf::from("/not encoded"))
        );
    }

    #[test]
    fn native_workspace_sessions_fail_closed_for_ambiguous_agents() {
        let tmp = TempDir::new().unwrap();
        let sessions = tmp.path().join("workspaceStorage/ws/chatSessions");
        fs::create_dir_all(&sessions).unwrap();
        let session = serde_json::json!({
            "version": 3,
            "sessionId": "ambiguous",
            "requests": [{
                "requestId": "r1",
                "message": "hello",
                "response": ["reply"]
            }]
        });
        write_json(&sessions, "ambiguous.json", &session.to_string());

        let connector = CopilotConnector::new();
        let convs = connector
            .scan(&ScanContext::local_default(
                tmp.path().join("workspaceStorage"),
                None,
            ))
            .unwrap();
        assert!(convs.is_empty());
    }

    #[test]
    fn native_workspace_sessions_ignore_malformed_tail_but_not_middle_corruption() {
        let tmp = TempDir::new().unwrap();
        let sessions = tmp.path().join("workspaceStorage/ws/chatSessions");
        fs::create_dir_all(&sessions).unwrap();
        let snapshot = serde_json::json!({
            "sessionId": "tail",
            "requests": [{
                "requestId": "r1",
                "message": "hello",
                "agent": {"id": "github.copilot-chat"},
                "response": ["reply"]
            }]
        });
        let valid = serde_json::json!({"kind": 0, "v": snapshot}).to_string();
        write_json(
            &sessions,
            "tail.jsonl",
            &format!("{valid}\n{{\"kind\":1,\"k\":[\"customTitle\"],\"v\":\n"),
        );
        let connector = CopilotConnector::new();
        let root = tmp.path().join("workspaceStorage");
        assert_eq!(
            connector
                .scan(&ScanContext::local_default(root.clone(), None))
                .unwrap()
                .len(),
            1
        );
        write_json(
            &sessions,
            "middle.jsonl",
            &format!(
                "{valid}\nnot-json\n{}\n",
                serde_json::json!({"kind": 1, "k": ["customTitle"], "v": "ignored"})
            ),
        );
        let convs = connector
            .scan(&ScanContext::local_default(root, None))
            .unwrap();
        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].external_id.as_deref(), Some("tail"));
    }

    #[cfg(feature = "copilot-sqlite")]
    #[test]
    fn scan_native_interactive_sessions_sqlite_and_deduplicates_file_copy() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("Code/User/workspaceStorage/ws");
        let sessions = workspace.join("chatSessions");
        fs::create_dir_all(&sessions).unwrap();
        write_json(
            &workspace,
            "workspace.json",
            r#"{"folder":"file:///db/work"}"#,
        );
        let session = serde_json::json!({
            "sessionId": "sqlite-session",
            "responderUsername": "GitHub Copilot",
            "requests": [{"requestId": "r1", "message": "from sqlite", "response": ["reply"]}]
        });
        let db_path = workspace.join("state.vscdb");
        let conn =
            crate::connectors::sqlite_sync::Connection::open(db_path.to_string_lossy().as_ref())
                .unwrap();
        conn.execute("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT)")
            .unwrap();
        conn.execute_compat(
            "INSERT INTO ItemTable (key, value) VALUES (?, ?)",
            params![
                "interactive.sessions",
                serde_json::json!([session]).to_string()
            ],
        )
        .unwrap();
        drop(conn);
        write_json(
            &sessions,
            "sqlite-session.json",
            &serde_json::json!({
                "sessionId": "sqlite-session",
                "responderUsername": "GitHub Copilot",
                "requests": [{"requestId": "r1", "message": "from file", "response": ["reply"]}]
            })
            .to_string(),
        );

        let connector = CopilotConnector::new();
        let ctx = ScanContext::local_default(tmp.path().join("Code/User"), None);
        let convs = connector.scan(&ctx).unwrap();
        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].external_id.as_deref(), Some("sqlite-session"));
        assert!(convs[0].source_path.ends_with("sqlite-session.json"));
        crate::connectors::assert_discovery_covers_scan_sources(&connector, &ctx);
    }

    #[test]
    fn scan_with_explicit_config_root_finds_vscode_storage() {
        let tmp = TempDir::new().unwrap();
        let config_root = tmp.path().join(".config");
        let copilot_dir = config_root.join("Code/User/globalStorage/github.copilot-chat");
        fs::create_dir_all(&copilot_dir).unwrap();

        let json = r#"[
            {
                "id": "conv-config",
                "workspaceFolder": "/work/config",
                "turns": [
                    {
                        "request": {"message": "Hello", "timestamp": 1700000000000},
                        "response": {"message": "Hi", "timestamp": 1700000001000}
                    }
                ]
            }
        ]"#;

        write_json(&copilot_dir, "conversations.json", json);

        let connector = CopilotConnector::new();
        let ctx = ScanContext::with_roots(
            tmp.path().join("cass"),
            vec![ScanRoot::local(config_root)],
            None,
        );
        let convs = connector.scan(&ctx).unwrap();

        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].external_id.as_deref(), Some("conv-config"));
    }

    #[test]
    fn scan_parses_turns_format() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("copilot-chat");
        fs::create_dir_all(&root).unwrap();

        let json = r#"[
            {
                "id": "conv-001",
                "workspaceFolder": "/home/user/project",
                "turns": [
                    {
                        "request": {
                            "message": "How do I sort a vector in Rust?",
                            "timestamp": 1700000000000
                        },
                        "response": {
                            "message": "You can use `.sort()` or `.sort_by()` on a Vec.",
                            "timestamp": 1700000001000
                        }
                    },
                    {
                        "request": {
                            "message": "Can you show me an example?",
                            "timestamp": 1700000002000
                        },
                        "response": {
                            "message": "Sure! `let mut v = vec![3,1,2]; v.sort();`",
                            "timestamp": 1700000003000
                        }
                    }
                ]
            }
        ]"#;

        write_json(&root, "conversations.json", json);

        let connector = CopilotConnector::new();
        let ctx = ScanContext::local_default(root, None);
        let convs = connector.scan(&ctx).unwrap();

        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].agent_slug, "copilot");
        assert_eq!(convs[0].external_id.as_deref(), Some("conv-001"));
        assert_eq!(
            convs[0].workspace,
            Some(PathBuf::from("/home/user/project"))
        );
        assert_eq!(convs[0].messages.len(), 4);
        assert_eq!(convs[0].messages[0].role, "user");
        assert!(convs[0].messages[0].content.contains("sort a vector"));
        assert_eq!(convs[0].messages[1].role, "assistant");
        assert!(convs[0].messages[1].content.contains(".sort()"));
        assert_eq!(convs[0].messages[2].role, "user");
        assert_eq!(convs[0].messages[3].role, "assistant");
        assert!(convs[0].started_at.is_some());
        assert!(convs[0].ended_at.is_some());
        assert!(convs[0].title.is_some());
        assert!(convs[0].title.as_ref().unwrap().contains("sort a vector"));
        crate::connectors::assert_discovery_covers_scan_sources(&connector, &ctx);
    }

    #[test]
    fn scan_parses_messages_format() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("copilot-chat");
        fs::create_dir_all(&root).unwrap();

        let json = r#"{
            "id": "conv-002",
            "title": "Explain lifetimes",
            "messages": [
                {
                    "role": "user",
                    "content": "Explain Rust lifetimes",
                    "timestamp": 1700000010000
                },
                {
                    "role": "assistant",
                    "content": "Lifetimes are a way of expressing the scope for which a reference is valid.",
                    "timestamp": 1700000011000
                }
            ]
        }"#;

        write_json(&root, "session-002.json", json);

        let connector = CopilotConnector::new();
        let ctx = ScanContext::local_default(root, None);
        let convs = connector.scan(&ctx).unwrap();

        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].title.as_deref(), Some("Explain lifetimes"));
        assert_eq!(convs[0].messages.len(), 2);
        assert_eq!(convs[0].messages[0].role, "user");
        assert_eq!(convs[0].messages[1].role, "assistant");
        assert_eq!(convs[0].messages[1].author.as_deref(), Some("copilot"));
    }

    #[test]
    fn scan_parses_conversations_wrapper() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("copilot-chat");
        fs::create_dir_all(&root).unwrap();

        let json = r#"{
            "conversations": [
                {
                    "id": "wrapped-001",
                    "messages": [
                        {"role": "user", "content": "Hello Copilot"},
                        {"role": "assistant", "content": "Hello! How can I help?"}
                    ]
                },
                {
                    "id": "wrapped-002",
                    "messages": [
                        {"role": "user", "content": "Write a function"},
                        {"role": "assistant", "content": "fn example() {}"}
                    ]
                }
            ]
        }"#;

        write_json(&root, "all-conversations.json", json);

        let connector = CopilotConnector::new();
        let ctx = ScanContext::local_default(root, None);
        let convs = connector.scan(&ctx).unwrap();

        assert_eq!(convs.len(), 2);
        assert_eq!(convs[0].external_id.as_deref(), Some("wrapped-001"));
        assert_eq!(convs[1].external_id.as_deref(), Some("wrapped-002"));
    }

    #[test]
    fn scan_skips_empty_conversations() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("copilot-chat");
        fs::create_dir_all(&root).unwrap();

        let json = r#"[
            {
                "id": "empty-conv",
                "turns": []
            },
            {
                "id": "nonempty-conv",
                "turns": [
                    {
                        "request": {"message": "Hello"},
                        "response": {"message": "Hi there"}
                    }
                ]
            }
        ]"#;

        write_json(&root, "mixed.json", json);

        let connector = CopilotConnector::new();
        let ctx = ScanContext::local_default(root, None);
        let convs = connector.scan(&ctx).unwrap();

        // Only the non-empty conversation should be returned.
        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].external_id.as_deref(), Some("nonempty-conv"));
    }

    #[test]
    fn find_conversation_files_returns_sorted_order() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("copilot-chat");
        fs::create_dir_all(root.join("nested")).unwrap();

        write_json(&root, "zeta.json", "[]");
        write_json(&root, "alpha.json", "[]");
        write_json(&root.join("nested"), "middle.json", "[]");

        let files = CopilotConnector::find_conversation_files(&root);
        let mut sorted = files.clone();
        sorted.sort();
        assert_eq!(files, sorted);
    }

    #[test]
    fn scan_sets_ended_at_when_only_created_at_present() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("copilot-chat");
        fs::create_dir_all(&root).unwrap();

        // Messages have no per-message timestamps; only createdAt exists.
        let json = r#"{
            "id": "conv-created-only",
            "createdAt": 1700000022000,
            "messages": [
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": "world"}
            ]
        }"#;
        write_json(&root, "created-only.json", json);

        let connector = CopilotConnector::new();
        let ctx = ScanContext::local_default(root, None);
        let convs = connector.scan(&ctx).unwrap();

        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].started_at, Some(1_700_000_022_000));
        assert_eq!(convs[0].ended_at, Some(1_700_000_022_000));
    }

    #[test]
    fn scan_respects_since_ts_filtering() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("copilot-chat");
        fs::create_dir_all(&root).unwrap();

        write_json(
            &root,
            "old.json",
            r#"[{"id":"old","turns":[{"request":{"message":"old msg"},"response":{"message":"old reply"}}]}]"#,
        );

        // Use a far-future timestamp to filter out everything.
        let connector = CopilotConnector::new();
        let far_future = chrono::Utc::now().timestamp_millis() + 86_400_000;
        let ctx = ScanContext::local_default(root, Some(far_future));
        let convs = connector.scan(&ctx).unwrap();
        assert!(convs.is_empty());
    }

    #[test]
    fn scan_with_scan_roots() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("fakehome");
        let copilot_dir = home.join(".config/Code/User/globalStorage/github.copilot-chat");
        fs::create_dir_all(&copilot_dir).unwrap();

        let json = r#"[{
            "id": "remote-001",
            "turns": [
                {"request": {"message": "test"}, "response": {"message": "reply"}}
            ]
        }]"#;

        write_json(&copilot_dir, "conversations.json", json);

        let connector = CopilotConnector::new();
        let scan_root = crate::connectors::ScanRoot::local(home);
        let ctx = ScanContext::with_roots(tmp.path().to_path_buf(), vec![scan_root], None);
        let convs = connector.scan(&ctx).unwrap();

        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].external_id.as_deref(), Some("remote-001"));
    }

    #[test]
    fn scan_with_copilot_root_scan_root() {
        let tmp = TempDir::new().unwrap();
        let copilot_root = tmp.path().join(".copilot");
        let events_dir = copilot_root.join("session-state").join("sess-001");

        let events = r#"{"type":"user.message","session_id":"sess-001","message":"hello"}"#;
        write_json(&events_dir, "events.jsonl", events);

        let connector = CopilotConnector::new();
        let ctx =
            ScanContext::with_roots(PathBuf::new(), vec![ScanRoot::local(copilot_root)], None);
        let convs = connector.scan(&ctx).unwrap();

        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].external_id.as_deref(), Some("sess-001"));
        assert_eq!(convs[0].metadata["source"], "copilot-cli");
    }

    #[test]
    fn scan_with_global_storage_scan_root() {
        let tmp = TempDir::new().unwrap();
        let global_storage = tmp.path().join("globalStorage");
        let copilot_dir = global_storage.join("github.copilot-chat");
        fs::create_dir_all(&copilot_dir).unwrap();

        let json = r#"[{
            "id": "global-001",
            "turns": [
                {"request": {"message": "hello"}, "response": {"message": "hi"}}
            ]
        }]"#;
        write_json(&copilot_dir, "conversations.json", json);

        let connector = CopilotConnector::new();
        let ctx =
            ScanContext::with_roots(PathBuf::new(), vec![ScanRoot::local(global_storage)], None);
        let convs = connector.scan(&ctx).unwrap();

        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].external_id.as_deref(), Some("global-001"));
    }

    #[test]
    fn scan_with_windows_style_scan_root() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("fakehome");
        let copilot_dir = home.join("AppData/Roaming/Code/User/globalStorage/github.copilot-chat");
        fs::create_dir_all(&copilot_dir).unwrap();

        let json = r#"[{
            "id": "win-001",
            "messages": [
                {"role": "user", "content": "from windows root"},
                {"role": "assistant", "content": "ack"}
            ]
        }]"#;

        write_json(&copilot_dir, "conversations.json", json);

        let connector = CopilotConnector::new();
        let scan_root = crate::connectors::ScanRoot::local(home);
        let ctx = ScanContext::with_roots(tmp.path().to_path_buf(), vec![scan_root], None);
        let convs = connector.scan(&ctx).unwrap();

        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].external_id.as_deref(), Some("win-001"));
    }

    #[test]
    fn scan_skips_invalid_json() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("copilot-chat");
        fs::create_dir_all(&root).unwrap();

        write_json(&root, "invalid.json", "not valid json {{{");

        let connector = CopilotConnector::new();
        let ctx = ScanContext::local_default(root, None);
        let convs = connector.scan(&ctx).unwrap();
        assert!(convs.is_empty());
    }

    #[test]
    fn looks_like_copilot_storage_works() {
        assert!(CopilotConnector::looks_like_copilot_storage(Path::new(
            "/home/user/.config/Code/User/globalStorage/github.copilot-chat"
        )));
        assert!(CopilotConnector::looks_like_copilot_storage(Path::new(
            "/tmp/copilot-chat/data"
        )));
        assert!(CopilotConnector::looks_like_copilot_storage(Path::new(
            "/home/user/.config/gh-copilot"
        )));
        assert!(!CopilotConnector::looks_like_copilot_storage(Path::new(
            "/home/user/.config/Code"
        )));
        assert!(!CopilotConnector::looks_like_copilot_storage(Path::new(
            "/home/user/projects/copilot-research"
        )));
    }

    #[test]
    fn default_impl() {
        let connector = CopilotConnector;
        let _ = connector;
    }

    #[test]
    fn all_candidate_paths_are_deduplicated() {
        let paths = CopilotConnector::all_candidate_paths();
        let mut deduped = paths.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(paths, deduped);
    }

    // --- Copilot CLI event log tests ---

    #[test]
    fn scan_parses_cli_events_jsonl() {
        let tmp = TempDir::new().unwrap();
        let session_dir = tmp.path().join(".copilot/session-state/abc-123");
        fs::create_dir_all(&session_dir).unwrap();

        let events = r#"{"type":"sessionStart","session_id":"abc-123","timestamp":1700000000000,"cwd":"/home/user/myproject"}
{"type":"user.message","role":"user","content":"How do I read a file in Rust?","timestamp":1700000001000}
{"type":"assistant.message","role":"assistant","content":"You can use std::fs::read_to_string() to read a file into a String.","timestamp":1700000002000}
{"type":"user.message","role":"user","content":"Show me an example","timestamp":1700000003000}
{"type":"assistant.message","role":"assistant","content":"let contents = std::fs::read_to_string(\"file.txt\")?;","timestamp":1700000004000}
"#;

        write_json(&session_dir, "events.jsonl", events);

        let connector = CopilotConnector::new();
        let root = tmp.path().join(".copilot/session-state");
        let ctx = ScanContext::local_default(root, None);
        let convs = connector.scan(&ctx).unwrap();

        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].agent_slug, "copilot");
        assert_eq!(convs[0].external_id.as_deref(), Some("abc-123"));
        assert_eq!(
            convs[0].workspace,
            Some(PathBuf::from("/home/user/myproject"))
        );
        assert_eq!(convs[0].messages.len(), 4);
        assert_eq!(convs[0].messages[0].role, "user");
        assert!(convs[0].messages[0].content.contains("read a file"));
        assert_eq!(convs[0].messages[1].role, "assistant");
        assert!(convs[0].messages[1].content.contains("read_to_string"));
        assert_eq!(convs[0].messages[2].role, "user");
        assert_eq!(convs[0].messages[3].role, "assistant");
        assert_eq!(convs[0].started_at, Some(1_700_000_000_000));
        assert_eq!(convs[0].ended_at, Some(1_700_000_004_000));
        assert!(convs[0].title.as_ref().unwrap().contains("read a file"));
    }

    #[test]
    fn scan_parses_cli_events_with_hook_event_types() {
        let tmp = TempDir::new().unwrap();
        let session_dir = tmp.path().join(".copilot/session-state/def-456");
        fs::create_dir_all(&session_dir).unwrap();

        // Using hook-style event names.
        let events = r#"{"type":"userPromptSubmitted","content":"Explain ownership","timestamp":1700000010000}
{"type":"assistantResponse","content":"Ownership is Rust's memory management model.","timestamp":1700000011000}
"#;

        write_json(&session_dir, "events.jsonl", events);

        let connector = CopilotConnector::new();
        let root = tmp.path().join(".copilot/session-state");
        let ctx = ScanContext::local_default(root, None);
        let convs = connector.scan(&ctx).unwrap();

        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].messages.len(), 2);
        assert_eq!(convs[0].messages[0].role, "user");
        assert!(convs[0].messages[0].content.contains("ownership"));
        assert_eq!(convs[0].messages[1].role, "assistant");
        assert!(convs[0].messages[1].content.contains("memory management"));
    }

    #[test]
    fn scan_parses_cli_legacy_session_json() {
        let tmp = TempDir::new().unwrap();
        let legacy_dir = tmp.path().join(".copilot/history-session-state");
        fs::create_dir_all(&legacy_dir).unwrap();

        let session_json = r#"{
            "session_id": "legacy-001",
            "cwd": "/home/user/legacy-project",
            "events": [
                {"type": "user.message", "content": "What is a trait?", "timestamp": 1700000020000},
                {"type": "assistant.message", "content": "A trait defines shared behavior.", "timestamp": 1700000021000}
            ]
        }"#;

        write_json(&legacy_dir, "legacy-001.json", session_json);

        let connector = CopilotConnector::new();
        let root = tmp.path().join(".copilot/history-session-state");
        let ctx = ScanContext::local_default(root, None);
        let convs = connector.scan(&ctx).unwrap();

        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].external_id.as_deref(), Some("legacy-001"));
        assert_eq!(
            convs[0].workspace,
            Some(PathBuf::from("/home/user/legacy-project"))
        );
        assert_eq!(convs[0].messages.len(), 2);
        assert_eq!(convs[0].messages[0].role, "user");
        assert!(convs[0].messages[0].content.contains("trait"));
        assert_eq!(convs[0].messages[1].role, "assistant");
    }

    #[test]
    fn scan_parses_cli_events_with_prompt_field() {
        let tmp = TempDir::new().unwrap();
        let session_dir = tmp.path().join(".copilot/session-state/ghi-789");
        fs::create_dir_all(&session_dir).unwrap();

        // Some events use "prompt" instead of "content".
        let events = r#"{"type":"user.message","prompt":"Deploy to production","timestamp":1700000030000}
{"type":"assistant.message","output":"Running deployment script...","timestamp":1700000031000}
"#;

        write_json(&session_dir, "events.jsonl", events);

        let connector = CopilotConnector::new();
        let root = tmp.path().join(".copilot/session-state");
        let ctx = ScanContext::local_default(root, None);
        let convs = connector.scan(&ctx).unwrap();

        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].messages.len(), 2);
        assert_eq!(convs[0].messages[0].role, "user");
        assert!(convs[0].messages[0].content.contains("Deploy"));
        assert_eq!(convs[0].messages[1].role, "assistant");
        assert!(convs[0].messages[1].content.contains("deployment"));
    }

    #[test]
    fn scan_cli_events_skips_non_message_events() {
        let tmp = TempDir::new().unwrap();
        let session_dir = tmp.path().join(".copilot/session-state/skip-test");
        fs::create_dir_all(&session_dir).unwrap();

        let events = r#"{"type":"sessionStart","timestamp":1700000040000}
{"type":"preToolUse","toolName":"shell","timestamp":1700000041000}
{"type":"user.message","content":"Hello","timestamp":1700000042000}
{"type":"postToolUse","toolName":"shell","timestamp":1700000043000}
{"type":"assistant.message","content":"Hi there!","timestamp":1700000044000}
{"type":"errorOccurred","error":"some error","timestamp":1700000045000}
"#;

        write_json(&session_dir, "events.jsonl", events);

        let connector = CopilotConnector::new();
        let root = tmp.path().join(".copilot/session-state");
        let ctx = ScanContext::local_default(root, None);
        let convs = connector.scan(&ctx).unwrap();

        assert_eq!(convs.len(), 1);
        // Only user.message and assistant.message events should produce messages.
        assert_eq!(convs[0].messages.len(), 2);
        assert_eq!(convs[0].messages[0].role, "user");
        assert_eq!(convs[0].messages[0].content, "Hello");
        assert_eq!(convs[0].messages[1].role, "assistant");
        assert_eq!(convs[0].messages[1].content, "Hi there!");
    }

    #[test]
    fn scan_cli_empty_events_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let session_dir = tmp.path().join(".copilot/session-state/empty");
        fs::create_dir_all(&session_dir).unwrap();

        // Only non-message events.
        let events = r#"{"type":"sessionStart","timestamp":1700000050000}
{"type":"sessionEnd","timestamp":1700000051000}
"#;

        write_json(&session_dir, "events.jsonl", events);

        let connector = CopilotConnector::new();
        let root = tmp.path().join(".copilot/session-state");
        let ctx = ScanContext::local_default(root, None);
        let convs = connector.scan(&ctx).unwrap();
        assert!(convs.is_empty());
    }

    #[test]
    fn scan_cli_events_with_scan_roots() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("fakehome");
        let session_dir = home.join(".copilot/session-state/remote-sess");
        fs::create_dir_all(&session_dir).unwrap();

        let events = r#"{"type":"user.message","content":"from remote","timestamp":1700000060000}
{"type":"assistant.message","content":"acknowledged","timestamp":1700000061000}
"#;

        write_json(&session_dir, "events.jsonl", events);

        let connector = CopilotConnector::new();
        let scan_root = crate::connectors::ScanRoot::local(home);
        let ctx = ScanContext::with_roots(tmp.path().to_path_buf(), vec![scan_root], None);
        let convs = connector.scan(&ctx).unwrap();

        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].messages.len(), 2);
    }

    #[test]
    fn is_cli_event_log_detection() {
        assert!(CopilotConnector::is_cli_event_log(Path::new(
            "/home/user/.copilot/session-state/abc/events.jsonl"
        )));
        assert!(CopilotConnector::is_cli_event_log(Path::new(
            "/tmp/test.jsonl"
        )));
        assert!(CopilotConnector::is_cli_event_log(Path::new(
            "/home/user/.copilot/session-state/abc/checkpoint.json"
        )));
        assert!(CopilotConnector::is_cli_event_log(Path::new(
            "/home/user/.copilot/history-session-state/old.json"
        )));
        assert!(!CopilotConnector::is_cli_event_log(Path::new(
            "/home/user/.config/Code/User/globalStorage/github.copilot-chat/conversations.json"
        )));
    }

    #[test]
    fn looks_like_copilot_storage_with_cli_paths() {
        assert!(CopilotConnector::looks_like_copilot_storage(Path::new(
            "/home/user/.copilot/session-state"
        )));
        assert!(CopilotConnector::looks_like_copilot_storage(Path::new(
            "/home/user/.copilot/history-session-state"
        )));
        assert!(CopilotConnector::looks_like_copilot_storage(Path::new(
            "/home/user/.copilot/session-state/abc-123"
        )));
    }

    #[test]
    fn scan_multiple_cli_sessions() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join(".copilot/session-state");

        let session_a = root.join("session-a");
        let session_b = root.join("session-b");
        fs::create_dir_all(&session_a).unwrap();
        fs::create_dir_all(&session_b).unwrap();

        write_json(
            &session_a,
            "events.jsonl",
            r#"{"type":"user.message","content":"Question A","timestamp":1700000070000}
{"type":"assistant.message","content":"Answer A","timestamp":1700000071000}
"#,
        );

        write_json(
            &session_b,
            "events.jsonl",
            r#"{"type":"user.message","content":"Question B","timestamp":1700000080000}
{"type":"assistant.message","content":"Answer B","timestamp":1700000081000}
"#,
        );

        let connector = CopilotConnector::new();
        let ctx = ScanContext::local_default(root, None);
        let convs = connector.scan(&ctx).unwrap();

        assert_eq!(convs.len(), 2);
        // Sessions should have different session IDs (from parent directory names).
        let ids: Vec<_> = convs
            .iter()
            .filter_map(|c| c.external_id.as_deref())
            .collect();
        assert!(ids.contains(&"session-a"));
        assert!(ids.contains(&"session-b"));
    }

    #[test]
    fn scan_cli_events_with_malformed_lines() {
        let tmp = TempDir::new().unwrap();
        let session_dir = tmp.path().join(".copilot/session-state/malformed");
        fs::create_dir_all(&session_dir).unwrap();

        // Mix of valid and invalid JSONL lines.
        let events = r#"not valid json
{"type":"user.message","content":"valid msg","timestamp":1700000090000}
{incomplete json...
{"type":"assistant.message","content":"also valid","timestamp":1700000091000}

"#;

        write_json(&session_dir, "events.jsonl", events);

        let connector = CopilotConnector::new();
        let root = tmp.path().join(".copilot/session-state");
        let ctx = ScanContext::local_default(root, None);
        let convs = connector.scan(&ctx).unwrap();

        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].messages.len(), 2);
        assert_eq!(convs[0].messages[0].content, "valid msg");
        assert_eq!(convs[0].messages[1].content, "also valid");
    }

    #[test]
    fn scan_cli_metadata_source_is_copilot_cli() {
        let tmp = TempDir::new().unwrap();
        let session_dir = tmp.path().join(".copilot/session-state/meta-test");
        fs::create_dir_all(&session_dir).unwrap();

        let events = r#"{"type":"user.message","content":"test","timestamp":1700000100000}
{"type":"assistant.message","content":"reply","timestamp":1700000101000}
"#;

        write_json(&session_dir, "events.jsonl", events);

        let connector = CopilotConnector::new();
        let root = tmp.path().join(".copilot/session-state");
        let ctx = ScanContext::local_default(root, None);
        let convs = connector.scan(&ctx).unwrap();

        assert_eq!(convs.len(), 1);
        assert_eq!(convs[0].metadata["source"], "copilot-cli");
    }
}
