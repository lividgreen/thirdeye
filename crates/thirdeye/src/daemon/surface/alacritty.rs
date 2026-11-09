//! Showing the head in a new Alacritty window.
//!
//! Alacritty has no splits, but its IPC socket can open a window in the
//! instance the agent runs in: `alacritty msg --socket <s> create-window`.
//! Verified against 0.13.2 (`docs/windows.md`).

use std::collections::BTreeMap;

use super::runner::Runner;
use super::window::{WORKSPACE, WindowSurface};

/// The instance's socket, and a `TERM` Alacritty sets. Its variables leak
/// into terminals started from its shells, which set `TERM` afresh.
pub fn detects(env: &BTreeMap<String, String>) -> bool {
    socket(env).is_some()
        && env
            .get("TERM")
            .is_some_and(|t| t == "alacritty" || t == "xterm-256color")
}

fn socket(env: &BTreeMap<String, String>) -> Option<&String> {
    env.get("ALACRITTY_SOCKET").filter(|s| !s.is_empty())
}

pub fn from_env<R: Runner>(
    env: &BTreeMap<String, String>,
    session: &str,
    runner: R,
) -> Option<WindowSurface<R>> {
    if !detects(env) {
        return None;
    }
    let args = [
        "msg".to_string(),
        "--socket".into(),
        socket(env)?.clone(),
        "create-window".into(),
        "--working-directory".into(),
        WORKSPACE.into(),
        "-T".into(),
        "thirdeye".into(),
        "-e".into(),
    ];
    Some(WindowSurface::new(
        "alacritty",
        "alacritty",
        ("alacritty".into(), args.to_vec()),
        session,
        runner,
    ))
}

#[cfg(test)]
mod tests {
    use super::super::Surface;
    use super::super::window::testing::*;
    use super::*;

    const SOCK: &str = "/run/user/1000/Alacritty-wayland-0-526150.sock";

    #[test]
    fn the_window_opens_in_the_agents_instance() {
        let r = Recorder::default();
        let env = env_of(&[("ALACRITTY_SOCKET", SOCK), ("TERM", "alacritty")]);
        let mut a = from_env(&env, "s1", r.clone()).unwrap();
        a.open(&request()).unwrap();
        assert_eq!(
            r.calls.lock().unwrap()[0],
            format!(
                "alacritty msg --socket {SOCK} create-window --working-directory /home/u/app \
                 -T thirdeye -e thirdeye head --launched-by-surface --session s1"
            )
        );
    }

    #[test]
    fn its_socket_leaked_into_another_terminal_is_not_alacritty() {
        assert!(!detects(&env_of(&[
            ("ALACRITTY_SOCKET", SOCK),
            ("TERM", "xterm-ghostty")
        ])));
        assert!(!detects(&env_of(&[("TERM", "alacritty")])));
    }
}
