//! Showing the head in a kitty split.
//!
//! kitty is a *surface*, not a head: it is **where** a viewer is displayed,
//! while the ratatui TUI is **what** the viewer is. A GUI head would make its
//! own window and need no surface at all, which is why the two are separate
//! axes (§5).
//!
//! Everything here is one `kitten @` invocation or another. That is the whole
//! implementation, and it is also the whole risk: the commands can only be
//! *proved* against a running kitty. So the process spawning is injected —
//! the tests below assert the exact argv, and a human runs the real thing
//! (see `docs/kitty.md`).

use std::collections::BTreeMap;
use std::fmt::Debug;

use anyhow::Result;

use super::runner::Runner;
use super::{Surface, ViewRequest};

/// Marks the window as ours, so it can be found again without guessing at
/// titles a human may have changed.
const MARKER: &str = "THIRDEYE_SESSION";

/// Whether the recorded environment names a kitty instance to drive.
///
/// §12 makes the absence of the address exactly the "kitty is not available"
/// signal — a normal state, not an error to report.
pub fn detects(env: &BTreeMap<String, String>) -> bool {
    env.get("KITTY_LISTEN_ON").is_some_and(|v| !v.is_empty())
}

#[derive(Debug)]
pub struct KittySurface<R> {
    /// `$KITTY_LISTEN_ON`, recorded by the hook. Which kitty to drive.
    listen_on: String,
    session: String,
    runner: R,
    open: bool,
}

impl<R: Runner> KittySurface<R> {
    pub fn new(listen_on: impl Into<String>, session: impl Into<String>, runner: R) -> Self {
        Self {
            listen_on: listen_on.into(),
            session: session.into(),
            runner,
            open: false,
        }
    }

    /// `kitten @ --to <addr> launch …`
    fn launch_args(&self, request: &ViewRequest) -> Vec<String> {
        vec![
            "@".into(),
            // Explicit rather than inherited: the daemon is socket-activated
            // by systemd and has none of the agent's environment, so there is
            // no ambient kitty for `kitten` to find.
            "--to".into(),
            self.listen_on.clone(),
            "launch".into(),
            "--type=window".into(),
            // The split must not steal focus: the human is typing at the
            // agent, and a viewer that grabs the cursor mid-sentence is worse
            // than no viewer.
            "--keep-focus".into(),
            format!("--cwd={}", request.workspace.display()),
            "--title=thirdeye".into(),
            "--env".into(),
            format!("{MARKER}={}", self.session),
            request.head.display().to_string(),
            "head".into(),
            // So the daemon reads this head's leaving as the window closing.
            "--launched-by-surface".into(),
            "--session".into(),
            self.session.clone(),
        ]
    }

    /// `kitten @ --to <addr> close-window --match env:…`
    ///
    /// Matched on our own marker rather than on the title, which the human is
    /// free to change and which another window could share.
    fn close_args(&self) -> Vec<String> {
        vec![
            "@".into(),
            "--to".into(),
            self.listen_on.clone(),
            "close-window".into(),
            "--match".into(),
            format!("env:{MARKER}={}", self.session),
        ]
    }
}

impl<R: Runner> KittySurface<R> {
    pub fn from_env(env: &BTreeMap<String, String>, session: &str, runner: R) -> Option<Self> {
        let listen_on = env.get("KITTY_LISTEN_ON")?;
        (!listen_on.is_empty()).then(|| Self::new(listen_on.clone(), session, runner))
    }
}

