//! `thirdeye hook` — the binary a Claude Code hook shells out to.
//!
//! Runs on every mutation, so it does the minimum: parse stdin, append,
//! exit. It never touches the terminal; that is the daemon's job.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use thirdeye_adapters::{Lifecycle, Observe, TurnEdge, translate};
use thirdeye_core::{
    AgentId, EventKind, EventPaths, FileEvent, LogRecord, Source, Timestamp, TurnKind, TurnMarker,
    Workspace, Writer,
};

use crate::workspace;

/// Producer name for this adapter's log segment.
const PRODUCER: &str = "hook";

/// Agents that also run hooks written into Claude Code's `settings.json`,
/// and the variable their hook runner sets on every hook it spawns.
///
/// That variable, not the agent's session id: a session id is inherited by
/// anything the agent's shell tool starts, including a Claude Code session,
/// whose own hooks would then go silent.
const CLAUDE_HOOK_BORROWERS: &[(&str, &str)] = &[
    ("GROK_HOOK_EVENT", "Grok Build"),
    // Copilot's shell tool inherits `COPILOT_CLI`; only hooks get this.
    ("COPILOT_PROJECT_DIR", "GitHub Copilot CLI"),
];

/// Which other agent is running a hook configured for `agent`, if one is.
fn borrowed_by(agent: &AgentId, env: impl Fn(&str) -> Option<String>) -> Option<&'static str> {
    if *agent != AgentId::Claude {
        return None;
    }
    CLAUDE_HOOK_BORROWERS
        .iter()
        .find(|(var, _)| env(var).is_some())
        .map(|(_, name)| *name)
}

/// `payload` inside the envelope OpenCode's and pi's plugins send, for an
/// agent whose payloads do not name their event: the hook's command line
/// does, and the adapter reads both as one document.
fn envelope(event: &str, payload: &str) -> Result<String> {
    let payload: serde_json::Value =
        serde_json::from_str(payload).context("parsing hook payload")?;
    Ok(serde_json::json!({ "hook": event, "event": payload }).to_string())
}

pub fn run(agent: &AgentId, event: Option<&str>, mut reader: impl Read) -> Result<usize> {
    // Read either way: the agent writes the payload to our stdin, and a pipe
    // closed under it is an error it reports to the human.
    let mut raw = String::new();
    reader
        .read_to_string(&mut raw)
        .context("reading hook payload from stdin")?;
    // That agent's events reach thirdeye through its own install, under its
    // own name. Grok's and Copilot's payloads repeat enough of Claude's
    // fields for the Claude adapter to accept them: they mark the agent's
    // session as the workspace's active one — the one an unattributed flag
    // joins — and Copilot's searches would be filed as Claude's.
    if borrowed_by(agent, |key| std::env::var(key).ok()).is_some() {
        return Ok(0);
    }
    let raw = match event {
        Some(event) => envelope(event, &raw)?,
        None => raw,
    };
    let appended = run_inner(
        raw.as_bytes(),
        agent,
        |cwd| workspace::resolve(None, cwd),
        EventPaths::discover,
        crate::notify::poke,
        crate::notify::stop,
    );
    // After the append, which must never wait on anything: a binary that a
    // package manager replaced finds thirdeye's directory published by its
    // predecessor, and republishes it. A read of one small file otherwise.
    crate::cmd_install::publish_if_stale();
    appended
}

/// Hermetic root resolution for tests: the shared resolver with the ambient
/// `$CLAUDE_PROJECT_DIR` lookup stubbed out. `workspace::resolve` cannot be
/// used directly because it prefers that variable — routing through it would
/// let whatever the test process happens to inherit silently decide which
/// workspace directory a test reads and writes.
#[cfg(test)]
fn run_with_roots(reader: impl Read, state: &Path, runtime: &Path) -> Result<usize> {
    let (state, runtime) = (state.to_path_buf(), runtime.to_path_buf());
    run_inner(
        reader,
        &AgentId::Claude,
        |cwd| Workspace::new(workspace::resolve_with(None, None, cwd)),
        move |ws| EventPaths::with_roots(state.clone(), runtime.clone(), ws),
        // Never the real ones: they would run `systemctl` against the
        // developer's own user manager for units that are not installed.
        |_| {},
        |_| {},
    )
}

