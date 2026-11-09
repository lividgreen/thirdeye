//! `thirdeye mcp` — the MCP server Claude Code starts once per session.
//!
//! One tool, `flag`: the agent marks a file the human should look at, and the
//! reason. It is the only event kind the agent has to *choose* to emit —
//! everything else is inferred from a tool call it was going to make anyway.
//!
//! **This is a second producer, not a second pipeline.** It writes `mcp.jsonl`
//! beside the hook's `hook.jsonl` in the same session directory, and
//! `read_session` merges them by timestamp. Nothing downstream knows a flag
//! arrived by a different road.
//!
//! **stdout is the JSON-RPC channel.** Nothing but a response may ever be
//! written there, or the transport desynchronizes and the server looks broken
//! to Claude Code. This is the exact inverse of `thirdeye hook`, where stderr
//! is the forbidden one because a hook's stderr surfaces inside the session.
//!
//! Environment facts below were captured from a live Claude Code 2.1.220
//! session (`docs/mcp-environment.md`), not inferred from documentation.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use thirdeye_core::{
    AgentId, EventKind, EventPaths, FileEvent, LineRange, LogRecord, Source, Timestamp, Workspace,
    Writer, open_turn_id, read_session,
};

use crate::workspace;

/// Producer name for this adapter's log segment.
const PRODUCER: &str = "mcp";

/// Protocol version to fall back on when the client names none.
///
/// The live client sends `2025-11-25` and we echo whatever it asks for, so
/// this is only reached by a hand-rolled caller — a test, or someone poking
/// the server by hand.
const DEFAULT_PROTOCOL_VERSION: &str = "2025-11-25";

/// The variable an agent exports its session id to MCP servers in.
///
/// Claude Code's is verified present at startup *and* at call time; without
/// it every flag would land in `_unattributed`, in a different directory from
/// the hook events of the very same session. Each agent's own, never another
/// agent's: Copilot started from inside Claude Code inherits Claude's. Codex
/// gives its servers none, and its flags join the active session instead.
fn session_env(agent: &AgentId) -> Option<&'static str> {
    match agent {
        AgentId::Claude => Some("CLAUDE_CODE_SESSION_ID"),
        AgentId::Copilot => Some("COPILOT_AGENT_SESSION_ID"),
        AgentId::Grok => Some("GROK_SESSION_ID"),
        _ => None,
    }
}

/// Other agents that start Claude's server from a project's Claude
/// settings, by a variable only their plugin loader gives a server.
///
/// Copilot loads the plugins a project's `.claude/settings*.json` enables
/// and starts their MCP servers (verified against 1.0.98). Served there, a
/// flag would be filed as Claude's, in whatever session the workspace last
/// had, so the server offers no tool. `COPILOT_PLUGIN_ROOT` and not
/// `COPILOT_AGENT_SESSION_ID`: Copilot's shell commands carry the latter,
/// and a Claude Code started from one would go silent.
const CLAUDE_SERVER_BORROWERS: &[(&str, &str)] = &[("COPILOT_PLUGIN_ROOT", "GitHub Copilot CLI")];

/// Which other agent started a server configured for `agent`, if one did.
fn borrowed_by(agent: &AgentId, env: impl Fn(&str) -> Option<String>) -> Option<&'static str> {
    if *agent != AgentId::Claude {
        return None;
    }
    CLAUDE_SERVER_BORROWERS
        .iter()
        .find(|(var, _)| env(var).is_some_and(|v| !v.is_empty()))
        .map(|(_, name)| *name)
}

pub fn run(agent: AgentId) -> Result<()> {
    let cwd = std::env::current_dir().ok();
    let server = Server {
        borrowed_by: borrowed_by(&agent, |var| std::env::var(var).ok()),
        session: session_env(&agent)
            .and_then(|var| std::env::var(var).ok())
            .filter(|s| !s.is_empty()),
        agent,
        workspace: workspace::resolve(None, cwd.as_deref()),
        make_paths: EventPaths::discover,
        now: Timestamp::now,
        notify: crate::notify::poke,
    };
    server.serve(std::io::stdin().lock(), &mut std::io::stdout().lock())
}

