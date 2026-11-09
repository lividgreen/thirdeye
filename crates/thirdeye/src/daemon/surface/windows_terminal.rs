//! Showing the head in a new Windows Terminal window, from WSL or natively.
//!
//! Windows Terminal can split, but not where it is told: `wt.exe split-pane`
//! splits whichever pane has focus — in another tab, in another window —
//! and gives the new pane the focus. A viewer that follows the human rather
//! than the agent is wrong exactly when the agent works unattended, so this
//! is a window, opened with `wt.exe -w new`, like the terminals that cannot
//! split at all. Captured against 1.24 (`docs/windows-terminal.md`).
//!
//! From WSL the head is a Linux program and `wt.exe` starts Windows ones, so
//! the window runs `wsl.exe -d <distro> --cd <workspace> -e <head> …`. On
//! Windows itself the head is a Windows program, and the window runs it.

use std::collections::BTreeMap;

use anyhow::{Result, bail};

use super::runner::Runner;
use super::window::{WORKSPACE, WindowSurface};

/// Which side of Windows the agent, and so this daemon, runs on.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Host {
    /// A WSL distribution, which `wsl.exe` must enter to run the head.
    Wsl(String),
    Windows,
}

/// A WSL shell in a Windows Terminal pane. Both variables leak into any
/// terminal started from that shell — a kitty or Ghostty under WSLg keeps
/// them — which is why this is the last surface tried.
pub fn detects(env: &BTreeMap<String, String>) -> bool {
    host(env, cfg!(windows)).is_some()
}

/// `native` is whether this build is a Windows one: a fact about the daemon,
/// which runs where the agent does, and not something the environment says.
fn host(env: &BTreeMap<String, String>, native: bool) -> Option<Host> {
    env.get("WT_SESSION").filter(|s| !s.is_empty())?;
    if native {
        // No `TERM` to check: only a hook run through Git Bash has one.
        return Some(Host::Windows);
    }
    let distro = env.get("WSL_DISTRO_NAME").filter(|s| !s.is_empty())?;
    env.get("TERM")
        .is_some_and(|t| t == "xterm-256color")
        .then(|| Host::Wsl(distro.clone()))
}

pub fn from_env<R: Runner>(
    env: &BTreeMap<String, String>,
    session: &str,
    runner: R,
) -> Option<WindowSurface<R>> {
    build(host(env, cfg!(windows))?, session, runner)
}

fn build<R: Runner>(host: Host, session: &str, runner: R) -> Option<WindowSurface<R>> {
    let mut args: Vec<String> = [
        // Never `0`, the most recently used window: that is the one the
        // human is in, which need not be the agent's.
        "-w", "new", "new-tab", "--title", "thirdeye",
    ]
    .map(String::from)
    .to_vec();
    let encode = match &host {
        Host::Wsl(distro) => {
            args.extend(["wsl.exe".into(), "-d".into(), encode_from_wsl(distro).ok()?]);
            args.extend(["--cd".into(), WORKSPACE.into()]);
            // Exec, not the login shell: an argv again once inside WSL.
            args.push("-e".into());
            encode_from_wsl
        }
        Host::Windows => {
            args.extend(["-d".into(), WORKSPACE.into()]);
            encode_native
        }
    };
    Some(
        WindowSurface::new(
            "windows-terminal",
            "wt.exe",
            ("wt.exe".into(), args),
            session,
            runner,
        )
        .encoding(encode),
    )
}

/// `wt.exe` reads its arguments as a command line of its own.
///
/// A `;` ends one command and starts the next — a session id of
/// `s1; new-tab calc.exe` would run it — unless written `\;`. A `"` or a
/// `\` cannot be written at all: the arguments are joined into one Windows
/// command line on the way, where a quote moved the boundaries between them
/// (measured: `q"q`, `back\slash` and `$HOME` arrived as one argument). So
/// those are refused, and the session then has no window rather than a
/// wrong command.
fn encode_from_wsl(arg: &str) -> Result<String> {
    if arg.contains(['"', '\\']) {
        bail!("{arg:?} cannot be passed through wt.exe");
    }
    Ok(arg.replace(';', r"\;"))
}

/// The same parser, reached from a Windows process, which writes the command
/// line itself: a `\` then arrives as it left, and every path here has one.
///
/// A `"` still does not (measured: `q"q` and the argument after it arrived
/// as `qq tail`), and `%NAME%` is replaced with that variable's value, so
/// both are refused. So is a `\` before a `;`, which nothing measured tells
/// apart from the escape.
fn encode_native(arg: &str) -> Result<String> {
    if arg.contains('"') || arg.contains(r"\;") || arg.matches('%').count() > 1 {
        bail!("{arg:?} cannot be passed through wt.exe");
    }
    Ok(arg.replace(';', r"\;"))
}

#[cfg(test)]
mod tests {
    use super::super::window::testing::*;
    use super::super::{Surface, ViewRequest};
    use super::*;