impl<R: Runner + Debug> Surface for KittySurface<R> {
    fn id(&self) -> &'static str {
        "kitty"
    }

    fn available(&self) -> bool {
        // An empty address means the agent was not started inside kitty. §12
        // makes that exactly the "no surface" signal, not an error.
        !self.listen_on.is_empty() && self.runner.available("kitten")
    }

    fn open(&mut self, request: &ViewRequest) -> Result<()> {
        if self.open {
            return Ok(());
        }
        // kitty prints the new window's id, but closing matches on our own
        // marker instead, so there is nothing here worth keeping.
        self.runner.run("kitten", &self.launch_args(request))?;
        self.open = true;
        Ok(())
    }

    fn close(&mut self) -> Result<()> {
        if !self.open {
            return Ok(());
        }
        // Marked closed before the call succeeds: if the window is already
        // gone — the human closed it — retrying forever is worse than
        // believing it closed, and the next open will simply launch again.
        self.open = false;
        self.runner.run("kitten", &self.close_args()).map(|_| ())
    }

    fn is_open(&self) -> bool {
        self.open
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::bail;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Default, Clone)]
    struct Recorder {
        calls: Arc<Mutex<Vec<Vec<String>>>>,
        available: bool,
        fail: bool,
    }

    impl Recorder {
        fn working() -> Self {
            Self {
                available: true,
                ..Default::default()
            }
        }
        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Runner for Recorder {
        fn run(&self, program: &str, args: &[String]) -> Result<String> {
            let mut call = vec![program.to_string()];
            call.extend(args.iter().cloned());
            self.calls.lock().unwrap().push(call);
            if self.fail {
                bail!("planted failure");
            }
            Ok(String::new())
        }
        fn available(&self, _program: &str) -> bool {
            self.available
        }
    }

    fn request() -> ViewRequest {
        ViewRequest {
            session: "s1".into(),
            workspace: "/home/u/app".into(),
            head: "thirdeye".into(),
        }
    }

    fn surface(runner: Recorder) -> KittySurface<Recorder> {
        KittySurface::new("unix:/tmp/kitty-4242", "s1", runner)
    }

    #[test]
    fn launching_names_the_instance_the_window_and_the_marker() {
        let recorder = Recorder::working();
        let mut kitty = surface(recorder.clone());
        kitty.open(&request()).unwrap();

        let call = &recorder.calls()[0];
        let joined = call.join(" ");
        assert!(
            joined.starts_with("kitten @ --to unix:/tmp/kitty-4242 launch"),
            "{joined}"
        );
        assert!(joined.contains("--type=window"), "{joined}");
        assert!(joined.contains("--cwd=/home/u/app"), "{joined}");
        assert!(joined.contains("--env THIRDEYE_SESSION=s1"), "{joined}");
        // The command the window runs, and the session it is for.
        assert!(
            joined.ends_with("thirdeye head --launched-by-surface --session s1"),
            "{joined}"
        );
    }

    #[test]
    fn the_split_never_steals_focus() {
        // The human is typing at the agent. A viewer that grabs the cursor
        // mid-sentence is worse than no viewer at all.
        let recorder = Recorder::working();
        surface(recorder.clone()).open(&request()).unwrap();
        assert!(recorder.calls()[0].iter().any(|a| a == "--keep-focus"));
    }

    #[test]
    fn closing_matches_our_own_marker_not_the_title() {
        // A human may rename a window, and another window could share a
        // title. The env marker is ours and nobody else's.
        let recorder = Recorder::working();
        let mut kitty = surface(recorder.clone());
        kitty.open(&request()).unwrap();
        kitty.close().unwrap();

        let joined = recorder.calls()[1].join(" ");
        assert!(joined.contains("close-window"), "{joined}");
        assert!(
            joined.contains("--match env:THIRDEYE_SESSION=s1"),
            "{joined}"
        );
    }

    #[test]
    fn opening_twice_launches_one_window() {
        // `refresh` asks on every event, so open is called far more often
        // than it transitions.
        let recorder = Recorder::working();
        let mut kitty = surface(recorder.clone());
        kitty.open(&request()).unwrap();
        kitty.open(&request()).unwrap();
        assert_eq!(recorder.calls().len(), 1);
    }

    #[test]
    fn closing_something_never_opened_runs_nothing() {
        let recorder = Recorder::working();
        let mut kitty = surface(recorder.clone());
        kitty.close().unwrap();
        assert!(recorder.calls().is_empty());
    }

    #[test]
    fn an_agent_started_outside_kitty_reports_no_surface() {
        // §12: the absence of KITTY_LISTEN_ON *is* the signal. Not an error,
        // and not something to report to the user.
        let kitty = KittySurface::new("", "s1", Recorder::working());
        assert!(!kitty.available());
    }

    #[test]
    fn a_missing_kitten_binary_reports_no_surface() {
        let kitty = KittySurface::new(
            "unix:/tmp/kitty-1",
            "s1",
            Recorder {
                available: false,
                ..Default::default()
            },
        );
        assert!(!kitty.available());
    }

    #[test]
    fn a_launch_that_fails_leaves_the_surface_closed() {
        // Otherwise the daemon believes a window exists, never retries, and
        // the human sees nothing for the rest of the session.
        let recorder = Recorder {
            available: true,
            fail: true,
            ..Default::default()
        };
        let mut kitty = surface(recorder);
        assert!(kitty.open(&request()).is_err());
        assert!(!kitty.is_open(), "a failed launch must not look open");
    }

    #[test]
    fn a_window_the_human_already_closed_does_not_trap_the_surface() {
        // `close-window` fails when the window is gone. Refusing to update
        // state would mean retrying forever and never reopening.
        let recorder = Recorder {
            available: true,
            fail: false,
            ..Default::default()
        };
        let mut kitty = surface(recorder.clone());
        kitty.open(&request()).unwrap();

        let failing = Recorder {
            available: true,
            fail: true,
            calls: recorder.calls.clone(),
        };
        let mut kitty = KittySurface {
            listen_on: kitty.listen_on.clone(),
            session: kitty.session.clone(),
            runner: failing,
            open: true,
        };
        assert!(kitty.close().is_err());
        assert!(!kitty.is_open(), "must not stay stuck open");
    }
}
