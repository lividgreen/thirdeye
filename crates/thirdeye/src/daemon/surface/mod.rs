//! Where a head gets displayed.
//!
//! A `Surface` embeds a terminal program somewhere the human can see it — a
//! kitty split, a tmux pane. It is deliberately *not* the head: kitty is
//! **where** a head is shown, a TUI is **what** the head is, and a future GUI
//! head makes its own window and needs no surface at all.
//!
//! Nothing terminal-shaped appears in this trait. The moment it does, the
//! daemon stops being able to drive a surface it cannot see, and `NullSurface`
//! stops being an honest stand-in for one.

pub mod alacritty;
pub mod ghostty;
pub mod gnome_terminal;
pub mod kitty;
pub mod null;
pub mod runner;
pub mod tmux;
pub mod wezterm;
pub mod window;
pub mod windows_terminal;
pub mod zellij;

pub use null::NullSurface;

use std::collections::BTreeMap;
use std::fmt::Debug;

use anyhow::Result;

/// The environment a surface may need, declared here and copied by the hook.
///
/// It lives beside the surfaces rather than in the hook because it is the
/// seam's contract with the hook: adding a surface must be adding a file and
/// a line here, never editing the hook. The hook merely copies what is listed.
///
/// An allowlist rather than the whole environment, because the recorded file
/// sits beside a log that already holds prompt text and every path the agent
/// touched. Copying `AWS_SECRET_ACCESS_KEY` in there would be a new class of
/// exposure for no reader's benefit.
pub const SURFACE_ENV: &[&str] = &[
    // A human naming a surface outranks detection. First, so it is visible
    // that the rest of this list is only consulted when nobody chose.
    "THIRDEYE_SURFACE",
    // How much of the agent's pane the head takes, in percent; see
    // `split_percent`.
    "THIRDEYE_SPLIT",
    // tmux: `<socket-path>,<server-pid>,<session-index>`.
    "TMUX",
    // tmux: the agent's own pane, and therefore the pane we split.
    "TMUX_PANE",
    // Zellij: the session to address, and the agent's own pane — which the
    // CLI places a no-focus pane beside only when told it in this variable.
    "ZELLIJ_SESSION_NAME",
    "ZELLIJ_PANE_ID",
    // kitty: which instance to drive.
    "KITTY_LISTEN_ON",
    // WezTerm: the GUI instance's socket, and the agent's own pane.
    "WEZTERM_UNIX_SOCKET",
    "WEZTERM_PANE",
    // GNOME Terminal: the server its windows come from, and the agent's own
    // screen — present only together in a GNOME Terminal window.
    "GNOME_TERMINAL_SERVICE",
    "GNOME_TERMINAL_SCREEN",
    // Alacritty: the instance's IPC socket.
    "ALACRITTY_SOCKET",
    // Set fresh by whichever emulator the agent is really in; the other
    // emulators' variables can leak across a launch.
    "TERM",
    // Windows Terminal from WSL: present together only in a WSL shell in
    // one of its panes, and the distribution is what `wsl.exe` must enter.
    "WT_SESSION",
    "WSL_DISTRO_NAME",
    // Where the agent's shell finds programs — see `runner::ProcessRunner`.
    // Not a secret, unlike most of what this list exists to keep out.
    "PATH",
];

/// Copy the declared variables out of an environment.
///
/// Takes a lookup rather than reading `std::env` directly for a reason that
/// is not stylistic: `std::env::set_var` is `unsafe` in Rust 2024 and the
/// workspace forbids `unsafe`, so a test could not otherwise set up an
/// environment to capture from.
pub fn capture(lookup: impl Fn(&str) -> Option<String>) -> BTreeMap<String, String> {
    SURFACE_ENV
        .iter()
        .filter_map(|key| {
            let value = lookup(key)?;
            // Empty is absent: every surface treats "not set" as its own
            // no-surface signal, and a blank string would read as set.
            (!value.is_empty()).then(|| ((*key).to_string(), value))
        })
        .collect()
}