struct Server<F, N, P> {
    /// Another agent running Claude's server: it then serves no tool.
    borrowed_by: Option<&'static str>,
    session: Option<String>,
    /// Who is raising the flags: one server binary serves every agent.
    agent: AgentId,
    workspace: Workspace,
    make_paths: F,
    /// Injected so tests can pin timestamps instead of racing the clock.
    now: N,
    /// Wakes the session's daemon after a flag is appended. Injected because
    /// the real one starts systemd units.
    notify: P,
}

impl<F, N, P> Server<F, N, P>
where
    F: Fn(Workspace) -> EventPaths,
    N: Fn() -> Timestamp,
    P: Fn(&Path),
{
    /// Read newline-delimited JSON-RPC from `input`, write responses to `out`.
    ///
    /// Returns when the client closes stdin, which is how Claude Code shuts a
    /// server down.
    fn serve(&self, input: impl BufRead, out: &mut impl Write) -> Result<()> {
        for line in input.lines() {
            let line = line.context("reading from stdin")?;
            if line.trim().is_empty() {
                continue;
            }
            if let Some(response) = self.handle(&line) {
                // One message per line, flushed immediately: the client is
                // blocking on this response and there is no other writer.
                writeln!(out, "{response}")?;
                out.flush()?;
            }
        }
        Ok(())
    }

    /// `None` for a notification, which by JSON-RPC must draw no reply.
    fn handle(&self, line: &str) -> Option<String> {
        let request: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            // No id can be recovered from an unparseable line, so the error
            // is reported against a null id rather than dropped silently.
            Err(e) => return Some(error_response(Value::Null, -32700, &e.to_string())),
        };

        let id = request.get("id").cloned();
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or(Value::Null);

        // A notification has no id. `notifications/initialized` is the one
        // that matters here; replying to it is a protocol violation.
        let id = id?;

        Some(match method {
            "initialize" => result_response(id, self.initialize(&params)),
            "tools/list" => match self.borrowed_by {
                Some(_) => result_response(id, json!({ "tools": [] })),
                None => result_response(id, json!({ "tools": [tool_schema()] })),
            },
            "ping" => result_response(id, json!({})),
            "tools/call" if self.borrowed_by.is_some() => result_response(
                id,
                tool_error(&format!(
                    "thirdeye's Claude Code server does not record for {}; \
                     `thirdeye install --user -a copilot` wires thirdeye for it",
                    self.borrowed_by.unwrap_or_default()
                )),
            ),
            "tools/call" => match self.call_tool(&params) {
                Ok(result) => result_response(id, result),
                // A tool that failed is a *result* carrying isError, not a
                // JSON-RPC error: the model is meant to read the reason and
                // correct itself, and a protocol error would be invisible to
                // it. Only an unknown method is a protocol error.
                Err(e) => result_response(id, tool_error(&e.to_string())),
            },
            other => error_response(id, -32601, &format!("unknown method: {other}")),
        })
    }

    fn initialize(&self, params: &Value) -> Value {
        // Echo the client's protocol version rather than asserting our own:
        // this server's surface is three methods that have not changed across
        // revisions, so refusing a version we would in fact speak correctly
        // buys nothing.
        let version = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_PROTOCOL_VERSION);
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": "thirdeye",
                "title": "thirdeye",
                "version": env!("CARGO_PKG_VERSION"),
            },
        })
    }

    fn call_tool(&self, params: &Value) -> Result<Value> {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        if name != "flag" {
            anyhow::bail!("unknown tool: {name}");
        }
        let args = params.get("arguments").cloned().unwrap_or(Value::Null);

        let path = required_str(&args, "path")?;
        let reason = required_str(&args, "reason")?;
        let range = line_range(&args);

        let mut paths = (self.make_paths)(self.workspace.clone());
        // Resolved per call, not at startup: Codex starts MCP servers before
        // any hook has run, so the active session is not known yet then.
        let session = self
            .session
            .clone()
            .or_else(|| crate::session_ref::load_active(&paths.workspace_dir()));
        // The workspace the session's own hook recorded outranks this
        // process's directory, which is the plugin's under Copilot. The
        // socket, and so the pointer beside it, is named by the session
        // alone, wherever this server was started.
        if let Some(found) = session
            .as_deref()
            .and_then(|s| paths.socket_path(s))
            .and_then(|socket| crate::session_ref::load(&socket))
            && found.workspace != paths.workspace().root()
        {
            paths = (self.make_paths)(Workspace::new(found.workspace));
        }
        // Claude Code stamps the tool call's own id into `_meta`, same shape
        // as the hook payload's `tool_use_id`, so a future transcript tailer
        // reporting this call dedups exactly rather than by timing.
        let dedup = params
            .get("_meta")
            .and_then(|m| m.get("claudecode/toolUseId"))
            .and_then(Value::as_str);
        let said = record_flag(
            &paths,
            self.agent.clone(),
            session,
            &Flag {
                path: &path,
                reason: &reason,
                range,
                dedup,
            },
            (self.now)(),
            &self.notify,
        )?;
        Ok(json!({ "content": [{ "type": "text", "text": said }] }))
    }
}

