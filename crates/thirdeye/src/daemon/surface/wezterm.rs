//! Showing the head in a WezTerm split.
//!
//! Everything here is one `wezterm cli` call or another, verified live
//! (`docs/wezterm.md`): the spawning is injected, the tests below assert the
//! exact argv, and a human runs the real thing.

use std::collections::BTreeMap;
use std::fmt::Debug;

use anyhow::{Result, bail};
use serde_json::Value;

use super::runner::Runner;
use super::{Surface, ViewRequest};

/// Whether the recorded environment describes a WezTerm pane.
///
/// Not when `TERM` is kitty's: kitty keeps the WezTerm variables it inherits
/// when started from a WezTerm shell, but sets `TERM` for its own children —
/// and splitting the WezTerm pane that launched kitty would put the viewer in
/// the wrong window.
pub fn detects(env: &BTreeMap<String, String>) -> bool {
    address(env).is_some() && env.get("TERM").map(String::as_str) != Some("xterm-kitty")
}

/// `(the GUI instance's socket, the agent's pane)`, if both are recorded.
fn address(env: &BTreeMap<String, String>) -> Option<(String, String)> {
    let socket = env.get("WEZTERM_UNIX_SOCKET")?.clone();
    let pane = env.get("WEZTERM_PANE")?.clone();
    (!socket.is_empty() && !pane.is_empty()).then_some((socket, pane))
}

#[derive(Debug)]
pub struct WeztermSurface<R> {
    socket: String,
    /// The agent's own pane — what we split.
    pane: String,
    session: String,
    /// Share of the agent's pane the head takes.
    percent: u8,
    runner: R,
    /// The pane we made, as WezTerm numbered it.
    opened: Option<String>,
}

impl<R: Runner> WeztermSurface<R> {
    pub fn from_env(env: &BTreeMap<String, String>, session: &str, runner: R) -> Option<Self> {
        let (socket, pane) = address(env)?;
        Some(Self {
            socket,
            pane,
            session: session.to_string(),
            percent: super::split_percent(env),
            runner,
            opened: None,
        })
    }

    /// `wezterm cli <args>` against the recorded instance.
    ///
    /// The socket travels in the environment because the daemon has none of
    /// the terminal's. `--no-auto-start` on every call: without it, a call
    /// that finds no GUI spawns a `wezterm-mux-server` of its own.
    fn cli(&self, args: &[&str]) -> Result<String> {
        let mut argv = vec!["cli".to_string(), "--no-auto-start".into()];
        argv.extend(args.iter().map(|a| (*a).to_string()));
        self.runner
            .run_with_env(&[("WEZTERM_UNIX_SOCKET", &self.socket)], "wezterm", &argv)
    }

    /// The pane that is active in the agent's tab — where the human's cursor
    /// is, if they are looking at that tab at all. Not necessarily the
    /// agent's pane: the human may have clicked elsewhere in it.
    fn active_in_agents_tab(&self) -> Option<String> {
        let panes: Value =
            serde_json::from_str(&self.cli(&["list", "--format", "json"]).ok()?).ok()?;
        let panes = panes.as_array()?;
        let id = |p: &Value| p["pane_id"].as_u64().map(|n| n.to_string());
        let tab = panes
            .iter()
            .find(|p| id(p).as_deref() == Some(&self.pane))?["tab_id"]
            .as_u64()?;
        panes
            .iter()
            .find(|p| p["tab_id"].as_u64() == Some(tab) && p["is_active"].as_bool() == Some(true))
            .and_then(id)
    }
}

