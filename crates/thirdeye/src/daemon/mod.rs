//! The per-session daemon.
//!
//! **It owns no unique data.** Producers append to the log before any daemon
//! logic runs and whether or not a daemon is alive; everything here is derived
//! state. That is what lets it be killed, crashed or orphaned without loss —
//! the next notification starts it and it replays the log to identical state.
//!
//! Two consequences run through the code below:
//!
//! - The socket notification is **advisory, never the data path**. The daemon
//!   learns *that* something happened from the socket and *what* happened from
//!   the log. A failed connect costs latency and nothing else.
//! - Every decision is a pure function of the whole session, not of what
//!   arrived since last time. A decision that depended on arrival order would
//!   make a restarted daemon disagree with one that had been running, and the
//!   restart is the common case, not the rare one.

pub mod activation;
pub mod protocol;
pub mod surface;
pub mod transport;

use crate::local_socket::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use thirdeye_core::{
    EventKind, EventPaths, LogRecord, Workspace, group_into_turns, open_turn_id, read_session,
};

use crate::session_ref;
use crate::workspace;
use activation::{Activation, SelfBind};
use protocol::{
    ClientMessage, DetailMode, HideUntil, PROTOCOL_VERSION, PolicyMode, Role, ServerMessage,
    TurnView, ViewState,
};
use surface::{Surface, ViewRequest};
use transport::{IoTransport, Transport};

pub fn run(session: String, explicit_workspace: Option<PathBuf>) -> Result<()> {
    let cwd = std::env::current_dir().ok();
    let ws = workspace::resolve(explicit_workspace.clone(), cwd.as_deref());
    let paths = EventPaths::discover(ws);
    // Both halves are named because either can be the culprit, and blaming the
    // session id for a long $XDG_RUNTIME_DIR sends the reader hunting the
    // wrong thing — which is exactly what the first smoke test of this code
    // did to its author.
    let socket = paths.socket_path(&session).with_context(|| {
        format!(
            "no socket path fits under {}: the runtime directory and session id \
             {session} together exceed the {} byte limit on unix socket paths",
            paths.runtime_root().display(),
            thirdeye_core::SUN_PATH_MAX,
        )
    })?;

    // Started by systemd, the daemon has none of the agent's environment and
    // an instance name that is a sanitized stem, not the session id. The hook
    // recorded both beside the socket precisely because only it knew them.
    // An explicit --workspace still wins: it is someone saying they know
    // better, which is the only reason to type it.
    let recorded = session_ref::load(&socket);
    let (session, paths) = match &recorded {
        Some(found) if explicit_workspace.is_none() => (
            found.session.clone(),
            EventPaths::discover(Workspace::new(found.workspace.clone())),
        ),
        _ => (session, paths),
    };

    // Which strategy is not a configuration choice — it is a fact about how
    // this process was started, and getting it wrong means either racing the
    // socket unit for the same path or waiting on a listener nobody passed.
    let socket_for_cleanup = socket.clone();
    #[cfg(unix)]
    let inherited = if activation::socket_activated() {
        Some(activation::SocketActivated.listener()?)
    } else {
        None
    };
    // Socket activation is systemd's, and there is none elsewhere.
    #[cfg(not(unix))]
    let inherited = None;
    // Held until this function returns: see `SelfBind::claim`.
    let claim;
    let listener = match inherited {
        Some(listener) => listener,
        None => {
            let bind = SelfBind::new(socket);
            claim = bind.claim()?;
            if claim.is_none() {
                // Routine where hooks start the daemon: every hook of a turn
                // that found none running started one, and one of them won.
                eprintln!("thirdeye daemon: another is already serving session {session}");
                return Ok(());
            }
            bind.listener()?
        }
    };

    // Only after the listener exists: READY=1 is a promise that a connection
    // will be accepted, and making it early would let `systemctl start`
    // return before that is true.
    #[cfg(unix)]
    activation::notify_ready()?;

    // Which surface is a fact about the world, not a setting: the hook
    // recorded what container the agent was started in, and §12 makes the
    // absence of any the "no surface" signal rather than an error. A daemon
    // with no surface still tails, groups and serves heads — it simply never
    // opens one unasked.
    let env = recorded.map(|r| r.env).unwrap_or_default();
    let surface = surface::resolve(&env, &session);
    let watched = socket_for_cleanup.clone();
    let ending = socket_for_cleanup.clone();
    let watch = AgentWatch {
        gone: Box::new(move || agent_gone(&watched, crate::agent_process::read_proc)),
        every: AGENT_POLL,
        // What `SessionEnd`'s hook does, for the agents that never send one.
        // Exiting rather than returning: the accept loop has no way to be
        // woken, and under systemd the stop below would end it anyway.
        then: Box::new(move || {
            eprintln!("thirdeye daemon: the agent has exited; ending the session");
            crate::observe::clear(&crate::observe::markers_for(&ending));
            let _ = crate::held_turn::close(&crate::held_turn::path_for(&ending));
            let _ = std::fs::remove_file(session_ref::path_for(&ending));
            #[cfg(unix)]
            crate::notify::stop(&ending);
            // Nothing to stop but this process, which is on its way out.
            #[cfg(windows)]
            crate::notify::forget(&ending);
            std::process::exit(0);
        }),
    };
    let result = serve(Daemon::new(&session, paths, surface), listener, Some(watch));

    // `RemoveOnStop` takes the socket but knows nothing about the pointer
    // beside it. Leaving it would accumulate one file per session until
    // logout, and a stale pointer is a worse failure than a missing one: the
    // daemon trusts it over its own flags. Only reached when the listener
    // ends on its own: under systemd the stop is SIGTERM, so `SessionEnd`'s
    // hook removes the pointer as well.
    let _ = std::fs::remove_file(session_ref::path_for(&socket_for_cleanup));
    result
}

/// Start [`run`] as a process of its own, and return.
///
/// Its diagnostics go beside the socket, where the journal is not; nothing
/// else of this process's is handed on.
pub fn run_detached(session: &str, explicit_workspace: Option<&std::path::Path>) -> Result<()> {
    use std::process::{Command, Stdio};

    let cwd = std::env::current_dir().ok();
    let ws = workspace::resolve(explicit_workspace.map(PathBuf::from), cwd.as_deref());
    let log = EventPaths::discover(ws)
        .socket_path(session)
        .and_then(|socket| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(socket.with_extension("log"))
                .ok()
        });

    let mut daemon = Command::new(std::env::current_exe().context("finding this program")?);
    daemon.args(["daemon", "--session", session]);
    if let Some(workspace) = explicit_workspace {
        daemon.arg("--workspace").arg(workspace);
    }
    daemon
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log.map_or_else(Stdio::null, Stdio::from))
        .spawn()
        .context("starting the daemon")?;
    Ok(())
}

pub struct Daemon<S> {
    session: String,
    paths: EventPaths,
    surface: S,
    /// Selection and folding: decisions the human made about the *session*,
    /// so they outlive the head showing them and are written through to
    /// `viewstate.json` rather than held in memory alone.
    view: ViewState,
    /// When the viewer may open itself. Durable, like the view state: a rule
    /// the human set should survive the daemon that was told it.
    policy: PolicyMode,
    /// The turn `NotThisTurn` was set during, so it can expire on its own.
    /// Without it the mode would be indistinguishable from `Never`.
    suppressed_for: Option<String>,
    /// Set by `Hide`, cleared by `Show` or by the condition the human chose.
    ///
    /// Separate from `suppressed_for`: a hide and a `not-this-turn` policy are
    /// different acts that can share an expiry rule, and conflating them made
    /// a plain hide lift itself on the next refresh. Durable, like the policy:
    /// "not this session" must outlive a daemon that restarts on failure.
    hide: Option<Hide>,
    /// Set by `End`: the connection that said so stops the process once its
    /// conversation is over.
    ended: bool,
    /// Next head id. Ids rather than indices: heads leave in any order.
    next_head: u64,
    /// One channel per connected head.
    ///
    /// A channel rather than the head's transport, because two threads writing
    /// to one socket can interleave mid-line and the protocol is
    /// newline-delimited — a torn message is not a slow message, it is a
    /// desynchronized connection. Every write to a head goes through here and
    /// out of a single writer thread.
    heads: Vec<Head>,
}

/// A connected head, and whether the surface launched it.
struct Head {
    id: u64,
    sink: Sender<ServerMessage>,
    launched_by_surface: bool,
}

impl<S: Surface> Daemon<S> {
    pub fn new(session: impl Into<String>, paths: EventPaths, surface: S) -> Self {
        let session = session.into();
        let view = load_view(&paths, &session);
        // A window that is not beside the agent but over it opens only when
        // the agent asks for attention — unless the human said otherwise.
        let policy = load_policy(&paths, &session).unwrap_or(if surface.detached() {
            PolicyMode::Attention
        } else {
            PolicyMode::default()
        });
        let hide = load_hide(&paths, &session);
        Self {
            session,
            paths,
            surface,
            view,
            policy,
            suppressed_for: None,
            hide,
            ended: false,
            heads: Vec::new(),
            next_head: 0,
        }
    }

