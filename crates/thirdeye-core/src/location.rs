//! Where events live on disk.
//!
//! Layout — session partitioning is structural, because a head attaches to
//! exactly one session:
//!
//! ```text
//! $XDG_STATE_HOME/thirdeye/
//!   <workspace-slug>/
//!     <session-id>/
//!       <producer>.jsonl
//! ```
//!
//! One file per producer rather than one shared log, because unrelated
//! producers (hook invocations, an MCP server, a tailer) have unrelated
//! lifetimes and failure modes; the reader merges them by timestamp.
//!
//! Segmentation does *not* remove the append-atomicity assumption, only its
//! cross-producer half. The plugin runs its hooks with `"async": true`, so
//! many concurrent `thirdeye hook` processes append to the same
//! `hook.jsonl`. Atomicity for those writers rests entirely on
//! [`crate::log::MAX_LINE_BYTES`] staying at or below `PIPE_BUF`.

use std::path::{Path, PathBuf};

/// Session directory used when a pull adapter cannot attribute an event.
///
/// Such events are bucketed rather than dropped, so a misconfigured adapter
/// is visible instead of silently invisible. The leading underscore cannot
/// collide with a real session id (see [`sanitize_component`]).
pub const UNATTRIBUTED: &str = "_unattributed";

/// Bytes available for a Unix socket path, NUL terminator included.
///
/// `sockaddr_un.sun_path` is 108 bytes on Linux and 104 on macOS; the
/// smaller figure is used everywhere so a path that binds on one platform
/// binds on the other.
pub const SUN_PATH_MAX: usize = 104;

const SOCKET_SUFFIX: &str = ".sock";

/// Hex digits in the hashed fallback socket name.
const SOCKET_HASH_LEN: usize = 16;

/// FNV-1a. Hand-rolled because `DefaultHasher` is explicitly not stable
/// across Rust releases, and these hashes name directories that must stay
/// valid across toolchain upgrades.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Reduce an arbitrary string to a safe single path component.
///
/// Session ids and workspace names come from agents and from the filesystem,
/// so they are untrusted with respect to path traversal: anything outside
/// `[A-Za-z0-9._-]` becomes `_`, and components that would resolve to a
/// parent directory are rejected outright.
fn sanitize_component(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();

    // "." and ".." would escape or alias the parent directory even after
    // character filtering, since both consist only of allowed characters.
    if out.chars().all(|c| c == '.') {
        out = format!("_{}", out.len());
    }
    if out.is_empty() {
        out.push('_');
    }
    out.truncate(96);
    out
}

/// Identifies the project a set of events belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// `root` should be absolute and canonicalized by the caller; the slug is
    /// derived from the path text, so two spellings of one directory would
    /// otherwise produce two workspaces.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Stable, human-recognizable directory name: `basename-<hash>`.
    ///
    /// The basename is for humans browsing the state dir; the hash is what
    /// actually disambiguates two projects with the same basename.
    pub fn slug(&self) -> String {
        let base = self
            .root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "root".to_string());
        format!(
            "{}-{:016x}",
            sanitize_component(&base),
            fnv1a64(self.root.to_string_lossy().as_bytes())
        )
    }

    /// Rewrite an absolute path as workspace-relative for display. Paths
    /// outside the workspace are returned unchanged.
    pub fn relativize<'p>(&self, path: &'p Path) -> &'p Path {
        path.strip_prefix(&self.root).unwrap_or(path)
    }

    /// `path` with the root spelled as the root is, when it lies under the
    /// root in a different case — which only Windows makes the same place.
    /// `None` when nothing needs rewriting.
    ///
    /// The root is canonicalized, so it has the disk's case; an agent
    /// started from `c:\SRC\app` names its files by its own working
    /// directory. Every comparison downstream is by component and so
    /// case-sensitive: such a file would draw as a full path, fall out of
    /// the observer's snapshot (its deletion unseen), and count as a second
    /// file beside the same one an observed write names in the disk's case.
    pub fn respell(&self, path: &Path) -> Option<PathBuf> {
        respell_in(&self.root, path, cfg!(windows))
    }
}

