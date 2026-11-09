//! OpenCode plugin events → log records.
//!
//! Shapes are captured, not assumed — see `docs/opencode-plugin-events.md`.
//!
//! OpenCode has no shell hooks. thirdeye's plugin (`agents/opencode/
//! plugin.js`) forwards each raw event inside a small envelope that adds what
//! OpenCode does not put on tool events — the turn (the user's prompt message,
//! remembered per session), the directory — and this module is the only place
//! the raw events are read. The plugin forwards; it never interprets.

use std::path::{Path, PathBuf};

use serde_json::Value;
use thirdeye_core::{
    AgentId, EventKind, FileEvent, LineRange, LogRecord, Source, Timestamp, TurnKind, TurnMarker,
};

use crate::{AdapterError, Lifecycle, Observe, Payload};

pub fn records_from_payload(raw: &str, now: Timestamp) -> Result<Payload, AdapterError> {
    let v: Value = serde_json::from_str(raw)?;
    let session = v["session"].as_str().map(str::to_owned);
    let turn = v["turn"].as_str().map(str::to_owned);
    let cwd = v["cwd"].as_str().map(PathBuf::from);
    let event = &v["event"];
    let mut out = Payload {
        session: session.clone(),
        cwd: cwd.clone(),
        ..Payload::default()
    };

    let tool = event["tool"].as_str().unwrap_or_default();
    let call = event["id"].as_str().map(str::to_owned);
    match v["hook"].as_str().unwrap_or_default() {
        "prompt" => {
            if let Some(turn) = turn {
                let prompt = event["prompt"]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                let mut open = TurnMarker::new(now, turn, TurnKind::Open { prompt });
                open.session = session;
                out.records.push(LogRecord::Turn(open));
            }
        }
        "turn.end" => {
            if let Some(turn) = turn {
                let mut close = TurnMarker::new(now, turn, TurnKind::Close);
                close.session = session;
                out.records.push(LogRecord::Turn(close));
            }
        }
        "session.end" => out.lifecycle = Some(Lifecycle::End),
        // `shell` is bracketed, not interpreted, like every agent's shell.
        "tool.before" if tool == "shell" => out.observe = call.map(|call| Observe::Begin { call }),
        "tool.after" if tool == "shell" => {
            if let Some(command) = event["input"]["command"].as_str() {
                out.records = crate::shell::read_records(
                    command,
                    AgentId::OpenCode,
                    session.as_deref(),
                    turn.as_deref(),
                    call.as_deref(),
                    now,
                    |p| crate::shell::under(cwd.as_deref(), p),
                );
            }
            out.observe = call.map(|call| Observe::End { call, turn });
        }
        // A tool that did not complete changed nothing worth ranking.
        "tool.after" if event["status"].as_str() == Some("completed") => {
            let input = &event["input"];
            let kind = match tool {
                "read" => EventKind::Read { range: None },
                "write" => EventKind::Write,
                "edit" => EventKind::Edit {
                    range: edit_range(event),
                },
                "grep" | "glob" => EventKind::Search {
                    pattern: input["pattern"].as_str().unwrap_or_default().to_string(),
                },
                // Code Mode's `execute` wraps nested calls that report
                // themselves; anything else names no single file.
                _ => return Ok(out),
            };
            let path = match (input["path"].as_str(), &kind) {
                (Some(path), _) => absolutize(Path::new(path), cwd.as_deref()),
                // A search without a path covers the working directory.
                (None, EventKind::Search { .. }) => match &cwd {
                    Some(cwd) => cwd.clone(),
                    None => return Ok(out),
                },
                (None, _) => return Ok(out),
            };
            let mut e = FileEvent::new(now, AgentId::OpenCode, path, kind, Source::Hook);
            e.session = session;
            e.prompt_id = turn;
            e.dedup_key = call;
            out.records.push(LogRecord::File(e));
        }
        _ => {}
    }
    Ok(out)
}

/// The span of every hunk in `edit`'s patches — see [`crate::patch::span`].
fn edit_range(event: &Value) -> Option<LineRange> {
    let patches = event["result"]["output"]["files"].as_array()?;
    crate::patch::span(patches.iter().filter_map(|f| f["patch"].as_str()))
}