/// One flag, as any producer raises it.
pub struct Flag<'a> {
    pub path: &'a str,
    pub reason: &'a str,
    pub range: Option<LineRange>,
    pub dedup: Option<&'a str>,
}

/// Append a flag to `session`'s log and wake its daemon; returns the
/// confirmation the agent is shown.
///
/// Shared by `thirdeye mcp` and `thirdeye flag`, so a flag is one record
/// whichever way it arrived.
pub fn record_flag(
    paths: &EventPaths,
    agent: AgentId,
    session: Option<String>,
    flag: &Flag,
    now: Timestamp,
    notify: impl Fn(&Path),
) -> Result<String> {
    let workspace = paths.workspace();
    let absolute = absolutize(workspace, flag.path);

    // Read the log back to find the turn in progress. The flag's producer is
    // told nothing about prompts, and a flag outside its turn is a flag in
    // the ungrouped bucket — precisely where it must not be.
    let prompt_id = match read_session(paths, session.as_deref()) {
        Ok(outcome) => open_turn_id(&outcome.records),
        // A log we cannot read is not a reason to drop the flag; it costs
        // the turn attribution and nothing else.
        Err(_) => None,
    };

    let mut event = FileEvent::new(
        now,
        agent,
        &absolute,
        EventKind::Flag {
            reason: flag.reason.to_string(),
            range: flag.range,
        },
        Source::Mcp,
    );
    if let Some(session) = &session {
        event = event.with_session(session);
    }
    if let Some(prompt_id) = prompt_id {
        event = event.with_prompt_id(prompt_id);
    }
    if let Some(key) = flag.dedup {
        event = event.with_dedup_key(key);
    }

    let mut writer = Writer::open(paths, session.as_deref(), PRODUCER)?;
    writer.append(&LogRecord::File(event))?;

    // Strictly after the append, as in the hook. A flag lifts any hide, but
    // only once the daemon re-reads the log — and a flag is often the last
    // thing in a turn, with no tool call behind it to do the waking. `poke`
    // is silent, which matters for `thirdeye mcp`: stdout is the protocol.
    if let Some(socket) = session
        .as_deref()
        .and_then(|session| paths.socket_path(session))
    {
        notify(&socket);
    }

    let shown = workspace.relativize(&absolute);
    let shown = if shown.as_os_str().is_empty() {
        absolute.display().to_string()
    } else {
        shown.display().to_string()
    };
    Ok(format!("Flagged {shown} for the human's attention."))
}

/// A model naming `src/log.rs` means the file in this workspace. Storing it
/// relative would break every consumer, which all key on absolute paths —
/// the hook adapter records what the tool payload gave it, and that is
/// always absolute.
fn absolutize(workspace: &Workspace, path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.root().join(path)
    }
}

