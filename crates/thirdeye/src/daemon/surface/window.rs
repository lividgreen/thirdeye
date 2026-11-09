//! A surface that opens a new terminal window and cannot close it.
//!
//! The behaviour every such terminal shares — GNOME Terminal, Alacritty,
//! Ghostty, Windows Terminal: one command opens a window running the head; nothing names that
//! window afterwards; it closes when the head exits. So `close` only forgets
//! it, and the daemon ends the head instead (`Surface::detached`). Each
//! terminal's file says how to detect it and what command opens a window.
//! Not a seam: a helper, as `runner.rs` is.

use std::fmt::Debug;

use anyhow::Result;

use super::runner::Runner;
use super::{Surface, ViewRequest};

/// Stands for the workspace root in a terminal's arguments, which is known
/// only when the window is opened.
pub const WORKSPACE: &str = "{workspace}";

#[derive(Debug)]
pub struct WindowSurface<R> {
    id: &'static str,
    /// What `available` looks for on `PATH`.
    program: &'static str,
    /// The command line up to, not including, the head; [`WORKSPACE`] in
    /// an argument is replaced when the window opens.
    command: (String, Vec<String>),
    session: String,
    /// How this terminal needs an argument written; see [`Self::encoding`].
    encode: Encode,
    runner: R,
    open: bool,
}

/// Rewrites one argument for a terminal's own command-line parser, or
/// refuses it.
pub type Encode = fn(&str) -> Result<String>;

impl<R: Runner> WindowSurface<R> {
    pub fn new(
        id: &'static str,
        program: &'static str,
        command: (String, Vec<String>),
        session: &str,
        runner: R,
    ) -> Self {
        Self {
            id,
            program,
            command,
            session: session.to_string(),
            encode: |arg| Ok(arg.to_string()),
            runner,
            open: false,
        }
    }

    /// For a terminal that parses its arguments again rather than passing
    /// an argv through. Applied to everything that comes from outside — the
    /// workspace, the head's path, the session id — never to the terminal's
    /// own arguments, which may mean what the parser reads into them.
    pub fn encoding(mut self, encode: Encode) -> Self {
        self.encode = encode;
        self
    }

    pub fn open_args(&self, request: &ViewRequest) -> Result<Vec<String>> {
        let workspace = (self.encode)(&request.workspace.display().to_string())?;
        let mut args: Vec<String> = self
            .command
            .1
            .iter()
            .map(|a| a.replace(WORKSPACE, &workspace))
            .collect();
        // An argv, never a shell string: the session id reaches no shell.
        args.push((self.encode)(&request.head.display().to_string())?);
        args.extend(["head", "--launched-by-surface", "--session"].map(String::from));
        args.push((self.encode)(&self.session)?);
        Ok(args)
    }
}

impl<R: Runner + Debug> Surface for WindowSurface<R> {
    fn id(&self) -> &'static str {
        self.id
    }

    fn available(&self) -> bool {
        self.runner.available(self.program)
    }

    fn open(&mut self, request: &ViewRequest) -> Result<()> {
        if self.open {
            return Ok(());
        }
        self.runner
            .run(&self.command.0, &self.open_args(request)?)?;
        self.open = true;
        Ok(())
    }

    /// Nothing to call: the daemon has already told the head to go, and its
    /// window goes with it.
    fn close(&mut self) -> Result<()> {
        self.open = false;
        Ok(())
    }

    fn is_open(&self) -> bool {
        self.open
    }

    fn detached(&self) -> bool {
        true
    }
}

#[cfg(test)]
pub mod testing {
    //! A runner that records argv, shared by the terminals' tests.
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Default, Clone)]
    pub struct Recorder {
        pub calls: Arc<Mutex<Vec<String>>>,
        pub fail: bool,
    }

    impl Runner for Recorder {
        fn run(&self, program: &str, args: &[String]) -> Result<String> {
            let mut call = vec![program.to_string()];
            call.extend(args.iter().cloned());
            self.calls.lock().unwrap().push(call.join(" "));
            if self.fail {
                anyhow::bail!("planted failure");
            }
            Ok(String::new())
        }
        fn available(&self, _program: &str) -> bool {
            true
        }
    }

    pub fn request() -> ViewRequest {
        ViewRequest {
            session: "s1".into(),
            workspace: "/home/u/app".into(),
            head: "thirdeye".into(),
        }
    }

    pub fn env_of(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    fn surface(runner: Recorder) -> WindowSurface<Recorder> {
        let args = vec!["--window".into(), format!("--cwd={WORKSPACE}")];
        WindowSurface::new("t", "t", ("t".into(), args), "s1", runner)
    }

    #[test]
    fn opening_twice_opens_one_window() {
        let r = Recorder::default();
        let mut w = surface(r.clone());
        w.open(&request()).unwrap();
        w.open(&request()).unwrap();
        assert_eq!(r.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn closing_calls_nothing_and_forgets_the_window() {
        let r = Recorder::default();
        let mut w = surface(r.clone());
        w.open(&request()).unwrap();
        w.close().unwrap();
        assert!(!w.is_open());
        assert_eq!(r.calls.lock().unwrap().len(), 1, "only the open");
        assert!(w.detached());
    }

    #[test]
    fn a_window_that_failed_to_open_is_not_open() {
        let mut w = surface(Recorder {
            fail: true,
            ..Default::default()
        });
        assert!(w.open(&request()).is_err());
        assert!(!w.is_open());
    }

    #[test]
    fn the_head_is_an_argv_after_the_terminals_own_arguments() {
        let w = surface(Recorder::default());
        assert_eq!(
            w.open_args(&request()).unwrap().join(" "),
            "--window --cwd=/home/u/app thirdeye head --launched-by-surface --session s1"
        );
    }
}
