//! GitHub Copilot CLI hook payloads → log records.
//!
//! Shapes are captured, not assumed — see `docs/copilot-hook-payload.md`.
//!
//! Copilot's payloads name neither their event, nor the turn, nor the call.
//! The hook wraps each payload in the envelope OpenCode's and pi's plugins
//! use, `{ "hook": <event>, "event": <payload> }`, with the event its
//! command line was registered for. Turns are kept by the hook from the edges
//! marked here ([`TurnEdge`]); calls are keyed by what they were asked to do.

use std::path::{Path, PathBuf};

use serde_json::Value;
use thirdeye_core::{
    AgentId, EventKind, FileEvent, LogRecord, Source, Timestamp, TurnKind, TurnMarker,
};

use crate::{AdapterError, Lifecycle, Observe, Payload, TurnEdge};

pub fn records_from_payload(raw: &str, now: Timestamp) -> Result<Payload, AdapterError> {
    let v: Value = serde_json::from_str(raw)?;
    let event = &v["event"];
    let session = event["sessionId"].as_str().map(str::to_owned);
    let cwd = event["cwd"].as_str().map(PathBuf::from);
    let mut out = Payload {
        session: session.clone(),
        cwd: cwd.clone(),
        call_ids_derived: true,
        ..Payload::default()
    };

    let tool = event["toolName"].as_str().unwrap_or_default();
    let args = &event["toolArgs"];
    match v["hook"].as_str().unwrap_or_default() {
        "userPromptSubmitted" => {
            // The prompt's own time names its turn: unique within a session,
            // which is all a turn id has to be.
            let id = event["timestamp"].as_u64().unwrap_or(now.0).to_string();
            let prompt = event["prompt"].as_str().unwrap_or_default().to_string();
            let mut open = TurnMarker::new(now, id.clone(), TurnKind::Open { prompt });
            open.session = session;
            out.records.push(LogRecord::Turn(open));
            out.turn = Some(TurnEdge::Open(id));
        }
        "agentStop" => out.turn = Some(TurnEdge::Close),
        "sessionStart" => out.lifecycle = Some(Lifecycle::Start),
        "sessionEnd" => out.lifecycle = Some(Lifecycle::End),
        "preToolUse" => {
            out.turn = Some(TurnEdge::Within);
            if bracketed(tool, args) {
                out.observe = Some(Observe::Begin {
                    call: derived_call(tool, args),
                });
            }
        }
        // A failure — a `view` of a missing file — read and changed nothing,
        // but a bracket it opened must still close.
        hook @ ("postToolUse" | "postToolUseFailure") => {
            out.turn = Some(TurnEdge::Within);
            if bracketed(tool, args) {
                if hook == "postToolUse"
                    && tool == "bash"
                    && let Some(command) = args["command"].as_str()
                {
                    out.records = crate::shell::read_records(
                        command,
                        AgentId::Copilot,
                        session.as_deref(),
                        None,
                        None,
                        now,
                        |p| crate::shell::under(cwd.as_deref(), p),
                    );
                }
                out.observe = Some(Observe::End {
                    call: derived_call(tool, args),
                    turn: None,
                });
            } else if hook == "postToolUse" {
                for (path, kind) in translated(tool, args, cwd.as_deref()) {
                    let mut e = FileEvent::new(now, AgentId::Copilot, path, kind, Source::Hook);
                    e.session = session.clone();
                    out.records.push(LogRecord::File(e));
                }
            }
        }
        _ => {}
    }
    Ok(out)
}

/// Whether a call is watched across rather than read.
///
/// Copilot's tool set follows the model, and only one model's could be
/// captured. A tool this adapter does not know is bracketed, so a model that
/// edits through `edit` or `create` is still seen — as writes without a
/// range — rather than not at all.
fn bracketed(tool: &str, args: &Value) -> bool {
    match tool {
        "view" | "rg" | "grep" | "glob" => false,
        // The patch text is what makes it readable; any other shape is a
        // tool this adapter has not seen.
        "apply_patch" => !args.is_string(),
        _ => true,
    }
}

/// The key a call's two edges share, in the absence of an id.
///
/// The edges are sequential — Copilot's hooks are synchronous — so only two
/// identical calls in parallel could share a window. FNV-1a, as everything
/// persisted here: the key names a marker file.
fn derived_call(tool: &str, args: &Value) -> String {
    let text = format!("{tool}\0{args}");
    format!(
        "{tool}-{:016x}",
        thirdeye_core::location::fnv1a64(text.as_bytes())
    )
}

