//! Showing the head in a Zellij pane.
//!
//! The other terminal multiplexer people choose. Like tmux it reaches every
//! emulator and survives SSH. Verified live against 0.45.1 (`docs/zellij.md`).
//!
//! The daemon runs outside Zellij, so every call names the session
//! (`zellij --session <name> action …`), and placement needs one more fact:
//! `new-pane --no-focus` opens "relative to the pane the command was issued
//! from", which Zellij reads from `ZELLIJ_PANE_ID` in the caller's
//! environment. Without it the pane splits whichever pane has focus.

use std::collections::BTreeMap;
use std::fmt::Debug;

use anyhow::{Result, bail};

use super::runner::Runner;
use super::{Surface, ViewRequest};

/// Whether the recorded environment describes a Zellij pane: the session to
/// address and the agent's own pane. A session name alone could be inherited
/// by a process that is in no pane of it.
pub fn detects(env: &BTreeMap<String, String>) -> bool {
    address(env).is_some()
}

fn address(env: &BTreeMap<String, String>) -> Option<(String, String)> {
    let session = env.get("ZELLIJ_SESSION_NAME")?.clone();
    let pane = env.get("ZELLIJ_PANE_ID")?.clone();
    (!session.is_empty() && !pane.is_empty()).then_some((session, pane))
}

/// How far one `resize` step moves a border: measured at about 5% of the
/// window's width on 0.45.1.
const RESIZE_STEP_PERCENT: i32 = 5;

#[derive(Debug)]
pub struct ZellijSurface<R> {
    zellij_session: String,
    /// The agent's pane, as Zellij numbers it (`3` for `terminal_3`).
    pane: String,
    session: String,
    /// Share of the agent's pane the head should take.
    percent: u8,
    runner: R,
    /// The pane we made (`terminal_7`), the only precise handle to close.
    opened: Option<String>,
}

impl<R: Runner> ZellijSurface<R> {
    pub fn from_env(env: &BTreeMap<String, String>, session: &str, runner: R) -> Option<Self> {
        let (zellij_session, pane) = address(env)?;
        Some(Self {
            zellij_session,
            pane,
            session: session.to_string(),
            percent: super::split_percent(env),
            runner,
            opened: None,
        })
    }

    /// `zellij --session <name> action <args…>`.
    fn action(&self, args: &[&str]) -> Vec<String> {
        ["--session", &self.zellij_session, "action"]
            .iter()
            .chain(args)
            .map(|s| s.to_string())
            .collect()
    }

    /// Run with `ZELLIJ_PANE_ID` set: the one variable placement depends on.
    fn new_pane_args(&self, request: &ViewRequest) -> Vec<String> {
        let workspace = request.workspace.display().to_string();
        let head = request.head.display().to_string();
        self.action(&[
            "new-pane",
            // Beside, not below: §10.1 is a side-by-side layout.
            "--direction",
            "right",
            // Never the cursor: the human is typing at the agent.
            "--no-focus",
            // A command pane outlives its command unless told otherwise; the
            // head exiting must take its pane with it, as in tmux.
            "--close-on-exit",
            "--cwd",
            &workspace,
            "--name",
            "thirdeye",
            // An argv, not a shell string: no session id can be parsed as
            // a second command.
            "--",
            &head,
            "head",
            "--launched-by-surface",
            "--session",
            &self.session,
        ])
    }

    /// Zellij splits a pane in half; the steps from there to `percent`.
    fn resize_args(&self, pane: &str) -> Vec<Vec<String>> {
        let delta = 50 - i32::from(self.percent);
        let steps = (delta.abs() + RESIZE_STEP_PERCENT / 2) / RESIZE_STEP_PERCENT;
        let how = if delta > 0 { "decrease" } else { "increase" };
        (0..steps)
            .map(|_| self.action(&["resize", "--pane-id", pane, how, "left"]))
            .collect()
    }
}