    /// Remember a head, and hand it everything that changes from now on.
    fn attach_head(&mut self, sink: Sender<ServerMessage>, launched_by_surface: bool) -> u64 {
        let id = self.next_head;
        self.next_head += 1;
        self.heads.push(Head {
            id,
            sink,
            launched_by_surface,
        });
        id
    }

    /// Forget a head whose conversation has ended.
    ///
    /// Explicit rather than inferred from a failed send: the daemon holds a
    /// clone of the head's sender, so the channel cannot close while that
    /// clone lives, and the connection's writer thread would wait on it
    /// forever. Detaching is what lets the writer finish.
    fn detach_head(&mut self, id: u64) {
        self.heads.retain(|head| head.id != id);
    }

    /// Send to every attached head, dropping the ones that have gone.
    ///
    /// A closed channel is the normal way a head leaves — it did not
    /// necessarily say goodbye — so it prunes rather than erroring.
    fn broadcast(&mut self, message: &ServerMessage) {
        self.heads
            .retain(|head| head.sink.send(message.clone()).is_ok());
    }

    /// Persist selection and folding, then tell every head.
    fn view_changed(&mut self) {
        save_view(&self.paths, &self.session, &self.view);
        let patch = ServerMessage::Patch {
            turns: Vec::new(),
            view: Some(self.view.clone()),
            policy: None,
        };
        self.broadcast(&patch);
    }

    /// Persist the policy and tell every head.
    fn policy_changed(&mut self) {
        save_policy(&self.paths, &self.session, self.policy);
        let patch = ServerMessage::Patch {
            turns: Vec::new(),
            view: None,
            policy: Some(self.policy),
        };
        self.broadcast(&patch);
    }

    /// Show the viewer regardless of policy, as a slash command does.
    fn show(&mut self) -> Result<()> {
        self.set_hide(None);
        // An explicit ask also lifts a suppression the human set earlier:
        // they have just contradicted it.
        self.suppressed_for = None;
        let request = self.view_request();
        self.surface.open(&request)
    }

    fn view_request(&self) -> ViewRequest {
        ViewRequest {
            session: self.session.clone(),
            workspace: self.paths.workspace().root().to_path_buf(),
            head: surface::head_program(),
        }
    }

    fn hide(&mut self, until: HideUntil) -> Result<()> {
        let records = read_session(&self.paths, Some(&self.session))
            .map(|o| o.records)
            .unwrap_or_default();
        let turn = open_turn_id(&records);
        let hide = Hide {
            until,
            seen: Tally::of(&records, turn.as_deref()),
            turn,
        };
        self.set_hide(Some(hide));
        self.close_surface()
    }

    /// Close the view, however this surface closes one.
    ///
    /// A detached surface's window closes when its head exits, so the head
    /// it launched is told to go. Its leaving then finds the surface already
    /// closed and records nothing more: the hide that caused it is the only
    /// one.
    fn close_surface(&mut self) -> Result<()> {
        if self.surface.detached() {
            let bye = ServerMessage::Close {
                reason: "the viewer was closed".into(),
            };
            for head in self.heads.iter().filter(|h| h.launched_by_surface) {
                let _ = head.sink.send(bye.clone());
            }
        }
        self.surface.close()
    }

    /// The surface's own head has gone, without saying why.
    ///
    /// A pane killed by hand, a head quit with ctrl-c, a head that crashed:
    /// none of them can say what the human wanted, so all of them mean what
    /// an unqualified hide means. A head that hid deliberately has already
    /// recorded its own condition — and closed the surface doing it, which is
    /// why checking `is_open` is enough to leave that condition alone.
    fn surface_head_left(&mut self) {
        if !self.surface.is_open() {
            return;
        }
        // The window is very likely gone already, so closing it may well
        // fail; what matters is that the surface stops believing in it.
        if let Err(e) = self.hide(HideUntil::NextTurn) {
            eprintln!("thirdeye daemon: the viewer's window had already gone: {e:#}");
        }
    }

    fn set_hide(&mut self, hide: Option<Hide>) {
        self.hide = hide;
        save_hide(&self.paths, &self.session, self.hide.as_ref());
    }

    /// Whether an automatic open is allowed right now.
    ///
    /// Separate from *whether anything happened worth showing*: one is the
    /// human's rule, the other is the log's content, and mixing them makes
    /// either impossible to change alone.
    fn policy_allows(&self, open_turn: Option<&str>) -> bool {
        match self.policy {
            PolicyMode::Never => false,
            // Expires by turn, not by clock: "not this turn" is about a piece
            // of work, and the next prompt is exactly when that work changes.
            PolicyMode::NotThisTurn => self.suppressed_for.as_deref() != open_turn,
            PolicyMode::Any | PolicyMode::Attention => true,
        }
    }

    /// Test-only: the daemon drives the surface, it never reports on it.
    #[cfg(test)]
    pub fn surface(&self) -> &S {
        &self.surface
    }

    /// The state a head is told about, derived fresh from the log.
    fn snapshot(&self) -> Result<ServerMessage> {
        let outcome = read_session(&self.paths, Some(&self.session))?;
        let turns = group_into_turns(&outcome.records)
            .iter()
            .map(TurnView::from_turn)
            .collect();
        Ok(ServerMessage::Snapshot {
            protocol_version: PROTOCOL_VERSION,
            session: self.session.clone(),
            workspace: self.paths.workspace().root().to_path_buf(),
            turns,
            view: self.view.clone(),
            policy: self.policy,
        })
    }

    /// Read a file for a `RequestDetail`.
    ///
    /// The daemon reads the disk, never the head (§4.3): it keeps heads thin,
    /// keeps git knowledge in one place, and is the difference between a
    /// remote head being possible and being a rewrite.
    fn detail(&self, path: PathBuf, mode: DetailMode) -> ServerMessage {
        // The requested path is client-controlled, and a head is not
        // necessarily trusted more than the socket it came over. Only files
        // the log actually mentions can be read — otherwise `RequestDetail`
        // is an arbitrary-file-read oracle for anything that can connect.
        if !self.is_known(&path) {
            return ServerMessage::Close {
                reason: format!("{} is not part of this session", path.display()),
            };
        }

        // The last range the agent touched — the same one `ls` and the
        // list show — so the head opens there rather than at line one.
        let highlight = thirdeye_core::load_session(&self.paths, Some(&self.session))
            .ok()
            .and_then(|(tracker, _)| tracker.get(&path).and_then(|f| f.last_range));

        if mode == DetailMode::Diff
            && let Some(lines) = git_diff(&path)
        {
            return ServerMessage::Detail {
                path,
                mode: DetailMode::Diff,
                first_line: 1,
                lines,
                highlight: None,
            };
        }

        let lines = match std::fs::read_to_string(&path) {
            Ok(text) => text.lines().map(str::to_owned).collect(),
            // A file the agent deleted since touching it is a normal state,
            // and an empty detail says so more usefully than an error.
            Err(_) => Vec::new(),
        };
        ServerMessage::Detail {
            path,
            // Content, and labelled so, when a diff was asked for but there
            // is none: an empty "diff" would read as an empty file.
            mode: DetailMode::Content,
            first_line: 1,
            lines,
            highlight,
        }
    }

    fn is_known(&self, path: &PathBuf) -> bool {
        read_session(&self.paths, Some(&self.session))
            .map(|o| o.file_events().any(|e| &e.path == path))
            .unwrap_or(false)
    }

    /// Re-read the session and bring the surface in line with it.
    ///
    /// Re-reading whole rather than tracking byte offsets is not only simpler:
    /// dedup and cross-producer timestamp merging are defined over the whole
    /// session, so a suffix read could reach a different answer than a replay.
    pub fn refresh(&mut self) -> Result<()> {
        let outcome = read_session(&self.paths, Some(&self.session))?;
        let turns = group_into_turns(&outcome.records);

        // Heads learn about new events here, not by asking. Without this a
        // head would show whatever the log said at the moment it connected
        // and never move again — the notification would reach the surface but
        // not the thing the human is actually reading.
        if !self.heads.is_empty() {
            let patch = ServerMessage::Patch {
                turns: turns.iter().map(TurnView::from_turn).collect(),
                view: None,
                policy: None,
            };
            self.broadcast(&patch);
        }

        // `available` is asked before opening rather than after failing: a
        // surface that cannot be used right now — kitty not running — is a
        // normal state of the world, not an error to report to the user.
        // A hide lasts until the condition the human chose when making it.
        let open_turn = open_turn_id(&outcome.records);
        if self
            .hide
            .as_ref()
            .is_some_and(|hide| hide.lifted_by(&outcome.records))
        {
            self.set_hide(None);
        }

        let trigger = match self.policy {
            // Only the agent's deliberate signal is worth interrupting for.
            PolicyMode::Attention => has_flag(&outcome.records),
            _ => wants_attention(&outcome.records),
        };

        let was_open = self.surface.is_open();
        if trigger
            && self.hide.is_none()
            && self.policy_allows(open_turn.as_deref())
            && self.surface.available()
        {
            let request = self.view_request();
            self.surface.open(&request)?;
        }

        // Only the transition is worth a line. `refresh` runs on every tool
        // call, and a daemon that logged each one would bury the moment it
        // actually decided something. Turn count comes from the grouping the
        // head will be sent at milestone 5 — computing it here is what shows
        // the daemon deriving state rather than storing it.
        if !was_open && self.surface.is_open() {
            eprintln!(
                // Naming the surface is what answers "why did it open there"
                // — or, when the view never appears, where it was supposed to.
                "thirdeye daemon: opening the view for session {} in {} ({} turn{})",
                self.session,
                self.surface.id(),
                turns.len(),
                if turns.len() == 1 { "" } else { "s" }
            );
        }
        Ok(())
    }
}

