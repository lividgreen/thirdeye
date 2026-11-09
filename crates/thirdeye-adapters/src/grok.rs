//! Grok Build hook payloads → log records.
//!
//! Shapes are captured, not assumed — see `docs/grok-hook-payload.md`.
//!
//! Grok's payloads name their event and carry a real call id, but **its tool
//! events carry no turn id**: only the prompt and the turn's `Stop` do. So
//! the turn is held by the hook between those two, from the edges marked
//! here, as for Copilot — with Grok's own `promptId` as its id.

use std::path::{Path, PathBuf};

use serde_json::Value;
use thirdeye_core::{
    AgentId, EventKind, FileEvent, LineRange, LogRecord, Source, Timestamp, TurnKind, TurnMarker,
};

use crate::{AdapterError, Lifecycle, Observe, Payload, TurnEdge};

pub fn records_from_payload(raw: &str, now: Timestamp) -> Result<Payload, AdapterError> {
    let v: Value = serde_json::from_str(raw)?;
    let session = v["sessionId"].as_str().map(str::to_owned);
    let cwd = v["cwd"].as_str().map(PathBuf::from);
    let mut out = Payload {
        session: session.clone(),
        cwd: cwd.clone(),
        ..Payload::default()
    };

    let tool = v["toolName"].as_str().unwrap_or_default();
    let input = &v["toolInput"];
    let call = v["toolUseId"].as_str().map(str::to_owned);
    match v["hookEventName"].as_str().unwrap_or_default() {
        "user_prompt_submit" => {
            let Some(id) = v["promptId"].as_str() else {
                return Ok(out);
            };
            let prompt = v["prompt"].as_str().unwrap_or_default().to_string();
            let mut open = TurnMarker::new(now, id, TurnKind::Open { prompt });
            open.session = session;
            out.records.push(LogRecord::Turn(open));
            out.turn = Some(TurnEdge::Open(id.to_string()));
        }
        // A turn ends in one of three ways, each naming it. The `Stop` that
        // follows `SessionEnd` names none, and closes nothing.
        "stop" | "stop_failure" | "stop_cancelled" if v["promptId"].is_string() => {
            out.turn = Some(TurnEdge::Close);
        }
        "session_start" => out.lifecycle = Some(Lifecycle::Start),
        "session_end" => out.lifecycle = Some(Lifecycle::End),
        "pre_tool_use" => {
            out.turn = Some(TurnEdge::Within);
            if is_shell(tool) {
                out.observe = call.map(|call| Observe::Begin { call });
            }
        }
        // A command that cannot be dispatched fires the failure event, and a
        // bracket it opened must still close.
        hook @ ("post_tool_use" | "post_tool_use_failure") => {
            out.turn = Some(TurnEdge::Within);
            if is_shell(tool) {
                if hook == "post_tool_use"
                    && let Some(command) = input["command"].as_str()
                {
                    out.records = crate::shell::read_records(
                        command,
                        AgentId::Grok,
                        session.as_deref(),
                        None,
                        call.as_deref(),
                        now,
                        |p| crate::shell::under(cwd.as_deref(), p),
                    );
                }
                out.observe = call.map(|call| Observe::End { call, turn: None });
            } else if hook == "post_tool_use"
                && let Some((path, kind)) =
                    translated(tool, input, &v["toolResult"], cwd.as_deref())
            {
                let mut e = FileEvent::new(now, AgentId::Grok, path, kind, Source::Hook);
                e.session = session;
                e.dedup_key = call;
                out.records.push(LogRecord::File(e));
            }
        }
        _ => {}
    }
    Ok(out)
}

/// `run_terminal_command` is what a live hook reported; Grok's headless docs
/// call the same tool `run_terminal_cmd`.
fn is_shell(tool: &str) -> bool {
    matches!(tool, "run_terminal_command" | "run_terminal_cmd")
}

/// What a structured tool did. `result` is read for its shape only — whether
/// the call did anything, and where — never for the file text it holds.
fn translated(
    tool: &str,
    input: &Value,
    result: &Value,
    cwd: Option<&Path>,
) -> Option<(PathBuf, EventKind)> {
    let at = |key: &str| input[key].as_str().map(|p| absolutize(Path::new(p), cwd));
    match tool {
        // A missing file is still a `PostToolUse`, with `FileNotFound` where
        // the content would be.
        "read_file" => result["FileContent"]
            .is_object()
            .then(|| at("target_file"))
            .flatten()
            .map(|path| (path, EventKind::Read { range: None })),
        // An edit that applied nothing reports no `EditsApplied`.
        "search_replace" => result["EditsApplied"]
            .is_object()
            .then(|| at("file_path"))
            .flatten()
            .map(|path| {
                (
                    path,
                    EventKind::Edit {
                        range: edit_range(result),
                    },
                )
            }),
        "write" => result["EditsApplied"]
            .is_object()
            .then(|| at("file_path"))
            .flatten()
            .map(|path| (path, EventKind::Write)),
        "grep" => {
            let pattern = input["pattern"].as_str().unwrap_or_default().to_string();
            Some((
                searched(input["path"].as_str(), cwd)?,
                EventKind::Search { pattern },
            ))
        }
        // A listing is a search that matches everything.
        "list_dir" => Some((
            searched(input["target_directory"].as_str(), cwd)?,
            EventKind::Search {
                pattern: String::new(),
            },
        )),
        _ => None,
    }
}

