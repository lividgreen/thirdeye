//! Antigravity CLI hook payloads → log records.
//!
//! Shapes are captured, not assumed — see `docs/antigravity-hook-payload.md`.
//!
//! Like Copilot's, Antigravity's payloads do not name their event: the hook
//! wraps each in `{ "hook": <event>, "event": <payload> }`. They carry no
//! turn id either, so the turn is held by the hook from the edges marked
//! here, but they do carry a real call id, `stepIdx`. There is no prompt
//! text and no session end; the daemon notices the agent exiting instead.

use std::path::{Path, PathBuf};

use serde_json::Value;
use thirdeye_core::{
    AgentId, EventKind, FileEvent, LineRange, LogRecord, Source, Timestamp, TurnKind, TurnMarker,
};

use crate::{AdapterError, Observe, Payload, TurnEdge};

pub fn records_from_payload(raw: &str, now: Timestamp) -> Result<Payload, AdapterError> {
    let v: Value = serde_json::from_str(raw)?;
    let event = &v["event"];
    let session = event["conversationId"].as_str().map(str::to_owned);
    // The hook runs in the directory holding `hooks.json`, not the project.
    let cwd = event["workspacePaths"][0].as_str().map(PathBuf::from);
    let mut out = Payload {
        session: session.clone(),
        cwd: cwd.clone(),
        ..Payload::default()
    };

    let tool = event["toolCall"]["name"].as_str().unwrap_or_default();
    let args = &event["toolCall"]["args"];
    let call = event["stepIdx"].as_u64().map(|n| n.to_string());
    match v["hook"].as_str().unwrap_or_default() {
        // Once per model call; the first of a prompt opens its turn. The
        // step it starts after is the user's message, unique within the
        // conversation.
        "PreInvocation" if event["invocationNum"].as_u64() == Some(0) => {
            let id = event["initialNumSteps"]
                .as_u64()
                .unwrap_or(now.0)
                .to_string();
            // Antigravity hands its hooks no prompt text.
            let mut open = TurnMarker::new(
                now,
                id.clone(),
                TurnKind::Open {
                    prompt: String::new(),
                },
            );
            open.session = session;
            out.records.push(LogRecord::Turn(open));
            out.turn = Some(TurnEdge::Open(id));
        }
        "Stop" => out.turn = Some(TurnEdge::Close),
        "PreToolUse" => {
            out.turn = Some(TurnEdge::Within);
            if bracketed(tool) {
                out.observe = call.map(|call| Observe::Begin { call });
            }
        }
        "PostToolUse" => {
            out.turn = Some(TurnEdge::Within);
            // A command that exited non-zero still reports `""`; only a call
            // that did not run says why.
            let failed = event["error"].as_str().is_some_and(|e| !e.is_empty());
            if bracketed(tool) {
                if tool == "run_command"
                    && !failed
                    && let Some(command) = args["CommandLine"].as_str()
                {
                    let dir = args["Cwd"].as_str().map(PathBuf::from).or(cwd.clone());
                    out.records = crate::shell::read_records(
                        command,
                        AgentId::Antigravity,
                        session.as_deref(),
                        None,
                        call.as_deref(),
                        now,
                        |p| crate::shell::under(dir.as_deref(), p),
                    );
                }
                out.observe = call.map(|call| Observe::End { call, turn: None });
            } else if !failed && let Some((path, kind)) = translated(tool, args, cwd.as_deref()) {
                let mut e = FileEvent::new(now, AgentId::Antigravity, path, kind, Source::Hook);
                e.session = session;
                e.dedup_key = call;
                out.records.push(LogRecord::File(e));
            }
        }
        _ => {}
    }
    Ok(out)
}

/// Whether a call is watched across rather than read: the shell, and any
/// tool this adapter has not seen, which may well write.
fn bracketed(tool: &str) -> bool {
    !matches!(tool, "view_file" | "replace_file_content" | "write_to_file")
}

fn translated(tool: &str, args: &Value, cwd: Option<&Path>) -> Option<(PathBuf, EventKind)> {
    let at = |key: &str| args[key].as_str().map(|p| absolutize(Path::new(p), cwd));
    let line = |key: &str| args[key].as_u64().map(|n| n as u32);
    match tool {
        "view_file" => {
            let path = at("AbsolutePath")?;
            // The agent reading its own tool schemas before calling one —
            // `~/.gemini/antigravity-cli/mcp/<server>/<tool>.json` — is it
            // loading its tools, not reading the project.
            if is_own_config(&path) {
                return None;
            }
            // Inclusive on the wire, exclusive in the log.
            let range = line("StartLine")
                .zip(line("EndLine"))
                .map(|(start, end)| LineRange::new(start, end + 1));
            Some((path, EventKind::Read { range }))
        }
        "replace_file_content" => {
            // The replaced region's start, spanning what replaced it: where
            // the human should look afterwards.
            let range = line("StartLine").map(|start| {
                let lines = args["ReplacementContent"]
                    .as_str()
                    .map_or(1, |text| text.lines().count().max(1))
                    as u32;
                LineRange::new(start, start + lines)
            });
            Some((at("TargetFile")?, EventKind::Edit { range }))
        }
        "write_to_file" => Some((at("TargetFile")?, EventKind::Write)),
        _ => None,
    }
}