/// Every surface this build knows about, **innermost first**.
///
/// Adding a surface is adding a file, a line here and an arm in `build`. That
/// is the whole point of the seam: no existing implementation is edited, and
/// nothing else in the daemon learns a new name.
pub const ORDER: &[(&str, Detects)] = &[
    ("tmux", tmux::detects),
    // The other multiplexer. Nested inside tmux or around it, the two cannot
    // be told apart by environment alone; tmux keeps its place.
    ("zellij", zellij::detects),
    // The emulators. Their variables can leak — kitty started from a WezTerm
    // shell keeps WEZTERM_* — so each `detects` also checks `TERM`, which
    // the emulator the agent is really in always sets.
    ("wezterm", wezterm::detects),
    ("kitty", kitty::detects),
    // Terminals that cannot split, only open a window: after every one that
    // can, so a split is always preferred. Their variables leak too, so each
    // checks `TERM`; GNOME Terminal is last because its windows come from a
    // server and its variables are the ones found in other terminals.
    ("ghostty", ghostty::detects),
    ("alacritty", alacritty::detects),
    ("gnome-terminal", gnome_terminal::detects),
    // After everything: a WSL shell's variables are in every terminal
    // started from it, and none of this one's can prove otherwise.
    ("windows-terminal", windows_terminal::detects),
];

/// Whether a recorded environment describes a given surface.
pub type Detects = fn(&BTreeMap<String, String>) -> bool;

/// The ids of every surface the recorded environment could support, innermost
/// first.
///
/// Split out from construction so the ordering rule — the part that is
/// actually easy to get wrong — is testable without spawning a process or
/// owning a terminal.
pub fn detected(env: &BTreeMap<String, String>) -> Vec<&'static str> {
    ORDER
        .iter()
        .filter(|(_, detects)| detects(env))
        .map(|(id, _)| *id)
        .collect()
}

/// Which surface to use, or `None` for no surface at all.
///
/// **Most-nested wins.** An agent running in tmux inside kitty gets a tmux
/// pane: the head must appear in the container the human is actually looking
/// at, and a kitty window would open beside the tmux client rather than
/// inside it.
///
/// An explicit `THIRDEYE_SURFACE` outranks detection entirely, including
/// `none`. When it names a surface that is not present the answer is `None`
/// rather than the detected one — silently ignoring the override would leave
/// the human debugging the wrong thing.
pub fn chosen(env: &BTreeMap<String, String>) -> Option<&'static str> {
    let available = detected(env);
    match env.get("THIRDEYE_SURFACE").map(String::as_str) {
        Some("none") => None,
        Some(named) => available.into_iter().find(|id| *id == named),
        None => available.into_iter().next(),
    }
}

/// The surface for this session.
///
/// `NullSurface` when nothing matches — a daemon with no surface still tails,
/// groups and serves heads (§12). It simply never opens one unasked.
pub fn resolve(env: &BTreeMap<String, String>, session: &str) -> Box<dyn Surface> {
    // `detects` said the environment describes this surface; `available` asks
    // whether the program is installed, which is a different question and can
    // still be no.
    chosen(env)
        .and_then(|id| build(id, env, session))
        .filter(|s| s.available())
        .unwrap_or_else(|| Box::new(NullSurface::default()))
}

