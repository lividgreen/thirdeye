//! Showing the head in a new Ghostty window.
//!
//! Ghostty has no scripting interface on Linux yet (promised for 1.4) and
//! no variable naming the agent's own surface, so it cannot split beside
//! the agent. It can open a window: `ghostty +new-window` asks the running
//! instance for one, over D-Bus, without starting a second process.
//! Verified against 1.3.1 (`docs/windows.md`).

use std::collections::BTreeMap;

use super::runner::Runner;
use super::window::{WORKSPACE, WindowSurface};

/// `TERM=xterm-ghostty`, which Ghostty sets fresh in every surface. Not
/// `TERM_PROGRAM`: Alacritty, for one, passes on the `TERM_PROGRAM=ghostty`
/// of the shell it was started from.
pub fn detects(env: &BTreeMap<String, String>) -> bool {
    env.get("TERM").is_some_and(|t| t == "xterm-ghostty")
}

pub fn from_env<R: Runner>(
    env: &BTreeMap<String, String>,
    session: &str,
    runner: R,
) -> Option<WindowSurface<R>> {
    detects(env).then(|| {
        let args = [
            "+new-window".to_string(),
            format!("--working-directory={WORKSPACE}"),
            "--title=thirdeye".into(),
            "-e".into(),
        ];
        WindowSurface::new(
            "ghostty",
            "ghostty",
            ("ghostty".into(), args.to_vec()),
            session,
            runner,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::super::Surface;
    use super::super::window::testing::*;
    use super::*;

    #[test]
    fn a_window_is_asked_of_the_running_instance() {
        let r = Recorder::default();
        let mut g = from_env(&env_of(&[("TERM", "xterm-ghostty")]), "s1", r.clone()).unwrap();
        g.open(&request()).unwrap();
        assert_eq!(
            r.calls.lock().unwrap()[0],
            "ghostty +new-window --working-directory=/home/u/app --title=thirdeye -e \
             thirdeye head --launched-by-surface --session s1"
        );
    }

    #[test]
    fn a_leaked_term_program_is_not_ghostty() {
        assert!(!detects(&env_of(&[
            ("TERM", "xterm-256color"),
            ("TERM_PROGRAM", "ghostty")
        ])));
    }
}