fn run_inner(
    mut reader: impl Read,
    agent: &AgentId,
    resolve_workspace: impl Fn(Option<&Path>) -> Workspace,
    make_paths: impl Fn(Workspace) -> EventPaths,
    notify: impl Fn(&Path),
    teardown: impl Fn(&Path),
) -> Result<usize> {
    let mut raw = String::new();
    reader
        .read_to_string(&mut raw)
        .context("reading hook payload from stdin")?;

    let mut payload = translate(agent, &raw, Timestamp::now())?;

    // A session ending carries no file events, so it must be handled before
    // the empty-records shortcut below.
    if payload.lifecycle == Some(Lifecycle::End)
        && let Some(session) = payload.session.as_deref()
    {
        let paths = make_paths(resolve_workspace(payload.cwd.as_deref()));
        if let Some(socket) = paths.socket_path(session) {
            teardown(&socket);
            crate::observe::clear(&crate::observe::markers_for(&socket));
            let _ = crate::held_turn::close(&crate::held_turn::path_for(&socket));
            // Here rather than in the daemon, which used to do it on its way
            // out: stopping it is SIGTERM, and that way out never runs.
            let _ = std::fs::remove_file(crate::session_ref::path_for(&socket));
        }
        return Ok(0);
    }

    let paths = make_paths(resolve_workspace(payload.cwd.as_deref()));

    // Every payload with a session, records or not: a flag that cannot name
    // its own session (Codex's MCP environment) joins the one that acted
    // last. Best-effort — losing it costs a flag its attribution, nothing
    // more.
    if let Some(session) = payload.session.as_deref() {
        let _ = crate::session_ref::record_active(&paths.workspace_dir(), session);
    }

    // For an agent whose events name no turn: the hook holds the open one.
    // Before the bracket below, whose records must join it too.
    let mut held = None;
    if let Some(edge) = payload.turn.take()
        && let Some(session) = payload.session.clone()
        && let Some(socket) = paths.socket_path(&session)
    {
        let file = crate::held_turn::path_for(&socket);
        match edge {
            TurnEdge::Open(id) => {
                let _ = crate::held_turn::open(&file, &id);
            }
            TurnEdge::Within => held = crate::held_turn::current(&file),
            TurnEdge::Close => {
                if let Some(id) = crate::held_turn::close(&file) {
                    let mut close = TurnMarker::new(Timestamp::now(), id, TurnKind::Close);
                    close.session = Some(session);
                    payload.records.push(LogRecord::Turn(close));
                }
            }
        }
    }
    if let Some(id) = &held {
        for record in &mut payload.records {
            if let LogRecord::File(e) = record
                && e.prompt_id.is_none()
            {
                e.prompt_id = Some(id.clone());
            }
        }
    }

    // Before the empty-records shortcut: a bracketed call carries no records
    // of its own, only what the workspace says it wrote.
    if let Some(observe) = payload.observe.take()
        && let Some(session) = payload.session.clone()
        && let Some(socket) = paths.socket_path(&session)
    {
        let markers = crate::observe::markers_for(&socket);
        match observe {
            Observe::Begin { call } => {
                // Every file the session has recorded: the ones whose
                // deletion this call can be seen making.
                let known: Vec<PathBuf> = thirdeye_core::read_session(&paths, Some(&session))
                    .map(|o| o.file_events().map(|e| e.path.clone()).collect())
                    .unwrap_or_default();
                let root = paths.workspace().root().to_path_buf();
                let _ = crate::observe::begin(&markers, &call, &root, &known);
            }
            Observe::End { call, turn } => {
                let turn = turn.or(held);
                // Taken before the walk: the next command may already be
                // running, and its writes are its own.
                let now = SystemTime::now();
                let root = paths.workspace().root().to_path_buf();
                let seen = crate::observe::end(&markers, &call, &root, now);
                for (paths, kind) in [
                    (seen.written, EventKind::Write),
                    (seen.deleted, EventKind::Delete),
                ] {
                    for path in paths {
                        payload.records.push(observed(
                            agent,
                            &session,
                            turn.as_deref(),
                            (!payload.call_ids_derived).then_some(call.as_str()),
                            path,
                            kind.clone(),
                        ));
                    }
                }
            }
        }
    }

    if payload.records.is_empty() {
        return Ok(0);
    }

    let mut writer = Writer::open(&paths, payload.session.as_deref(), PRODUCER)?;
    for record in &payload.records {
        writer.append(record)?;
    }

    // Strictly after the append, and strictly non-fatal. The event is durable
    // at this point; the notification only decides whether a viewer learns
    // about it now or on the next one. An unattributed event has no session
    // and therefore no daemon to tell.
    if let Some(session) = payload.session.as_deref()
        && let Some(socket) = paths.socket_path(session)
    {
        // Recorded before the poke: a socket-activated daemon may be running
        // and reading this before `notify` returns, and the alternative is a
        // daemon that starts, finds no pointer, and resolves the wrong tree.
        // Captured here because this is the only process that has it: the
        // daemon inherits systemd's environment, not the agent's. What is
        // worth capturing is the surfaces' business, not the hook's — see
        // `surface::SURFACE_ENV`.
        let env = crate::daemon::surface::capture(|key| std::env::var(key).ok());
        let _ = crate::session_ref::record(
            &socket,
            session,
            paths.workspace().root(),
            env,
            crate::agent_process::current_agent(),
        );
        notify(&socket);
    }

    Ok(payload.records.len())
}