    fn env() -> BTreeMap<String, String> {
        env_of(&[
            ("WT_SESSION", "237ccab1-b79e-47bb-8523-07ca0c8eb464"),
            ("WSL_DISTRO_NAME", "Ubuntu"),
            ("TERM", "xterm-256color"),
        ])
    }

    fn from_wsl(session: &str, runner: Recorder) -> WindowSurface<Recorder> {
        build(host(&env(), false).unwrap(), session, runner).unwrap()
    }

    fn native(session: &str, runner: Recorder) -> WindowSurface<Recorder> {
        build(Host::Windows, session, runner).unwrap()
    }

    fn windows_request() -> ViewRequest {
        ViewRequest {
            workspace: r"C:\Users\u\my app".into(),
            head: r"C:\Users\u\.cargo\bin\thirdeye.exe".into(),
            ..request()
        }
    }

    #[test]
    fn the_head_opens_in_a_new_window_through_wsl() {
        let r = Recorder::default();
        let mut w = from_wsl("s1", r.clone());
        w.open(&request()).unwrap();
        assert_eq!(
            r.calls.lock().unwrap()[0],
            "wt.exe -w new new-tab --title thirdeye wsl.exe -d Ubuntu --cd /home/u/app -e \
             thirdeye head --launched-by-surface --session s1"
        );
    }

    #[test]
    fn on_windows_the_window_runs_the_head_itself() {
        // No `wsl.exe` to enter, and paths that are all backslashes — which
        // the WSL side has to refuse and this side must not.
        let r = Recorder::default();
        let mut w = native("s1", r.clone());
        w.open(&windows_request()).unwrap();
        assert_eq!(
            r.calls.lock().unwrap()[0],
            concat!(
                r"wt.exe -w new new-tab --title thirdeye -d C:\Users\u\my app ",
                r"C:\Users\u\.cargo\bin\thirdeye.exe head --launched-by-surface --session s1"
            )
        );
    }

    #[test]
    fn a_semicolon_cannot_start_a_second_wt_command() {
        // wt.exe splits its command line on `;`. Session ids are agent
        // supplied and a workspace is any directory name.
        let r = Recorder::default();
        let mut w = from_wsl("s1; new-tab calc.exe", r.clone());
        let request = ViewRequest {
            workspace: "/home/u/a;b".into(),
            ..request()
        };
        w.open(&request).unwrap();
        let call = r.calls.lock().unwrap()[0].clone();
        assert!(call.contains(r"--cd /home/u/a\;b -e"), "{call}");
        assert!(call.ends_with(r"--session s1\; new-tab calc.exe"), "{call}");
        assert!(!call.replace(r"\;", "").contains(';'), "{call}");

        let r = Recorder::default();
        let mut w = native("s1; new-tab calc.exe", r.clone());
        w.open(&windows_request()).unwrap();
        let call = r.calls.lock().unwrap()[0].clone();
        assert!(call.ends_with(r"--session s1\; new-tab calc.exe"), "{call}");
        assert!(!call.replace(r"\;", "").contains(';'), "{call}");
    }

    #[test]
    fn what_cannot_be_written_for_wt_opens_no_window() {
        // A quote moves the boundaries between arguments on the way through
        // Windows, so there is no safe spelling — and a refused open must
        // not leave the daemon believing in a window.
        for hostile in [r#"s1" x"#, r"s1\"] {
            let r = Recorder::default();
            let mut w = from_wsl(hostile, r.clone());
            assert!(w.open(&request()).is_err(), "{hostile}");
            assert!(!w.is_open());
            assert!(r.calls.lock().unwrap().is_empty());
        }
        // Natively a backslash is only a backslash. What wt.exe would
        // rewrite is still refused: a quote, a variable it would expand, and
        // a backslash where it would read as the escape.
        for hostile in [r#"s1" x"#, "s1%USERPROFILE%", r"s1\;x"] {
            let r = Recorder::default();
            let mut w = native(hostile, r.clone());
            assert!(w.open(&windows_request()).is_err(), "{hostile}");
            assert!(!w.is_open());
            assert!(r.calls.lock().unwrap().is_empty());
        }
        let r = Recorder::default();
        assert!(native("50%", r).open(&windows_request()).is_ok());
    }

    #[test]
    fn windows_terminal_outside_wsl_or_under_another_terminal_is_not_this() {
        let mut no_wsl = env();
        no_wsl.remove("WSL_DISTRO_NAME");
        assert_eq!(host(&no_wsl, false), None);
        let mut leaked = env();
        leaked.insert("TERM".into(), "xterm-ghostty".into());
        assert_eq!(host(&leaked, false), None);
    }

    #[test]
    fn a_windows_build_needs_only_the_terminals_own_variable() {
        // No distribution to enter, and no `TERM` unless the hook happened
        // to run through Git Bash.
        let pane = env_of(&[("WT_SESSION", "237ccab1-b79e-47bb-8523-07ca0c8eb464")]);
        assert_eq!(host(&pane, true), Some(Host::Windows));
        assert_eq!(host(&pane, false), None, "not from a Linux build");
        assert_eq!(host(&env_of(&[("TERM", "xterm-256color")]), true), None);
    }
}