fn is_own_config(path: &Path) -> bool {
    let parts: Vec<&std::ffi::OsStr> = path.iter().collect();
    parts
        .windows(2)
        .any(|pair| pair[0] == ".gemini" && pair[1] == "antigravity-cli")
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

    const COMMON: &str = r#""conversationId":"c8cb","workspacePaths":["/w"],
        "transcriptPath":"/h/t.jsonl","artifactDirectoryPath":"/h/a","modelName":"gemini-3.8-flash-low""#;

    fn envelope(hook: &str, fields: &str) -> String {
        format!(r#"{{"hook":"{hook}","event":{{{COMMON}{fields}}}}}"#)
    }

    fn tool(hook: &str, name: &str, args: &str, step: u64, extra: &str) -> String {
        envelope(
            hook,
            &format!(r#","toolCall":{{"name":"{name}","args":{args}}},"stepIdx":{step}{extra}"#),
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
    fn the_first_model_call_of_a_prompt_opens_its_turn() {
        let p = translate(&envelope(
            "PreInvocation",
            r#","invocationNum":0,"initialNumSteps":21"#,
        ));
        assert_eq!(p.turn, Some(TurnEdge::Open("21".into())));
        let [LogRecord::Turn(open)] = p.records.as_slice() else {
            panic!("{:?}", p.records)
        };
        assert_eq!(open.prompt_id, "21");
        assert_eq!(p.session.as_deref(), Some("c8cb"));
        assert_eq!(
            p.cwd.as_deref(),
            Some(Path::new("/w")),
            "the project, not .agents/"
        );

        let later = translate(&envelope(
            "PreInvocation",
            r#","invocationNum":1,"initialNumSteps":24"#,
        ));
        assert_eq!(later.turn, None, "later model calls are inside it");
        assert!(later.records.is_empty());
    }

    #[test]
    fn stopping_closes_the_turn() {
        let p = translate(&envelope(
            "Stop",
            r#","executionNum":0,"terminationReason":"NO_TOOL_CALL","error":"","fullyIdle":true"#,
        ));
        assert_eq!(p.turn, Some(TurnEdge::Close));
    }

    #[test]
    fn a_view_is_a_read_with_its_lines() {
        let p = translate(&tool(
            "PostToolUse",
            "view_file",
            r#"{"AbsolutePath":"/w/src/calc.py","StartLine":1,"EndLine":2}"#,
            23,
            r#","error":"""#,
        ));
        let e = only_file(&p);
        assert_eq!(e.path, Path::new("/w/src/calc.py"));
        assert_eq!(
            e.kind,
            EventKind::Read {
                range: Some(LineRange::new(1, 3))
            }
        );
        assert_eq!(e.dedup_key.as_deref(), Some("23"));
        assert_eq!(p.turn, Some(TurnEdge::Within));
    }

    #[test]
    fn an_edit_spans_what_replaced_the_region_and_never_logs_it() {
        let p = translate(&tool(
            "PostToolUse",
            "replace_file_content",
            r#"{"TargetFile":"/w/src/calc.py","StartLine":5,"EndLine":7,
               "TargetContent":"def sub(a, b):\n    return a - b\n",
               "ReplacementContent":"def sub(a, b):\n    SECRET\n\n\ndef mul(a, b):\n    return a * b\n"}"#,
            4,
            r#","error":"""#,
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
            "PostToolUse",
            "write_to_file",
            r#"{"TargetFile":"/w/src/div.py","CodeContent":"x","Overwrite":true}"#,
            6,
            r#","error":"""#,
        ));
        assert_eq!(only_file(&p).kind, EventKind::Write);
    }

    #[test]
    fn a_command_is_bracketed_by_its_step() {
        let args = r#"{"CommandLine":"sed -i 's/a/b/' notes.txt","Cwd":"/w","IsDaemon":false}"#;
        let pre = translate(&tool("PreToolUse", "run_command", args, 12, ""));
        assert_eq!(pre.observe, Some(Observe::Begin { call: "12".into() }));
        let post = translate(&tool(
            "PostToolUse",
            "run_command",
            args,
            12,
            r#","error":"""#,
        ));
        assert_eq!(
            post.observe,
            Some(Observe::End {
                call: "12".into(),
                turn: None
            })
        );
        assert!(!post.call_ids_derived, "stepIdx is Antigravity's own id");
    }

    #[test]
    fn a_shell_read_is_recorded_against_the_commands_directory() {
        let p = translate(&tool(
            "PostToolUse",
            "run_command",
            r#"{"CommandLine":"cat notes.txt","Cwd":"/w/sub"}"#,
            8,
            r#","error":"""#,
        ));
        assert_eq!(only_file(&p).path, Path::new("/w/sub/notes.txt"));
    }

    #[test]
    fn the_agent_reading_its_own_tool_schema_is_not_a_read() {
        let p = translate(&tool(
            "PostToolUse",
            "view_file",
            r#"{"AbsolutePath":"/home/u/.gemini/antigravity-cli/mcp/thirdeye/flag.json"}"#,
            2,
            r#","error":"""#,
        ));
        assert!(p.records.is_empty());
    }

    #[test]
    fn a_call_that_failed_records_nothing() {
        let p = translate(&tool(
            "PostToolUse",
            "view_file",
            r#"{"AbsolutePath":"/w/missing.txt"}"#,
            18,
            r#","error":"no such file""#,
        ));
        assert!(p.records.is_empty());
    }

    #[test]
    fn an_unknown_tool_is_watched() {
        let p = translate(&tool(
            "PreToolUse",
            "multi_replace_file_content",
            "{}",
            30,
            "",
        ));
        assert_eq!(p.observe, Some(Observe::Begin { call: "30".into() }));
    }
}