/// A file a bracketed tool call wrote or deleted.
///
/// `Write`, never `Edit` with a range: the window says the file was written,
/// not how, and a range would be invented. Keyed per call, path *and* kind,
/// since one command can write many files — and write one it then deletes —
/// and dedup is exact set membership. `call` is `None` when the agent gave no
/// id of its own: a derived one is shared by every run of the same command,
/// and running `cargo fmt` each turn must count each time.
fn observed(
    agent: &AgentId,
    session: &str,
    turn: Option<&str>,
    call: Option<&str>,
    path: PathBuf,
    kind: EventKind,
) -> LogRecord {
    let key = match (&kind, call) {
        // The form already in logs, kept so a replay dedups the same.
        (EventKind::Write, Some(call)) => Some(format!("{call}:{}", path.display())),
        (EventKind::Write, None) => None,
        // A deletion has no time of its own to bound it by, so two
        // overlapping calls can both find the file gone. It was deleted
        // once: keyed by turn, not call, the second report collapses.
        _ => Some(format!(
            "{}:{}:{}",
            turn.unwrap_or(session),
            kind.label(),
            path.display()
        )),
    };
    let mut event = FileEvent::new(Timestamp::now(), agent.clone(), &path, kind, Source::Hook);
    event.session = Some(session.to_string());
    event.prompt_id = turn.map(str::to_owned);
    event.dedup_key = key;
    LogRecord::File(event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use thirdeye_core::{EventPaths, Workspace, load_session};

    fn temp_roots(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("thirdeye-hook-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        (base.join("state"), base.join("run"))
    }

    #[test]
    fn a_read_payload_lands_in_the_session_log() {
        let (state, run) = temp_roots("read");
        let raw = r#"{"session_id":"s1","cwd":"/w","prompt_id":"p1",
          "hook_event_name":"PostToolUse","tool_name":"Write",
          "tool_input":{"file_path":"/w/src/a.rs"},"tool_use_id":"t1"}"#;

        let written = run_with_roots(raw.as_bytes(), &state, &run).unwrap();
        assert_eq!(written, 1);

        let paths = EventPaths::with_roots(&state, &run, Workspace::new("/w"));
        let (tracker, outcome) = load_session(&paths, Some("s1")).unwrap();
        assert_eq!(outcome.malformed_lines, 0);
        assert_eq!(tracker.len(), 1);
        assert!(tracker.get(std::path::Path::new("/w/src/a.rs")).is_some());
    }

    #[test]
    fn an_unhandled_event_writes_nothing_and_still_succeeds() {
        // A hook that errors would surface as noise in the user's agent.
        let (state, run) = temp_roots("noop");
        let raw = r#"{"session_id":"s1","cwd":"/w","hook_event_name":"Notification"}"#;
        assert_eq!(run_with_roots(raw.as_bytes(), &state, &run).unwrap(), 0);
    }

    #[test]
    fn a_claude_hook_another_agent_runs_is_not_claudes() {
        // Grok runs `.claude/settings.json` hooks itself, handing them its
        // own payload, which repeats enough of Claude's field names to be
        // accepted by the Claude adapter without an error.
        let grok = |key: &str| (key == "GROK_HOOK_EVENT").then(|| "post_tool_use".to_string());
        assert_eq!(borrowed_by(&AgentId::Claude, grok), Some("Grok Build"));
    }

    #[test]
    fn a_claude_hook_copilot_runs_is_not_claudes() {
        // Copilot also runs `.claude/settings.json` hooks; `COPILOT_CLI`
        // would not do, since Copilot's shell tool passes it to anything it
        // starts — a Claude Code session included.
        let copilot = |key: &str| (key == "COPILOT_PROJECT_DIR").then(|| "/w".to_string());
        assert_eq!(
            borrowed_by(&AgentId::Claude, copilot),
            Some("GitHub Copilot CLI")
        );
        let shell = |key: &str| (key == "COPILOT_CLI").then(|| "1".to_string());
        assert_eq!(borrowed_by(&AgentId::Claude, shell), None);
    }

    #[test]
    fn claude_running_its_own_hook_is_not_borrowed() {
        assert_eq!(borrowed_by(&AgentId::Claude, |_| None), None);
        // The guard is about Claude's configuration being read by others; an
        // agent installed under its own name is never second-guessed.
        let grok = |_: &str| Some("x".to_string());
        assert_eq!(borrowed_by(&AgentId::Codex, grok), None);
    }

    #[test]
    fn malformed_input_reports_an_error_without_panicking() {
        let (state, run) = temp_roots("bad");
        assert!(run_with_roots(b"{not json".as_slice(), &state, &run).is_err());
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn roots(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("thirdeye-life-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        (base.join("state"), base.join("run"))
    }

    /// Run one payload, reporting how often each side effect fired.
    ///
    /// Counters are per-call, not `static`: tests run on threads that share
    /// process globals, and a shared counter made these pass alone and fail
    /// together — the worst failure mode a test can have.
    fn run_payload(tag: &str, raw: &str) -> (usize, usize) {
        let pokes = Arc::new(AtomicUsize::new(0));
        let stops = Arc::new(AtomicUsize::new(0));
        let (poked, stopped) = (Arc::clone(&pokes), Arc::clone(&stops));

        let (state, runtime) = roots(tag);
        run_inner(
            raw.as_bytes(),
            &AgentId::Claude,
            |cwd| Workspace::new(workspace::resolve_with(None, None, cwd)),
            move |ws| EventPaths::with_roots(state.clone(), runtime.clone(), ws),
            move |_| {
                poked.fetch_add(1, Ordering::SeqCst);
            },
            move |_| {
                stopped.fetch_add(1, Ordering::SeqCst);
            },
        )
        .unwrap();
        (pokes.load(Ordering::SeqCst), stops.load(Ordering::SeqCst))
    }

    #[test]
    fn a_session_ending_stops_the_daemon() {
        // Otherwise the daemon outlives the session, and so does the split it
        // opened — left pointing at work nobody is doing.
        let (pokes, stops) = run_payload(
            "end",
            r#"{"session_id":"s1","cwd":"/w","hook_event_name":"SessionEnd"}"#,
        );
        assert_eq!(stops, 1, "SessionEnd must stop the unit");
        assert_eq!(pokes, 0, "there is nothing left to notify");
    }

    #[test]
    fn a_session_ending_removes_its_pointer() {
        // The daemon used to remove it on the way out, but `systemctl stop`
        // is SIGTERM and the way out never runs. The hook wrote the file; it
        // is the one process certain to be there to remove it.
        let (state, runtime) = roots("end-pointer");
        let paths = EventPaths::with_roots(&state, &runtime, Workspace::new("/w"));
        let socket = paths.socket_path("s1").unwrap();
        crate::session_ref::record(&socket, "s1", Path::new("/w"), Default::default(), None)
            .unwrap();

        run_inner(
            br#"{"session_id":"s1","cwd":"/w","hook_event_name":"SessionEnd"}"#.as_slice(),
            &AgentId::Claude,
            |cwd| Workspace::new(workspace::resolve_with(None, None, cwd)),
            move |ws| EventPaths::with_roots(state.clone(), runtime.clone(), ws),
            |_| {},
            |_| {},
        )
        .unwrap();
        assert!(crate::session_ref::load(&socket).is_none());
    }

    /// A path as it is written inside a JSON string: Windows' separators
    /// are escapes there.
    fn in_json(path: &Path) -> String {
        path.display().to_string().replace('\\', r"\\")
    }

    /// One Codex payload against a real workspace directory, with the side
    /// effects stubbed. Observation walks the disk, so `/w` will not do.
    fn run_codex(state: &Path, runtime: &Path, raw: &str) -> usize {
        let (state, runtime) = (state.to_path_buf(), runtime.to_path_buf());
        run_inner(
            raw.as_bytes(),
            &AgentId::Codex,
            |cwd| Workspace::new(workspace::resolve_with(None, None, cwd)),
            move |ws| EventPaths::with_roots(state.clone(), runtime.clone(), ws),
            |_| {},
            |_| {},
        )
        .unwrap()
    }

    fn codex_tool(ws: &Path, event: &str) -> String {
        format!(
            r#"{{"session_id":"s1","turn_id":"t1","cwd":"{}","hook_event_name":"{event}",
               "model":"m","permission_mode":"default","transcript_path":null,
               "tool_name":"Bash","tool_input":{{"command":"printf checked > notes.md"}},
               "tool_response":"","tool_use_id":"exec-1"}}"#,
            in_json(ws)
        )
    }

    #[test]
    fn a_codex_tool_call_records_the_files_it_wrote() {
        let (state, runtime) = roots("codex-observe");
        let ws = state.parent().unwrap().join("ws");
        std::fs::create_dir_all(&ws).unwrap();

        run_codex(&state, &runtime, &codex_tool(&ws, "PreToolUse"));
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(ws.join("notes.md"), "checked").unwrap();
        assert_eq!(
            run_codex(&state, &runtime, &codex_tool(&ws, "PostToolUse")),
            1
        );

        let paths = EventPaths::with_roots(&state, &runtime, Workspace::new(&ws));
        let outcome = thirdeye_core::read_session(&paths, Some("s1")).unwrap();
        let events: Vec<_> = outcome.file_events().collect();
        assert_eq!(events.len(), 1);
        let e = events[0];
        assert_eq!(e.path, ws.join("notes.md"));
        assert_eq!(e.kind, thirdeye_core::EventKind::Write);
        assert_eq!(e.source, thirdeye_core::Source::Hook);
        assert_eq!(e.agent, thirdeye_core::AgentId::Codex);
        assert_eq!(e.prompt_id.as_deref(), Some("t1"));
        let key = format!("exec-1:{}", ws.join("notes.md").display());
        assert_eq!(e.dedup_key.as_deref(), Some(key.as_str()));
    }

    /// One Copilot payload, wrapped as `--event` wraps it.
    fn run_copilot(state: &Path, runtime: &Path, event: &str, raw: &str) -> usize {
        let (state, runtime) = (state.to_path_buf(), runtime.to_path_buf());
        run_inner(
            envelope(event, raw).unwrap().as_bytes(),
            &AgentId::Copilot,
            |cwd| Workspace::new(workspace::resolve_with(None, None, cwd)),
            move |ws| EventPaths::with_roots(state.clone(), runtime.clone(), ws),
            |_| {},
            |_| {},
        )
        .unwrap()
    }

    fn copilot(ws: &Path, fields: &str) -> String {
        format!(
            r#"{{"sessionId":"c1","timestamp":1791541891065,"cwd":"{}"{fields}}}"#,
            in_json(ws)
        )
    }

    #[test]
    fn a_copilot_turn_is_held_from_its_prompt_to_its_stop() {
        // Copilot names no turn: the prompt's invocation opens one, and the
        // ones after it — separate processes — must still join it.
        let (state, runtime) = roots("copilot-turn");
        let ws = state.parent().unwrap().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let view = format!(
            r#","toolName":"view","toolArgs":{{"path":"{}/a.py"}}"#,
            in_json(&ws)
        );

        run_copilot(
            &state,
            &runtime,
            "userPromptSubmitted",
            &copilot(&ws, r#","prompt":"go""#),
        );
        run_copilot(&state, &runtime, "postToolUse", &copilot(&ws, &view));
        run_copilot(
            &state,
            &runtime,
            "agentStop",
            &copilot(&ws, r#","stopReason":"end_turn""#),
        );
        // After the stop, nothing is held: a straggler joins no turn.
        run_copilot(&state, &runtime, "postToolUse", &copilot(&ws, &view));

        let paths = EventPaths::with_roots(&state, &runtime, Workspace::new(&ws));
        let outcome = thirdeye_core::read_session(&paths, Some("c1")).unwrap();
        let turns: Vec<_> = outcome
            .records
            .iter()
            .filter_map(|r| match r {
                LogRecord::Turn(t) => Some((t.prompt_id.as_str(), t.kind.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            turns,
            vec![
                (
                    "1791541891065",
                    TurnKind::Open {
                        prompt: "go".into()
                    }
                ),
                ("1791541891065", TurnKind::Close),
            ]
        );
        let reads: Vec<_> = outcome
            .file_events()
            .map(|e| e.prompt_id.as_deref())
            .collect();
        assert_eq!(reads, vec![Some("1791541891065"), None]);
    }

    #[test]
    fn a_command_copilot_runs_twice_counts_twice() {
        // Its call key is derived from the command, so both runs share it;
        // keying their writes by it would collapse the second into the first.
        let (state, runtime) = roots("copilot-twice");
        let ws = state.parent().unwrap().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let bash = r#","toolName":"bash","toolArgs":{"command":"make notes"}"#;
        for _ in 0..2 {
            run_copilot(&state, &runtime, "preToolUse", &copilot(&ws, bash));
            std::thread::sleep(std::time::Duration::from_millis(20));
            std::fs::write(ws.join("notes.md"), "made").unwrap();
            run_copilot(&state, &runtime, "postToolUse", &copilot(&ws, bash));
        }

        let paths = EventPaths::with_roots(&state, &runtime, Workspace::new(&ws));
        let (tracker, _) = thirdeye_core::load_session(&paths, Some("c1")).unwrap();
        let file = tracker.get(&ws.join("notes.md")).expect("notes.md tracked");
        assert_eq!(file.counts.write, 2);
    }

    #[test]
    fn a_session_ending_lets_go_of_its_turn() {
        let (state, runtime) = roots("copilot-end");
        let ws = state.parent().unwrap().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        run_copilot(
            &state,
            &runtime,
            "userPromptSubmitted",
            &copilot(&ws, r#","prompt":"go""#),
        );
        let paths = EventPaths::with_roots(&state, &runtime, Workspace::new(&ws));
        let held = crate::held_turn::path_for(&paths.socket_path("c1").unwrap());
        assert!(held.exists());
        run_copilot(
            &state,
            &runtime,
            "sessionEnd",
            &copilot(&ws, r#","reason":"complete""#),
        );
        assert!(!held.exists());
    }

    #[test]
    fn every_hook_marks_its_session_active() {
        let (state, runtime) = roots("active");
        let ws = state.parent().unwrap().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        run_codex(&state, &runtime, &codex_tool(&ws, "PreToolUse"));
        let paths = EventPaths::with_roots(&state, &runtime, Workspace::new(&ws));
        assert_eq!(
            crate::session_ref::load_active(&paths.workspace_dir()).as_deref(),
            Some("s1")
        );
    }

    #[test]
    fn a_file_a_shell_call_deletes_is_recorded_as_deleted() {
        let (state, runtime) = roots("codex-delete");
        let ws = state.parent().unwrap().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("old.rs"), "x").unwrap();
        // The session already knows the file: an earlier call wrote it.
        run_codex(&state, &runtime, &codex_tool(&ws, "PreToolUse"));
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(ws.join("old.rs"), "y").unwrap();
        run_codex(&state, &runtime, &codex_tool(&ws, "PostToolUse"));

        let second = |event: &str| codex_tool(&ws, event).replace("exec-1", "exec-2");
        run_codex(&state, &runtime, &second("PreToolUse"));
        std::fs::remove_file(ws.join("old.rs")).unwrap();
        run_codex(&state, &runtime, &second("PostToolUse"));

        let paths = EventPaths::with_roots(&state, &runtime, Workspace::new(&ws));
        let outcome = thirdeye_core::read_session(&paths, Some("s1")).unwrap();
        let kinds: Vec<_> = outcome.file_events().map(|e| e.kind.clone()).collect();
        assert_eq!(kinds, vec![EventKind::Write, EventKind::Delete]);
    }

    #[test]
    fn one_deletion_seen_by_two_calls_counts_once() {
        // Overlapping calls both find the file gone; it was deleted once.
        let (state, runtime) = roots("codex-delete-twice");
        let ws = state.parent().unwrap().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("old.rs"), "x").unwrap();
        run_codex(&state, &runtime, &codex_tool(&ws, "PreToolUse"));
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(ws.join("old.rs"), "y").unwrap();
        run_codex(&state, &runtime, &codex_tool(&ws, "PostToolUse"));

        let call = |id: &str, event: &str| codex_tool(&ws, event).replace("exec-1", id);
        run_codex(&state, &runtime, &call("exec-a", "PreToolUse"));
        run_codex(&state, &runtime, &call("exec-b", "PreToolUse"));
        std::fs::remove_file(ws.join("old.rs")).unwrap();
        run_codex(&state, &runtime, &call("exec-b", "PostToolUse"));
        run_codex(&state, &runtime, &call("exec-a", "PostToolUse"));

        let paths = EventPaths::with_roots(&state, &runtime, Workspace::new(&ws));
        let outcome = thirdeye_core::read_session(&paths, Some("s1")).unwrap();
        let deletes = outcome
            .file_events()
            .filter(|e| e.kind == EventKind::Delete)
            .count();
        assert_eq!(deletes, 1);
    }

    #[test]
    fn a_session_ending_clears_its_markers() {
        let (state, runtime) = roots("codex-clear");
        let ws = state.parent().unwrap().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        run_codex(&state, &runtime, &codex_tool(&ws, "PreToolUse"));

        let paths = EventPaths::with_roots(&state, &runtime, Workspace::new(&ws));
        let markers = crate::observe::markers_for(&paths.socket_path("s1").unwrap());
        assert!(markers.exists());

        let end = format!(
            r#"{{"session_id":"s1","cwd":"{}","hook_event_name":"SessionEnd","reason":"other"}}"#,
            in_json(&ws)
        );
        run_codex(&state, &runtime, &end);
        assert!(!markers.exists());
    }

    #[test]
    fn an_ordinary_tool_call_never_stops_the_daemon() {
        // The perturbation half: if teardown fired on anything else, the
        // viewer would vanish mid-session.
        let (pokes, stops) = run_payload(
            "tool",
            r#"{"session_id":"s1","cwd":"/w","prompt_id":"p1","hook_event_name":"PostToolUse",
                "tool_name":"Write","tool_input":{"file_path":"/w/a.rs"},"tool_use_id":"t1"}"#,
        );
        assert_eq!(stops, 0);
        assert_eq!(pokes, 1);
    }

    #[test]
    fn a_session_ending_without_an_id_stops_nothing() {
        // No session means no unit to name, and guessing would stop someone
        // else's.
        let (_, stops) = run_payload("no-id", r#"{"cwd":"/w","hook_event_name":"SessionEnd"}"#);
        assert_eq!(stops, 0);
    }
}
