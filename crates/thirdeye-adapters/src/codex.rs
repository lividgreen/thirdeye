//! Codex hook payload → log records.
//!
//! Payload shapes are captured, not assumed — see `docs/codex-hook-payload.md`.
//!
//! Unlike Claude Code, Codex's payloads cannot say what a tool call changed:
//! every captured edit went through the shell, once as a Python heredoc. So
//! this adapter translates turns and lifecycle, and only *brackets* tool calls
//! with [`Observe`]; the hook works out what was written by watching the
//! workspace across the bracket.

use std::path::PathBuf;

use serde_json::Value;
use thirdeye_core::{AgentId, LogRecord, Timestamp, TurnKind, TurnMarker};

use crate::{AdapterError, Lifecycle, Observe, Payload};

/// Translate one hook payload. Unrecognized events yield an empty payload
/// rather than an error, for the same reason as Claude's: Codex adds events,
/// and a hook that failed on one would break the user's session.
pub fn records_from_payload(raw: &str, now: Timestamp) -> Result<Payload, AdapterError> {
    let v: Value = serde_json::from_str(raw)?;

    let session = v["session_id"].as_str().map(str::to_owned);
    // `turn_id` is Claude's `prompt_id` under another name, and goes where
    // it goes — grouping is keyed by it.
    let turn = v["turn_id"].as_str().map(str::to_owned);
    let call = v["tool_use_id"].as_str().map(str::to_owned);
    let mut out = Payload {
        session: session.clone(),
        cwd: v["cwd"].as_str().map(PathBuf::from),
        ..Payload::default()
    };

    match v["hook_event_name"].as_str().unwrap_or_default() {
        "UserPromptSubmit" => {
            if let Some(turn) = turn {
                let prompt = v["prompt"].as_str().unwrap_or_default().to_string();
                let mut open = TurnMarker::new(now, turn, TurnKind::Open { prompt });
                open.session = session;
                out.records.push(LogRecord::Turn(open));
            }
        }
        "Stop" => {
            if let Some(turn) = turn {
                let mut close = TurnMarker::new(now, turn, TurnKind::Close);
                close.session = session;
                out.records.push(LogRecord::Turn(close));
            }
        }
        // Every tool, not only `Bash`: any of them can write a file, and the
        // bracket costs one marker file. `tool_response` — for `cat`, the
        // file's content — is never read.
        "PreToolUse" => out.observe = call.map(|call| Observe::Begin { call }),
        "PostToolUse" => {
            // Codex reads only through its shell; the command says which
            // files, for the forms `shell` recognizes.
            if let Some(command) = v["tool_input"]["command"].as_str() {
                let cwd = out.cwd.clone();
                out.records = crate::shell::read_records(
                    command,
                    AgentId::Codex,
                    session.as_deref(),
                    turn.as_deref(),
                    call.as_deref(),
                    now,
                    |p| crate::shell::under(cwd.as_deref(), p),
                );
            }
            out.observe = call.map(|call| Observe::End { call, turn });
        }
        "SessionStart" => out.lifecycle = Some(Lifecycle::Start),
        "SessionEnd" => out.lifecycle = Some(Lifecycle::End),
        _ => {}
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use thirdeye_core::TurnKind;

    // Shapes from docs/codex-hook-payload.md, trimmed to what matters.
    const PROMPT: &str = r#"{"session_id":"s1","turn_id":"t1","cwd":"/w",
        "hook_event_name":"UserPromptSubmit","model":"m","permission_mode":"default",
        "transcript_path":null,"prompt":"change beta"}"#;
    const STOP: &str = r#"{"session_id":"s1","turn_id":"t1","cwd":"/w",
        "hook_event_name":"Stop","model":"m","permission_mode":"default",
        "transcript_path":null,"stop_hook_active":false,"last_assistant_message":"DONE"}"#;
    const END: &str = r#"{"session_id":"s1","cwd":"/w","hook_event_name":"SessionEnd",
        "reason":"other","transcript_path":null}"#;

    fn tool(event: &str, id: Option<&str>) -> String {
        let id = id
            .map(|id| format!(r#","tool_use_id":"{id}""#))
            .unwrap_or_default();
        format!(
            r#"{{"session_id":"s1","turn_id":"t1","cwd":"/w","hook_event_name":"{event}",
               "model":"m","permission_mode":"default","transcript_path":null,
               "tool_name":"Bash","tool_input":{{"command":"cat data.txt"}},
               "tool_response":"alpha\nbeta\n"{id}}}"#
        )
    }

    fn translate(raw: &str) -> Payload {
        records_from_payload(raw, Timestamp(1)).unwrap()
    }

    #[test]
    fn a_prompt_opens_a_turn_keyed_by_turn_id() {
        // Codex's turn_id is Claude's prompt_id under another name; grouping
        // is keyed by it, so it must land in the same place.
        let payload = translate(PROMPT);
        let [LogRecord::Turn(open)] = payload.records.as_slice() else {
            panic!("expected one turn marker, got {:?}", payload.records);
        };
        assert_eq!(open.prompt_id, "t1");
        assert_eq!(open.session.as_deref(), Some("s1"));
        assert_eq!(
            open.kind,
            TurnKind::Open {
                prompt: "change beta".into()
            }
        );
    }

    #[test]
    fn stop_closes_the_turn_it_belongs_to() {
        let payload = translate(STOP);
        let [LogRecord::Turn(close)] = payload.records.as_slice() else {
            panic!("expected one turn marker, got {:?}", payload.records);
        };
        assert_eq!(close.prompt_id, "t1");
        assert_eq!(close.kind, TurnKind::Close);
    }

    #[test]
    fn a_tool_call_is_bracketed_not_interpreted() {
        // Codex changes files through arbitrary commands. Guessing what a
        // command wrote from its text would put files in the list that were
        // never touched; the hook watches the workspace instead.
        let begin = translate(&tool("PreToolUse", Some("exec-1")));
        assert_eq!(
            begin.observe,
            Some(Observe::Begin {
                call: "exec-1".into()
            })
        );
        assert!(begin.records.is_empty());

        let end = translate(&tool("PostToolUse", Some("exec-1")));
        assert_eq!(
            end.observe,
            Some(Observe::End {
                call: "exec-1".into(),
                turn: Some("t1".into()),
            })
        );
        // The command's reads are recorded; never a write inferred from it.
        assert!(end.records.iter().all(|r| matches!(
            r,
            LogRecord::File(e) if matches!(e.kind, thirdeye_core::EventKind::Read { .. })
        )));
        assert_eq!(end.session.as_deref(), Some("s1"));
    }

    #[test]
    fn a_command_output_never_reaches_the_log() {
        // For `cat`, tool_response is the file's content. The log must not
        // become a second copy of the user's source tree.
        let payload = translate(&tool("PostToolUse", Some("exec-1")));
        for record in &payload.records {
            let line = serde_json::to_string(record).unwrap();
            assert!(!line.contains("alpha"), "{line}");
        }
    }

    #[test]
    fn session_end_is_a_lifecycle_not_a_record() {
        let payload = translate(END);
        assert_eq!(payload.lifecycle, Some(Lifecycle::End));
        assert!(payload.records.is_empty());
    }

    #[test]
    fn a_tool_call_without_an_id_is_not_observed() {
        // A window with no key could never be closed.
        assert_eq!(translate(&tool("PostToolUse", None)).observe, None);
    }

    #[test]
    fn an_unknown_event_yields_nothing_rather_than_failing() {
        // Codex adds events; a hook that errored on one would surface as
        // noise in the user's session.
        let payload = translate(r#"{"session_id":"s1","hook_event_name":"PreCompact"}"#);
        assert_eq!(payload.records.len(), 0);
        assert_eq!(payload.observe, None);
    }

    #[test]
    fn a_shell_read_is_recorded_with_its_range() {
        // Codex reads only through its shell: `sed -n 2p data.txt`, as captured.
        let raw = tool("PostToolUse", Some("exec-1")).replace("cat data.txt", "sed -n 2p data.txt");
        let p = translate(&raw);
        let [LogRecord::File(e)] = p.records.as_slice() else {
            panic!("{:?}", p.records)
        };
        assert_eq!(e.path, std::path::Path::new("/w/data.txt"));
        assert_eq!(
            e.kind,
            thirdeye_core::EventKind::Read {
                range: Some(thirdeye_core::LineRange::new(2, 3))
            }
        );
        assert_eq!(e.prompt_id.as_deref(), Some("t1"));
        assert!(p.observe.is_some(), "the window still closes");
    }
}