/// Construct the surface registered as `id`, driving real processes.
fn build(id: &str, env: &BTreeMap<String, String>, session: &str) -> Option<Box<dyn Surface>> {
    match id {
        "tmux" => tmux::TmuxSurface::from_env(env, session, runner::ProcessRunner::for_env(env))
            .map(|s| Box::new(s) as Box<dyn Surface>),
        "zellij" => {
            zellij::ZellijSurface::from_env(env, session, runner::ProcessRunner::for_env(env))
                .map(|s| Box::new(s) as Box<dyn Surface>)
        }
        "ghostty" => ghostty::from_env(env, session, runner::ProcessRunner::for_env(env))
            .map(|s| Box::new(s) as Box<dyn Surface>),
        "alacritty" => alacritty::from_env(env, session, runner::ProcessRunner::for_env(env))
            .map(|s| Box::new(s) as Box<dyn Surface>),
        "gnome-terminal" => {
            gnome_terminal::from_env(env, session, runner::ProcessRunner::for_env(env))
                .map(|s| Box::new(s) as Box<dyn Surface>)
        }
        "windows-terminal" => {
            windows_terminal::from_env(env, session, runner::ProcessRunner::for_env(env))
                .map(|s| Box::new(s) as Box<dyn Surface>)
        }
        "kitty" => kitty::KittySurface::from_env(env, session, runner::ProcessRunner::for_env(env))
            .map(|s| Box::new(s) as Box<dyn Surface>),
        "wezterm" => {
            wezterm::WeztermSurface::from_env(env, session, runner::ProcessRunner::for_env(env))
                .map(|s| Box::new(s) as Box<dyn Surface>)
        }
        _ => None,
    }
}

/// The share of the agent's pane a split gives the head, in percent:
/// `THIRDEYE_SPLIT` as the hook recorded it, 40 by default, kept between 10
/// and 90 so neither side vanishes. One setting for every surface that can
/// size a split; kitty cannot without knowing the user's layout, whose
/// meaning of a size it would get wrong, so it follows the layout.
pub fn split_percent(env: &BTreeMap<String, String>) -> u8 {
    env.get("THIRDEYE_SPLIT")
        .and_then(|v| v.trim().trim_end_matches('%').parse::<u8>().ok())
        .map_or(DEFAULT_SPLIT_PERCENT, |p| p.clamp(10, 90))
}

const DEFAULT_SPLIT_PERCENT: u8 = 40;

/// Whether the surface registered as `id` opens windows rather than splits —
/// asked without building it, for `doctor`.
pub fn resolve_detached(id: &str) -> bool {
    matches!(
        id,
        "ghostty" | "alacritty" | "gnome-terminal" | "windows-terminal"
    )
}

/// What the daemon wants shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewRequest {
    /// The session whose head this is. Surfaces tag their windows with it so
    /// they can find them again without guessing at titles.
    pub session: String,
    /// Workspace root, so the head starts somewhere sensible.
    pub workspace: std::path::PathBuf,
    /// The program that is the head — see `head_program`.
    pub head: std::path::PathBuf,
}

/// The head a surface should launch: this very binary, by its full path.
///
/// Not a bare `thirdeye`, because the name is resolved with a `PATH` that is
/// neither the human's nor reliably the terminal's. tmux resolves a pane's
/// command with the `PATH` of the client that asked for the split — the
/// daemon — and a systemd user manager's has no `~/.local/bin` unless a
/// login session exported one into it, which WSL's never does. The pane then
/// died with 127 behind a daemon that believed it open. The full path also
/// makes the head the same build as the daemon it talks to.
///
/// Falls back to the bare name when the path names no file: reinstalling
/// over a running daemon leaves it with `… (deleted)`.
pub fn head_program() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .filter(|exe| exe.is_file())
        .unwrap_or_else(|| "thirdeye".into())
}

/// Somewhere a head can be displayed.
pub trait Surface: Debug + Send {
    /// Stable name, as `THIRDEYE_SURFACE` spells it and as `doctor` reports
    /// it. A surface the user cannot name is one they cannot override.
    fn id(&self) -> &'static str;

    /// Whether this surface can be used at all right now.
    ///
    /// Checked before opening rather than discovered by a failed open: "kitty
    /// is not running" is a normal state, not an error to report to the user.
    fn available(&self) -> bool;

    /// Show a head. Idempotent — asking twice must not open two.
    fn open(&mut self, request: &ViewRequest) -> Result<()>;