/// What a structured tool did, by path.
fn translated(tool: &str, args: &Value, cwd: Option<&Path>) -> Vec<(PathBuf, EventKind)> {
    let at = |p: &str| absolutize(Path::new(p), cwd);
    match tool {
        "view" => args["path"]
            .as_str()
            .map(|p| vec![(at(p), EventKind::Read { range: None })])
            .unwrap_or_default(),
        "rg" | "grep" | "glob" => {
            let pattern = args["pattern"].as_str().unwrap_or_default().to_string();
            // A search without a path covers the working directory.
            let path = match (args["path"].as_str(), cwd) {
                (Some(p), _) => at(p),
                (None, Some(cwd)) => cwd.to_path_buf(),
                (None, None) => return Vec::new(),
            };
            vec![(path, EventKind::Search { pattern })]
        }
        "apply_patch" => args
            .as_str()
            .map(|patch| patch_files(patch, cwd))
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// The files an `apply_patch` patch touches.
///
/// Its hunks are anchored by context (`@@` with no line numbers), so an
/// update yields an edit without a range rather than an invented one.
fn patch_files(patch: &str, cwd: Option<&Path>) -> Vec<(PathBuf, EventKind)> {
    let at = |p: &str| absolutize(Path::new(p.trim()), cwd);
    let mut out = Vec::new();
    for line in patch.lines() {
        if let Some(p) = line.strip_prefix("*** Update File: ") {
            out.push((at(p), EventKind::Edit { range: None }));
        } else if let Some(p) = line.strip_prefix("*** Add File: ") {
            out.push((at(p), EventKind::Write));
        } else if let Some(p) = line.strip_prefix("*** Delete File: ") {
            out.push((at(p), EventKind::Delete));
        } else if let Some(p) = line.strip_prefix("*** Move to: ") {
            // The update just pushed was of the file's old name, which no
            // longer exists; the content now lives at the new one.
            if let Some(last) = out.last_mut()
                && matches!(last.1, EventKind::Edit { .. })
            {
                last.1 = EventKind::Delete;
                out.push((at(p), EventKind::Write));
            }
        }
    }
    out
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

    // Payloads from docs/copilot-hook-payload.md, inside the envelope the
    // hook wraps them in.
    fn envelope(hook: &str, event: &str) -> String {
        format!(r#"{{"hook":"{hook}","event":{event}}}"#)
    }

    fn tool(hook: &str, name: &str, args: &str, extra: &str) -> String {
        envelope(
            hook,
            &format!(
                r#"{{"sessionId":"473fb94c","timestamp":1791541891000,"cwd":"/w",
                    "toolName":"{name}","toolArgs":{args}{extra}}}"#
            ),
        )
    }

    fn translate(raw: &str) -> Payload {
        records_from_payload(raw, Timestamp(1)).unwrap()
    }

    fn files(payload: &Payload) -> Vec<(PathBuf, EventKind)> {
        payload
            .records
            .iter()
            .map(|r| match r {
                LogRecord::File(e) => (e.path.clone(), e.kind.clone()),
                other => panic!("expected file events, got {other:?}"),
            })
            .collect()
    }

    const VIEW_RESULT: &str = r#","toolResult":{"resultType":"success",
        "textResultForLlm":"SECRET-BODY def add(a, b)"}"#;

    #[test]
    fn a_prompt_opens_a_turn_named_by_its_time() {
        let p = translate(&envelope(
            "userPromptSubmitted",
            r#"{"sessionId":"s1","timestamp":1791541891065,"cwd":"/w","prompt":"edit it"}"#,
        ));
        let [LogRecord::Turn(open)] = p.records.as_slice() else {
            panic!("expected a turn marker, got {:?}", p.records);
        };
        assert_eq!(open.prompt_id, "1791541891065");
        assert_eq!(
            open.kind,
            TurnKind::Open {
                prompt: "edit it".into()
            }
        );
        assert_eq!(p.turn, Some(TurnEdge::Open("1791541891065".into())));
    }

    #[test]
    fn the_agent_stopping_closes_the_turn_it_is_in() {
        let p = translate(&envelope(
            "agentStop",
            r#"{"sessionId":"s1","timestamp":2,"cwd":"/w","stopReason":"end_turn"}"#,
        ));
        assert_eq!(p.turn, Some(TurnEdge::Close));
        assert!(p.records.is_empty(), "the hook writes the marker");
    }

    #[test]
    fn the_session_ending_ends_the_session() {
        let p = translate(&envelope(
            "sessionEnd",
            r#"{"sessionId":"s1","timestamp":2,"cwd":"/w","reason":"complete"}"#,
        ));
        assert_eq!(p.lifecycle, Some(Lifecycle::End));
        assert_eq!(p.session.as_deref(), Some("s1"));
    }

    #[test]
    fn a_view_is_a_read_and_its_content_never_reaches_the_log() {
        let p = translate(&tool(
            "postToolUse",
            "view",
            r#"{"path":"/w/src/calc.py"}"#,
            VIEW_RESULT,
        ));
        assert_eq!(
            files(&p),
            vec![("/w/src/calc.py".into(), EventKind::Read { range: None })]
        );
        assert_eq!(p.turn, Some(TurnEdge::Within));
        let LogRecord::File(e) = &p.records[0] else {
            unreachable!()
        };
        assert!(!serde_json::to_string(e).unwrap().contains("SECRET-BODY"));
    }

    #[test]
    fn a_failed_view_records_nothing() {
        let p = translate(&tool(
            "postToolUseFailure",
            "view",
            r#"{"path":"/w/missing.txt"}"#,
            r#","error":"Path does not exist""#,
        ));
        assert!(p.records.is_empty());
        assert_eq!(p.observe, None);
    }

    #[test]
    fn a_patch_reports_what_it_updated_added_deleted_and_moved() {
        let patch = "*** Begin Patch\n*** Update File: src/calc.py\n@@\n def sub(a, b):\n+def mul(a, b):\n\
                     *** Add File: src/div.py\n+def div(a, b):\n*** Delete File: old.py\n\
                     *** Update File: a.py\n*** Move to: b.py\n@@\n-x\n+y\n*** End Patch\n";
        let args = serde_json::to_string(patch).unwrap();
        let p = translate(&tool("postToolUse", "apply_patch", &args, ""));
        assert_eq!(
            files(&p),
            vec![
                ("/w/src/calc.py".into(), EventKind::Edit { range: None }),
                ("/w/src/div.py".into(), EventKind::Write),
                ("/w/old.py".into(), EventKind::Delete),
                ("/w/a.py".into(), EventKind::Delete),
                ("/w/b.py".into(), EventKind::Write),
            ]
        );
        assert_eq!(p.observe, None, "a patch is read, not watched");
    }

    #[test]
    fn a_search_is_recorded_against_where_it_looked() {
        let p = translate(&tool(
            "postToolUse",
            "rg",
            r#"{"pattern":"return","output_mode":"content"}"#,
            "",
        ));
        assert_eq!(
            files(&p),
            vec![(
                "/w".into(),
                EventKind::Search {
                    pattern: "return".into()
                }
            )]
        );
        let p = translate(&tool(
            "postToolUse",
            "glob",
            r#"{"pattern":"**/*.py","path":"src"}"#,
            "",
        ));
        assert_eq!(files(&p)[0].0, PathBuf::from("/w/src"));
    }

    #[test]
    fn a_shell_call_is_bracketed_under_a_key_both_edges_agree_on() {
        let args = r#"{"command":"sed -i 's/world/there/' notes.txt","description":"Replace"}"#;
        let pre = translate(&tool("preToolUse", "bash", args, ""));
        let post = translate(&tool(
            "postToolUse",
            "bash",
            args,
            r#","toolResult":{"resultType":"success","textResultForLlm":"exit code 0"}"#,
        ));
        let Some(Observe::Begin { call: begun }) = pre.observe else {
            panic!("no bracket opened: {:?}", pre.observe);
        };
        let Some(Observe::End { call: ended, .. }) = post.observe else {
            panic!("no bracket closed: {:?}", post.observe);
        };
        assert_eq!(begun, ended);
        assert!(post.call_ids_derived);

        let other = translate(&tool("preToolUse", "bash", r#"{"command":"rm x"}"#, ""));
        let Some(Observe::Begin { call }) = other.observe else {
            unreachable!()
        };
        assert_ne!(call, begun, "different commands, different windows");
    }

    #[test]
    fn a_shell_read_is_recorded_too() {
        let p = translate(&tool(
            "postToolUse",
            "bash",
            r#"{"command":"cat notes.txt"}"#,
            "",
        ));
        assert_eq!(
            files(&p),
            vec![("/w/notes.txt".into(), EventKind::Read { range: None })]
        );
    }

    #[test]
    fn a_tool_this_adapter_does_not_know_is_watched() {
        // Another model's `edit`, say: whatever it wrote is still seen.
        let p = translate(&tool(
            "preToolUse",
            "edit",
            r#"{"path":"/w/a.py","old_str":"x","new_str":"y"}"#,
            "",
        ));
        assert!(matches!(p.observe, Some(Observe::Begin { .. })));
        let p = translate(&tool(
            "postToolUseFailure",
            "edit",
            r#"{"path":"/w/a.py","old_str":"x","new_str":"y"}"#,
            r#","error":"no match""#,
        ));
        assert!(
            matches!(p.observe, Some(Observe::End { .. })),
            "a failure still closes its window"
        );
    }
}