/// [`Workspace::respell`] with the platform passed in, so both answers are
/// tested on either. ASCII case only: that is the whole of a drive letter
/// and of nearly every directory name, and NTFS's own folding table is not
/// something to imitate.
fn respell_in(root: &Path, path: &Path, folds_case: bool) -> Option<PathBuf> {
    if !folds_case || path.starts_with(root) {
        return None;
    }
    let mut rest = path.components();
    for ours in root.components() {
        let theirs = rest.next()?;
        if !ours.as_os_str().eq_ignore_ascii_case(theirs.as_os_str()) {
            return None;
        }
    }
    Some(root.join(rest.as_path()))
}

/// The user's home directory, where their agents keep their own settings.
///
/// `%USERPROFILE%` on Windows, where that is what an agent asks the system
/// for; a `HOME` there is something a shell made up, and need not agree.
pub fn home_dir() -> Option<PathBuf> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(key).map(PathBuf::from)
}

/// Where the logs live, from an environment.
///
/// On Windows `%LOCALAPPDATA%` and never `$HOME`: a hook run through Git
/// Bash has a `HOME`, the viewer its terminal starts has none, and the two
/// must name one directory. Every Windows process has `LOCALAPPDATA`.
fn state_root_in(
    env: impl Fn(&str) -> Option<std::ffi::OsString>,
    windows: bool,
) -> Option<PathBuf> {
    let under = |key: &str, rest: &str| env(key).map(|dir| PathBuf::from(dir).join(rest));
    env("THIRDEYE_STATE_DIR")
        .map(PathBuf::from)
        .or_else(|| under("XDG_STATE_HOME", "thirdeye"))
        .or_else(|| {
            if windows {
                under("LOCALAPPDATA", "thirdeye")
            } else {
                under("HOME", ".local/state/thirdeye")
            }
        })
}

/// The log directory, from this process's environment.
pub fn state_root() -> Option<PathBuf> {
    state_root_in(|key| std::env::var_os(key), cfg!(windows))
}

/// Where thirdeye keeps the files its agent integrations load, from an
/// environment: `$THIRDEYE_DATA_DIR`, else `$XDG_DATA_HOME/thirdeye`, else
/// `~/.local/share/thirdeye` — on Windows `%LOCALAPPDATA%\thirdeye-data`.
///
/// Not the log directory, nor inside it — on Windows that is
/// `%LOCALAPPDATA%\thirdeye`: people are told they may delete it to clear
/// the logs, and every agent integration loads its files from this one.
fn data_root_in(
    env: impl Fn(&str) -> Option<std::ffi::OsString>,
    windows: bool,
) -> Option<PathBuf> {
    let set = |key: &str| env(key).filter(|v| !v.is_empty()).map(PathBuf::from);
    set("THIRDEYE_DATA_DIR")
        .or_else(|| set("XDG_DATA_HOME").map(|d| d.join("thirdeye")))
        .or_else(|| {
            if windows {
                set("LOCALAPPDATA").map(|d| d.join("thirdeye-data"))
            } else {
                set("HOME").map(|d| d.join(".local/share/thirdeye"))
            }
        })
}

/// [`data_root_in`] for this process.
pub fn data_root() -> Option<PathBuf> {
    data_root_in(|key| std::env::var_os(key), cfg!(windows))
}

/// Where sockets live, from an environment; `None` leaves the temporary
/// directory. Windows has no runtime directory, and its temporary one is
/// spelled differently by a hook run through Git Bash and by a viewer that
/// was not, so sockets go beside the logs there — in a directory no
/// workspace's slug can be, since every slug ends in a hash.
fn runtime_root_in(
    env: impl Fn(&str) -> Option<std::ffi::OsString>,
    windows: bool,
) -> Option<PathBuf> {
    let under = |key: &str, rest: &str| env(key).map(|dir| PathBuf::from(dir).join(rest));
    under("XDG_RUNTIME_DIR", "thirdeye").or_else(|| {
        windows
            .then(|| under("LOCALAPPDATA", "thirdeye"))
            .flatten()
            .map(|state| state.join("run"))
    })
}

/// Resolves on-disk locations for one workspace's event logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventPaths {
    state_root: PathBuf,
    runtime_root: PathBuf,
    workspace: Workspace,
}