fn tool_schema() -> Value {
    json!({
        "name": "flag",
        "title": "Flag a file for the human's attention",
        "description": "\
    Mark a file the human should look at, with a one-line reason. Use this the \
    moment you would otherwise write \"you should look at X\" or \"check Y\" in \
    your reply: the file jumps to the top of the current turn in the human's \
    thirdeye viewer, with your reason and the exact lines. Flag the file you want \
    read, not every file you touched — reads and edits are already recorded \
    automatically, so flagging is for what the human would otherwise miss.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "File to flag. Absolute, or relative to the project root.",
                },
                "reason": {
                    "type": "string",
                    "description": "One line on why this file needs a human eye.",
                },
                "start_line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "First line worth reading, 1-based. Omit to flag the file as a whole.",
                },
                "end_line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Last line worth reading, 1-based and inclusive.",
                },
            },
            "required": ["path", "reason"],
        },
    })
}

fn required_str(args: &Value, field: &str) -> Result<String> {
    match args.get(field).and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => Ok(s.to_string()),
        // Named explicitly so the model can fix the call rather than guess.
        Some(_) => anyhow::bail!("`{field}` must not be empty"),
        None => anyhow::bail!("`{field}` is required"),
    }
}

/// `LineRange` is end-exclusive; the tool takes an inclusive `end_line`,
/// because that is what a human — and a model writing "lines 40-52" — means.
fn line_range(args: &Value) -> Option<LineRange> {
    let start = args.get("start_line").and_then(Value::as_u64)? as u32;
    let end = args
        .get("end_line")
        .and_then(Value::as_u64)
        .map(|e| e as u32)
        .unwrap_or(start);
    Some(LineRange::new(start, end.saturating_add(1)))
}

