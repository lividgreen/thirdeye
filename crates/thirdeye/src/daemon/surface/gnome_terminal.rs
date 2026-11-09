//! Showing the head in a new GNOME Terminal window.
//!
//! GNOME Terminal cannot split. `gnome-terminal --window` asks the
//! terminal server — the one the agent's window belongs to, named by the
//! recorded `GNOME_TERMINAL_SERVICE` — for a new window. A window, not a
//! tab: a tab would take the agent's place on screen mid-turn. Verified
//! against 3.52 (`docs/windows.md`).
//!
//! The window closes when the head exits only while the profile's "when
//! command exits" is "exit the terminal", the default; `doctor` says when
//! it is not.

use std::collections::BTreeMap;

use super::runner::Runner;
use super::window::{WORKSPACE, WindowSurface};

/// Both variables, and VTE's `TERM`. They leak into terminals started from
/// its shells; GNOME Terminal's own windows come from its server and never
/// inherit another terminal's.
pub fn detects(env: &BTreeMap<String, String>) -> bool {
    service(env).is_some()
        && env
            .get("GNOME_TERMINAL_SCREEN")
            .is_some_and(|s| !s.is_empty())
        && env.get("TERM").is_some_and(|t| t == "xterm-256color")
}

fn service(env: &BTreeMap<String, String>) -> Option<&String> {
    env.get("GNOME_TERMINAL_SERVICE").filter(|s| !s.is_empty())
}

pub fn from_env<R: Runner>(
    env: &BTreeMap<String, String>,
    session: &str,
    runner: R,
) -> Option<WindowSurface<R>> {
    if !detects(env) {
        return None;
    }
    // Through `env`: the client finds its server in this variable, and the
    // daemon, started by systemd, has none of the agent's environment.
    let args = [
        format!("GNOME_TERMINAL_SERVICE={}", service(env)?),
        "gnome-terminal".into(),
        "--window".into(),
        "--title=thirdeye".into(),
        format!("--working-directory={WORKSPACE}"),
        "--".into(),
    ];
    Some(WindowSurface::new(
        "gnome-terminal",
        "gnome-terminal",
        ("env".into(), args.to_vec()),
        session,
        runner,
    ))
}

#[cfg(test)]
mod tests {
    use super::super::Surface;
    use super::super::window::testing::*;
    use super::*;

    fn env() -> BTreeMap<String, String> {
        env_of(&[
            ("GNOME_TERMINAL_SERVICE", ":1.811"),
            ("GNOME_TERMINAL_SCREEN", "/org/gnome/Terminal/screen/f5c5"),
            ("TERM", "xterm-256color"),
        ])
    }

    #[test]
    fn the_window_opens_from_the_agents_terminal_server() {
        let r = Recorder::default();
        let mut g = from_env(&env(), "s1", r.clone()).unwrap();
        g.open(&request()).unwrap();
        assert_eq!(
            r.calls.lock().unwrap()[0],
            "env GNOME_TERMINAL_SERVICE=:1.811 gnome-terminal --window --title=thirdeye \
             --working-directory=/home/u/app -- thirdeye head --launched-by-surface --session s1"
        );
    }

    #[test]
    fn its_variables_leaked_into_another_terminal_are_not_gnome_terminal() {
        let mut leaked = env();
        leaked.insert("TERM".into(), "xterm-ghostty".into());
        assert!(!detects(&leaked));
    }
}