/// The file's uncommitted changes, from git — `None` when it is not in a
/// repository or has none.
///
/// The daemon reads the disk, never the head (§4.3), which keeps git
/// knowledge in one place. Run from the file's own directory, so it finds
/// whichever repository holds it; `--no-ext-diff` so a user's diff tool
/// cannot turn this into an interactive program.
fn git_diff(path: &std::path::Path) -> Option<Vec<String>> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(path.parent()?)
        // Against HEAD, so staged changes count: an agent that `git add`s
        // its work would otherwise show no diff at all.
        .args(["diff", "--no-color", "--no-ext-diff", "HEAD", "--"])
        .arg(path.file_name()?)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    (out.status.success() && !text.trim().is_empty())
        .then(|| text.lines().map(str::to_owned).collect())
}

/// The turn in progress, for scoping a suppression to it.
fn current_turn<S: Surface>(daemon: &Daemon<S>) -> Option<String> {
    read_session(&daemon.paths, Some(&daemon.session))
        .ok()
        .and_then(|o| open_turn_id(&o.records))
}

/// Whether the agent explicitly asked for attention.
fn has_flag(records: &[LogRecord]) -> bool {
    records.iter().any(|r| match r {
        LogRecord::File(e) => matches!(e.kind, EventKind::Flag { .. }),
        _ => false,
    })
}

/// A hide, and what was in the log when it was made.
///
/// Lifting compares counts against the log rather than watching events
/// arrive, so a daemon replaying the session after a restart reaches the same
/// answer as one that never stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Hide {
    until: HideUntil,
    /// The turn it was made in, which `NextTurn` waits to be out of.
    turn: Option<String>,
    /// What could lift it, counted when it was made.
    seen: Tally,
}

impl Hide {
    fn lifted_by(&self, records: &[LogRecord]) -> bool {
        let now = Tally::of(records, self.turn.as_deref());
        // New ones only: a flag from before the hide is exactly what the
        // human was hiding from.
        if now.flags > self.seen.flags {
            return true;
        }
        match self.until {
            HideUntil::NextMutation => now.total > self.seen.total,
            HideUntil::NextTurn => now.elsewhere > self.seen.elsewhere,
            HideUntil::Session => false,
        }
    }
}

/// Counts of what can lift a hide.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Tally {
    /// Edits and writes, anywhere in the session.
    total: usize,
    /// Edits and writes in any turn but the one the hide was made in.
    ///
    /// A count outside the turn rather than "the turn changed": a new prompt
    /// arrives before the agent has done anything, and reopening in reply to
    /// the human's own typing answers nothing.
    elsewhere: usize,
    flags: usize,
}

impl Tally {
    fn of(records: &[LogRecord], turn: Option<&str>) -> Self {
        let mut tally = Self::default();
        for record in records {
            let LogRecord::File(e) = record else {
                continue;
            };
            if e.kind.is_mutation() {
                tally.total += 1;
                if e.prompt_id.as_deref() != turn {
                    tally.elsewhere += 1;
                }
            } else if matches!(e.kind, EventKind::Flag { .. }) {
                tally.flags += 1;
            }
        }
        tally
    }
}

