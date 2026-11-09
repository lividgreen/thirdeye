//! How a socket-activated daemon learns which session it is for.
//!
//! Under systemd the daemon is started as `thirdeye@<instance>.service`, and
//! the instance name is all it gets. That is not enough, for two reasons the
//! spec did not anticipate:
//!
//! 1. **The instance name is the socket *stem*, not the session id.** They are
//!    equal for a UUID, but a session id too long for `sun_path` is hashed
//!    into the stem, and a hash cannot be inverted.
//! 2. **Nothing names the workspace.** The daemon inherits systemd's working
//!    directory and none of the agent's environment, and the workspace slug is
//!    `basename + hash`, which cannot be inverted into a path either. Without
//!    this file the daemon would read a different workspace's log — or, more
//!    likely, an empty one — and report nothing while everything worked.
//!
//! So the hook records the pair beside the socket, in the runtime directory
//! where per-session ephemera belong: `<runtime>/<stem>.json` next to
//! `<runtime>/<stem>.sock`. Only the hook environment has these facts, which
//! is exactly why it is the one that writes them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agent_process::AgentProcess;

/// What the daemon needs that its instance name cannot tell it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRef {
    /// The agent's session id, verbatim — not the sanitized stem.
    pub session: String,
    /// Absolute workspace root, as resolved by the hook.
    pub workspace: PathBuf,
    /// The parts of the agent's environment a surface needs.
    ///
    /// A map rather than a field per surface, because §5 says adding a surface
    /// is adding a file — not editing a struct that every other surface
    /// already depends on. The keys are exactly `surface::SURFACE_ENV`.
    ///
    /// Recorded for the same reason as the rest of this file: it exists only
    /// in the hook's environment, and the daemon has none of it. An empty map
    /// is a normal value — it means "no surface", not "not recorded".
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// The agent's process, which the daemon watches for agents that never
    /// say their session ended — see `agent_process`. Absent where it could
    /// not be found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentProcess>,
}

/// Where the pointer for `socket` lives: the same path with a `.json` suffix.
///
/// Derived from the socket path rather than recomputed from the session id, so
/// the two cannot drift apart. `%i` names both.
pub fn path_for(socket: &Path) -> PathBuf {
    socket.with_extension("json")
}

/// Record the pair, if it is not already correct.
///
/// Called on every tool call, so it avoids a write when nothing changed —
/// not for speed, but because rewriting a file the daemon may be reading is
/// churn with no reader-visible benefit.
pub fn record(
    socket: &Path,
    session: &str,
    workspace: &Path,
    env: BTreeMap<String, String>,
    agent: Option<AgentProcess>,
) -> std::io::Result<()> {
    let wanted = SessionRef {
        session: session.to_string(),
        workspace: workspace.to_path_buf(),
        env,
        agent,
    };
    let path = path_for(socket);

    if let Ok(existing) = std::fs::read_to_string(&path)
        && serde_json::from_str::<SessionRef>(&existing).is_ok_and(|found| found == wanted)
    {
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let encoded = serde_json::to_string(&wanted)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, encoded)
}

