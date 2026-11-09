//! pi extension events → log records.
//!
//! Shapes are captured, not assumed — see `docs/pi-extension-events.md`.
//!
//! thirdeye's extension (`agents/pi/extension.js`) forwards each raw event in
//! the envelope OpenCode's plugin uses, adding the session, the directory and
//! the turn id pi does not have. This module is the only place the raw events
//! are read; the extension forwards and never interprets.

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

    let tool = event["toolName"].as_str().unwrap_or_default();
    let call = event["toolCallId"].as_str().map(str::to_owned);
    match v["hook"].as_str().unwrap_or_default() {
        "prompt" => {
            if let Some(turn) = turn {
                let prompt = event["prompt"].as_str().unwrap_or_default().to_string();
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
        // `bash` is bracketed, not interpreted, like every agent's shell.
        "tool.before" if tool == "bash" => out.observe = call.map(|call| Observe::Begin { call }),
        "tool.after" if tool == "bash" => {
            if let Some(command) = event["input"]["command"].as_str() {
                let home = thirdeye_core::home_dir();
                out.records = crate::shell::read_records(
                    command,
                    AgentId::Pi,
                    session.as_deref(),
                    turn.as_deref(),
                    call.as_deref(),
                    now,
                    |p| resolve(p, cwd.as_deref(), home.as_deref()),
                );
            }
            out.observe = call.map(|call| Observe::End { call, turn });
        }
        // A tool that failed changed nothing worth ranking.
        "tool.after" if event["isError"].as_bool() == Some(false) => {
            let input = &event["input"];
            let kind = match tool {
                "read" => EventKind::Read { range: None },
                "write" => EventKind::Write,
                "edit" => EventKind::Edit {
                    range: edit_range(event),
                },
                "grep" | "find" => EventKind::Search {
                    pattern: input["pattern"].as_str().unwrap_or_default().to_string(),
                },
                // A listing is a search that matches everything.
                "ls" => EventKind::Search {
                    pattern: String::new(),
                },
                _ => return Ok(out),
            };
            // A search without a path covers the working directory, which
            // `resolve` spells as the directory itself.
            let fallback = matches!(kind, EventKind::Search { .. }).then_some(".");
            let Some(path) = input["path"].as_str().or(fallback) else {
                return Ok(out);
            };
            let home = thirdeye_core::home_dir();
            let mut e = FileEvent::new(
                now,
                AgentId::Pi,
                resolve(path, cwd.as_deref(), home.as_deref()),
                kind,
                Source::Hook,
            );
            e.session = session;
            e.prompt_id = turn;
            e.dedup_key = call;
            out.records.push(LogRecord::File(e));
        }
        _ => {}
    }
    Ok(out)
}

/// The edit's span, from the hunk headers of `details.patch`.
fn edit_range(event: &Value) -> Option<LineRange> {
    crate::patch::span(event["details"]["patch"].as_str().into_iter())
}

/// A tool's path as pi itself resolves it (`resolveToCwd` in pi's
/// `utils/paths`): a leading `@` stripped, `~` expanded, relative to the
/// session's directory, normalized. Recording the raw text would list files
/// that do not exist — `/w/@src/a.rs` — and one file twice, `./a.rs` beside
/// `a.rs`.
fn resolve(raw: &str, cwd: Option<&Path>, home: Option<&Path>) -> PathBuf {
    let raw = raw.strip_prefix('@').unwrap_or(raw);
    let path = match (raw.strip_prefix('~'), home) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            home.join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(raw),
    };
    let path = match cwd {
        Some(cwd) if !path.is_absolute() => cwd.join(path),
        _ => path,
    };
    // Lexically, as pi does — not `canonicalize`, which would follow links
    // and fail for a file just deleted.
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use thirdeye_core::{EventKind, LineRange, TurnKind};

    // Raw events from docs/pi-extension-events.md, inside the envelope
    // thirdeye's extension wraps them in.
    fn envelope(hook: &str, turn: Option<&str>, event: &str) -> String {
        let turn = turn
            .map(|t| format!(r#""turn":"{t}","#))
            .unwrap_or_default();
        format!(r#"{{"hook":"{hook}","session":"01a1-sess",{turn}"cwd":"/w","event":{event}}}"#)
    }

    fn result(tool: &str, input: &str, is_error: bool, details: &str) -> String {
        envelope(
            "tool.after",
            Some("t1"),
            &format!(
                r#"{{"type":"tool_result","toolName":"{tool}","toolCallId":"call_1|fc_1",
                    "input":{input},"content":[{{"type":"text","text":"SECRET-BODY"}}],
                    "structuredContent":"SECRET-BODY","isError":{is_error},"details":{details}}}"#
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
    fn a_search_is_recorded_against_where_it_looked() {
        // Shapes from pi 1.1.0's tool schemas: `grep` and `find` take
        // `pattern` and an optional `path`; `ls` only the path.
        let p = translate(&result(
            "grep",
            r#"{"pattern":"TODO","path":"src"}"#,
            false,
            "null",
        ));
        let e = only_file(&p);
        assert_eq!(e.path, Path::new("/w/src"));
        assert_eq!(
            e.kind,
            EventKind::Search {
                pattern: "TODO".into()
            }
        );

        let p = translate(&result("find", r#"{"pattern":"*.ts"}"#, false, "null"));
        let e = only_file(&p);
        assert_eq!(
            e.path,
            Path::new("/w"),
            "a pathless search covers the directory"
        );
        assert_eq!(
            e.kind,
            EventKind::Search {
                pattern: "*.ts".into()
            }
        );

        // A listing is a search that matches everything.
        let p = translate(&result("ls", r#"{"path":"./docs"}"#, false, "null"));
        let e = only_file(&p);
        assert_eq!(e.path, Path::new("/w/docs"));
        assert_eq!(
            e.kind,
            EventKind::Search {
                pattern: String::new()
            }
        );
    }

    #[test]
    fn a_failed_search_is_not_recorded() {
        let p = translate(&result("grep", r#"{"pattern":"("}"#, true, "null"));
        assert!(p.records.is_empty());
    }

    #[test]
    fn a_prompt_opens_a_turn() {
        let p = translate(&envelope("prompt", Some("t1"), r#"{"prompt":"edit it"}"#));
        let [LogRecord::Turn(open)] = p.records.as_slice() else {
            panic!("{:?}", p.records)
        };
        assert_eq!(open.prompt_id, "t1");
        assert_eq!(open.session.as_deref(), Some("01a1-sess"));
        assert_eq!(
            open.kind,
            TurnKind::Open {
                prompt: "edit it".into()
            }
        );
    }

    #[test]
    fn the_agent_settling_closes_the_turn() {
        let p = translate(&envelope("turn.end", Some("t1"), r#"{"aborted":false}"#));
        let [LogRecord::Turn(close)] = p.records.as_slice() else {
            panic!("{:?}", p.records)
        };
        assert_eq!(close.kind, TurnKind::Close);
        assert_eq!(close.prompt_id, "t1");
    }

    #[test]
    fn a_read_write_and_edit_are_recorded_with_their_paths() {
        let read = translate(&result(
            "read",
            r#"{"path":"data.txt","offset":1,"limit":10}"#,
            false,
            "null",
        ));
        let e = only_file(&read);
        assert_eq!(e.path, std::path::Path::new("/w/data.txt"));
        assert!(matches!(e.kind, EventKind::Read { range: None }));
        assert_eq!(e.agent, AgentId::Pi);
        assert_eq!(e.prompt_id.as_deref(), Some("t1"));
        assert_eq!(e.dedup_key.as_deref(), Some("call_1|fc_1"));

        let write = translate(&result(
            "write",
            r#"{"path":"notes.md","content":"x"}"#,
            false,
            "null",
        ));
        assert_eq!(only_file(&write).kind, EventKind::Write);
    }

    #[test]
    fn an_edit_carries_its_hunk_range() {
        let details = r#"{"patch":"--- data.txt\n+++ data.txt\n@@ -1,3 +1,3 @@\n alpha\n-beta\n+BETA\n gamma\n","firstChangedLine":2}"#;
        let p = translate(&result(
            "edit",
            r#"{"path":"data.txt","edits":[]}"#,
            false,
            details,
        ));
        assert_eq!(
            only_file(&p).kind,
            EventKind::Edit {
                range: Some(LineRange::new(1, 4))
            }
        );
    }

    #[test]
    fn a_bash_call_is_bracketed() {
        let before = translate(&envelope(
            "tool.before",
            Some("t1"),
            r#"{"type":"tool_call","toolName":"bash","toolCallId":"call_9","input":{"command":"x"}}"#,
        ));
        assert_eq!(
            before.observe,
            Some(Observe::Begin {
                call: "call_9".into()
            })
        );
        let after = translate(
            &result("bash", r#"{"command":"x"}"#, false, "null").replace("call_1|fc_1", "call_9"),
        );
        assert_eq!(
            after.observe,
            Some(Observe::End {
                call: "call_9".into(),
                turn: Some("t1".into())
            })
        );
        assert!(after.records.is_empty());
    }

    #[test]
    fn a_failed_edit_is_not_recorded() {
        assert!(
            translate(&result("edit", r#"{"path":"data.txt"}"#, true, "null"))
                .records
                .is_empty()
        );
    }

    #[test]
    fn file_contents_never_reach_the_log() {
        let p = translate(&result("read", r#"{"path":"data.txt"}"#, false, "null"));
        let line = serde_json::to_string(&p.records).unwrap();
        assert!(!line.contains("SECRET-BODY"), "{line}");
    }

    #[test]
    fn a_tool_with_no_turn_is_still_recorded() {
        let raw =
            result("write", r#"{"path":"a.md"}"#, false, "null").replace(r#""turn":"t1","#, "");
        assert_eq!(only_file(&translate(&raw)).prompt_id, None);
    }

    #[test]
    fn paths_resolve_the_way_pi_resolves_them() {
        // pi strips a leading `@`, expands `~` and normalizes; recording the
        // raw text would list files that do not exist, and one file twice.
        let home = Some(Path::new("/home/u"));
        let cwd = Some(Path::new("/w"));
        assert_eq!(
            resolve("@src/a.rs", cwd, home),
            PathBuf::from("/w/src/a.rs")
        );
        assert_eq!(
            resolve("~/.bashrc", cwd, home),
            PathBuf::from("/home/u/.bashrc")
        );
        assert_eq!(resolve("~", cwd, home), PathBuf::from("/home/u"));
        assert_eq!(resolve("./a.rs", cwd, home), PathBuf::from("/w/a.rs"));
        assert_eq!(resolve("src/../a.rs", cwd, home), PathBuf::from("/w/a.rs"));
        assert_eq!(resolve("/abs/./x", cwd, home), PathBuf::from("/abs/x"));
    }

    #[test]
    fn the_session_shutting_down_ends_it() {
        let p = translate(&envelope("session.end", None, r#"{"reason":"quit"}"#));
        assert_eq!(p.lifecycle, Some(Lifecycle::End));
    }

    #[test]
    fn a_bash_read_is_recorded_too() {
        let p = translate(&result(
            "bash",
            r#"{"command":"cat src/a.rs"}"#,
            false,
            "null",
        ));
        let [LogRecord::File(e)] = p.records.as_slice() else {
            panic!("{:?}", p.records)
        };
        assert_eq!(e.path, std::path::Path::new("/w/src/a.rs"));
        assert!(matches!(e.kind, EventKind::Read { range: None }));
    }
}