fn load_hide(paths: &EventPaths, session: &str) -> Option<Hide> {
    std::fs::read_to_string(hide_path(paths, session))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

/// Best-effort, and removed rather than written empty when there is no hide,
/// so a stale file can only ever err towards showing the viewer.
fn save_hide(paths: &EventPaths, session: &str, hide: Option<&Hide>) {
    let path = hide_path(paths, session);
    let Some(hide) = hide else {
        let _ = std::fs::remove_file(path);
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(encoded) = serde_json::to_string(hide) {
        let _ = std::fs::write(path, encoded);
    }
}

fn hide_path(paths: &EventPaths, session: &str) -> PathBuf {
    paths.session_dir(Some(session)).join("hide.json")
}

/// The policy last set for this session, or the default.
/// The policy the human set for this session, if they set one.
fn load_policy(paths: &EventPaths, session: &str) -> Option<PolicyMode> {
    std::fs::read_to_string(policy_path(paths, session))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

fn save_policy(paths: &EventPaths, session: &str, policy: PolicyMode) {
    let path = policy_path(paths, session);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(encoded) = serde_json::to_string(&policy) {
        let _ = std::fs::write(path, encoded);
    }
}

fn policy_path(paths: &EventPaths, session: &str) -> PathBuf {
    paths.session_dir(Some(session)).join("policy.json")
}

/// Selection and folding, as last saved for this session.
///
/// A head restarting — or a second one attaching — must resume where the human
/// left off, so this outlives any head. Unreadable or corrupt state reads as
/// default: losing a selection is a smaller harm than a daemon that will not
/// start.
fn load_view(paths: &EventPaths, session: &str) -> ViewState {
    std::fs::read_to_string(view_path(paths, session))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Best-effort: a view state that cannot be written costs the human their
/// selection on the next restart, which does not justify failing the update
/// they just made.
fn save_view(paths: &EventPaths, session: &str, view: &ViewState) {
    let path = view_path(paths, session);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(encoded) = serde_json::to_string(view) {
        let _ = std::fs::write(path, encoded);
    }
}

fn view_path(paths: &EventPaths, session: &str) -> PathBuf {
    paths.session_dir(Some(session)).join("viewstate.json")
}

/// Whether the log holds anything worth showing the human unprompted.
///
/// §10.2: a mutation or an explicit flag opens the view; reads alone never do.
/// Reading is constant and cheap — opening a split for it would mean the split
/// is always open, which is the same as having no visibility rule at all.
///
/// Deliberately a pure function of every record, not of newly-arrived ones: a
/// daemon replaying a log after a restart must reach the same answer as one
/// that watched the events arrive.
fn wants_attention(records: &[LogRecord]) -> bool {
    records.iter().any(|r| match r {
        LogRecord::File(e) => e.kind.is_mutation() || matches!(e.kind, EventKind::Flag { .. }),
        _ => false,
    })
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use surface::NullSurface;
    use thirdeye_core::{
        AgentId, FileEvent, LineRange, Source, Timestamp, TurnKind, TurnMarker, Writer,
    };

    const WS: &str = "/w";
    const SESSION: &str = "s1";

    pub(crate) struct Harness {
        state: PathBuf,
        runtime: PathBuf,
    }

    pub(crate) fn harness(tag: &str) -> Harness {
        let base =
            std::env::temp_dir().join(format!("thirdeye-daemon-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        Harness {
            state: base.join("state"),
            runtime: base.join("run"),
        }
    }

    impl Harness {
        fn paths(&self) -> EventPaths {
            EventPaths::with_roots(&self.state, &self.runtime, Workspace::new(WS))
        }

        pub(crate) fn write(&self, records: &[LogRecord]) {
            let mut w = Writer::open(&self.paths(), Some(SESSION), "hook").unwrap();
            for r in records {
                w.append(r).unwrap();
            }
        }

        pub(crate) fn daemon(&self) -> Daemon<NullSurface> {
            Daemon::new(SESSION, self.paths(), NullSurface::default())
        }

        /// A daemon whose surface opens windows it cannot close.
        pub(crate) fn detached_daemon(&self) -> Daemon<NullSurface> {
            Daemon::new(SESSION, self.paths(), NullSurface::detached())
        }
    }

    #[test]
    fn a_daemon_can_be_built_over_a_resolved_surface() {
        // The daemon must not be generic over a concrete surface type, or
        // adding one would mean editing `run`. Resolving to a trait object is
        // what keeps the registry the only place that names a surface.
        let surface = surface::resolve(&std::collections::BTreeMap::new(), SESSION);
        let daemon = Daemon::new(SESSION, harness("resolved").paths(), surface);
        assert_eq!(daemon.surface.id(), "none");
    }

    pub(crate) fn file_event(ts: u64, path: &str, kind: EventKind) -> LogRecord {
        LogRecord::File(FileEvent::new(
            Timestamp(ts),
            AgentId::Claude,
            path,
            kind,
            Source::Hook,
        ))
    }

    /// A turn-opening marker.
    pub(crate) fn turn_marker(id: &str, prompt: &str) -> LogRecord {
        LogRecord::Turn(
            TurnMarker::new(
                Timestamp(1),
                id,
                TurnKind::Open {
                    prompt: prompt.into(),
                },
            )
            .with_session(SESSION),
        )
    }

    /// A write attributed to a turn.
    pub(crate) fn in_turn_event(ts: u64, path: &str, turn: &str) -> LogRecord {
        match file_event(ts, path, EventKind::Write) {
            LogRecord::File(mut e) => {
                e.prompt_id = Some(turn.to_string());
                LogRecord::File(e)
            }
            other => other,
        }
    }

    pub(crate) fn read(ts: u64, path: &str) -> LogRecord {
        file_event(ts, path, EventKind::Read { range: None })
    }

    #[test]
    fn a_mutation_opens_the_view() {
        let h = harness("mutation");
        h.write(&[
            read(1, "/w/a.rs"),
            file_event(2, "/w/b.rs", EventKind::Write),
        ]);

        let mut d = h.daemon();
        d.refresh().unwrap();
        assert!(d.surface().is_open());
    }

    #[test]
    fn reads_alone_never_open_the_view() {
        // The perturbation half of the rule above: if this ever goes green
        // with the same code that opens on a mutation, the indicator is
        // vacuous — it would open for everything, and reading is constant.
        let h = harness("reads");
        h.write(&[read(1, "/w/a.rs"), read(2, "/w/b.rs"), read(3, "/w/c.rs")]);

        let mut d = h.daemon();
        d.refresh().unwrap();
        assert!(!d.surface().is_open(), "reads must not summon a viewer");
        assert_eq!(d.surface().opens(), 0);
    }

    #[test]
    fn a_flag_opens_the_view_without_any_mutation() {
        // A flag is the agent asking for the human's attention explicitly. If
        // it did not open the view, the one deliberate signal in the system
        // would be the one that goes unseen.
        let h = harness("flag");
        h.write(&[
            read(1, "/w/a.rs"),
            file_event(
                2,
                "/w/b.rs",
                EventKind::Flag {
                    reason: "look here".into(),
                    range: Some(LineRange::new(1, 4)),
                },
            ),
        ]);

        let mut d = h.daemon();
        d.refresh().unwrap();
        assert!(d.surface().is_open());
    }

    #[test]
    fn a_restarted_daemon_reaches_the_same_view_as_one_that_never_stopped() {
        // The load-bearing invariant: the daemon owns no unique data. If a
        // decision depended on what arrived while it was running, a restart —
        // the common case, since it is socket-activated — would disagree.
        let h = harness("replay");
        h.write(&[read(1, "/w/a.rs")]);

        let mut live = h.daemon();
        live.refresh().unwrap();
        h.write(&[file_event(2, "/w/b.rs", EventKind::Write)]);
        live.refresh().unwrap();

        let mut restarted = h.daemon();
        restarted.refresh().unwrap();

        assert_eq!(live.surface().is_open(), restarted.surface().is_open());
        assert!(restarted.surface().is_open());
    }

    #[test]
    fn refreshing_repeatedly_does_not_reopen_the_view() {
        // `refresh` runs on every notification, which is every tool call.
        let h = harness("idempotent");
        h.write(&[file_event(1, "/w/a.rs", EventKind::Write)]);

        let mut d = h.daemon();
        for _ in 0..5 {
            d.refresh().unwrap();
        }
        assert_eq!(d.surface().opens(), 1);
    }

    #[test]
    fn a_session_with_no_log_at_all_is_not_an_error() {
        // The daemon can be started by the very first hook of a session, in
        // the window before anything has been appended.
        let h = harness("empty");
        let mut d = h.daemon();
        d.refresh().unwrap();
        assert!(!d.surface().is_open());
    }

    #[test]
    fn a_turn_marker_alone_does_not_open_the_view() {
        // UserPromptSubmit fires before the agent does anything. Opening on it
        // would mean the viewer appears for every prompt, including ones the
        // agent answers in prose.
        let h = harness("marker");
        h.write(&[LogRecord::Turn(
            TurnMarker::new(
                Timestamp(1),
                "p1",
                TurnKind::Open {
                    prompt: "hello".into(),
                },
            )
            .with_session(SESSION),
        )]);

        let mut d = h.daemon();
        d.refresh().unwrap();
        assert!(!d.surface().is_open());
    }
}

/// Accept connections until the listener closes.
///
/// One thread per connection, and the lock is taken **per message**, never for
/// the length of a conversation. A head holds its connection open for the
/// whole session; serving it under a held lock — or on the accept thread —
/// would mean no hook notification is processed while anyone is watching,
/// which is precisely when the view needs to update.
pub fn serve<S: Surface + Send + 'static>(
    daemon: Daemon<S>,
    listener: UnixListener,
    watch: Option<AgentWatch>,
) -> Result<()> {
    let shared = Arc::new(Mutex::new(daemon));

    // Replay before serving: a daemon started by the first notification of a
    // session that already has events must not present an empty view.
    lock(&shared).refresh()?;

    if let Some(watch) = watch {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || {
            watch_agent(&shared, watch.gone, watch.every);
            (watch.then)();
        });
    }

    for stream in listener.incoming() {
        let stream = stream.context("accepting a connection")?;
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || {
            let reader = match stream.try_clone() {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("thirdeye daemon: could not clone a connection: {e}");
                    return;
                }
            };
            // One writer per connection, fed by a channel. The conversation
            // and any pushed patch both go through it, so two messages can
            // never interleave into one torn line on a protocol that is
            // delimited by newlines.
            let (tx, rx) = channel::<ServerMessage>();
            let writer = std::thread::spawn(move || {
                let mut out = IoTransport::new(std::io::empty(), stream);
                for message in rx {
                    match serde_json::to_string(&message) {
                        Ok(encoded) => {
                            if out.send_line(&encoded).is_err() {
                                break; // The head is gone; so is the point.
                            }
                        }
                        Err(e) => eprintln!("thirdeye daemon: undeliverable message: {e}"),
                    }
                }
            });

            let mut transport = IoTransport::new(reader, std::io::sink());
            // One bad client must not take the daemon down with it: it holds
            // the only view of a session that may already be in progress.
            if let Err(e) = converse(&shared, &mut transport, tx) {
                eprintln!("thirdeye daemon: dropped a connection: {e:#}");
            }
            // Exiting rather than returning, as when the agent's process is
            // gone: nothing can wake the accept loop.
            if lock(&shared).ended {
                std::process::exit(0);
            }
            // Dropping the last sender ends the writer's loop.
            let _ = writer.join();
        });
    }

    // The listener closing is the session ending. Leaving a head on screen
    // pointed at a daemon that is gone would be worse than showing nothing.
    let mut daemon = lock(&shared);
    if daemon.surface.is_open() {
        daemon.close_surface()?;
    }
    Ok(())
}

/// How often the daemon looks for the agent. A view left open a few seconds
/// after its agent exited is harmless; a `/proc` read is cheap either way.
const AGENT_POLL: std::time::Duration = std::time::Duration::from_secs(5);

/// Watching the agent's process, for agents that never say they ended.
pub struct AgentWatch {
    pub gone: Box<dyn Fn() -> bool + Send>,
    pub every: std::time::Duration,
    /// Run once the agent is gone and the view is closed.
    pub then: Box<dyn FnOnce() + Send>,
}

/// Whether the agent the session's pointer names has exited. A pointer
/// naming no agent — another platform, an older hook, one already removed by
/// `SessionEnd` — says nothing, and nothing is ended on its account.
fn agent_gone(
    socket: &std::path::Path,
    info: impl Fn(u32) -> Option<crate::agent_process::ProcInfo>,
) -> bool {
    session_ref::load(socket)
        .and_then(|r| r.agent)
        .is_some_and(|agent| !crate::agent_process::alive(&agent, info))
}

/// Wait for the agent to exit, then close the view it left.
fn watch_agent<S: Surface>(
    shared: &Mutex<Daemon<S>>,
    gone: impl Fn() -> bool,
    every: std::time::Duration,
) {
    while !gone() {
        std::thread::sleep(every);
    }
    let mut daemon = lock(shared);
    if daemon.surface.is_open() {
        let _ = daemon.close_surface();
    }
}

/// A poisoned lock means another connection thread panicked mid-update.
///
/// Recovering is right here: the daemon owns no unique data, so the worst a
/// panicked thread can have left behind is stale derived state that the next
/// `refresh` recomputes from the log. Propagating the poison instead would
/// take down a viewer over a fault it can fully repair.
fn lock<S>(shared: &Mutex<Daemon<S>>) -> std::sync::MutexGuard<'_, Daemon<S>> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

/// Handle one connection from `Hello` to hangup.
///
/// Generic over the transport so the whole protocol is testable in memory,
/// with no socket and no threads — §14's requirement, and the difference
/// between a protocol bug being an assertion failure and being a timeout.
fn converse<S: Surface>(
    shared: &Mutex<Daemon<S>>,
    t: &mut impl Transport,
    sink: Sender<ServerMessage>,
) -> Result<()> {
    let Some(first) = t.recv_line()? else {
        return Ok(()); // Connected and said nothing. Nothing to do.
    };
    // Every reply goes through the sink, never straight to the transport:
    // pushed patches share the same connection, and one writer is what keeps
    // two messages from interleaving into one torn line.
    let say = |m: ServerMessage| {
        let _ = sink.send(m);
    };

    let hello: ClientMessage = serde_json::from_str(&first).context("parsing the first message")?;
    let ClientMessage::Hello {
        role,
        protocol_version,
        launched_by_surface,
        head_id,
    } = hello
    else {
        say(ServerMessage::Close {
            reason: "the first message must be hello".into(),
        });
        return Ok(());
    };

    // A hook's poke carries no data — the log does — so its version says
    // nothing the daemon needs. After an update, a session's daemon is still
    // the old binary while its hooks are the new one, and refusing their
    // pokes would freeze the view until the session ended.
    //
    // The advisory notification. The connection carries no data; the log
    // does. Nothing is sent back — the hook is already gone.
    if role == Role::Hook {
        lock(shared).refresh()?;
        return Ok(());
    }

    // Nor is the end of a session, which only says `End`: refused, it would
    // leave this daemon — and its window — running after the session that
    // a newer binary's hook says is over.
    let ending = head_id.as_deref() == Some(ClientMessage::SESSION_END);
    if protocol_version != PROTOCOL_VERSION && !ending {
        // Say so rather than failing obscurely: the mismatch is routine during
        // development, when a stale head meets a fresh daemon.
        say(ServerMessage::Close {
            reason: format!("protocol {protocol_version} is not {PROTOCOL_VERSION}"),
        });
        return Ok(());
    }

    serve_head(shared, t, sink, launched_by_surface)
}

fn serve_head<S: Surface>(
    shared: &Mutex<Daemon<S>>,
    t: &mut impl Transport,
    sink: Sender<ServerMessage>,
    launched_by_surface: bool,
) -> Result<()> {
    // Full state first, and attach only after sending it: a patch that
    // overtook the snapshot would describe a state the head has not been
    // given the baseline for.
    let head_id = {
        let mut daemon = lock(shared);
        // A window that finished opening after the view was closed: its
        // head is the one thing left to close it with.
        if launched_by_surface && daemon.surface.detached() && !daemon.surface.is_open() {
            let _ = sink.send(ServerMessage::Close {
                reason: "the viewer was closed".into(),
            });
            return Ok(());
        }
        let snapshot = daemon.snapshot()?;
        let _ = sink.send(snapshot);
        daemon.attach_head(sink.clone(), launched_by_surface)
    };
    let result = head_conversation(shared, t, &sink);
    let mut daemon = lock(shared);
    daemon.detach_head(head_id);
    if launched_by_surface {
        daemon.surface_head_left();
    }
    result
}

fn head_conversation<S: Surface>(
    shared: &Mutex<Daemon<S>>,
    t: &mut impl Transport,
    sink: &Sender<ServerMessage>,
) -> Result<()> {
    while let Some(line) = t.recv_line()? {
        if line.trim().is_empty() {
            continue;
        }
        let message: ClientMessage = match serde_json::from_str(&line) {
            Ok(m) => m,
            // A message this build does not understand is not fatal: a newer
            // head talking to an older daemon should lose the feature, not
            // the connection.
            Err(e) => {
                eprintln!("thirdeye daemon: ignoring an unreadable message: {e}");
                continue;
            }
        };

        let reply = {
            let mut daemon = lock(shared);
            match message {
                // View changes are broadcast, not answered: a second head
                // showing a stale selection is the bug this prevents.
                ClientMessage::Select { path } => {
                    daemon.view.selected = Some(path);
                    daemon.view_changed();
                    None
                }
                ClientMessage::ExpandTurn { turn, expanded } => {
                    daemon.view.collapsed.retain(|t| t != &turn);
                    if !expanded {
                        daemon.view.collapsed.push(turn);
                    }
                    daemon.view_changed();
                    None
                }
                ClientMessage::SetPolicy { mode } => {
                    daemon.policy = mode;
                    // Setting it now is what "not this turn" is scoped to.
                    daemon.suppressed_for = match mode {
                        PolicyMode::NotThisTurn => current_turn(&daemon),
                        _ => None,
                    };
                    daemon.policy_changed();
                    None
                }
                ClientMessage::Show => {
                    if let Err(e) = daemon.show() {
                        eprintln!("thirdeye daemon: could not show the viewer: {e:#}");
                    }
                    None
                }
                ClientMessage::Hide { until } => {
                    // The hide itself cannot fail; only closing the window
                    // can, and when the head hid itself that window is
                    // usually gone before the close arrives. Saying "could
                    // not hide" there sent a reader after a working feature.
                    if let Err(e) = daemon.hide(until) {
                        eprintln!(
                            "thirdeye daemon: hidden, but closing the window failed \
                             (a head that hid itself has usually closed it already): {e:#}"
                        );
                    }
                    None
                }
                ClientMessage::RequestDetail { path, mode } => Some(daemon.detail(path, mode)),
                ClientMessage::End => {
                    daemon.ended = true;
                    if daemon.surface.is_open() {
                        let _ = daemon.close_surface();
                    }
                    Some(ServerMessage::Close {
                        reason: "the session ended".into(),
                    })
                }
                // A second hello means the head is confused about its own
                // state; carrying on would leave both sides guessing.
                ClientMessage::Hello { .. } => Some(ServerMessage::Close {
                    reason: "hello was already sent".into(),
                }),
            }
        };

        if let Some(reply) = reply {
            let closing = matches!(reply, ServerMessage::Close { .. });
            let _ = sink.send(reply);
            if closing {
                return Ok(());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod conversation_tests {
    use super::tests::*;
    use super::*;
    use std::path::Path;
    use surface::NullSurface;
    use transport::memory;

    /// A file event attributed to a turn, as a hook writes one mid-turn.
    /// The bare `file_event` helper carries no prompt_id and so lands in the
    /// ungrouped bucket — correct, but not what these tests are about.
    fn in_turn(ts: u64, path: &str, kind: EventKind, turn: &str) -> LogRecord {
        match file_event(ts, path, kind) {
            LogRecord::File(mut e) => {
                e.prompt_id = Some(turn.to_string());
                LogRecord::File(e)
            }
            other => other,
        }
    }

    /// A turn-opening marker, as `UserPromptSubmit` writes one.
    fn turn_open(id: &str, prompt: &str) -> LogRecord {
        LogRecord::Turn(
            thirdeye_core::TurnMarker::new(
                thirdeye_core::Timestamp(1),
                id,
                thirdeye_core::TurnKind::Open {
                    prompt: prompt.into(),
                },
            )
            .with_session("s1"),
        )
    }

    /// Run one client-side script against a daemon, returning what it said.
    fn talk(daemon: Daemon<NullSurface>, messages: &[ClientMessage]) -> Vec<ServerMessage> {
        let shared = Mutex::new(daemon);
        let (mut client, mut server) = memory::pair();
        for m in messages {
            client
                .send_line(&serde_json::to_string(m).unwrap())
                .unwrap();
        }
        client.hang_up();

        // Replies leave through the sink, exactly as they do in production —
        // the transport carries only what the client says.
        let (tx, rx) = channel();
        converse(&shared, &mut server, tx).unwrap();
        rx.try_iter().collect()
    }

    fn head_hello() -> ClientMessage {
        ClientMessage::Hello {
            role: Role::Head,
            protocol_version: PROTOCOL_VERSION,
            head_id: Some("tui".into()),
            launched_by_surface: false,
        }
    }

    fn surface_head_hello() -> ClientMessage {
        ClientMessage::Hello {
            role: Role::Head,
            protocol_version: PROTOCOL_VERSION,
            head_id: Some("tui".into()),
            launched_by_surface: true,
        }
    }

    /// One whole connection against a daemon that outlives it.
    fn converse_with(shared: &Mutex<Daemon<NullSurface>>, messages: &[ClientMessage]) {
        let (mut client, mut server) = memory::pair();
        for m in messages {
            client
                .send_line(&serde_json::to_string(m).unwrap())
                .unwrap();
        }
        client.hang_up();
        let (tx, _rx) = channel();
        converse(shared, &mut server, tx).unwrap();
    }

    fn showing(tag: &str) -> (Harness, Mutex<Daemon<NullSurface>>) {
        let h = harness(tag);
        h.write(&[
            turn_marker("p1", "first"),
            in_turn_event(2, "/w/a.rs", "p1"),
        ]);
        let mut daemon = h.daemon();
        daemon.refresh().unwrap();
        assert!(daemon.surface().is_open());
        (h, Mutex::new(daemon))
    }

    #[test]
    fn a_window_is_closed_by_telling_its_head_to_go() {
        // GNOME Terminal, Alacritty, Ghostty: no call closes the window, but
        // the window closes when the head in it exits. Only the head the
        // surface launched: one the human started by hand stays.
        let h = harness("detached-hangup");
        h.write(&[
            turn_marker("p1", "first"),
            in_turn_event(2, "/w/a.rs", "p1"),
        ]);
        let mut daemon = h.detached_daemon();
        daemon.show().unwrap();
        let (launched, from_launched) = channel();
        let (by_hand, from_by_hand) = channel();
        let id = daemon.attach_head(launched, true);
        daemon.attach_head(by_hand, false);

        daemon.hide(HideUntil::Session).unwrap();
        assert!(
            from_launched
                .try_iter()
                .any(|m| matches!(m, ServerMessage::Close { .. }))
        );
        assert!(
            !from_by_hand
                .try_iter()
                .any(|m| matches!(m, ServerMessage::Close { .. }))
        );

        // Its leaving is the hide's doing, not a second hide of its own.
        daemon.detach_head(id);
        daemon.surface_head_left();
        assert_eq!(
            daemon.hide.as_ref().map(|h| h.until),
            Some(HideUntil::Session)
        );
        assert_eq!(daemon.surface().log.len(), 2, "opened, closed — once");
    }

    #[test]
    fn a_window_that_opens_after_the_view_closed_is_told_to_go() {
        // The window takes a moment to start its head; a hide in between
        // must not leave it showing.
        let h = harness("detached-late");
        let replies = talk(h.detached_daemon(), &[surface_head_hello()]);
        assert!(
            matches!(replies.as_slice(), [ServerMessage::Close { .. }]),
            "{replies:?}"
        );
    }

    #[test]
    fn a_window_opens_unasked_only_for_a_flag_unless_the_human_says_otherwise() {
        let h = harness("detached-policy");
        h.write(&[
            turn_marker("p1", "first"),
            in_turn_event(2, "/w/a.rs", "p1"),
        ]);
        let mut daemon = h.detached_daemon();
        daemon.refresh().unwrap();
        assert!(
            !daemon.surface().is_open(),
            "an edit does not open a window"
        );

        daemon.policy = PolicyMode::Any;
        daemon.policy_changed();
        let mut told = h.detached_daemon();
        told.refresh().unwrap();
        assert!(told.surface().is_open(), "the human's own policy wins");
    }

    #[test]
    fn the_agent_exiting_closes_the_view_it_left() {
        // Antigravity never says its session ended, and a crashed Claude
        // Code cannot: the daemon notices the process is gone instead.
        let (_h, shared) = showing("agent-gone");
        let checks = std::cell::Cell::new(0);
        watch_agent(
            &shared,
            || {
                checks.set(checks.get() + 1);
                checks.get() > 2
            },
            std::time::Duration::from_millis(1),
        );
        assert!(!lock(&shared).surface().is_open());
        assert_eq!(checks.get(), 3, "watched until it was gone, not after");
    }

    #[test]
    fn a_session_that_says_it_ended_closes_its_view_and_stops() {
        // Where no service manager stops the daemon, the hook's `SessionEnd`
        // says so over the socket. An ordinary head leaving must not: it is
        // what `thirdeye open` does a moment after opening the view.
        let (_h, shared) = showing("ended");
        converse_with(&shared, &[head_hello()]);
        assert!(!lock(&shared).ended);
        assert!(lock(&shared).surface().is_open());

        converse_with(&shared, &[ClientMessage::end_hello(), ClientMessage::End]);
        assert!(lock(&shared).ended);
        assert!(!lock(&shared).surface().is_open());
    }

    #[test]
    fn a_session_end_from_another_version_still_ends_it() {
        // After an update the hook is the new binary and the daemon the old.
        let (_h, shared) = showing("ended-version");
        let ClientMessage::Hello { role, head_id, .. } = ClientMessage::end_hello() else {
            unreachable!()
        };
        let hello = ClientMessage::Hello {
            role,
            protocol_version: PROTOCOL_VERSION + 1,
            head_id,
            launched_by_surface: false,
        };
        converse_with(&shared, &[hello, ClientMessage::End]);
        assert!(lock(&shared).ended);
    }

    #[test]
    fn only_an_agent_the_pointer_names_can_be_gone() {
        let dir = std::env::temp_dir().join(format!("thirdeye-agent-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let socket = dir.join("s1.sock");
        let none = |_| None;
        assert!(!agent_gone(&socket, none), "no pointer says nothing");

        session_ref::record(
            &socket,
            "s1",
            std::path::Path::new("/w"),
            Default::default(),
            None,
        )
        .unwrap();
        assert!(
            !agent_gone(&socket, none),
            "a pointer naming no agent says nothing"
        );

        let agent = crate::agent_process::AgentProcess {
            pid: 4242,
            start: 7,
        };
        std::fs::remove_file(session_ref::path_for(&socket)).unwrap();
        session_ref::record(
            &socket,
            "s1",
            std::path::Path::new("/w"),
            Default::default(),
            Some(agent),
        )
        .unwrap();
        let running = |_| {
            Some(crate::agent_process::ProcInfo {
                ppid: 1,
                comm: "agy".into(),
                start: 7,
                zombie: false,
            })
        };
        assert!(!agent_gone(&socket, running));
        assert!(agent_gone(&socket, none));
    }

    #[test]
    fn the_surfaces_head_leaving_means_the_human_closed_it() {
        // A pane killed by hand, or a head quit with ctrl-c, says nothing on
        // its way out. Believing the window still existed would keep the
        // viewer shut for the rest of the session — even `thirdeye open`
        // would see it "already open" and do nothing.
        let (h, shared) = showing("surface-head-left");
        converse_with(&shared, &[surface_head_hello()]);
        assert!(!lock(&shared).surface().is_open());

        // And it means what an unqualified hide means: back on the next
        // turn's edit, not before.
        h.write(&[in_turn_event(3, "/w/a.rs", "p1")]);
        lock(&shared).refresh().unwrap();
        assert!(!lock(&shared).surface().is_open());
        h.write(&[
            turn_marker("p2", "second"),
            in_turn_event(4, "/w/b.rs", "p2"),
        ]);
        lock(&shared).refresh().unwrap();
        assert!(lock(&shared).surface().is_open());
    }

    #[test]
    fn any_other_head_leaving_leaves_the_view_alone() {
        // `thirdeye open` hangs up straight after opening a window, before
        // that window's own head has even connected.
        let (_h, shared) = showing("cli-head-left");
        converse_with(&shared, &[head_hello()]);
        assert!(lock(&shared).surface().is_open());
    }

    #[test]
    fn the_condition_the_human_chose_survives_their_head_leaving() {
        // Hiding from the head closes its own window, so the head always
        // leaves right after. That departure must not overwrite "for this
        // session" with the default.
        let (h, shared) = showing("chosen-survives");
        converse_with(
            &shared,
            &[
                surface_head_hello(),
                ClientMessage::Hide {
                    until: HideUntil::Session,
                },
            ],
        );
        h.write(&[
            turn_marker("p2", "second"),
            in_turn_event(3, "/w/b.rs", "p2"),
        ]);
        lock(&shared).refresh().unwrap();
        assert!(!lock(&shared).surface().is_open());
    }

    #[test]
    fn a_head_is_sent_the_whole_state_before_anything_else() {
        // A head that had to assemble history from patches would show a
        // different session depending on when it connected.
        let h = harness("snapshot");
        h.write(&[
            turn_open("p1", "fix the parser"),
            in_turn(2, "/w/a.rs", EventKind::Write, "p1"),
        ]);

        let replies = talk(h.daemon(), &[head_hello()]);
        match &replies[0] {
            ServerMessage::Snapshot {
                turns,
                session,
                workspace,
                ..
            } => {
                assert_eq!(session, "s1");
                assert_eq!(workspace, Path::new("/w"));
                assert_eq!(turns.len(), 1);
                assert_eq!(turns[0].prompt.as_deref(), Some("fix the parser"));
                assert_eq!(turns[0].files[0].path, Path::new("/w/a.rs"));
            }
            other => panic!("expected a snapshot first, got {other:?}"),
        }
    }

    #[test]
    fn a_hook_gets_no_reply_at_all() {
        // It has already exited. Writing to it would at best be wasted and at
        // worst block on a socket nobody is reading.
        let h = harness("hook-quiet");
        h.write(&[file_event(1, "/w/a.rs", EventKind::Write)]);
        assert!(talk(h.daemon(), &[ClientMessage::hook_hello()]).is_empty());
    }

    #[test]
    fn selection_is_echoed_back_so_every_head_agrees() {
        let h = harness("select");
        h.write(&[file_event(1, "/w/a.rs", EventKind::Write)]);

        let replies = talk(
            h.daemon(),
            &[
                head_hello(),
                ClientMessage::Select {
                    path: "/w/a.rs".into(),
                },
            ],
        );
        match replies.last().unwrap() {
            ServerMessage::Patch {
                view: Some(view), ..
            } => {
                assert_eq!(view.selected.as_deref(), Some(Path::new("/w/a.rs")));
            }
            other => panic!("expected a view patch, got {other:?}"),
        }
    }

    #[test]
    fn folding_a_turn_and_unfolding_it_leaves_no_trace() {
        // `collapsed` is a set the head will re-send from; an entry that
        // survived unfolding would fold the turn again on the next snapshot.
        let h = harness("fold");
        h.write(&[turn_open("p1", "hello")]);

        let replies = talk(
            h.daemon(),
            &[
                head_hello(),
                ClientMessage::ExpandTurn {
                    turn: "p1".into(),
                    expanded: false,
                },
                ClientMessage::ExpandTurn {
                    turn: "p1".into(),
                    expanded: true,
                },
            ],
        );
        let folded = match &replies[1] {
            ServerMessage::Patch { view: Some(v), .. } => v.collapsed.clone(),
            other => panic!("expected a patch, got {other:?}"),
        };
        assert_eq!(folded, vec!["p1".to_string()]);

        match replies.last().unwrap() {
            ServerMessage::Patch { view: Some(v), .. } => assert!(v.collapsed.is_empty()),
            other => panic!("expected a patch, got {other:?}"),
        }
    }

    #[test]
    fn a_detail_request_returns_raw_lines_and_no_styling() {
        let dir = std::env::temp_dir().join(format!("thirdeye-detail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.rs");
        std::fs::write(&file, "fn main() {}\n// second\n").unwrap();

        let h = harness("detail");
        h.write(&[file_event(1, file.to_str().unwrap(), EventKind::Write)]);

        let replies = talk(
            h.daemon(),
            &[
                head_hello(),
                ClientMessage::RequestDetail {
                    path: file.clone(),
                    mode: DetailMode::Content,
                },
            ],
        );
        match replies.last().unwrap() {
            ServerMessage::Detail {
                lines, first_line, ..
            } => {
                assert_eq!(lines, &["fn main() {}", "// second"]);
                assert_eq!(*first_line, 1);
            }
            other => panic!("expected a detail, got {other:?}"),
        }
    }

    /// A scratch file the log says was touched, and the daemon over it.
    fn touched(tag: &str, body: &str, kind: EventKind) -> (Harness, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("thirdeye-detail-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.rs");
        std::fs::write(&file, body).unwrap();
        let h = harness(tag);
        h.write(&[file_event(1, file.to_str().unwrap(), kind)]);
        (h, file)
    }

    fn ask_detail(h: &Harness, file: &Path, mode: DetailMode) -> ServerMessage {
        let replies = talk(
            h.daemon(),
            &[
                head_hello(),
                ClientMessage::RequestDetail {
                    path: file.to_path_buf(),
                    mode,
                },
            ],
        );
        replies.last().unwrap().clone()
    }

    #[test]
    fn a_detail_carries_the_range_the_agent_touched() {
        // The product's promise: the file opens at the lines the agent was
        // working on, not at line one of a long file.
        let range = thirdeye_core::LineRange::new(10, 14);
        let (h, file) = touched("highlight", "x\n", EventKind::Edit { range: Some(range) });
        match ask_detail(&h, &file, DetailMode::Content) {
            ServerMessage::Detail { highlight, .. } => assert_eq!(highlight, Some(range)),
            other => panic!("expected a detail, got {other:?}"),
        }
    }

    #[test]
    fn a_diff_is_served_from_git() {
        let (h, file) = touched("diff", "alpha\nbeta\n", EventKind::Write);
        let dir = file.parent().unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .output()
                .unwrap()
        };
        git(&["init", "-q"]);
        git(&["add", "a.rs"]);
        git(&["commit", "-qm", "base"]);
        std::fs::write(&file, "alpha\nBETA\n").unwrap();
        match ask_detail(&h, &file, DetailMode::Diff) {
            ServerMessage::Detail { mode, lines, .. } => {
                assert_eq!(mode, DetailMode::Diff);
                assert!(lines.iter().any(|l| l == "-beta"), "{lines:?}");
                assert!(lines.iter().any(|l| l == "+BETA"), "{lines:?}");
            }
            other => panic!("expected a detail, got {other:?}"),
        }
    }

    #[test]
    fn staged_changes_are_part_of_the_diff() {
        // Against the index, an agent that `git add`s its work would show
        // no diff at all.
        let (h, file) = touched("staged", "alpha\nbeta\n", EventKind::Write);
        let dir = file.parent().unwrap();
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .output()
                .unwrap()
        };
        git(&["init", "-q"]);
        git(&["add", "a.rs"]);
        git(&["commit", "-qm", "base"]);
        std::fs::write(&file, "alpha\nBETA\n").unwrap();
        git(&["add", "a.rs"]);
        match ask_detail(&h, &file, DetailMode::Diff) {
            ServerMessage::Detail { mode, lines, .. } => {
                assert_eq!(mode, DetailMode::Diff);
                assert!(lines.iter().any(|l| l == "+BETA"), "{lines:?}");
            }
            other => panic!("expected a detail, got {other:?}"),
        }
    }

    #[test]
    fn a_file_with_no_diff_is_shown_as_content_and_says_so() {
        // Not a repository, or nothing changed: an empty "diff" would read as
        // an empty file. Content, labelled as content, is the honest answer.
        let (h, file) = touched("nodiff", "alpha\n", EventKind::Write);
        match ask_detail(&h, &file, DetailMode::Diff) {
            ServerMessage::Detail { mode, lines, .. } => {
                assert_eq!(mode, DetailMode::Content);
                assert_eq!(lines, ["alpha"]);
            }
            other => panic!("expected a detail, got {other:?}"),
        }
    }

    #[test]
    fn a_head_cannot_read_a_file_this_session_never_touched() {
        // `RequestDetail` is client-controlled. Without this check anything
        // that can reach the socket can read any file the user can — the
        // daemon would be an arbitrary-file-read oracle.
        let h = harness("oracle");
        h.write(&[file_event(1, "/w/a.rs", EventKind::Write)]);

        let replies = talk(
            h.daemon(),
            &[
                head_hello(),
                ClientMessage::RequestDetail {
                    path: "/etc/passwd".into(),
                    mode: DetailMode::Content,
                },
            ],
        );
        match replies.last().unwrap() {
            ServerMessage::Close { reason } => assert!(reason.contains("not part of this session")),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn an_attached_head_is_told_when_the_log_changes() {
        // The gap that made a head worth attaching: without this it would
        // show whatever the log said when it connected and never move, while
        // the surface saw every update.
        let h = harness("push");
        h.write(&[turn_open("p1", "start")]);

        let shared = Mutex::new(h.daemon());
        let (tx, rx) = channel();
        {
            let mut daemon = lock(&shared);
            let snapshot = daemon.snapshot().unwrap();
            tx.send(snapshot).unwrap();
            daemon.attach_head(tx.clone(), false);
        }
        let _ = rx.try_iter().count(); // drain the snapshot

        h.write(&[in_turn(2, "/w/new.rs", EventKind::Write, "p1")]);
        lock(&shared).refresh().unwrap();

        let pushed: Vec<ServerMessage> = rx.try_iter().collect();
        match pushed.first() {
            Some(ServerMessage::Patch { turns, .. }) => {
                assert!(
                    turns
                        .iter()
                        .any(|t| t.files.iter().any(|f| f.path == Path::new("/w/new.rs"))),
                    "the new file must reach the head: {turns:?}"
                );
            }
            other => panic!("expected a pushed patch, got {other:?}"),
        }
    }

    #[test]
    fn a_head_that_left_stops_being_written_to() {
        // The daemon holds a clone of every head's sender. If a departed head
        // were never detached, its channel could not close and the
        // connection's writer thread would wait on it forever.
        let h = harness("detach");
        h.write(&[turn_open("p1", "start")]);

        let shared = Mutex::new(h.daemon());
        let (tx, rx) = channel();
        let id = lock(&shared).attach_head(tx, false);
        assert_eq!(lock(&shared).heads.len(), 1);

        lock(&shared).detach_head(id);
        assert!(lock(&shared).heads.is_empty());

        lock(&shared).refresh().unwrap();
        drop(rx);
    }

    #[test]
    fn selection_outlives_the_head_that_made_it() {
        // §11 calls view state durable: a restarted head must resume where the
        // human left off, so it belongs to the session and not to the viewer.
        let h = harness("durable");
        h.write(&[file_event(1, "/w/a.rs", EventKind::Write)]);

        talk(
            h.daemon(),
            &[
                head_hello(),
                ClientMessage::Select {
                    path: "/w/a.rs".into(),
                },
            ],
        );

        // A brand-new daemon, as after a restart.
        let replies = talk(h.daemon(), &[head_hello()]);
        match &replies[0] {
            ServerMessage::Snapshot { view, .. } => {
                assert_eq!(view.selected.as_deref(), Some(Path::new("/w/a.rs")));
            }
            other => panic!("expected a snapshot, got {other:?}"),
        }
    }

    #[test]
    fn attention_mode_ignores_edits_and_waits_for_a_flag() {
        // The mode exists for agents that edit constantly: only the
        // deliberate signal is worth interrupting a human for.
        let h = harness("attention");
        h.write(&[file_event(1, "/w/a.rs", EventKind::Write)]);

        let mut daemon = h.daemon();
        daemon.policy = PolicyMode::Attention;
        daemon.refresh().unwrap();
        assert!(!daemon.surface().is_open(), "an edit must not open it here");

        h.write(&[file_event(
            2,
            "/w/b.rs",
            EventKind::Flag {
                reason: "look".into(),
                range: None,
            },
        )]);
        daemon.refresh().unwrap();
        assert!(daemon.surface().is_open(), "a flag must");
    }

    #[test]
    fn never_mode_keeps_it_shut_but_still_collects() {
        let h = harness("never");
        h.write(&[file_event(1, "/w/a.rs", EventKind::Write)]);

        let mut daemon = h.daemon();
        daemon.policy = PolicyMode::Never;
        daemon.refresh().unwrap();
        assert!(!daemon.surface().is_open());

        // Collection is unaffected — that is the whole point of the log
        // living outside the daemon.
        let snapshot = daemon.snapshot().unwrap();
        match snapshot {
            ServerMessage::Snapshot { turns, .. } => {
                assert!(turns.iter().any(|t| !t.files.is_empty()))
            }
            other => panic!("expected a snapshot, got {other:?}"),
        }
    }

    #[test]
    fn an_explicit_show_overrides_even_never() {
        // §10.3: the slash commands bypass policy. A human asking outranks a
        // rule the same human set earlier.
        let h = harness("show");
        h.write(&[file_event(1, "/w/a.rs", EventKind::Write)]);
        let mut daemon = h.daemon();
        daemon.policy = PolicyMode::Never;
        daemon.show().unwrap();
        assert!(daemon.surface().is_open());
    }

    #[test]
    fn not_this_turn_expires_when_the_next_prompt_arrives() {
        // Scoped to a piece of work rather than a duration: a timer would
        // either outlive the annoyance or fail to reach it.
        let h = harness("not-this-turn");
        h.write(&[
            turn_marker("p1", "first"),
            in_turn_event(2, "/w/a.rs", "p1"),
        ]);

        let mut daemon = h.daemon();
        daemon.policy = PolicyMode::NotThisTurn;
        daemon.suppressed_for = Some("p1".into());
        daemon.refresh().unwrap();
        assert!(!daemon.surface().is_open(), "suppressed for this turn");

        // A new prompt is exactly when the work changes.
        h.write(&[
            turn_marker("p2", "second"),
            in_turn_event(3, "/w/b.rs", "p2"),
        ]);
        daemon.refresh().unwrap();
        assert!(daemon.surface().is_open(), "the next turn must lift it");
    }

    /// A session already showing the view, for hiding it.
    fn open_in_turn(tag: &str) -> (Harness, Daemon<NullSurface>) {
        let h = harness(tag);
        h.write(&[
            turn_marker("p1", "first"),
            in_turn_event(2, "/w/a.rs", "p1"),
        ]);
        let mut daemon = h.daemon();
        daemon.refresh().unwrap();
        assert!(daemon.surface().is_open());
        (h, daemon)
    }

    fn flag_in_turn(ts: u64, path: &str, turn: &str) -> LogRecord {
        match file_event(
            ts,
            path,
            EventKind::Flag {
                reason: "look".into(),
                range: None,
            },
        ) {
            LogRecord::File(mut e) => {
                e.prompt_id = Some(turn.to_string());
                LogRecord::File(e)
            }
            other => other,
        }
    }

    #[test]
    fn hiding_until_the_next_turn_outlasts_this_turns_edits() {
        // A human closing the viewer means "not now", not "never again" —
        // that is what the `never` policy is for.
        let (h, mut daemon) = open_in_turn("hide-turn");
        daemon.hide(HideUntil::NextTurn).unwrap();

        h.write(&[in_turn_event(3, "/w/a.rs", "p1")]);
        daemon.refresh().unwrap();
        assert!(!daemon.surface().is_open(), "must stay hidden this turn");

        h.write(&[
            turn_marker("p2", "second"),
            in_turn_event(4, "/w/b.rs", "p2"),
        ]);
        daemon.refresh().unwrap();
        assert!(
            daemon.surface().is_open(),
            "a new turn's edit brings it back"
        );
    }

    #[test]
    fn a_new_prompt_alone_does_not_bring_a_hidden_view_back() {
        // Every hide condition is about mutations. The prompt that opens a
        // turn is the human's own typing; popping the viewer up in reply to
        // it, before the agent has changed anything, answers nothing.
        let (h, mut daemon) = open_in_turn("hide-prompt");
        daemon.hide(HideUntil::NextTurn).unwrap();

        h.write(&[turn_marker("p2", "second")]);
        daemon.refresh().unwrap();
        assert!(!daemon.surface().is_open());
    }

    #[test]
    fn hiding_until_the_next_mutation_yields_to_the_very_next_edit() {
        let (h, mut daemon) = open_in_turn("hide-mutation");
        daemon.hide(HideUntil::NextMutation).unwrap();

        // Reads never open the view, hidden or not (§10.2).
        h.write(&[read(3, "/w/a.rs")]);
        daemon.refresh().unwrap();
        assert!(!daemon.surface().is_open(), "a read must not lift it");

        h.write(&[in_turn_event(4, "/w/a.rs", "p1")]);
        daemon.refresh().unwrap();
        assert!(daemon.surface().is_open(), "the same turn's next edit must");
    }

    #[test]
    fn hiding_for_the_session_outlasts_every_turn() {
        let (h, mut daemon) = open_in_turn("hide-session");
        daemon.hide(HideUntil::Session).unwrap();

        h.write(&[
            in_turn_event(3, "/w/a.rs", "p1"),
            turn_marker("p2", "second"),
            in_turn_event(4, "/w/b.rs", "p2"),
        ]);
        daemon.refresh().unwrap();
        assert!(!daemon.surface().is_open());

        // Only the human contradicting themselves ends it.
        daemon.show().unwrap();
        assert!(daemon.surface().is_open());
    }

    #[test]
    fn a_flag_brings_back_even_a_session_long_hide() {
        // The flag is the agent's one deliberate "look at this" — the signal
        // the attention policy exists to let through. Silencing it too is
        // what `never` is for.
        let (h, mut daemon) = open_in_turn("hide-flag");
        daemon.hide(HideUntil::Session).unwrap();

        h.write(&[flag_in_turn(3, "/w/a.rs", "p1")]);
        daemon.refresh().unwrap();
        assert!(daemon.surface().is_open());
    }

    #[test]
    fn a_flag_from_before_the_hide_does_not_undo_it() {
        // The perturbation half: if lifting counted every flag in the
        // session rather than new ones, one early flag would make every hide
        // in the session last exactly until the next refresh.
        let h = harness("hide-old-flag");
        h.write(&[turn_marker("p1", "first"), flag_in_turn(2, "/w/a.rs", "p1")]);
        let mut daemon = h.daemon();
        daemon.refresh().unwrap();
        daemon.hide(HideUntil::Session).unwrap();

        h.write(&[read(3, "/w/a.rs")]);
        daemon.refresh().unwrap();
        assert!(!daemon.surface().is_open());
    }

    #[test]
    fn a_session_long_hide_survives_the_daemon_that_was_told_it() {
        // The daemon restarts on failure. A human who said "not this
        // session" must not have the viewer reappear because of it.
        let (h, mut daemon) = open_in_turn("hide-durable");
        daemon.hide(HideUntil::Session).unwrap();

        let mut restarted = h.daemon();
        h.write(&[in_turn_event(3, "/w/a.rs", "p1")]);
        restarted.refresh().unwrap();
        assert!(!restarted.surface().is_open());
    }

    #[test]
    fn showing_clears_a_hide_for_good() {
        // Otherwise a restart would resurrect a hide the human had already
        // contradicted.
        let (h, mut daemon) = open_in_turn("hide-shown");
        daemon.hide(HideUntil::Session).unwrap();
        daemon.show().unwrap();

        let mut restarted = h.daemon();
        restarted.refresh().unwrap();
        assert!(restarted.surface().is_open());
    }

    #[test]
    fn the_policy_survives_the_daemon_that_was_told_it() {
        let h = harness("policy-durable");
        h.write(&[file_event(1, "/w/a.rs", EventKind::Write)]);

        let mut daemon = h.daemon();
        daemon.policy = PolicyMode::Attention;
        daemon.policy_changed();

        let restarted = h.daemon();
        assert_eq!(restarted.policy, PolicyMode::Attention);
    }

    #[test]
    fn a_mismatched_protocol_is_named_rather_than_left_to_hang() {
        let h = harness("version");
        let replies = talk(
            h.daemon(),
            &[ClientMessage::Hello {
                role: Role::Head,
                protocol_version: 99,
                head_id: None,
                launched_by_surface: false,
            }],
        );
        match &replies[0] {
            ServerMessage::Close { reason } => assert!(reason.contains("99"), "got {reason}"),
            other => panic!("expected a close, got {other:?}"),
        }
    }

    #[test]
    fn a_hook_from_another_version_still_wakes_the_daemon() {
        // After an update the daemon is the old binary and the hooks the new
        // one; the poke must not be refused, or the view stops moving.
        let h = harness("hook-version");
        let replies = talk(
            h.daemon(),
            &[ClientMessage::Hello {
                role: Role::Hook,
                protocol_version: 99,
                head_id: None,
                launched_by_surface: false,
            }],
        );
        assert!(replies.is_empty(), "{replies:?}");
    }

    #[test]
    fn talking_before_saying_hello_is_refused() {
        let h = harness("no-hello");
        let replies = talk(
            h.daemon(),
            &[ClientMessage::Select {
                path: "/w/a.rs".into(),
            }],
        );
        match &replies[0] {
            ServerMessage::Close { reason } => assert!(reason.contains("hello")),
            other => panic!("expected a close, got {other:?}"),
        }
    }

    #[test]
    fn an_unreadable_message_costs_the_message_not_the_connection() {
        // A newer head sending something this build cannot parse should lose
        // the feature, not the session.
        let h = harness("garbage");
        h.write(&[file_event(1, "/w/a.rs", EventKind::Write)]);

        let shared = Mutex::new(h.daemon());
        let (mut client, mut server) = memory::pair();
        client
            .send_line(&serde_json::to_string(&head_hello()).unwrap())
            .unwrap();
        client.send_line(r#"{"message":"invented_later"}"#).unwrap();
        client
            .send_line(
                &serde_json::to_string(&ClientMessage::Select {
                    path: "/w/a.rs".into(),
                })
                .unwrap(),
            )
            .unwrap();
        let (tx, rx) = channel();
        converse(&shared, &mut server, tx).unwrap();

        let replies: Vec<ServerMessage> = rx.try_iter().collect();
        assert!(
            matches!(replies.last(), Some(ServerMessage::Patch { .. })),
            "the connection must survive: {replies:?}"
        );
    }
}
