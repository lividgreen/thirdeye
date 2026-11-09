//! Claude Code hook payload → log records.
//!
//! Payload shapes are verified, not assumed — see `docs/hook-payload.md`.

use std::path::{Path, PathBuf};

use serde_json::Value;
use thirdeye_core::{
    AgentId, EventKind, FileEvent, LineRange, LogRecord, Source, Timestamp, TurnKind, TurnMarker,
};

use crate::{AdapterError, Lifecycle, Observe, Payload};

/// Translate one hook payload. Unrecognized events yield no records rather
/// than an error: Claude Code may add hook events and tools at any time, and a
/// hook that fails on an unknown shape would break the user's agent.
pub fn records_from_payload(raw: &str, now: Timestamp) -> Result<Payload, AdapterError> {
    let v: Value = serde_json::from_str(raw)?;

    let session = v["session_id"].as_str().map(str::to_owned);
    let cwd = v["cwd"].as_str().map(PathBuf::from);
    let prompt_id = v["prompt_id"].as_str().map(str::to_owned);
    let mut out = Payload {
        session: session.clone(),
        cwd: cwd.clone(),
        ..Payload::default()
    };

    let event = v["hook_event_name"].as_str().unwrap_or_default();
    // Bash is bracketed, not translated: the command text cannot say what it
    // wrote, so the hook watches the workspace across the call instead.
    // Structured tools stay translated — bracketing them as well would report
    // every edit twice.
    let call = v["tool_use_id"].as_str().map(str::to_owned);
    let is_shell = v["tool_name"].as_str() == Some("Bash");
    match event {
        "PreToolUse" if is_shell => out.observe = call.map(|call| Observe::Begin { call }),
        // A command exiting non-zero fires PostToolUseFailure instead, and
        // `sed -i ...; false` has still written. Its `error` carries the
        // command's output and is never read.
        "PostToolUse" | "PostToolUseFailure" if is_shell => {
            // A failed command read nothing worth listing — `cat
            // missing.rs` — though its window still closes below.
            if event == "PostToolUse"
                && let Some(command) = v["tool_input"]["command"].as_str()
            {
                out.records = crate::shell::read_records(
                    command,
                    AgentId::Claude,
                    session.as_deref(),
                    prompt_id.as_deref(),
                    call.as_deref(),
                    now,
                    |p| crate::shell::under(cwd.as_deref(), p),
                );
            }
            out.observe = call.map(|call| Observe::End {
                call,
                turn: prompt_id,
            });
        }
        "PostToolUse" => {
            if let Some(kind_and_path) = tool_to_event(&v) {
                let (path, kind) = kind_and_path;
                let mut ev = FileEvent::new(
                    now,
                    AgentId::Claude,
                    absolutize(&path, cwd.as_deref()),
                    kind,
                    Source::Hook,
                );
                ev.session = session;
                ev.prompt_id = prompt_id;
                ev.dedup_key = v["tool_use_id"].as_str().map(str::to_owned);
                out.records.push(LogRecord::File(ev));
            }
        }
        "UserPromptSubmit" => {
            let Some(prompt_id) = prompt_id else {
                return Ok(out);
            };
            let prompt = v["prompt"].as_str().unwrap_or_default().to_string();
            let mut open = TurnMarker::new(
                now,
                prompt_id.clone(),
                TurnKind::Open {
                    prompt: prompt.clone(),
                },
            );
            open.session = session.clone();
            out.records.push(LogRecord::Turn(open));
            for mention in extract_mentions(&prompt) {
                let mut ev = FileEvent::new(
                    now,
                    AgentId::Claude,
                    absolutize(Path::new(&mention), cwd.as_deref()),
                    EventKind::Mention,
                    Source::Hook,
                );
                ev.session = session.clone();
                ev.prompt_id = Some(prompt_id.clone());
                out.records.push(LogRecord::File(ev));
            }
        }
        "SessionStart" => out.lifecycle = Some(Lifecycle::Start),
        "SessionEnd" => out.lifecycle = Some(Lifecycle::End),
        "Stop" => {
            if let Some(prompt_id) = prompt_id {
                let mut close = TurnMarker::new(now, prompt_id, TurnKind::Close);
                close.session = session;
                out.records.push(LogRecord::Turn(close));
            }
        }
        _ => {}
    }

    Ok(out)
}