impl EventPaths {
    /// Explicit roots. Tests and `--state-dir` use this.
    pub fn with_roots(
        state_root: impl Into<PathBuf>,
        runtime_root: impl Into<PathBuf>,
        workspace: Workspace,
    ) -> Self {
        Self {
            state_root: state_root.into(),
            runtime_root: runtime_root.into(),
            workspace,
        }
    }

    /// Uses an explicit state root. Tests and `--state-dir` use this; normal
    /// runs use [`EventPaths::discover`].
    pub fn with_state_root(state_root: impl Into<PathBuf>, workspace: Workspace) -> Self {
        Self::with_roots(state_root, Self::discover_runtime_root(), workspace)
    }

    fn discover_runtime_root() -> PathBuf {
        runtime_root_in(|key| std::env::var_os(key), cfg!(windows))
            .unwrap_or_else(|| std::env::temp_dir().join("thirdeye"))
    }

    /// `$THIRDEYE_STATE_DIR`, else `$XDG_STATE_HOME/thirdeye`, else
    /// `$HOME/.local/state/thirdeye` — on Windows `%LOCALAPPDATA%\thirdeye`.
    pub fn discover(workspace: Workspace) -> Self {
        let state_root = state_root_in(|key| std::env::var_os(key), cfg!(windows))
            .unwrap_or_else(|| PathBuf::from(".thirdeye"));
        Self::with_roots(state_root, Self::discover_runtime_root(), workspace)
    }

    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// Where sockets live: `$XDG_RUNTIME_DIR/thirdeye`.
    ///
    /// Deliberately NOT under the state dir — see
    /// `socket_paths_fit_inside_sun_path_on_every_platform`.
    pub fn runtime_root(&self) -> &Path {
        &self.runtime_root
    }

    pub fn workspace_dir(&self) -> PathBuf {
        self.state_root.join(self.workspace.slug())
    }

    /// Path to bind for `session`, or `None` when no name can fit.
    ///
    /// The name is bounded by what is left of [`SUN_PATH_MAX`] after the
    /// runtime root, not by [`sanitize_component`]'s generic 96-byte cap: a
    /// 96-byte name under `/run/user/1000/thirdeye` is already ~126 bytes,
    /// past both platforms' limits. Session ids too long for the remaining
    /// budget are hashed, which keeps distinct sessions on distinct sockets.
    ///
    /// `None` rather than an unbindable path when even the hash does not fit
    /// — that only happens for an unusually long `$XDG_RUNTIME_DIR`, and a
    /// caller that must handle it is better than a `bind()` that fails with
    /// an opaque `EINVAL`.
    pub fn socket_path(&self, session: &str) -> Option<PathBuf> {
        // root + "/" + stem + ".sock" + NUL
        let budget = SUN_PATH_MAX
            .checked_sub(self.runtime_root.as_os_str().len() + 1 + SOCKET_SUFFIX.len() + 1)?;

        let sanitized = sanitize_component(session);
        let stem = if sanitized.len() <= budget {
            sanitized
        } else {
            let hash = format!("{:016x}", fnv1a64(session.as_bytes()));
            match budget.checked_sub(SOCKET_HASH_LEN + 1) {
                // Keep a human-recognizable prefix where there is room for
                // one; the hash is what actually disambiguates.
                Some(prefix) if prefix > 0 => format!("{}-{hash}", &sanitized[..prefix]),
                _ if budget >= SOCKET_HASH_LEN => hash,
                _ => return None,
            }
        };
        Some(self.runtime_root.join(format!("{stem}{SOCKET_SUFFIX}")))
    }

    /// Transport descriptor. Lives in the long, browsable session dir; it is
    /// read as a file, so its length is unconstrained.
    pub fn endpoint_file(&self, session: Option<&str>) -> PathBuf {
        self.session_dir(session).join("endpoint.json")
    }

    /// Directory holding every producer segment for one session.
    pub fn session_dir(&self, session: Option<&str>) -> PathBuf {
        let component = match session {
            Some(s) => sanitize_component(s),
            None => UNATTRIBUTED.to_string(),
        };
        self.workspace_dir().join(component)
    }

