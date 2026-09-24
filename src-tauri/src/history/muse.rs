//! Muse session discovery + transcript read for the history sidebar.
//!
//! Source of truth is the Muse session index
//! (`<muse-home>/session-index.db`, table `sessions`): one row per
//! session with title, workspace root, prompt count, and microsecond
//! timestamps, plus `session_log_path` pointing at the event-sourced
//! `session.jsonl` journal.
//!
//! MVP coverage: sidebar listing (index) + user turns (journal
//! `runtime.user_intent.accepted` records). Assistant text, tool calls,
//! and usage live behind output refs in the journal and are an explicit
//! follow-up — opened sessions show user prompts until then.

use super::{same_or_child, Message, ParsedSession, ScanSummary};
use std::path::{Path, PathBuf};

/// Muse home: `~/.local/share/muse` (no MUSE_HOME override observed).
pub fn muse_home() -> PathBuf {
    crate::engine::engine_home(None, ".local/share/muse")
}

fn index_db_path() -> PathBuf {
    muse_home().join("session-index.db")
}

struct IndexRow {
    session_id: String,
    log_path: PathBuf,
    workspace_root: String,
    title: String,
    first_prompt: String,
    prompt_count: i64,
    created_ms: Option<i64>,
    updated_ms: Option<i64>,
}

fn open_index() -> Result<rusqlite::Connection, String> {
    let path = index_db_path();
    rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|e| format!("muse index {}: {e}", path.display()))
}

fn query_index(conn: &rusqlite::Connection) -> Result<Vec<IndexRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT session_id, session_log_path, workspace_root, title, \
             first_user_prompt, prompt_count, created_at_us, updated_at_us \
             FROM sessions",
        )
        .map_err(|e| format!("muse index query: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            let us_to_ms = |v: rusqlite::Result<Option<i64>>| {
                v.unwrap_or(None).map(|us| us / 1000)
            };
            Ok(IndexRow {
                session_id: row.get::<_, String>(0)?,
                log_path: PathBuf::from(row.get::<_, String>(1)?),
                workspace_root: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                title: row.get::<_, String>(3)?,
                first_prompt: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                prompt_count: row.get::<_, Option<i64>>(5)?.unwrap_or(0),
                created_ms: us_to_ms(row.get(6)),
                updated_ms: us_to_ms(row.get(7)),
            })
        })
        .map_err(|e| format!("muse index rows: {e}"))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| format!("muse index row: {e}"))?);
    }
    Ok(out)
}

/// Sessions whose workspace root is the given workspace (or under it).
pub(super) fn discover_muse(workspace: &Path) -> Vec<super::SessionFile> {
    let Ok(conn) = open_index() else {
        return Vec::new();
    };
    let Ok(rows) = query_index(&conn) else {
        return Vec::new();
    };
    let ws = workspace.to_string_lossy();
    rows.into_iter()
        .filter(|r| !r.workspace_root.is_empty() && (r.workspace_root == ws || same_or_child(Path::new(&r.workspace_root), workspace)))
        .map(|r| super::SessionFile {
            engine: "muse",
            session_id: r.session_id,
            workspace_path: r.workspace_root,
            file_path: r.log_path,
        })
        .collect()
}

/// Session id from a log path (`.../sessions/YYYY/MM/DD/<id>/session.jsonl`).
fn session_id_from_log(path: &Path) -> Option<String> {
    path.parent()?
        .file_name()?
        .to_str()
        .map(str::to_string)
}