/// Map a tool call to (path, kind). `None` for tools with no single file
/// subject — `Bash` path extraction is fuzzy and out of PoC scope.
fn tool_to_event(v: &Value) -> Option<(PathBuf, EventKind)> {
    let tool = v["tool_name"].as_str()?;
    let input = &v["tool_input"];
    let file_path = |key: &str| input[key].as_str().map(PathBuf::from);

    match tool {
        "Read" => Some((
            file_path("file_path")?,
            EventKind::Read {
                range: read_range(v),
            },
        )),
        "Edit" | "MultiEdit" => Some((
            file_path("file_path")?,
            EventKind::Edit {
                range: edit_range(v),
            },
        )),
        "NotebookEdit" => Some((file_path("notebook_path")?, EventKind::Edit { range: None })),
        "Write" => Some((file_path("file_path")?, EventKind::Write)),
        "Grep" | "Glob" => {
            let pattern = input["pattern"].as_str().unwrap_or_default().to_string();
            // A pathless search covers the working directory. Defaulting to
            // "." instead would absolutize to `<cwd>/.` — a second entry for
            // a directory already tracked, rendering as a bare "." in `ls`.
            let path = file_path("path").unwrap_or_default();
            Some((path, EventKind::Search { pattern }))
        }
        _ => None,
    }
}

/// The touched range for `Read`, straight from `tool_response.file`. Correct
/// even when the agent read a whole file, unlike reconstructing from
/// `tool_input`'s offset/limit. `Read` is the only tool with this shape —
/// see `edit_range` for `Edit`/`MultiEdit`, which carry no `file` object at
/// all.
fn read_range(v: &Value) -> Option<LineRange> {
    let file = &v["tool_response"]["file"];
    let start = file["startLine"].as_u64()? as u32;
    let lines = file["numLines"].as_u64()? as u32;
    Some(LineRange::new(start, start + lines))
}

/// The touched range for `Edit`/`MultiEdit`, from
/// `tool_response.structuredPatch` — a list of hunks, one per edited region,
/// each carrying old- and new-file coordinates. `Edit`/`MultiEdit` never
/// carry a `tool_response.file` object, so `read_range` always returns
/// `None` for them; this reads the hunk list instead.
///
/// The range spans every hunk using *new*-file coordinates (post-edit line
/// numbers, which is what the log should point a human at): from the lowest
/// `newStart` to the highest `newStart + newLines`. An empty or malformed
/// `structuredPatch` yields `None` rather than a bogus range.
fn edit_range(v: &Value) -> Option<LineRange> {
    let hunks = v["tool_response"]["structuredPatch"].as_array()?;
    let mut spans = hunks.iter().filter_map(|hunk| {
        let start = hunk["newStart"].as_u64()? as u32;
        let lines = hunk["newLines"].as_u64()? as u32;
        Some((start, start + lines))
    });
    let (mut min_start, mut max_end) = spans.next()?;
    for (start, end) in spans {
        min_start = min_start.min(start);
        max_end = max_end.max(end);
    }
    Some(LineRange::new(min_start, max_end))
}

fn absolutize(path: &Path, cwd: Option<&Path>) -> PathBuf {
    let Some(cwd) = cwd.filter(|_| !path.is_absolute()) else {
        return path.to_path_buf();
    };
    // "" and "." both *mean* the working directory; joining them would spell
    // it as `<cwd>/` or `<cwd>/.`, which tracks and renders as a distinct
    // entry from `<cwd>` itself.
    if path.as_os_str().is_empty() || path == Path::new(".") {
        return cwd.to_path_buf();
    }
    cwd.join(path)
}

/// Longest `@`-token still treated as a file reference.
///
/// Comfortably past `PATH_MAX`-shaped real paths, but far below the line cap
/// in [`thirdeye_core::log`]. Mentions are split on whitespace alone, so a
/// prompt containing `@` followed by a paragraph would otherwise become one
/// enormous "path" and push the record over that cap.
const MAX_MENTION_BYTES: usize = 512;