    /// The segment one producer appends to.
    pub fn segment(&self, session: Option<&str>, producer: &str) -> PathBuf {
        self.session_dir(session)
            .join(format!("{}.jsonl", sanitize_component(producer)))
    }

    /// Session ids with at least one segment on disk. Sorted for determinism.
    pub fn sessions(&self) -> std::io::Result<Vec<String>> {
        let mut out = Vec::new();
        let dir = match std::fs::read_dir(self.workspace_dir()) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e),
        };
        for entry in dir {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                out.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        out.sort();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(
        pairs: &'static [(&'static str, &'static str)],
    ) -> impl Fn(&str) -> Option<std::ffi::OsString> {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| std::ffi::OsString::from(v))
        }
    }

    #[test]
    fn a_path_under_the_root_in_another_case_takes_the_roots_spelling() {
        let root = Path::new("/Users/u/Work/app");
        assert_eq!(
            respell_in(root, Path::new("/users/U/work/APP/src/a.rs"), true),
            Some(PathBuf::from("/Users/u/Work/app/src/a.rs"))
        );
        // The rest is the agent's: only the root is known to be the disk's.
        assert_eq!(
            respell_in(root, Path::new("/USERS/u/work/app/SRC"), true),
            Some(PathBuf::from("/Users/u/Work/app/SRC"))
        );
        // Already right, elsewhere, or a sibling sharing a text prefix.
        assert_eq!(
            respell_in(root, Path::new("/Users/u/Work/app/a"), true),
            None
        );
        assert_eq!(
            respell_in(root, Path::new("/users/u/work/other/a"), true),
            None
        );
        assert_eq!(
            respell_in(root, Path::new("/users/u/work/apples/a"), true),
            None
        );
        assert_eq!(respell_in(root, Path::new("/users/u"), true), None);
        // Where case makes another place, it is left alone.
        assert_eq!(
            respell_in(root, Path::new("/users/u/work/app/a"), false),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_drive_letter_and_directories_fold_on_windows() {
        let ws = Workspace::new(r"C:\Users\u\Work\app");
        assert_eq!(
            ws.respell(Path::new(r"c:\users\U\work\APP\notes.txt")),
            Some(PathBuf::from(r"C:\Users\u\Work\app\notes.txt"))
        );
    }

    #[test]
    fn on_windows_a_hook_in_git_bash_and_a_viewer_outside_it_share_one_place() {
        // Git Bash gives the hook a HOME; the terminal that starts the viewer
        // has none. Had either been used, the viewer would look in one
        // directory for a session recorded in another, and show nothing.
        let bash = env_of(&[("HOME", "C:/Users/u"), ("LOCALAPPDATA", "C:/Users/u/Local")]);
        let terminal = env_of(&[("LOCALAPPDATA", "C:/Users/u/Local")]);
        let state = state_root_in(&bash, true);
        assert_eq!(state, Some(PathBuf::from("C:/Users/u/Local/thirdeye")));
        assert_eq!(state, state_root_in(&terminal, true));

        let runtime = runtime_root_in(&bash, true);
        assert_eq!(
            runtime,
            Some(PathBuf::from("C:/Users/u/Local/thirdeye/run"))
        );
        assert_eq!(runtime, runtime_root_in(&terminal, true));
    }

    #[test]
    fn elsewhere_the_roots_are_where_they_always_were() {
        let env = env_of(&[("HOME", "/home/u"), ("LOCALAPPDATA", "/stray")]);
        assert_eq!(
            state_root_in(&env, false),
            Some(PathBuf::from("/home/u/.local/state/thirdeye"))
        );
        assert_eq!(runtime_root_in(&env, false), None, "the temporary one");

        let set = env_of(&[
            ("THIRDEYE_STATE_DIR", "/s"),
            ("XDG_STATE_HOME", "/x"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
        ]);
        for windows in [false, true] {
            assert_eq!(state_root_in(&set, windows), Some(PathBuf::from("/s")));
            assert_eq!(
                runtime_root_in(&set, windows),
                Some(PathBuf::from("/run/user/1000/thirdeye"))
            );
        }
    }

    #[test]
    fn slug_is_stable_and_disambiguates_equal_basenames() {
        let a = Workspace::new("/home/u/work/app");
        let b = Workspace::new("/home/u/other/app");

        assert_eq!(a.slug(), Workspace::new("/home/u/work/app").slug());
        assert_ne!(a.slug(), b.slug());
        assert!(a.slug().starts_with("app-"), "got {}", a.slug());
    }

    #[test]
    fn traversal_in_session_id_cannot_escape_the_workspace_dir() {
        let paths = EventPaths::with_state_root("/state", Workspace::new("/home/u/app"));

        for hostile in ["../../etc", "a/../../b", "/abs/path", "x/y"] {
            let dir = paths.session_dir(Some(hostile));
            assert!(
                dir.starts_with(paths.workspace_dir()),
                "escaped on {hostile:?}: {dir:?}"
            );
            // The property that matters is that the id collapses to exactly
            // one non-parent component. A literal ".." *substring* is
            // harmless once the separators are gone (`../../etc` becomes the
            // single component `.._.._etc`), so assert structure, not text.
            let tail: Vec<_> = dir
                .strip_prefix(paths.workspace_dir())
                .unwrap()
                .components()
                .collect();
            assert_eq!(tail.len(), 1, "not a single component on {hostile:?}");
            assert!(matches!(tail[0], std::path::Component::Normal(_)));
        }
    }

    #[test]
    fn dot_only_session_ids_do_not_alias_the_parent() {
        let paths = EventPaths::with_state_root("/state", Workspace::new("/w"));
        for raw in ["..", ".", "..."] {
            let dir = paths.session_dir(Some(raw));
            let last = dir.file_name().unwrap().to_string_lossy().into_owned();
            assert!(!last.chars().all(|c| c == '.'), "aliased on {raw:?}");
            assert!(dir.starts_with(paths.workspace_dir()));
        }
    }

    #[test]
    fn unattributed_bucket_cannot_collide_with_a_real_session() {
        let paths = EventPaths::with_state_root("/state", Workspace::new("/w"));
        // A hostile session id spelled like the bucket still sanitizes to
        // something that is only equal if it was already exactly that name.
        assert_eq!(paths.session_dir(None).file_name().unwrap(), UNATTRIBUTED);
        assert_ne!(
            paths.session_dir(Some("_unattributed/../x")),
            paths.session_dir(None)
        );
    }

    #[test]
    fn segments_are_per_producer_within_a_session() {
        let paths = EventPaths::with_state_root("/state", Workspace::new("/w"));
        let hook = paths.segment(Some("s1"), "hook");
        let tail = paths.segment(Some("s1"), "transcript");

        assert_ne!(hook, tail);
        assert_eq!(hook.parent(), tail.parent());
        assert_eq!(hook.extension().unwrap(), "jsonl");
    }

    #[test]
    fn relativize_leaves_outside_paths_alone() {
        let ws = Workspace::new("/home/u/app");
        assert_eq!(
            ws.relativize(Path::new("/home/u/app/src/main.rs")),
            Path::new("src/main.rs")
        );
        assert_eq!(
            ws.relativize(Path::new("/etc/hosts")),
            Path::new("/etc/hosts")
        );
    }

    /// Guards a real measured bug: a socket under the *state* dir is 111
    /// bytes against sun_path's 108-byte limit on Linux (104 on macOS), so it
    /// cannot bind. Sockets live in the short runtime dir instead — but a
    /// short root is not on its own sufficient, because the session id is
    /// attacker- and agent-controlled. This asserts the property over
    /// adversarial inputs rather than over one UUID-shaped id, since whoever
    /// writes `bind()` later will read the name as a guarantee.
    #[test]
    fn socket_paths_fit_inside_sun_path_on_every_platform() {
        let roots = [
            "/run/user/1000/thirdeye",
            "/tmp/thirdeye",
            // A long but not absurd `$XDG_RUNTIME_DIR`.
            "/run/user/1000/some-deeply-nested-runtime-location/thirdeye",
        ];
        let sessions = [
            "4a818239-5b2a-45fc-95c0-a7de87e9e4be",
            "s",
            &"z".repeat(200),
            &"../".repeat(60),
            &"é".repeat(150),
        ];

        for root in roots {
            let paths = EventPaths::with_roots(
                "/home/u/.local/state/thirdeye",
                root,
                Workspace::new("/home/u/projects/thirdeye"),
            );
            for session in sessions {
                let Some(sock) = paths.socket_path(session) else {
                    continue; // Refusing to name a socket is the honest answer.
                };
                // +1 for sun_path's NUL terminator.
                let len = sock.as_os_str().len() + 1;
                assert!(
                    len <= SUN_PATH_MAX,
                    "socket path is {len} bytes, over the {SUN_PATH_MAX}-byte limit: {sock:?}"
                );
                assert_eq!(sock.parent().unwrap(), Path::new(root));
            }
        }
    }

    #[test]
    fn overlong_session_ids_still_get_distinct_sockets() {
        // Hashing must disambiguate, not collapse: two long ids sharing a
        // prefix would otherwise contend for one socket.
        let paths =
            EventPaths::with_roots("/state", "/run/user/1000/thirdeye", Workspace::new("/w"));
        let a = format!("{}-a", "x".repeat(200));
        let b = format!("{}-b", "x".repeat(200));
        assert_ne!(paths.socket_path(&a), paths.socket_path(&b));
        assert!(paths.socket_path(&a).is_some());
    }

    #[test]
    fn a_runtime_root_with_no_room_left_yields_no_socket_path() {
        // Better than handing back a path that `bind()` rejects with EINVAL.
        let paths = EventPaths::with_roots(
            "/state",
            format!("/run/{}", "d".repeat(SUN_PATH_MAX)),
            Workspace::new("/w"),
        );
        assert_eq!(paths.socket_path("s1"), None);
    }

    #[test]
    fn the_endpoint_descriptor_may_live_in_the_long_state_path() {
        // Only the socket is length-constrained; the descriptor is read as a
        // file, so it stays in the browsable session dir.
        let paths = EventPaths::with_roots(
            "/home/u/.local/state/thirdeye",
            "/run/user/1000/thirdeye",
            Workspace::new("/home/u/projects/thirdeye"),
        );
        assert!(
            paths
                .endpoint_file(Some("4a818239-5b2a-45fc-95c0-a7de87e9e4be"))
                .starts_with(paths.workspace_dir())
        );
    }

    #[test]
    fn socket_names_are_sanitized_like_session_dirs() {
        let paths =
            EventPaths::with_roots("/state", "/run/user/1000/thirdeye", Workspace::new("/w"));
        let sock = paths.socket_path("../../escape").unwrap();
        assert_eq!(sock.parent().unwrap(), Path::new("/run/user/1000/thirdeye"));
        assert!(sock.extension().is_some_and(|e| e == "sock"));
    }

    #[test]
    fn the_data_directory_is_apart_from_the_logs() {
        // People are told they may delete the log directory; the files every
        // agent integration loads must not be in it.
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| std::ffi::OsString::from(v))
            }
        };
        let home = env(&[("HOME", "/h"), ("LOCALAPPDATA", "/l")]);
        assert_eq!(
            data_root_in(home, false),
            Some(PathBuf::from("/h/.local/share/thirdeye"))
        );
        assert_eq!(
            data_root_in(home, true),
            Some(PathBuf::from("/l/thirdeye-data"))
        );
        // Apart, and neither inside the other: deleting the logs must not
        // take the integrations with them.
        for windows in [false, true] {
            let (data, state) = (
                data_root_in(home, windows).unwrap(),
                state_root_in(home, windows).unwrap(),
            );
            assert!(
                !data.starts_with(&state) && !state.starts_with(&data),
                "{data:?} {state:?}"
            );
        }
        let xdg = env(&[("HOME", "/h"), ("XDG_DATA_HOME", "/x")]);
        assert_eq!(data_root_in(xdg, false), Some(PathBuf::from("/x/thirdeye")));
        let set = env(&[("XDG_DATA_HOME", "/x"), ("THIRDEYE_DATA_DIR", "/d")]);
        assert_eq!(data_root_in(set, false), Some(PathBuf::from("/d")));
    }
}