/// Where a search looked: the working directory when it names none, or `.`.
fn searched(path: Option<&str>, cwd: Option<&Path>) -> Option<PathBuf> {
    match path {
        Some(p) if !p.is_empty() && p != "." => Some(absolutize(Path::new(p), cwd)),
        _ => cwd.map(Path::to_path_buf),
    }
}

/// The span of every replaced region, in the new file: each detail gives the
/// line it now starts at, and its replacement's length says how far it runs.
fn edit_range(result: &Value) -> Option<LineRange> {
    let details = result["EditsApplied"]["edits"]["details"].as_array()?;
    let mut spans = details.iter().filter_map(|d| {
        let start = d["new_line"].as_u64()? as u32;
        let lines = d["new_string"].as_str()?.lines().count().max(1) as u32;
        Some((start, start + lines))
    });
    let (mut first, mut last) = spans.next()?;
    for (start, end) in spans {
        first = first.min(start);
        last = last.max(end);
    }
    Some(LineRange::new(first, last))
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

    // Payloads from docs/grok-hook-payload.md, trimmed to what is read.
    fn event(name: &str, fields: &str) -> String {
        format!(
            r#"{{"hookEventName":"{name}","sessionId":"01a1-sess","cwd":"/w",
                "workspaceRoot":"/w/","permissionMode":"default"{fields}}}"#
        )
    }

    fn tool(name: &str, tool: &str, input: &str, result: &str) -> String {
        let result = if result.is_empty() {
            String::new()
        } else {
            format!(r#","toolResult":{result}"#)
        };
        event(
            name,
            &format!(
                r#","toolName":"{tool}","toolUseId":"{tool}_0_6638","toolInput":{input}{result}"#
            ),
        )
    }

    fn translate(raw: &str) -> Payload {
        records_from_payload(raw, Timestamp(1)).unwrap()
    }

    fn only_file(p: &Payload) -> &FileEvent {
        match p.records.as_slice() {
            [LogRecord::File(e)] => e,
            other => panic!("expected one file event, got {other:?}"),
        }
    }

    #[test]
    fn a_prompt_opens_the_turn_grok_named() {
        let p = translate(&event(
            "user_prompt_submit",
            r#","promptId":"5815887c","prompt":"edit it""#,
        ));
        assert_eq!(p.turn, Some(TurnEdge::Open("5815887c".into())));
        let [LogRecord::Turn(open)] = p.records.as_slice() else {
            panic!("{:?}", p.records)
        };
        assert_eq!(open.prompt_id, "5815887c");
        assert_eq!(
            open.kind,
            TurnKind::Open {
                prompt: "edit it".into()
            }
        );
    }

    #[test]
    fn a_turn_closes_however_it_ends_but_not_on_the_stop_after_the_session() {
        for name in ["stop", "stop_failure", "stop_cancelled"] {
            let p = translate(&event(
                name,
                r#","promptId":"5815887c","reason":"end_turn""#,
            ));
            assert_eq!(p.turn, Some(TurnEdge::Close), "{name}");
        }
        // Captured: a `Stop` follows `SessionEnd`, naming no turn.
        let p = translate(&event(
            "stop",
            r#","reason":"shutdown","stopHookActive":false"#,
        ));
        assert_eq!(p.turn, None);
    }

    #[test]
    fn the_session_ending_ends_the_session() {
        let p = translate(&event("session_end", r#","reason":"shutdown""#));
        assert_eq!(p.lifecycle, Some(Lifecycle::End));
    }

    #[test]
    fn a_read_is_recorded_and_its_content_never_is() {
        let p = translate(&tool(
            "post_tool_use",
            "read_file",
            r#"{"target_file":"src/calc.py"}"#,
            r#"{"type":"ReadFile","FileContent":{"content":"SECRET-BODY","total_lines":7}}"#,
        ));
        let e = only_file(&p);
        assert_eq!(e.path, Path::new("/w/src/calc.py"));
        assert_eq!(e.kind, EventKind::Read { range: None });
        assert_eq!(e.dedup_key.as_deref(), Some("read_file_0_6638"));
        assert_eq!(p.turn, Some(TurnEdge::Within), "tool events name no turn");
        assert!(!serde_json::to_string(e).unwrap().contains("SECRET-BODY"));
    }

    #[test]
    fn a_read_of_a_missing_file_is_not_a_read() {
        // Still a PostToolUse, with the error where the content would be.
        let p = translate(&tool(
            "post_tool_use",
            "read_file",
            r#"{"target_file":"does-not-exist.txt"}"#,
            r#"{"type":"ReadFile","FileNotFound":"Error: /w/does-not-exist.txt does not exist."}"#,
        ));
        assert!(p.records.is_empty());
    }

    #[test]
    fn an_edit_carries_the_lines_it_now_occupies() {
        let p = translate(&tool(
            "post_tool_use",
            "search_replace",
            r#"{"file_path":"src/calc.py","old_string":"a","new_string":"b"}"#,
            r#"{"type":"SearchReplace","EditsApplied":{"absolute_path":"/w/src/calc.py","edits":{"details":[
                {"old_line":5,"new_line":5,"old_string":"def sub(a, b):\n    return a - b\n",
                 "new_string":"def sub(a, b):\n    return a - b\n\n\ndef mul(a, b):\n    SECRET\n"}]}}}"#,
        ));
        let e = only_file(&p);
        assert_eq!(
            e.kind,
            EventKind::Edit {
                range: Some(LineRange::new(5, 11))
            }
        );
        assert!(!serde_json::to_string(e).unwrap().contains("SECRET"));
    }

    #[test]
    fn a_write_is_a_write() {
        let p = translate(&tool(
            "post_tool_use",
            "write",
            r#"{"file_path":"/w/src/div.py","content":"def div(a, b):\n"}"#,
            r#"{"type":"SearchReplace","EditsApplied":{"edits":{"details":[{"new_line":1,"new_string":"x"}]}}}"#,
        ));
        let e = only_file(&p);
        assert_eq!(
            (e.path.as_path(), &e.kind),
            (Path::new("/w/src/div.py"), &EventKind::Write)
        );
    }

    #[test]
    fn a_search_and_a_listing_are_recorded_against_where_they_looked() {
        let p = translate(&tool(
            "post_tool_use",
            "grep",
            r#"{"pattern":"return","path":"."}"#,
            r#"{"type":"GrepSearch","match_count":4}"#,
        ));
        let e = only_file(&p);
        assert_eq!(e.path, Path::new("/w"), "`.` is the directory itself");
        assert_eq!(
            e.kind,
            EventKind::Search {
                pattern: "return".into()
            }
        );

        let p = translate(&tool(
            "post_tool_use",
            "list_dir",
            r#"{"target_directory":"src"}"#,
            r#"{"type":"ListDir","Content":{}}"#,
        ));
        let e = only_file(&p);
        assert_eq!(e.path, Path::new("/w/src"));
        assert_eq!(
            e.kind,
            EventKind::Search {
                pattern: String::new()
            }
        );
    }

    #[test]
    fn a_command_is_bracketed_by_groks_own_call_id() {
        let input = r#"{"command":"sed -i 's/a/b/' notes.txt","description":"x"}"#;
        let pre = translate(&tool("pre_tool_use", "run_terminal_command", input, ""));
        assert_eq!(
            pre.observe,
            Some(Observe::Begin {
                call: "run_terminal_command_0_6638".into()
            })
        );
        // Exit code 1 is still a PostToolUse; so the window closes either way.
        let post = translate(&tool(
            "post_tool_use",
            "run_terminal_command",
            input,
            r#"{"type":"Bash","exit_code":1}"#,
        ));
        assert_eq!(
            post.observe,
            Some(Observe::End {
                call: "run_terminal_command_0_6638".into(),
                turn: None
            })
        );
        assert!(!post.call_ids_derived);
    }

    #[test]
    fn a_shell_read_is_recorded_too() {
        let p = translate(&tool(
            "post_tool_use",
            "run_terminal_command",
            r#"{"command":"cat notes.txt"}"#,
            r#"{"type":"Bash","exit_code":0}"#,
        ));
        assert_eq!(only_file(&p).path, Path::new("/w/notes.txt"));
    }

    #[test]
    fn an_mcp_call_and_a_tool_search_record_nothing() {
        for (name, input) in [
            (
                "thirdeye__flag",
                r#"{"tool_name":"thirdeye__flag","tool_input":{"path":"a"}}"#,
            ),
            ("search_tool", r#"{"query":"thirdeye flag"}"#),
        ] {
            let p = translate(&tool("post_tool_use", name, input, "{}"));
            assert!(p.records.is_empty() && p.observe.is_none(), "{name}");
        }
    }
}