fn absolutize(path: &Path, cwd: Option<&Path>) -> PathBuf {
    match cwd {
        Some(cwd) if !path.is_absolute() => cwd.join(path),
        _ => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thirdeye_core::{EventKind, LineRange, TurnKind};

    // Raw events from docs/opencode-plugin-events.md, inside the envelope
    // thirdeye's plugin wraps them in.
    fn envelope(hook: &str, turn: Option<&str>, event: &str) -> String {
        let turn = turn
            .map(|t| format!(r#""turn":"{t}","#))
            .unwrap_or_default();
        format!(r#"{{"hook":"{hook}","session":"ses_1",{turn}"cwd":"/w","event":{event}}}"#)
    }

    fn after(tool: &str, input: &str, status: &str, result: &str) -> String {
        envelope(
            "tool.after",
            Some("msg_user"),
            &format!(
                r#"{{"tool":"{tool}","sessionID":"ses_1","messageID":"msg_asst","id":"call_1",
                    "input":{input},"status":"{status}","result":{result}}}"#
            ),
        )
    }

    fn translate(raw: &str) -> Payload {
        records_from_payload(raw, Timestamp(1)).unwrap()
    }

    fn only_file(payload: &Payload) -> &thirdeye_core::FileEvent {
        match payload.records.as_slice() {
            [LogRecord::File(e)] => e,
            other => panic!("expected one file event, got {other:?}"),
        }
    }

    #[test]
    fn a_prompt_opens_a_turn() {
        let p = translate(&envelope(
            "prompt",
            Some("msg_user"),
            r#"{"sessionID":"ses_1","messageID":"msg_user","prompt":{"text":"edit it","files":[]}}"#,
        ));
        let [LogRecord::Turn(open)] = p.records.as_slice() else {
            panic!("{:?}", p.records)
        };
        assert_eq!(open.prompt_id, "msg_user");
        assert_eq!(open.session.as_deref(), Some("ses_1"));
        assert_eq!(
            open.kind,
            TurnKind::Open {
                prompt: "edit it".into()
            }
        );
    }

    #[test]
    fn an_execution_ending_closes_the_turn() {
        let p = translate(&envelope(
            "turn.end",
            Some("msg_user"),
            r#"{"type":"session.execution.succeeded","data":{"sessionID":"ses_1"}}"#,
        ));
        let [LogRecord::Turn(close)] = p.records.as_slice() else {
            panic!("{:?}", p.records)
        };
        assert_eq!(close.prompt_id, "msg_user");
        assert_eq!(close.kind, TurnKind::Close);
    }

    #[test]
    fn a_read_write_and_edit_are_recorded_with_their_paths() {
        // The turn is the user's message the plugin remembered, not the
        // assistant message on the tool event, which changes every step.
        let read = translate(&after("read", r#"{"path":"data.txt"}"#, "completed", "{}"));
        let e = only_file(&read);
        assert_eq!(e.path, std::path::Path::new("/w/data.txt"));
        assert!(matches!(e.kind, EventKind::Read { range: None }));
        assert_eq!(e.prompt_id.as_deref(), Some("msg_user"));
        assert_eq!(e.dedup_key.as_deref(), Some("call_1"));
        assert_eq!(e.agent, AgentId::OpenCode);

        let write = translate(&after(
            "write",
            r#"{"path":"notes.md","content":"x"}"#,
            "completed",
            "{}",
        ));
        assert_eq!(only_file(&write).kind, EventKind::Write);

        let edit = translate(&after(
            "edit",
            r#"{"path":"/abs/data.txt"}"#,
            "completed",
            "{}",
        ));
        let e = only_file(&edit);
        assert_eq!(e.path, std::path::Path::new("/abs/data.txt"));
        assert!(matches!(e.kind, EventKind::Edit { .. }));
    }

    #[test]
    fn an_edit_carries_its_hunk_range() {
        // New-file coordinates, spanning every hunk: what to open the human at.
        let result = r#"{"output":{"files":[{"file":"data.txt","status":"modified",
            "patch":"--- data.txt\n+++ data.txt\n@@ -1,3 +1,3 @@\n alpha\n-beta\n+BETA\n@@ -10,2 +10,4 @@\n x\n"}]}}"#;
        let p = translate(&after(
            "edit",
            r#"{"path":"data.txt"}"#,
            "completed",
            result,
        ));
        assert_eq!(
            only_file(&p).kind,
            EventKind::Edit {
                range: Some(LineRange::new(1, 14))
            }
        );
    }

    #[test]
    fn a_shell_call_is_bracketed() {
        let before = translate(&envelope(
            "tool.before",
            Some("msg_user"),
            r#"{"tool":"shell","sessionID":"ses_1","id":"call_9","input":{"command":"x"}}"#,
        ));
        assert_eq!(
            before.observe,
            Some(Observe::Begin {
                call: "call_9".into()
            })
        );
        let end = translate(
            &after("shell", r#"{"command":"x"}"#, "completed", "{}").replace("call_1", "call_9"),
        );
        assert_eq!(
            end.observe,
            Some(Observe::End {
                call: "call_9".into(),
                turn: Some("msg_user".into())
            })
        );
        assert!(end.records.is_empty());
    }

    #[test]
    fn a_search_is_recorded_against_where_it_looked() {
        // Shapes from OpenCode 2.0.24's tool schemas: `grep` and `glob` both
        // take `pattern` and an optional `path`.
        let p = translate(&after(
            "grep",
            r#"{"pattern":"fn main","path":"src"}"#,
            "completed",
            "{}",
        ));
        let e = only_file(&p);
        assert_eq!(e.path, Path::new("/w/src"));
        assert_eq!(
            e.kind,
            EventKind::Search {
                pattern: "fn main".into()
            }
        );
        assert_eq!(e.dedup_key.as_deref(), Some("call_1"));

        let p = translate(&after(
            "glob",
            r#"{"pattern":"**/*.rs"}"#,
            "completed",
            "{}",
        ));
        let e = only_file(&p);
        assert_eq!(
            e.path,
            Path::new("/w"),
            "a pathless search covers the directory"
        );
        assert_eq!(
            e.kind,
            EventKind::Search {
                pattern: "**/*.rs".into()
            }
        );
    }

    #[test]
    fn a_failed_edit_is_not_recorded() {
        // The file did not change; recording it would rank a file the agent
        // never touched.
        let p = translate(&after("edit", r#"{"path":"data.txt"}"#, "error", "{}"));
        assert!(p.records.is_empty());
    }

    #[test]
    fn a_tool_with_no_turn_is_still_recorded() {
        // thirdeye installed mid-session: the plugin never saw the prompt.
        let raw = after("write", r#"{"path":"a.md"}"#, "completed", "{}")
            .replace(r#""turn":"msg_user","#, "");
        let p = translate(&raw);
        assert_eq!(only_file(&p).prompt_id, None);
    }

    #[test]
    fn file_contents_never_reach_the_log() {
        let result = r#"{"output":{"type":"file","content":"SECRET-BODY"},"content":[{"type":"text","text":"SECRET-BODY"}]}"#;
        let p = translate(&after(
            "read",
            r#"{"path":"data.txt"}"#,
            "completed",
            result,
        ));
        let line = serde_json::to_string(&p.records).unwrap();
        assert!(!line.contains("SECRET-BODY"), "{line}");
    }

    #[test]
    fn code_mode_wrappers_are_ignored() {
        // `execute` wraps a nested call that reports itself.
        let p = translate(&after("execute", r#"{"code":"..."}"#, "completed", "{}"));
        assert!(p.records.is_empty());
        assert_eq!(p.observe, None);
    }

    #[test]
    fn the_server_shutting_down_ends_the_session() {
        let p = translate(&envelope(
            "session.end",
            None,
            r#"{"type":"location.shutdown"}"#,
        ));
        assert_eq!(p.lifecycle, Some(Lifecycle::End));
    }

    #[test]
    fn a_shell_read_is_recorded_too() {
        let p = translate(&after(
            "shell",
            r#"{"command":"head -n 3 notes.md"}"#,
            "completed",
            "{}",
        ));
        let [LogRecord::File(e)] = p.records.as_slice() else {
            panic!("{:?}", p.records)
        };
        assert_eq!(e.path, std::path::Path::new("/w/notes.md"));
        assert_eq!(
            e.kind,
            EventKind::Read {
                range: Some(LineRange::new(1, 4))
            }
        );
    }
}