    /// Hide it. Idempotent, and must not fail merely because it was not open.
    fn close(&mut self) -> Result<()>;

    fn is_open(&self) -> bool;

    /// Whether this surface opens a window it cannot close: a new terminal
    /// window or tab, as opposed to a split it can kill by id.
    ///
    /// Such a window closes when its command exits, so closing it is the
    /// daemon hanging up on the head it launched (`close` then only forgets
    /// it). And it is not opened unasked by default: a window appearing
    /// over the agent mid-turn is an interruption a split beside it is not.
    fn detached(&self) -> bool {
        false
    }
}

/// So a resolved surface can be held as a trait object without the daemon
/// becoming generic over a box.
impl Surface for Box<dyn Surface> {
    fn id(&self) -> &'static str {
        (**self).id()
    }
    fn available(&self) -> bool {
        (**self).available()
    }
    fn open(&mut self, request: &ViewRequest) -> Result<()> {
        (**self).open(request)
    }
    fn close(&mut self) -> Result<()> {
        (**self).close()
    }
    fn is_open(&self) -> bool {
        (**self).is_open()
    }
    fn detached(&self) -> bool {
        (**self).detached()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_split_share_is_one_setting_kept_within_bounds() {
        let with = |v: &str| {
            let mut env = BTreeMap::new();
            env.insert("THIRDEYE_SPLIT".to_string(), v.to_string());
            split_percent(&env)
        };
        assert_eq!(split_percent(&BTreeMap::new()), 40);
        assert_eq!(with("30"), 30);
        assert_eq!(with("55%"), 55);
        assert_eq!(with("100"), 90, "the agent's side never vanishes");
        assert_eq!(with("2"), 10);
        assert_eq!(with("wide"), 40, "nonsense is the default, not an error");
    }

    #[test]
    fn zellij_is_found_from_the_variables_the_hook_records() {
        let env = capture(|key| match key {
            "ZELLIJ_SESSION_NAME" => Some("work".into()),
            "ZELLIJ_PANE_ID" => Some("3".into()),
            _ => None,
        });
        assert_eq!(chosen(&env), Some("zellij"));
    }

    #[test]
    fn capturing_takes_only_what_a_surface_declared() {
        // The log already holds prompt text and every path the agent touched.
        // Copying the agent's whole environment beside it would add API keys
        // and tokens to that pile for no reader's benefit.
        let env = capture(|key| match key {
            "TMUX" => Some("/tmp/tmux-1000/default,7,0".into()),
            "AWS_SECRET_ACCESS_KEY" => Some("hunter2".into()),
            _ => None,
        });
        assert_eq!(
            env.get("TMUX").map(String::as_str),
            Some("/tmp/tmux-1000/default,7,0")
        );
        assert!(!env.contains_key("AWS_SECRET_ACCESS_KEY"));
    }

    #[test]
    fn an_empty_variable_is_absent_rather_than_present_and_blank() {
        // Every surface treats "not set" as its own no-surface signal (§12).
        // An empty string that survived to the map would read as "set", and
        // the daemon would try to drive a terminal that is not there.
        let env = capture(|key| match key {
            "KITTY_LISTEN_ON" => Some(String::new()),
            _ => None,
        });
        assert!(env.is_empty());
    }

    fn env_of(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn a_multiplexer_beats_the_emulator_hosting_it() {
        // An agent in tmux inside kitty must get a tmux pane. A kitty window
        // would open *outside* the tmux client the human is looking at, and
        // over SSH kitty's socket is not reachable from the host the agent is
        // running on at all — while tmux's is.
        let env = env_of(&[
            ("TMUX", "/tmp/tmux-1000/default,7,0"),
            ("TMUX_PANE", "%3"),
            ("KITTY_LISTEN_ON", "unix:/tmp/kitty-4242"),
            ("WEZTERM_UNIX_SOCKET", "/run/user/1000/wezterm/gui-sock-42"),
            ("WEZTERM_PANE", "3"),
        ]);
        assert_eq!(detected(&env), vec!["tmux", "wezterm", "kitty"]);
        assert_eq!(chosen(&env), Some("tmux"));
    }

    #[test]
    fn an_agent_in_no_known_container_detects_nothing() {
        assert!(detected(&BTreeMap::new()).is_empty());
        assert_eq!(chosen(&BTreeMap::new()), None);
    }

    #[test]
    fn an_explicit_choice_outranks_detection() {
        // Someone who sets THIRDEYE_SURFACE is saying they know better, which
        // is the only reason to type it.
        let env = env_of(&[
            ("THIRDEYE_SURFACE", "kitty"),
            ("TMUX", "/tmp/tmux-1000/default,7,0"),
            ("TMUX_PANE", "%3"),
            ("KITTY_LISTEN_ON", "unix:/tmp/kitty-4242"),
        ]);
        assert_eq!(chosen(&env), Some("kitty"));
    }

    #[test]
    fn naming_a_surface_that_is_not_there_opens_nothing() {
        // Falling back to the detected surface would silently ignore the
        // override, and the human would be debugging the wrong thing.
        let env = env_of(&[
            ("THIRDEYE_SURFACE", "kitty"),
            ("TMUX", "/tmp/tmux-1000/default,7,0"),
            ("TMUX_PANE", "%3"),
        ]);
        assert_eq!(chosen(&env), None);
    }

    #[test]
    fn a_human_can_turn_the_surface_off_from_inside_one() {
        let env = env_of(&[
            ("THIRDEYE_SURFACE", "none"),
            ("TMUX", "/tmp/tmux-1000/default,7,0"),
            ("TMUX_PANE", "%3"),
        ]);
        assert_eq!(chosen(&env), None);
    }

    #[test]
    fn every_registered_surface_has_a_distinct_id() {
        // The id is what THIRDEYE_SURFACE matches on; a collision would make
        // one surface silently answer for another.
        let mut ids: Vec<&str> = ORDER.iter().map(|(id, _)| *id).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count);
    }

    #[test]
    fn a_registered_id_is_the_id_its_surface_reports() {
        // The registry and the surface each spell the name. If they drifted,
        // THIRDEYE_SURFACE would match one spelling while doctor printed the
        // other.
        let env = env_of(&[
            ("TMUX", "/tmp/tmux-1000/default,7,0"),
            ("TMUX_PANE", "%3"),
            ("KITTY_LISTEN_ON", "unix:/tmp/kitty-4242"),
            ("WEZTERM_UNIX_SOCKET", "/run/user/1000/wezterm/gui-sock-42"),
            ("WEZTERM_PANE", "3"),
            ("ZELLIJ_SESSION_NAME", "work"),
            ("ZELLIJ_PANE_ID", "3"),
        ]);
        let window_env = env_of(&[("TERM", "xterm-ghostty")]);
        for (id, _) in ORDER {
            let built = build(id, &env, "s1")
                .or_else(|| build(id, &window_env, "s1"))
                .or_else(|| {
                    let alacritty = env_of(&[("ALACRITTY_SOCKET", "/s"), ("TERM", "alacritty")]);
                    build(id, &alacritty, "s1")
                })
                .or_else(|| {
                    let gnome = env_of(&[
                        ("GNOME_TERMINAL_SERVICE", ":1.8"),
                        ("GNOME_TERMINAL_SCREEN", "/s"),
                        ("TERM", "xterm-256color"),
                    ]);
                    build(id, &gnome, "s1")
                })
                .or_else(|| {
                    let wt = env_of(&[
                        ("WT_SESSION", "237c"),
                        ("WSL_DISTRO_NAME", "Ubuntu"),
                        ("TERM", "xterm-256color"),
                    ]);
                    build(id, &wt, "s1")
                })
                .expect("every registered id builds");
            assert_eq!(built.id(), *id);
            assert_eq!(built.detached(), resolve_detached(id), "{id}");
        }
    }
}