impl<R: Runner + Debug> Surface for WeztermSurface<R> {
    fn id(&self) -> &'static str {
        "wezterm"
    }

    fn available(&self) -> bool {
        self.runner.available("wezterm")
    }

    fn open(&mut self, request: &ViewRequest) -> Result<()> {
        if self.opened.is_some() {
            return Ok(());
        }
        // Without the pane list, the agent's pane: where the human usually
        // is, and better than leaving the cursor on the viewer.
        let restore = self
            .active_in_agents_tab()
            .unwrap_or_else(|| self.pane.clone());
        let workspace = request.workspace.display().to_string();
        let percent = self.percent.to_string();
        let head = request.head.display().to_string();
        let pane = self.cli(&[
            "split-pane",
            "--pane-id",
            &self.pane,
            "--right",
            "--percent",
            &percent,
            "--cwd",
            &workspace,
            // Argv, not a shell string: the session id reaches no shell.
            "--",
            &head,
            "head",
            "--launched-by-surface",
            "--session",
            &self.session,
        ])?;
        if pane.is_empty() {
            bail!("wezterm split-pane printed no pane id");
        }
        self.opened = Some(pane);
        // split-pane has no "keep focus" flag and makes the new pane active.
        // Handing it back is best-effort: a viewer that stole focus is worse
        // than one that did not open, but not worth failing the open over.
        let _ = self.cli(&["activate-pane", "--pane-id", &restore]);
        Ok(())
    }

    fn close(&mut self) -> Result<()> {
        let Some(pane) = self.opened.take() else {
            return Ok(());
        };
        // Taken before the call succeeds, as for tmux and kitty: a pane the
        // human already closed makes kill-pane fail, and the next open must
        // still split again.
        self.cli(&["kill-pane", "--pane-id", &pane]).map(|_| ())
    }

    fn is_open(&self) -> bool {
        self.opened.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Answers `list` with `list`, `split-pane` with `split`, anything else
    /// with nothing — and records every call.
    #[derive(Debug, Default, Clone)]
    struct Recorder {
        calls: Arc<Mutex<Vec<Vec<String>>>>,
        list: String,
        split: String,
        fail: bool,
    }

    impl Recorder {
        fn working() -> Self {
            Self {
                // The agent's pane 3 is in tab 1, but the human last clicked
                // pane 5 there; pane 9 is another tab's.
                list: r#"[{"pane_id":3,"tab_id":1,"is_active":false},
                          {"pane_id":5,"tab_id":1,"is_active":true},
                          {"pane_id":9,"tab_id":2,"is_active":true}]"#
                    .into(),
                split: "7\n".into(),
                ..Default::default()
            }
        }
        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }
        fn call(&self, verb: &str) -> Vec<String> {
            self.calls()
                .into_iter()
                .find(|c| c.iter().any(|a| a == verb))
                .unwrap_or_else(|| panic!("no {verb} call in {:?}", self.calls()))
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
            Ok(if args.iter().any(|a| a == "list") {
                self.list.clone()
            } else if args.iter().any(|a| a == "split-pane") {
                self.split.trim().to_string()
            } else {
                String::new()
            })
        }
        fn available(&self, _program: &str) -> bool {
            true
        }
    }

    fn env() -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        env.insert(
            "WEZTERM_UNIX_SOCKET".into(),
            "/run/user/1000/wezterm/gui-sock-42".into(),
        );
        env.insert("WEZTERM_PANE".into(), "3".into());
        env
    }

    fn request() -> ViewRequest {
        ViewRequest {
            session: "s1".into(),
            workspace: "/home/u/app".into(),
            head: "thirdeye".into(),
        }
    }

    fn surface(runner: Recorder) -> WeztermSurface<Recorder> {
        WeztermSurface::from_env(&env(), "s1", runner).unwrap()
    }

    #[test]
    fn the_instance_is_named_by_its_socket_and_never_started() {
        // The daemon has none of the terminal's environment. And without
        // --no-auto-start, wezterm cli with no GUI to reach spawns a mux
        // server of its own — found by doing exactly that.
        let mut w = surface(Recorder::working());
        w.open(&request()).unwrap();
        w.close().unwrap();
        for call in w.runner.calls() {
            assert_eq!(
                call[..4],
                [
                    "env",
                    "WEZTERM_UNIX_SOCKET=/run/user/1000/wezterm/gui-sock-42",
                    "wezterm",
                    "cli"
                ],
                "{call:?}"
            );
            assert_eq!(call[4], "--no-auto-start", "{call:?}");
        }
    }

    #[test]
    fn the_split_lands_beside_the_agents_own_pane() {
        let mut w = surface(Recorder::working());
        w.open(&request()).unwrap();
        let joined = w.runner.call("split-pane").join(" ");
        assert!(
            joined.contains("split-pane --pane-id 3 --right --percent 40"),
            "{joined}"
        );
        assert!(joined.contains("--cwd /home/u/app"), "{joined}");
    }

    #[test]
    fn focus_returns_to_the_pane_that_had_it() {
        // split-pane makes the new pane active. Handing focus to the agent's
        // pane instead would move a human who had clicked elsewhere.
        let mut w = surface(Recorder::working());
        w.open(&request()).unwrap();
        assert_eq!(
            w.runner.call("activate-pane")[5..],
            ["activate-pane", "--pane-id", "5"]
        );
    }

    #[test]
    fn the_head_runs_as_argv_so_a_session_id_cannot_reach_a_shell() {
        let hostile = "s1; rm -rf ~";
        let mut w = WeztermSurface::from_env(&env(), hostile, Recorder::working()).unwrap();
        w.open(&request()).unwrap();
        let split = w.runner.call("split-pane");
        let dashdash = split.iter().position(|a| a == "--").unwrap();
        assert_eq!(
            split[dashdash + 1..],
            [
                "thirdeye",
                "head",
                "--launched-by-surface",
                "--session",
                hostile
            ]
        );
    }

    #[test]
    fn closing_targets_the_pane_id_wezterm_gave_us() {
        let mut w = surface(Recorder::working());
        w.open(&request()).unwrap();
        w.close().unwrap();
        assert_eq!(
            w.runner.call("kill-pane")[5..],
            ["kill-pane", "--pane-id", "7"]
        );
    }

    #[test]
    fn opening_twice_splits_once() {
        let mut w = surface(Recorder::working());
        w.open(&request()).unwrap();
        w.open(&request()).unwrap();
        let splits = w
            .runner
            .calls()
            .iter()
            .filter(|c| c.contains(&"split-pane".to_string()))
            .count();
        assert_eq!(splits, 1);
    }

    #[test]
    fn a_split_that_prints_no_pane_id_leaves_the_surface_closed() {
        let mut w = surface(Recorder {
            split: String::new(),
            ..Recorder::working()
        });
        assert!(w.open(&request()).is_err());
        assert!(!w.is_open());
    }

    #[test]
    fn a_pane_the_human_already_closed_does_not_trap_the_surface() {
        let recorder = Recorder::working();
        let mut w = surface(recorder.clone());
        w.open(&request()).unwrap();
        w.runner = Recorder {
            fail: true,
            calls: recorder.calls.clone(),
            ..Recorder::working()
        };
        assert!(w.close().is_err());
        assert!(!w.is_open());
    }

    #[test]
    fn kitty_started_from_wezterm_is_not_mistaken_for_wezterm() {
        // kitty does not clear the WezTerm variables it inherits, but it
        // does set TERM for its own children. Splitting the WezTerm pane
        // that launched kitty would open the viewer in the wrong window.
        let mut leaked = env();
        leaked.insert("TERM".into(), "xterm-kitty".into());
        assert!(!detects(&leaked));
        leaked.insert("TERM".into(), "xterm-256color".into());
        assert!(detects(&leaked));
    }

    #[test]
    fn without_the_pane_list_focus_goes_back_to_the_agent() {
        // The human is usually in the agent's pane; leaving focus on the
        // viewer would send their next keystrokes there.
        let mut w = surface(Recorder {
            list: "not json".into(),
            ..Recorder::working()
        });
        w.open(&request()).unwrap();
        assert_eq!(
            w.runner.call("activate-pane")[5..],
            ["activate-pane", "--pane-id", "3"]
        );
    }

    #[test]
    fn handing_focus_back_never_fails_the_open() {
        // Best-effort: the split exists and must be tracked even when the
        // focus call fails, or nothing could ever close it.
        #[derive(Debug, Clone)]
        struct NoFocus(Recorder);
        impl Runner for NoFocus {
            fn run(&self, program: &str, args: &[String]) -> Result<String> {
                if args.iter().any(|a| a == "activate-pane") {
                    bail!("focus refused");
                }
                self.0.run(program, args)
            }
            fn available(&self, _program: &str) -> bool {
                true
            }
        }
        let mut w = WeztermSurface::from_env(&env(), "s1", NoFocus(Recorder::working())).unwrap();
        w.open(&request()).unwrap();
        assert!(w.is_open());
    }

    #[test]
    fn an_agent_outside_wezterm_has_no_wezterm_surface() {
        assert!(!detects(&BTreeMap::new()));
        let mut socket_only = env();
        socket_only.remove("WEZTERM_PANE");
        assert!(!detects(&socket_only));
        assert!(detects(&env()));
    }
}