/// Read back what the hook recorded for this socket.
pub fn load(socket: &Path) -> Option<SessionRef> {
    let raw = std::fs::read_to_string(path_for(socket)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// The file naming a workspace's most recently active session.
pub const ACTIVE_FILE: &str = "active-session";

/// Record that `session` just acted in the workspace whose state lives in
/// `workspace_dir`.
///
/// For producers that cannot learn their own session: Codex gives MCP servers
/// a scrubbed environment, so `flag` has no session id of its own. Latest
/// wins, which is wrong only when two sessions share one directory at once.
/// Skips the write when unchanged, since every hook calls it.
pub fn record_active(workspace_dir: &Path, session: &str) -> std::io::Result<()> {
    use std::io::Write;

    let path = workspace_dir.join(ACTIVE_FILE);
    if load_active(workspace_dir).as_deref() == Some(session) {
        return Ok(());
    }
    // Owner-only like every segment beside it: the directory holds prompt
    // text and every path the agent touched. Windows has no modes to set;
    // there the state directory is inside the user's own profile.
    let mut dir = std::fs::DirBuilder::new();
    dir.recursive(true);
    let mut file = std::fs::OpenOptions::new();
    file.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        dir.mode(0o700);
        file.mode(0o600);
    }
    dir.create(workspace_dir)?;
    file.open(path)?.write_all(session.as_bytes())
}

/// The workspace's most recently active session, if any has acted.
pub fn load_active(workspace_dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(workspace_dir.join(ACTIVE_FILE)).ok()?;
    let session = raw.trim();
    (!session.is_empty()).then(|| session.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("thirdeye-sref-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_pointer_sits_beside_its_socket() {
        // `%i` in the unit file names both, so they must differ only by suffix.
        let socket = Path::new("/run/user/1000/thirdeye/abc.sock");
        assert_eq!(
            path_for(socket),
            Path::new("/run/user/1000/thirdeye/abc.json")
        );
    }

    #[test]
    fn a_hashed_socket_stem_still_recovers_the_real_session_id() {
        // The case the instance name cannot express: the stem is a hash, so
        // without this the daemon would look for a session directory named
        // after the hash and find nothing.
        let socket = tmp("hashed").join("prefix-0123456789abcdef.sock");
        record(
            &socket,
            "a-session-id-far-too-long-to-fit",
            Path::new("/w"),
            BTreeMap::new(),
            None,
        )
        .unwrap();

        let found = load(&socket).unwrap();
        assert_eq!(found.session, "a-session-id-far-too-long-to-fit");
        assert_eq!(found.workspace, Path::new("/w"));
    }

    #[test]
    fn re_recording_the_same_pair_does_not_rewrite_the_file() {
        let socket = tmp("stable").join("s.sock");
        record(&socket, "s1", Path::new("/w"), BTreeMap::new(), None).unwrap();
        let first = std::fs::metadata(path_for(&socket))
            .unwrap()
            .modified()
            .unwrap();

        record(&socket, "s1", Path::new("/w"), BTreeMap::new(), None).unwrap();
        let second = std::fs::metadata(path_for(&socket))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(first, second, "an unchanged pair must not be rewritten");
    }

    #[test]
    fn a_changed_workspace_overwrites_the_stale_pair() {
        // Session ids are unique, so this means the previous run's file was
        // left behind. Trusting it would point the daemon at the wrong tree.
        let socket = tmp("changed").join("s.sock");
        record(&socket, "s1", Path::new("/old"), BTreeMap::new(), None).unwrap();
        record(&socket, "s1", Path::new("/new"), BTreeMap::new(), None).unwrap();
        assert_eq!(load(&socket).unwrap().workspace, Path::new("/new"));
    }

    #[test]
    fn the_surface_environment_travels_with_the_session() {
        // Only the hook's environment has any of this, and the daemon
        // inherits systemd's. Both the present and the absent state must
        // round-trip, because absence is what §12 makes the "no surface"
        // signal.
        let socket = tmp("env").join("s.sock");
        let mut env = BTreeMap::new();
        env.insert("TMUX".to_string(), "/tmp/tmux-1000/default,7,0".to_string());
        env.insert("TMUX_PANE".to_string(), "%3".to_string());
        record(&socket, "s1", Path::new("/w"), env, None).unwrap();

        let found = load(&socket).unwrap();
        assert_eq!(found.env.get("TMUX_PANE").map(String::as_str), Some("%3"));

        record(&socket, "s1", Path::new("/w"), BTreeMap::new(), None).unwrap();
        assert!(load(&socket).unwrap().env.is_empty());
    }

    #[test]
    fn a_pointer_written_before_the_env_map_still_loads() {
        // Old files predate the field. A missing key must not read as corrupt,
        // which would make the daemon resolve the wrong workspace and report
        // nothing while everything else worked.
        let socket = tmp("older").join("s.sock");
        std::fs::write(path_for(&socket), br#"{"session":"s1","workspace":"/w"}"#).unwrap();
        let found = load(&socket).unwrap();
        assert_eq!(found.session, "s1");
        assert!(found.env.is_empty());
    }

    #[test]
    fn a_missing_or_corrupt_pointer_reads_as_absent_rather_than_failing() {
        // The daemon falls back to its flags; a half-written file must not be
        // the difference between starting and crashing.
        let dir = tmp("corrupt");
        assert_eq!(load(&dir.join("nothing.sock")), None);

        let socket = dir.join("bad.sock");
        std::fs::write(path_for(&socket), b"{not json").unwrap();
        assert_eq!(load(&socket), None);
    }

    #[test]
    fn the_last_session_to_act_is_the_active_one() {
        // Two sessions in one workspace: a flag that cannot name its own
        // lands in whichever acted last. A decision, pinned, not an accident.
        let ws = tmp("active");
        record_active(&ws, "a").unwrap();
        record_active(&ws, "b").unwrap();
        assert_eq!(load_active(&ws).as_deref(), Some("b"));
    }

    #[test]
    fn a_workspace_with_no_session_has_no_active_one() {
        assert_eq!(load_active(&tmp("no-active")), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_active_pointer_is_owner_only() {
        // It sits among logs that hold prompt text; the default umask would
        // leave it readable to everyone on the machine.
        use std::os::unix::fs::PermissionsExt;
        let ws = tmp("active-perms").join("fresh");
        record_active(&ws, "a").unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&ws), 0o700);
        assert_eq!(mode(&ws.join(ACTIVE_FILE)), 0o600);
    }
}