impl<R: Runner + Debug> Surface for ZellijSurface<R> {
    fn id(&self) -> &'static str {
        "zellij"
    }

    fn available(&self) -> bool {
        self.runner.available("zellij")
    }

    fn open(&mut self, request: &ViewRequest) -> Result<()> {
        if self.opened.is_some() {
            return Ok(());
        }
        let pane = self.runner.run_with_env(
            &[("ZELLIJ_PANE_ID", &self.pane)],
            "zellij",
            &self.new_pane_args(request),
        )?;
        if !pane.starts_with("terminal_") {
            bail!("zellij new-pane printed no pane id: {pane:?}");
        }
        // Best-effort: a pane at half width is still a viewer.
        for args in self.resize_args(&pane) {
            if self.runner.run("zellij", &args).is_err() {
                break;
            }
        }
        self.opened = Some(pane);
        Ok(())
    }

    fn close(&mut self) -> Result<()> {
        let Some(pane) = self.opened.take() else {
            return Ok(());
        };
        // Forgotten before the call succeeds, as for tmux: closing a pane the
        // human already closed fails, and the next open simply splits again.
        let args = self.action(&["close-pane", "--pane-id", &pane]);
        self.runner.run("zellij", &args).map(|_| ())
    }

    fn is_open(&self) -> bool {
        self.opened.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Default, Clone)]
    struct Recorder {
        calls: Arc<Mutex<Vec<Vec<String>>>>,
        stdout: String,
        fail: bool,
    }

    impl Recorder {
        fn working() -> Self {
            Self {
                stdout: "terminal_7".into(),
                ..Default::default()
            }
        }
        fn calls(&self) -> Vec<String> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .map(|c| c.join(" "))
                .collect()
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
            Ok(self.stdout.clone())
        }
        fn available(&self, _program: &str) -> bool {
            true
        }
    }

    fn env() -> BTreeMap<String, String> {
        [
            ("ZELLIJ_SESSION_NAME", "work"),
            ("ZELLIJ_PANE_ID", "3"),
            ("ZELLIJ", "0"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    fn request() -> ViewRequest {
        ViewRequest {
            session: "s1".into(),
            workspace: "/home/u/app".into(),
            head: "thirdeye".into(),
        }
    }

    fn surface(runner: Recorder) -> ZellijSurface<Recorder> {
        ZellijSurface::from_env(&env(), "s1", runner).unwrap()
    }

    #[test]
    fn the_split_lands_beside_the_agents_own_pane_without_taking_focus() {
        // Verified live: without ZELLIJ_PANE_ID the pane splits whichever
        // pane has focus, which need not be the agent's.
        let mut z = surface(Recorder::working());
        z.open(&request()).unwrap();
        let call = &z.runner.calls()[0];
        assert!(
            call.starts_with("env ZELLIJ_PANE_ID=3 zellij --session work action new-pane --direction right --no-focus"),
            "{call}"
        );
        assert!(call.contains("--cwd /home/u/app"), "{call}");
    }

    #[test]
    fn the_head_runs_as_an_argv_and_its_pane_closes_with_it() {
        let mut z = surface(Recorder::working());
        z.open(&request()).unwrap();
        let call = &z.runner.calls()[0];
        assert!(call.contains("--close-on-exit"), "{call}");
        assert!(
            call.ends_with("-- thirdeye head --launched-by-surface --session s1"),
            "{call}"
        );
    }

    #[test]
    fn the_pane_is_narrowed_from_half_to_the_configured_share() {
        // Default 40%: two 5% steps from Zellij's even split.
        let mut z = surface(Recorder::working());
        z.open(&request()).unwrap();
        let calls = z.runner.calls();
        assert_eq!(
            calls[1..],
            [
                "zellij --session work action resize --pane-id terminal_7 decrease left",
                "zellij --session work action resize --pane-id terminal_7 decrease left",
            ]
        );

        let mut wide = env();
        wide.insert("THIRDEYE_SPLIT".into(), "60".into());
        let mut z = ZellijSurface::from_env(&wide, "s1", Recorder::working()).unwrap();
        z.open(&request()).unwrap();
        assert!(z.runner.calls()[1].ends_with("increase left"));
        assert_eq!(z.runner.calls().len(), 3);
    }

    #[test]
    fn closing_targets_the_pane_id_zellij_gave_us() {
        let mut z = surface(Recorder::working());
        z.open(&request()).unwrap();
        z.close().unwrap();
        assert_eq!(
            z.runner.calls().last().unwrap(),
            "zellij --session work action close-pane --pane-id terminal_7"
        );
        assert!(!z.is_open());
    }

    #[test]
    fn opening_twice_splits_once() {
        let mut z = surface(Recorder::working());
        z.open(&request()).unwrap();
        let after_first = z.runner.calls().len();
        z.open(&request()).unwrap();
        assert_eq!(z.runner.calls().len(), after_first);
    }

    #[test]
    fn a_split_without_a_pane_id_leaves_the_surface_closed() {
        let mut z = surface(Recorder::default());
        assert!(z.open(&request()).is_err());
        assert!(!z.is_open());
        let mut z = surface(Recorder {
            fail: true,
            ..Recorder::working()
        });
        assert!(z.open(&request()).is_err());
        assert!(!z.is_open());
    }

    #[test]
    fn a_session_name_without_a_pane_is_not_a_surface() {
        let mut env = env();
        env.remove("ZELLIJ_PANE_ID");
        assert!(!detects(&env));
        assert!(!detects(&BTreeMap::new()));
    }
}