fn result_response(id: Value, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

fn error_response(id: Value, code: i32, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
    .to_string()
}

fn tool_error(message: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": message }],
        "isError": true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use thirdeye_core::{TurnKind, TurnMarker};

    const WS: &str = "/w";
    const SESSION: &str = "s1";

    struct Harness {
        state: PathBuf,
        runtime: PathBuf,
        /// Every socket the server poked, in order.
        pokes: std::sync::Arc<std::sync::Mutex<Vec<PathBuf>>>,
    }

    fn harness(tag: &str) -> Harness {
        let base = std::env::temp_dir().join(format!("thirdeye-mcp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        Harness {
            state: base.join("state"),
            runtime: base.join("run"),
            pokes: Default::default(),
        }
    }

    impl Harness {
        fn paths(&self) -> EventPaths {
            EventPaths::with_roots(&self.state, &self.runtime, Workspace::new(WS))
        }

        /// A server wired to this harness's scratch state, never the user's.
        fn server(
            &self,
        ) -> Server<
            impl Fn(Workspace) -> EventPaths + use<>,
            impl Fn() -> Timestamp,
            impl Fn(&Path) + use<>,
        > {
            let (state, runtime) = (self.state.clone(), self.runtime.clone());
            let pokes = self.pokes.clone();
            Server {
                borrowed_by: None,
                session: Some(SESSION.to_string()),
                agent: AgentId::Claude,
                workspace: Workspace::new(WS),
                make_paths: move |ws| EventPaths::with_roots(state.clone(), runtime.clone(), ws),
                now: || Timestamp(1_000),
                // Never the real one: it would start units in the developer's
                // own user manager.
                notify: move |socket: &Path| pokes.lock().unwrap().push(socket.to_path_buf()),
            }
        }

        fn open_turn(&self, id: &str) {
            let mut w = Writer::open(&self.paths(), Some(SESSION), "hook").unwrap();
            w.append(&LogRecord::Turn(
                TurnMarker::new(
                    Timestamp(1),
                    id,
                    TurnKind::Open {
                        prompt: "look at the parser".into(),
                    },
                )
                .with_session(SESSION),
            ))
            .unwrap();
        }

        fn flags(&self) -> Vec<FileEvent> {
            read_session(&self.paths(), Some(SESSION))
                .unwrap()
                .records
                .into_iter()
                .filter_map(|r| match r {
                    LogRecord::File(e) if matches!(e.kind, EventKind::Flag { .. }) => Some(e),
                    _ => None,
                })
                .collect()
        }
    }

    /// Send one request, parse the reply.
    fn ask(
        server: &Server<impl Fn(Workspace) -> EventPaths, impl Fn() -> Timestamp, impl Fn(&Path)>,
        req: Value,
    ) -> Value {
        let raw = server
            .handle(&req.to_string())
            .expect("request must draw a response");
        serde_json::from_str(&raw).expect("response must be JSON")
    }

    fn flag_call(args: Value) -> Value {
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": "flag", "arguments": args },
        })
    }

    #[test]
    fn initialize_speaks_the_version_the_client_asked_for() {
        let h = harness("init");
        let reply = ask(
            &h.server(),
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "protocolVersion": "2025-06-18" },
            }),
        );
        assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
        assert!(
            reply["result"]["capabilities"]["tools"].is_object(),
            "a server that does not advertise tools is never asked for them"
        );
    }

    #[test]
    fn a_notification_draws_no_reply() {
        // Replying to `notifications/initialized` desynchronizes the client,
        // which is matching responses to ids it never sent.
        let h = harness("notify");
        assert!(
            h.server()
                .handle(
                    &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string()
                )
                .is_none()
        );
    }

    #[test]
    fn the_flag_tool_is_advertised_with_its_required_fields() {
        let h = harness("list");
        let reply = ask(
            &h.server(),
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        );
        let tool = &reply["result"]["tools"][0];
        assert_eq!(tool["name"], "flag");
        assert_eq!(tool["inputSchema"]["required"], json!(["path", "reason"]));
    }

    #[test]
    fn claudes_server_started_by_copilot_offers_no_tool_and_records_nothing() {
        let env = |var: &str| (var == "COPILOT_PLUGIN_ROOT").then(|| "/p".to_string());
        assert_eq!(
            borrowed_by(&AgentId::Claude, env),
            Some("GitHub Copilot CLI")
        );
        // Copilot's own server, and Claude run from Copilot's shell, which
        // carries COPILOT_AGENT_SESSION_ID but no plugin root.
        assert_eq!(borrowed_by(&AgentId::Copilot, env), None);
        let shell = |var: &str| (var == "COPILOT_AGENT_SESSION_ID").then(|| "s".to_string());
        assert_eq!(borrowed_by(&AgentId::Claude, shell), None);

        let h = harness("borrowed");
        let mut server = h.server();
        server.borrowed_by = Some("GitHub Copilot CLI");
        let list = ask(
            &server,
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        );
        assert_eq!(list["result"]["tools"], json!([]));
        let call = ask(&server, flag_call(json!({ "path": "a.rs", "reason": "r" })));
        assert_eq!(call["result"]["isError"], true);
        assert!(
            read_session(&h.paths(), Some(SESSION))
                .unwrap()
                .records
                .is_empty()
        );
    }

    #[test]
    fn a_flag_lands_in_the_log_as_a_flag_event() {
        let h = harness("write");
        let reply = ask(
            &h.server(),
            flag_call(json!({ "path": "src/log.rs", "reason": "the cap is enforced here" })),
        );
        assert!(reply["result"]["isError"].is_null(), "got {reply}");

        let flags = h.flags();
        assert_eq!(flags.len(), 1);
        assert_eq!(
            flags[0].path,
            Path::new("/w/src/log.rs"),
            "a relative path must resolve against the workspace root"
        );
        assert_eq!(flags[0].source, Source::Mcp);
        assert_eq!(flags[0].session.as_deref(), Some(SESSION));
        match &flags[0].kind {
            EventKind::Flag { reason, range } => {
                assert_eq!(reason, "the cap is enforced here");
                assert_eq!(*range, None);
            }
            other => panic!("expected a flag, got {other:?}"),
        }
    }

    #[test]
    fn a_flag_wakes_the_daemon_rather_than_waiting_for_the_next_hook() {
        // A flag lifts even a session-long hide — but only once the daemon
        // re-reads the log. Without a poke that waits for the agent's next
        // tool call, and a flag is often the last thing in a turn.
        let h = harness("poke");
        ask(
            &h.server(),
            flag_call(json!({ "path": "src/log.rs", "reason": "look" })),
        );
        let socket = h.paths().socket_path(SESSION).unwrap();
        assert_eq!(*h.pokes.lock().unwrap(), vec![socket]);
    }

    #[test]
    fn a_flag_with_no_session_in_its_environment_joins_the_active_one() {
        // Codex gives MCP servers a scrubbed environment: no session id.
        // The hooks record which session acted last in this workspace.
        let h = harness("active");
        crate::session_ref::record_active(&h.paths().workspace_dir(), SESSION).unwrap();
        let mut server = h.server();
        server.session = None;
        ask(
            &server,
            flag_call(json!({ "path": "a.rs", "reason": "look" })),
        );
        assert_eq!(
            h.flags().len(),
            1,
            "the flag must land in the active session"
        );
        assert_eq!(h.flags()[0].session.as_deref(), Some(SESSION));
    }

    #[test]
    fn a_flag_lands_in_the_workspace_its_session_recorded() {
        // Copilot starts MCP servers in the plugin's own directory. The
        // session's hook recorded where the agent really works.
        let h = harness("pointer");
        let socket = h.paths().socket_path(SESSION).unwrap();
        crate::session_ref::record(&socket, SESSION, Path::new(WS), Default::default(), None)
            .unwrap();
        let mut server = h.server();
        server.workspace = Workspace::new("/home/u/.copilot/plugins/thirdeye");
        ask(
            &server,
            flag_call(json!({ "path": "a.rs", "reason": "look" })),
        );
        assert_eq!(
            h.flags().len(),
            1,
            "the flag must land in the session's workspace"
        );
        assert_eq!(h.flags()[0].path, Path::new(WS).join("a.rs"));
    }

    #[test]
    fn each_agent_names_its_session_in_its_own_variable() {
        // Copilot started from inside Claude Code inherits Claude's session
        // id; filing Copilot's flag under it would put it in another session.
        assert_eq!(
            session_env(&AgentId::Claude),
            Some("CLAUDE_CODE_SESSION_ID")
        );
        assert_eq!(
            session_env(&AgentId::Copilot),
            Some("COPILOT_AGENT_SESSION_ID")
        );
        assert_eq!(session_env(&AgentId::Grok), Some("GROK_SESSION_ID"));
        assert_eq!(session_env(&AgentId::Codex), None);
    }

    #[test]
    fn a_flag_is_labelled_with_the_agent_that_raised_it() {
        // The server is shared by every agent; under Codex a flag stamped
        // Claude would misreport who asked for the human's attention.
        let h = harness("agent");
        let mut server = h.server();
        server.agent = AgentId::Codex;
        ask(
            &server,
            flag_call(json!({ "path": "a.rs", "reason": "look" })),
        );
        assert_eq!(h.flags()[0].agent, AgentId::Codex);
    }

    #[test]
    fn an_explicit_session_outranks_the_active_one() {
        let h = harness("explicit");
        crate::session_ref::record_active(&h.paths().workspace_dir(), "other").unwrap();
        ask(
            &h.server(),
            flag_call(json!({ "path": "a.rs", "reason": "look" })),
        );
        assert_eq!(h.flags().len(), 1);
    }

    #[test]
    fn a_flag_joins_the_turn_that_is_still_open() {
        // The whole point: the flagged file has to appear beside the files of
        // the request that prompted it, not in the ungrouped bucket.
        let h = harness("turn");
        h.open_turn("p7");
        ask(
            &h.server(),
            flag_call(json!({ "path": "/w/a.rs", "reason": "read this" })),
        );
        assert_eq!(h.flags()[0].prompt_id.as_deref(), Some("p7"));
    }

    #[test]
    fn a_flag_outside_any_turn_is_still_recorded() {
        // Degrading to ungrouped beats refusing the flag: the agent asked for
        // the human's attention either way.
        let h = harness("noturn");
        ask(
            &h.server(),
            flag_call(json!({ "path": "/w/a.rs", "reason": "read this" })),
        );
        let flags = h.flags();
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].prompt_id, None);
    }

    #[test]
    fn an_inclusive_line_range_is_stored_end_exclusive() {
        // The model writes "lines 40-52" meaning 52 is worth reading;
        // LineRange is end-exclusive, and getting this wrong silently drops
        // the last line of every jump target.
        let h = harness("range");
        ask(
            &h.server(),
            flag_call(
                json!({ "path": "/w/a.rs", "reason": "here", "start_line": 40, "end_line": 52 }),
            ),
        );
        match &h.flags()[0].kind {
            EventKind::Flag { range, .. } => {
                assert_eq!(*range, Some(LineRange::new(40, 53)));
                assert_eq!(range.unwrap().len(), 13);
            }
            other => panic!("expected a flag, got {other:?}"),
        }
    }

    #[test]
    fn a_lone_start_line_flags_that_one_line() {
        let h = harness("oneline");
        ask(
            &h.server(),
            flag_call(json!({ "path": "/w/a.rs", "reason": "here", "start_line": 12 })),
        );
        match &h.flags()[0].kind {
            EventKind::Flag { range, .. } => assert_eq!(*range, Some(LineRange::new(12, 13))),
            other => panic!("expected a flag, got {other:?}"),
        }
    }

    #[test]
    fn the_tool_use_id_becomes_the_dedup_key() {
        let h = harness("dedup");
        ask(
            &h.server(),
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {
                    "name": "flag",
                    "arguments": { "path": "/w/a.rs", "reason": "x" },
                    "_meta": { "claudecode/toolUseId": "toolu_abc" },
                },
            }),
        );
        assert_eq!(h.flags()[0].dedup_key.as_deref(), Some("toolu_abc"));
    }

    #[test]
    fn a_call_missing_its_arguments_is_an_error_the_model_can_read() {
        // isError in a *result*, not a JSON-RPC error: the model has to see
        // the message to fix the call.
        let h = harness("missing");
        for args in [json!({ "reason": "no path" }), json!({ "path": "/w/a.rs" })] {
            let reply = ask(&h.server(), flag_call(args));
            assert_eq!(reply["result"]["isError"], json!(true), "got {reply}");
            assert!(reply["error"].is_null());
        }
        assert!(h.flags().is_empty(), "a rejected call must write nothing");
    }

    #[test]
    fn an_empty_reason_is_refused() {
        // A flag with no reason is a file with a star and no explanation,
        // which is worse than not flagging it.
        let h = harness("blank");
        let reply = ask(
            &h.server(),
            flag_call(json!({ "path": "/w/a.rs", "reason": "   " })),
        );
        assert_eq!(reply["result"]["isError"], json!(true));
        assert!(h.flags().is_empty());
    }

    #[test]
    fn an_unknown_tool_does_not_kill_the_server() {
        let h = harness("unknown-tool");
        let server = h.server();
        let reply = ask(
            &server,
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "explode", "arguments": {} },
            }),
        );
        assert_eq!(reply["result"]["isError"], json!(true));

        // Still serving afterwards.
        let reply = ask(
            &server,
            flag_call(json!({"path": "/w/a.rs", "reason": "ok"})),
        );
        assert!(reply["result"]["isError"].is_null());
    }

    #[test]
    fn an_unknown_method_is_a_protocol_error() {
        let h = harness("unknown-method");
        let reply = ask(
            &h.server(),
            json!({"jsonrpc": "2.0", "id": 9, "method": "resources/list"}),
        );
        assert_eq!(reply["error"]["code"], -32601);
        assert_eq!(reply["id"], 9);
    }

    #[test]
    fn an_unparseable_line_is_reported_against_a_null_id() {
        let h = harness("garbage");
        let raw = h.server().handle("{not json").expect("must answer");
        let reply: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(reply["error"]["code"], -32700);
        assert_eq!(reply["id"], Value::Null);
    }

    #[test]
    fn the_transport_stays_one_message_per_line() {
        // A response carrying a raw newline would be read as two messages and
        // desynchronize the client. Reasons are agent-authored text, so this
        // is reachable, not theoretical.
        let h = harness("newlines");
        let mut out = Vec::new();
        let request = flag_call(json!({
            "path": "/w/a.rs",
            "reason": "line one\nline two\r\nline three",
        }))
        .to_string();
        h.server().serve(request.as_bytes(), &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.lines().count(), 1, "got {text:?}");
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn blank_lines_between_messages_are_ignored() {
        let h = harness("blank-lines");
        let mut out = Vec::new();
        let input = format!(
            "\n{}\n\n",
            json!({"jsonrpc": "2.0", "id": 1, "method": "ping"})
        );
        h.server().serve(input.as_bytes(), &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap().lines().count(), 1);
    }
}