/// `@path` references in a prompt. Trailing sentence punctuation is stripped
/// so "review @src/log.rs." does not yield a path ending in a dot. Tokens
/// past [`MAX_MENTION_BYTES`] are dropped rather than truncated: a clipped
/// path is a *wrong* path, which is worse than no path at all.
fn extract_mentions(prompt: &str) -> Vec<String> {
    prompt
        .split_whitespace()
        .filter_map(|word| word.strip_prefix('@'))
        .map(|w| {
            w.trim_end_matches(['.', ',', ';', ':', ')', ']'])
                .to_string()
        })
        .filter(|w| !w.is_empty() && w.len() <= MAX_MENTION_BYTES)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use thirdeye_core::log::MAX_LINE_BYTES;

    const NOW: Timestamp = Timestamp(1_700_000_000_000);

    /// Distinctive stand-in for the file body Claude Code embeds in
    /// `tool_response`. Asserting on the *value* catches a leak that copied
    /// the content into a differently-named field; asserting on the field
    /// name would not.
    const CONTENT_SENTINEL: &str = "zzQUUX-tool-response-body-must-not-be-logged-QUUXzz";

    /// Verbatim shape from docs/hook-payload.md.
    const READ_PAYLOAD: &str = r#"{
      "session_id": "328abcfc-2bd5-4aa9-8aa8-07d14569d550",
      "transcript_path": "/home/u/.claude/projects/x/y.jsonl",
      "cwd": "/home/u/app",
      "prompt_id": "838748f2-65af-4e75-948b-5629215d63b9",
      "permission_mode": "default",
      "hook_event_name": "PostToolUse",
      "tool_name": "Read",
      "tool_input": { "file_path": "/home/u/app/src/log.rs" },
      "tool_response": { "type": "text", "file": {
          "filePath": "/home/u/app/src/log.rs",
          "content": "…", "numLines": 8, "startLine": 1, "totalLines": 8 } },
      "tool_use_id": "toolu_011NdbU3kC4B29Gh2uqYTvJJ",
      "duration_ms": 1
    }"#;

    #[test]
    fn read_payload_becomes_a_ranged_read_event() {
        let out = records_from_payload(READ_PAYLOAD, NOW).unwrap();

        assert_eq!(
            out.session.as_deref(),
            Some("328abcfc-2bd5-4aa9-8aa8-07d14569d550")
        );
        assert_eq!(out.cwd.as_deref(), Some(Path::new("/home/u/app")));
        assert_eq!(out.records.len(), 1);

        let ev = out.records[0].as_file().unwrap();
        assert_eq!(ev.path, Path::new("/home/u/app/src/log.rs"));
        assert_eq!(ev.source, Source::Hook);
        assert_eq!(ev.agent, AgentId::Claude);
        assert_eq!(
            ev.dedup_key.as_deref(),
            Some("toolu_011NdbU3kC4B29Gh2uqYTvJJ")
        );
        assert_eq!(
            ev.prompt_id.as_deref(),
            Some("838748f2-65af-4e75-948b-5629215d63b9")
        );
        assert_eq!(
            ev.kind,
            EventKind::Read {
                range: Some(LineRange::new(1, 9))
            },
            "range must come from tool_response startLine/numLines"
        );
    }

    #[test]
    fn tool_response_content_is_never_retained() {
        // The payload embeds the whole file; the log must not become a second
        // copy of the user's source tree. The `…` placeholder in the recorded
        // payload stands in for that body, so planting the sentinel there
        // reproduces the real shape.
        let raw = READ_PAYLOAD.replace('…', CONTENT_SENTINEL);
        assert!(raw.contains(CONTENT_SENTINEL), "sentinel was not planted");

        let out = records_from_payload(&raw, NOW).unwrap();
        let line = serde_json::to_string(&out.records[0]).unwrap();
        assert!(
            !line.contains(CONTENT_SENTINEL),
            "leaked the tool_response body into the log: {line}"
        );
        assert!(
            !line.contains("content"),
            "leaked the tool_response content field: {line}"
        );
        assert!(
            line.len() < MAX_LINE_BYTES,
            "line must stay within MAX_LINE_BYTES once the newline is added"
        );
    }

    /// `tool_response` is optional because most of these tests don't need
    /// one — but the `Edit`/`MultiEdit` range tests below do, to exercise
    /// the real `structuredPatch` shape rather than the absence of a field.
    fn tool_payload(tool: &str, input: &str, tool_response: Option<&str>) -> String {
        let response_field = tool_response
            .map(|r| format!(r#","tool_response":{r}"#))
            .unwrap_or_default();
        format!(
            r#"{{"session_id":"s1","cwd":"/w","prompt_id":"p1",
                 "hook_event_name":"PostToolUse","tool_name":"{tool}",
                 "tool_input":{input}{response_field},"tool_use_id":"t1"}}"#
        )
    }

    #[test]
    fn mutating_tools_map_to_mutating_kinds() {
        for (tool, input) in [
            ("Edit", r#"{"file_path":"/w/a.rs"}"#),
            ("Write", r#"{"file_path":"/w/a.rs"}"#),
            ("NotebookEdit", r#"{"notebook_path":"/w/a.ipynb"}"#),
        ] {
            let out = records_from_payload(&tool_payload(tool, input, None), NOW).unwrap();
            let ev = out.records[0].as_file().unwrap();
            assert!(
                matches!(ev.kind, EventKind::Edit { .. } | EventKind::Write),
                "{tool} produced {:?}",
                ev.kind
            );
        }
    }

    #[test]
    fn search_tools_record_their_pattern_and_path() {
        let out = records_from_payload(
            &tool_payload("Grep", r#"{"pattern":"fn dedup","path":"/w/src"}"#, None),
            NOW,
        )
        .unwrap();
        let ev = out.records[0].as_file().unwrap();
        assert_eq!(ev.path, Path::new("/w/src"));
        assert_eq!(
            ev.kind,
            EventKind::Search {
                pattern: "fn dedup".into()
            }
        );
    }

    #[test]
    fn edit_range_comes_from_structured_patch_new_file_coordinates() {
        // Verified live shape: Edit's tool_response has no `file` object at
        // all — only filePath/newString/oldString/originalFile/replaceAll/
        // structuredPatch/userModified. The range must come from the hunk's
        // *new*-file coordinates (newStart/newLines), not from the `file`
        // shape that only `Read` produces.
        let tool_response = r#"{
            "filePath": "/w/a.rs", "newString": "x", "oldString": "y",
            "originalFile": "z", "replaceAll": false, "userModified": false,
            "structuredPatch": [
              { "oldStart": 1, "oldLines": 5, "newStart": 1, "newLines": 5,
                "lines": [" line one", " line two", "-line three", "+line THREE", " line four", " line five"] }
            ]
        }"#;
        let out = records_from_payload(
            &tool_payload("Edit", r#"{"file_path":"/w/a.rs"}"#, Some(tool_response)),
            NOW,
        )
        .unwrap();
        let ev = out.records[0].as_file().unwrap();
        assert_eq!(
            ev.kind,
            EventKind::Edit {
                range: Some(LineRange::new(1, 6))
            },
            "range must come from structuredPatch newStart/newLines"
        );
    }

    #[test]
    fn multi_edit_range_spans_from_the_first_hunk_to_the_last() {
        let tool_response = r#"{
            "structuredPatch": [
              { "oldStart": 3, "oldLines": 2, "newStart": 3, "newLines": 2, "lines": [] },
              { "oldStart": 38, "oldLines": 6, "newStart": 40, "newLines": 6, "lines": [] }
            ]
        }"#;
        let out = records_from_payload(
            &tool_payload(
                "MultiEdit",
                r#"{"file_path":"/w/a.rs"}"#,
                Some(tool_response),
            ),
            NOW,
        )
        .unwrap();
        let ev = out.records[0].as_file().unwrap();
        assert_eq!(
            ev.kind,
            EventKind::Edit {
                range: Some(LineRange::new(3, 46))
            },
            "range must span from the lowest newStart to the highest newStart + newLines"
        );
    }

    #[test]
    fn empty_structured_patch_yields_no_range_rather_than_panicking() {
        let tool_response = r#"{"structuredPatch": []}"#;
        let out = records_from_payload(
            &tool_payload("Edit", r#"{"file_path":"/w/a.rs"}"#, Some(tool_response)),
            NOW,
        )
        .unwrap();
        let ev = out.records[0].as_file().unwrap();
        assert_eq!(ev.kind, EventKind::Edit { range: None });
    }

    #[test]
    fn prompt_submission_opens_a_turn_and_extracts_mentions() {
        let raw = r#"{"session_id":"s1","cwd":"/w","prompt_id":"p1",
          "hook_event_name":"UserPromptSubmit",
          "prompt":"please review @src/log.rs and @docs/notes.md"}"#;
        let out = records_from_payload(raw, NOW).unwrap();

        let turn = match &out.records[0] {
            LogRecord::Turn(t) => t,
            other => panic!("expected a turn marker, got {other:?}"),
        };
        assert_eq!(turn.prompt_id, "p1");
        assert!(matches!(turn.kind, TurnKind::Open { .. }));

        let mentions: Vec<_> = out
            .records
            .iter()
            .filter_map(LogRecord::as_file)
            .filter(|e| e.kind == EventKind::Mention)
            .map(|e| e.path.clone())
            .collect();
        assert_eq!(
            mentions,
            vec![Path::new("/w/src/log.rs"), Path::new("/w/docs/notes.md")],
            "mentions must be resolved against cwd"
        );
    }

    #[test]
    fn an_overlong_at_token_is_not_treated_as_a_mention() {
        // Mentions split on whitespace alone, so "@" followed by a paragraph
        // would otherwise become one enormous path and push the record past
        // the log's line cap.
        let long = "a".repeat(MAX_MENTION_BYTES + 1);
        let raw = format!(
            r#"{{"session_id":"s1","cwd":"/w","prompt_id":"p1",
              "hook_event_name":"UserPromptSubmit",
              "prompt":"look at @{long} and @src/log.rs"}}"#
        );
        let out = records_from_payload(&raw, NOW).unwrap();

        let mentions: Vec<_> = out
            .records
            .iter()
            .filter_map(LogRecord::as_file)
            .map(|e| e.path.clone())
            .collect();
        assert_eq!(
            mentions,
            vec![Path::new("/w/src/log.rs")],
            "the real reference must survive while the overlong token is dropped"
        );
    }

    #[test]
    fn a_pathless_search_is_attributed_to_the_working_directory_itself() {
        // `<cwd>/.` would track as a second entry for a directory already
        // tracked, and render as a bare "." in `thirdeye ls`.
        let out =
            records_from_payload(&tool_payload("Glob", r#"{"pattern":"**/*.rs"}"#, None), NOW)
                .unwrap();
        let ev = out.records[0].as_file().unwrap();
        assert_eq!(ev.path, Path::new("/w"));
    }

    #[test]
    fn stop_closes_the_turn() {
        let raw = r#"{"session_id":"s1","cwd":"/w","prompt_id":"p1","hook_event_name":"Stop"}"#;
        let out = records_from_payload(raw, NOW).unwrap();
        assert_eq!(out.records.len(), 1);
        assert!(matches!(
            &out.records[0],
            LogRecord::Turn(TurnMarker {
                kind: TurnKind::Close,
                ..
            })
        ));
    }

    #[test]
    fn unhandled_events_and_pathless_tools_produce_nothing() {
        // Bash path extraction is fuzzy and deliberately out of PoC scope;
        // producing nothing is correct, erroring is not.
        //
        // NOTE: the three payloads are bound to locals before building the
        // array. The brief's original snippet mixed `&tool_payload(...)`
        // (a `&String` tied to a temporary) with
        // `r#"..."#.to_string().as_str()` (a borrow of a temporary dropped
        // at the end of that statement) — that does not compile (E0716 plus
        // a type mismatch between `&String` and `&str`). Binding every
        // payload to a named `String` first gives all three entries the
        // same type and a lifetime that outlives the loop.
        let bash = tool_payload("Bash", r#"{"command":"ls -la /w"}"#, None);
        let web_fetch = tool_payload("WebFetch", r#"{"url":"https://example.com"}"#, None);
        let session_start =
            r#"{"session_id":"s1","hook_event_name":"SessionStart","cwd":"/w"}"#.to_string();

        for raw in [&bash, &web_fetch, &session_start] {
            let out = records_from_payload(raw, NOW).unwrap();
            assert!(out.records.is_empty(), "expected no records from {raw}");
        }
    }

    #[test]
    fn malformed_json_is_an_error_not_a_panic() {
        assert!(records_from_payload("{not json", NOW).is_err());
    }

    fn bash(event: &str) -> String {
        format!(
            r#"{{"session_id":"s1","cwd":"/w","prompt_id":"p1","hook_event_name":"{event}",
               "tool_name":"Bash","tool_input":{{"command":"sed -i s/a/b/ data.txt"}},
               "tool_response":{{"stdout":"SECRET-OUTPUT","stderr":""}},
               "tool_use_id":"toolu_1"}}"#
        )
    }

    #[test]
    fn a_shell_call_is_bracketed() {
        // Claude edits through Bash too (sed -i, redirects, scripts), and the
        // command text cannot say what it wrote. The hook watches instead.
        let begin = records_from_payload(&bash("PreToolUse"), NOW).unwrap();
        assert_eq!(
            begin.observe,
            Some(Observe::Begin {
                call: "toolu_1".into()
            })
        );
        let end = records_from_payload(&bash("PostToolUse"), NOW).unwrap();
        assert_eq!(
            end.observe,
            Some(Observe::End {
                call: "toolu_1".into(),
                turn: Some("p1".into()),
            })
        );
        assert!(end.records.is_empty());
    }

    #[test]
    fn structured_tools_are_not_bracketed() {
        // Bracketing an Edit as well would report every Claude edit twice.
        let raw = r#"{"session_id":"s1","cwd":"/w","prompt_id":"p1","hook_event_name":"PostToolUse",
                      "tool_name":"Edit","tool_input":{"file_path":"/w/a.rs"},"tool_use_id":"toolu_2"}"#;
        let payload = records_from_payload(raw, NOW).unwrap();
        assert_eq!(payload.observe, None);
        assert_eq!(payload.records.len(), 1);
    }

    #[test]
    fn a_shell_calls_output_never_reaches_the_log() {
        let payload = records_from_payload(&bash("PostToolUse"), NOW).unwrap();
        for record in &payload.records {
            let line = serde_json::to_string(record).unwrap();
            assert!(!line.contains("SECRET-OUTPUT"), "{line}");
        }
    }

    #[test]
    fn a_shell_call_that_fails_still_closes_its_window() {
        // A command that exits non-zero fires PostToolUseFailure instead,
        // and `sed -i ...; false` has still written. Shape from the hooks
        // reference; `error` carries the command's output and is never read.
        let raw = r#"{"session_id":"s1","cwd":"/w","prompt_id":"p1",
            "hook_event_name":"PostToolUseFailure","tool_name":"Bash",
            "tool_input":{"command":"sed -i s/a/b/ f; false"},"tool_use_id":"toolu_9",
            "error":"Exit code 1\nSECRET-OUTPUT","is_interrupt":false}"#;
        let payload = records_from_payload(raw, NOW).unwrap();
        assert_eq!(
            payload.observe,
            Some(Observe::End {
                call: "toolu_9".into(),
                turn: Some("p1".into()),
            })
        );
        assert!(payload.records.is_empty());
    }

    #[test]
    fn a_bash_read_is_recorded_too() {
        let raw = bash("PostToolUse").replace("sed -i s/a/b/ data.txt", "cat notes.md");
        let p = records_from_payload(&raw, NOW).unwrap();
        let [LogRecord::File(e)] = p.records.as_slice() else {
            panic!("{:?}", p.records)
        };
        assert_eq!(e.path, Path::new("/w/notes.md"));
        assert!(matches!(e.kind, EventKind::Read { .. }));
    }

    #[test]
    fn a_failed_command_read_nothing() {
        // `cat missing.rs` exits 1; recording a read would list a file that
        // does not exist. Its window still closes — a failing command can
        // still have written.
        let raw = bash("PostToolUseFailure").replace("sed -i s/a/b/ data.txt", "cat missing.rs");
        let p = records_from_payload(&raw, NOW).unwrap();
        assert!(p.records.is_empty(), "{:?}", p.records);
        assert!(p.observe.is_some());
    }
}
