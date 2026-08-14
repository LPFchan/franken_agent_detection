//! Replays VS Code's append-only chat session mutation logs.
//!
//! VS Code stores a complete snapshot as a `kind: 0` record, then persists
//! object mutations as `kind: 1` (set), `kind: 2` (array push/splice), and
//! `kind: 3` (delete) records.  This module deliberately operates on JSON
//! values rather than VS Code's private Rust/TypeScript model so the connector
//! can read every persisted session version without inventing provider data.

use anyhow::{Result, anyhow, bail};
use serde_json::Value;

/// Reconstruct a JSON value from a VS Code chat-session operation log.
///
/// A malformed line is treated as an incomplete tail: the last known-good
/// state is returned.  A log without an initial snapshot, an invalid mutation,
/// or an unknown operation kind is rejected.  This mirrors VS Code's mutation
/// semantics while keeping a partially-written tail from producing fabricated
/// messages.
pub fn replay_operation_log(content: &str) -> Result<Value> {
    let mut state: Option<Value> = None;
    let mut line_count = 0usize;

    let lines: Vec<&str> = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    for (line_index, line) in lines.iter().enumerate() {
        let entry = match serde_json::from_str::<Value>(line) {
            Ok(value) => value,
            Err(error) => {
                if state.is_some() && line_index + 1 == lines.len() {
                    // VS Code appends records one at a time.  A malformed
                    // final record is therefore most safely treated as a
                    // truncated write, preserving only the valid prefix.
                    tracing::debug!(error = %error, "vscode chat: ignoring malformed log tail");
                    break;
                }
                return Err(anyhow!("invalid VS Code chat log entry: {error}"));
            }
        };

        let kind = entry
            .get("kind")
            .and_then(Value::as_i64)
            .ok_or_else(|| anyhow!("VS Code chat log entry has no numeric kind"))?;

        match kind {
            0 => {
                // VS Code's reader assigns snapshots directly. Normal logs
                // begin with one, but a later snapshot still replaces state.
                state = Some(
                    entry
                        .get("v")
                        .cloned()
                        .ok_or_else(|| anyhow!("VS Code chat initial entry has no value"))?,
                );
            }
            1 => {
                let state_ref = state
                    .as_mut()
                    .ok_or_else(|| anyhow!("VS Code chat log is missing an initial entry"))?;
                let path = object_path(&entry)?;
                let value = entry
                    .get("v")
                    .cloned()
                    .ok_or_else(|| anyhow!("VS Code chat set entry has no value"))?;
                set_at_path(state_ref, &path, value)?;
            }
            2 => {
                let state_ref = state
                    .as_mut()
                    .ok_or_else(|| anyhow!("VS Code chat log is missing an initial entry"))?;
                let path = object_path(&entry)?;
                let values = match entry.get("v") {
                    None => None,
                    Some(Value::Array(values)) => Some(values.clone()),
                    Some(_) => bail!("VS Code chat push entry value is not an array"),
                };
                let start_index = match entry.get("i") {
                    None => None,
                    Some(value) => Some(
                        usize::try_from(value.as_u64().ok_or_else(|| {
                            anyhow!("VS Code chat push index is not non-negative")
                        })?)
                        .map_err(|_| anyhow!("VS Code chat push index is too large"))?,
                    ),
                };
                push_at_path(state_ref, &path, values.as_deref(), start_index)?;
            }
            3 => {
                let state_ref = state
                    .as_mut()
                    .ok_or_else(|| anyhow!("VS Code chat log is missing an initial entry"))?;
                let path = object_path(&entry)?;
                delete_at_path(state_ref, &path)?;
            }
            _ => bail!("unknown VS Code chat log kind {kind}"),
        }
        line_count += 1;
    }

    if line_count == 0 {
        bail!("empty VS Code chat log");
    }
    state.ok_or_else(|| anyhow!("VS Code chat log has no initial state"))
}

fn object_path(entry: &Value) -> Result<Vec<&Value>> {
    let Some(path) = entry.get("k").and_then(Value::as_array) else {
        bail!("VS Code chat mutation has no path");
    };
    if path.iter().any(|part| !part.is_string() && !part.is_u64()) {
        bail!("VS Code chat mutation path contains an invalid segment");
    }
    Ok(path.iter().collect())
}

fn key_for_path(part: &Value) -> Result<PathKey<'_>> {
    if let Some(key) = part.as_str() {
        return Ok(PathKey::String(key));
    }
    let Some(index) = part.as_u64() else {
        bail!("VS Code chat mutation path segment is not a string or index");
    };
    let index =
        usize::try_from(index).map_err(|_| anyhow!("VS Code chat path index is too large"))?;
    Ok(PathKey::Index(index))
}

enum PathKey<'a> {
    String(&'a str),
    Index(usize),
}