/// Sidebar summary, index-first (title/preview/timestamps/count without
/// touching the journal); journal walk fallback when the index is absent.
pub(super) fn scan_muse_summary(path: &Path) -> ScanSummary {
    if let Some(id) = session_id_from_log(path) {
        if let Ok(conn) = open_index() {
            if let Ok(rows) = query_index(&conn) {
                if let Some(row) = rows.into_iter().find(|r| r.session_id == id) {
                    let title = if row.title.trim().is_empty() {
                        super::truncate_chars(&row.first_prompt, 80)
                    } else {
                        super::truncate_chars(&row.title, 80)
                    };
                    return ScanSummary {
                        title,
                        preview: super::truncate_chars(&row.first_prompt, 120),
                        first_ts: row.created_ms,
                        last_ts: row.updated_ms,
                        message_count: row.prompt_count,
                    };
                }
            }
        }
    }
    // Fallback: user turns from the journal.
    let parsed = parse_muse_session(path).unwrap_or(ParsedSession { messages: vec![] });
    let mut title = String::new();
    let mut first_ts = None;
    let mut last_ts = None;
    let mut count = 0i64;
    for m in &parsed.messages {
        if title.is_empty() && !m.text.trim().is_empty() {
            title = super::truncate_chars(&m.text, 80);
        }
        if first_ts.is_none() {
            first_ts = m.ts.as_deref().and_then(super::parse_ts_ms_str);
        }
        last_ts = m.ts.as_deref().and_then(super::parse_ts_ms_str);
        count += 1;
    }
    ScanSummary {
        title,
        preview: String::new(),
        first_ts,
        last_ts,
        message_count: count,
    }
}

/// User turns from the journal: `runtime.user_intent.accepted` records
/// carry `payload.model_messages[].content[]` text parts.
pub(super) fn parse_muse_session(path: &Path) -> Result<ParsedSession, String> {
    let data =
        std::fs::read_to_string(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut messages = Vec::new();
    let mut seq = 0i64;
    for line in data.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("payload_type").and_then(|v| v.as_str()) != Some("runtime.user_intent.accepted") {
            continue;
        }
        let Some(payload) = value.get("payload") else {
            continue;
        };
        let mut texts = Vec::new();
        if let Some(msgs) = payload.get("model_messages").and_then(|v| v.as_array()) {
            for m in msgs {
                if let Some(parts) = m.get("content").and_then(|v| v.as_array()) {
                    for part in parts {
                        let is_text = part.get("kind").and_then(|v| v.as_str()) == Some("text");
                        if is_text {
                            if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                                if !t.trim().is_empty() {
                                    texts.push(t.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
        if texts.is_empty() {
            continue;
        }
        let ts = value
            .get("recorded_at")
            .and_then(|v| v.as_i64())
            .map(|us| (us / 1000).to_string());
        seq += 1;
        messages.push(Message {
            seq,
            role: "user".to_string(),
            text: texts.join("\n"),
            ts,
            path: None,
            args: None,
            result: None,
            todos: None,
            usage: None,
            model: None,
            effort: None,
            duration_ms: None,
            images: Vec::new(),
        });
    }
    Ok(ParsedSession { messages })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_journal() -> &'static str {
        r#"{"schema_version":1,"stream":{"kind":"session","id":"SID"},"sequence":22,"recorded_at":1790205652193311,"record_type":"event","payload_type":"runtime.user_intent.accepted","payload":{"model_messages":[{"content":[{"kind":"text","text":"do the thing"}]}]}}
{"schema_version":1,"stream":{"kind":"session","id":"SID"},"sequence":23,"recorded_at":1790205701305287,"record_type":"event","payload_type":"runtime.session","payload":{"kind":"task"}}
{"schema_version":1,"stream":{"kind":"session","id":"SID"},"sequence":24,"recorded_at":1790205800000000,"record_type":"event","payload_type":"runtime.user_intent.accepted","payload":{"model_messages":[{"content":[{"kind":"text","text":"and another"}]}]}}
not json
"#
    }

    #[test]
    fn journal_yields_user_turns_with_ms_ts() {
        let dir = std::env::temp_dir().join(format!("ccgui-muse-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.jsonl");
        std::fs::write(&path, fixture_journal()).unwrap();
        let parsed = parse_muse_session(&path).unwrap();
        assert_eq!(parsed.messages.len(), 2);
        assert_eq!(parsed.messages[0].role, "user");
        assert_eq!(parsed.messages[0].text, "do the thing");
        assert_eq!(parsed.messages[0].ts.as_deref(), Some("1790205652193"));
        assert_eq!(parsed.messages[1].text, "and another");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn session_id_comes_from_parent_dir() {
        let p = Path::new("/x/sessions/2026/09/23/ABC123/session.jsonl");
        assert_eq!(session_id_from_log(p).as_deref(), Some("ABC123"));
    }
}