fn child_mut<'a>(value: &'a mut Value, part: &Value) -> Result<&'a mut Value> {
    match key_for_path(part)? {
        PathKey::String(key) => value
            .as_object_mut()
            .and_then(|object| object.get_mut(key))
            .ok_or_else(|| anyhow!("VS Code chat mutation parent is not an object key")),
        PathKey::Index(index) => value
            .as_array_mut()
            .and_then(|array| array.get_mut(index))
            .ok_or_else(|| anyhow!("VS Code chat mutation parent is not an array index")),
    }
}

fn set_at_path(state: &mut Value, path: &[&Value], value: Value) -> Result<()> {
    // VS Code treats an empty set path as a root no-op; root replacement is
    // handled only by the initial entry.
    if path.is_empty() {
        return Ok(());
    }

    let mut parent = state;
    for part in &path[..path.len() - 1] {
        parent = child_mut(parent, part)?;
    }
    match key_for_path(path[path.len() - 1])? {
        PathKey::String(key) => {
            parent
                .as_object_mut()
                .ok_or_else(|| anyhow!("VS Code chat set parent is not an object"))?
                .insert(key.to_string(), value);
        }
        PathKey::Index(index) => {
            let array = parent
                .as_array_mut()
                .ok_or_else(|| anyhow!("VS Code chat set parent is not an array"))?;
            if index >= array.len() {
                array.resize(index + 1, Value::Null);
            }
            array[index] = value;
        }
    }
    Ok(())
}

fn push_at_path(
    state: &mut Value,
    path: &[&Value],
    values: Option<&[Value]>,
    start_index: Option<usize>,
) -> Result<()> {
    if path.is_empty() {
        bail!("VS Code chat push path is empty");
    }
    let mut parent = state;
    for part in &path[..path.len() - 1] {
        parent = child_mut(parent, part)?;
    }
    let target = match key_for_path(path[path.len() - 1])? {
        PathKey::String(key) => parent
            .as_object_mut()
            .ok_or_else(|| anyhow!("VS Code chat push parent is not an object"))?
            .entry(key.to_string())
            .or_insert_with(|| Value::Array(Vec::new())),
        PathKey::Index(index) => parent
            .as_array_mut()
            .and_then(|array| array.get_mut(index))
            .ok_or_else(|| anyhow!("VS Code chat push parent is not an array index"))?,
    };
    let array = target
        .as_array_mut()
        .ok_or_else(|| anyhow!("VS Code chat push target is not an array"))?;
    if let Some(index) = start_index {
        if index > array.len() {
            array.resize(index, Value::Null);
        }
        array.truncate(index);
    }
    if let Some(values) = values {
        array.extend(values.iter().cloned());
    }
    Ok(())
}

fn delete_at_path(state: &mut Value, path: &[&Value]) -> Result<()> {
    if path.is_empty() {
        return Ok(());
    }
    let mut parent = state;
    for part in &path[..path.len() - 1] {
        parent = child_mut(parent, part)?;
    }
    match key_for_path(path[path.len() - 1])? {
        PathKey::String(key) => {
            parent
                .as_object_mut()
                .ok_or_else(|| anyhow!("VS Code chat delete parent is not an object"))?
                .remove(key);
        }
        PathKey::Index(index) => {
            let array = parent
                .as_array_mut()
                .ok_or_else(|| anyhow!("VS Code chat delete parent is not an array"))?;
            if index >= array.len() {
                bail!("VS Code chat delete index is out of bounds");
            }
            // JavaScript's `delete array[index]` leaves a hole. JSON has no
            // holes, so null is the faithful serialized representation.
            array[index] = Value::Null;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn replays_snapshot_set_and_append() {
        let content = concat!(
            r#"{"kind":0,"v":{"requests":[{"message":"one"}]}}"#,
            "\n",
            r#"{"kind":1,"k":["customTitle"],"v":"title"}"#,
            "\n",
            r#"{"kind":2,"k":["requests"],"v":[{"message":"two"}]}"#,
        );
        assert_eq!(
            replay_operation_log(content).unwrap(),
            json!({"requests":[{"message":"one"},{"message":"two"}],"customTitle":"title"})
        );
    }

    #[test]
    fn ignores_truncated_tail_after_valid_prefix() {
        let content = concat!(
            r#"{"kind":0,"v":{"requests":[]}}"#,
            "\n",
            r#"{"kind":2,"k":["requests"],"v":[{"message":"ok"}]}"#,
            "\n",
            r#"{"kind":1,"k":["requests",0,"response"],"v"#,
        );
        assert_eq!(
            replay_operation_log(content).unwrap()["requests"][0]["message"],
            "ok"
        );
    }

    #[test]
    fn rejects_patch_without_snapshot() {
        assert!(replay_operation_log(r#"{"kind":1,"k":["x"],"v":1}"#).is_err());
    }

    #[test]
    fn later_snapshot_replaces_the_prior_state() {
        let content = concat!(
            r#"{"kind":0,"v":{"old":true}}"#,
            "\n",
            r#"{"kind":0,"v":{"requests":[]}}"#,
        );
        assert_eq!(
            replay_operation_log(content).unwrap(),
            json!({"requests": []})
        );
    }
}
