//! Showing the head in a tmux pane.
//!
//! The surface with the widest reach: tmux is a substrate inside *any*
//! emulator, so one implementation covers the terminals that expose no
//! control interface of their own — and it is the only surface that survives
//! SSH, where a local emulator's control socket is unreachable from the host
//! the agent is actually running on.
//!
//! Everything here is one `tmux` invocation or another, which is also the
//! whole risk: the commands can only be *proved* against a running server. So
//! the spawning is injected, the tests below assert the exact argv, and a
//! human runs the real thing (see `docs/tmux.md`).

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::path::Path;

use anyhow::{Result, bail};

use super::runner::Runner;
use super::{Surface, ViewRequest};

/// Whether the recorded environment describes a tmux pane.
///
/// Separate from construction so the registry can order surfaces without
/// building any of them, and so `mod.rs` never needs to know what a tmux
/// socket looks like.
pub fn detects(env: &BTreeMap<String, String>) -> bool {
    address(env).is_some()
}

/// `(server socket, the agent's pane)`, if both are recorded.
///
/// `$TMUX` is `<socket-path>,<server-pid>,<session-index>`; only the first
/// field addresses a server. `$TMUX_PANE` is required as well: `$TMUX` alone
/// can be inherited by a process that is not itself in a pane, and splitting
/// on a guess would put the head in someone else's window.
fn address(env: &BTreeMap<String, String>) -> Option<(String, String)> {
    let socket = env.get("TMUX")?.split(',').next()?.to_string();
    let pane = env.get("TMUX_PANE")?.clone();
    (!socket.is_empty() && !pane.is_empty()).then_some((socket, pane))
}

#[derive(Debug)]
pub struct TmuxSurface<R> {
    socket: String,
    /// The agent's own pane — what we split, and never the server's notion of
    /// the current one.
    pane: String,
    session: String,
    /// Share of the agent's pane the head takes.
    percent: u8,
    runner: R,
    /// The pane we made, as tmux named it (`%7`). The only precise handle for
    /// closing it: titles are renameable and positions move.
    opened: Option<String>,
}

impl<R: Runner> TmuxSurface<R> {
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

    fn split_args(&self, request: &ViewRequest) -> Vec<String> {
        vec![
            // Explicit rather than inherited: the daemon is socket-activated
            // and has none of the agent's environment, so there is no ambient
            // server for `tmux` to find.
            "-S".into(),
            self.socket.clone(),
            "split-window".into(),
            // Detached — the new pane is created without becoming active.
            "-d".into(),
            // Print the new pane's id. Nothing else ever tells us what it is.
            "-P".into(),
            "-F".into(),
            "#{pane_id}".into(),
            // Beside, not below: §10.1 is a side-by-side layout.
            "-h".into(),
            "-l".into(),
            format!("{}%", self.percent),
            "-t".into(),
            self.pane.clone(),
            "-c".into(),
            request.workspace.display().to_string(),
            head_command(&request.head, &self.session),
        ]
    }

    fn kill_args(&self, pane: &str) -> Vec<String> {
        vec![
            "-S".into(),
            self.socket.clone(),
            "kill-pane".into(),
            "-t".into(),
            pane.to_string(),
        ]
    }
}

/// The command the pane runs.
///
/// tmux hands `shell-command` to `sh`, and session ids are agent-supplied —
/// the same untrusted input every path already routes through
/// `sanitize_component`. Quoting it is what stops a session id containing `;`
/// from running whatever follows.
fn head_command(head: &Path, session: &str) -> String {
    // `--launched-by-surface` so the daemon reads this head's leaving as the
    // pane closing — nothing else reports a pane killed by hand.
    format!(
        "{} head --launched-by-surface --session {}",
        // Quoted too: an install prefix may hold a space.
        single_quote(&head.display().to_string()),
        single_quote(session)
    )
}

fn single_quote(s: &str) -> String {
    // The POSIX idiom: end the quoted run, emit an escaped quote, start a new
    // run. There is no escape *inside* single quotes to use instead.
    format!("'{}'", s.replace('\'', r"'\''"))
}

impl<R: Runner + Debug> Surface for TmuxSurface<R> {
    fn id(&self) -> &'static str {
        "tmux"
    }

    fn available(&self) -> bool {
        self.runner.available("tmux")
    }

    fn open(&mut self, request: &ViewRequest) -> Result<()> {
        if self.opened.is_some() {
            return Ok(());
        }
        let pane = self.runner.run("tmux", &self.split_args(request))?;
        if pane.is_empty() {
            // No id means no handle to close with. Reporting success here
            // would leave a pane nothing can ever tear down.
            bail!("tmux split-window printed no pane id");
        }
        self.opened = Some(pane);
        Ok(())
    }

    fn close(&mut self) -> Result<()> {
        let Some(pane) = self.opened.take() else {
            return Ok(());
        };
        // Taken before the call succeeds: `kill-pane` fails when the human
        // already closed it, and retrying forever is worse than believing it
        // is gone — the next open simply splits again. Same rule as kitty.
        self.runner.run("tmux", &self.kill_args(&pane)).map(|_| ())
    }

    fn is_open(&self) -> bool {
        // The stored id, not a `list-panes` call: `is_open` is asked on every
        // refresh, and a process spawn per event is a cost the hot path must
        // not carry. A pane the human closed is discovered at the next
        // `close`, which is exactly when it matters.
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
        available: bool,
        fail: bool,
    }

    impl Recorder {
        fn working() -> Self {
            Self {
                stdout: "%7".into(),
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
            Ok(self.stdout.clone())
        }
        fn available(&self, _program: &str) -> bool {
            self.available
        }
    }

    fn env() -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        env.insert("TMUX".into(), "/tmp/tmux-1000/default,4242,0".into());
        env.insert("TMUX_PANE".into(), "%3".into());
        env
    }

    fn request() -> ViewRequest {
        ViewRequest {
            session: "s1".into(),
            workspace: "/home/u/app".into(),
            head: "thirdeye".into(),
        }
    }

    fn surface(runner: Recorder) -> TmuxSurface<Recorder> {
        TmuxSurface::from_env(&env(), "s1", runner).unwrap()
    }

    #[test]
    fn the_server_is_named_by_the_socket_inside_tmux() {
        // The daemon shares none of the agent's environment, so there is no
        // ambient server for `tmux` to find. $TMUX is
        // `<socket>,<pid>,<index>`; only the first field addresses a server.
        let mut tmux = surface(Recorder::working());
        tmux.open(&request()).unwrap();
        let joined = tmux.runner.calls()[0].join(" ");
        assert!(
            joined.starts_with("tmux -S /tmp/tmux-1000/default split-window"),
            "{joined}"
        );
    }

    #[test]
    fn the_pane_runs_the_head_by_the_path_it_was_given() {
        // tmux resolves a pane's command with the PATH of the client asking
        // for the split, which is the daemon's — and under systemd that need
        // not hold the install directory. A bare name there is a pane that
        // dies with 127 behind a daemon that believes it open.
        let mut tmux = surface(Recorder::working());
        let request = ViewRequest {
            head: "/opt/my tools/bin/thirdeye".into(),
            ..request()
        };
        tmux.open(&request).unwrap();
        let command = tmux.runner.calls()[0].last().unwrap().clone();
        assert_eq!(
            command,
            "'/opt/my tools/bin/thirdeye' head --launched-by-surface --session 's1'"
        );
    }

    #[test]
    fn the_split_lands_beside_the_agents_own_pane() {
        // Splitting the server's "current" pane would put the head next to
        // whatever the human last touched, which on a busy server is not the
        // agent at all.
        let mut tmux = surface(Recorder::working());
        tmux.open(&request()).unwrap();
        let joined = tmux.runner.calls()[0].join(" ");
        assert!(joined.contains("-t %3"), "{joined}");
        assert!(joined.contains("-c /home/u/app"), "{joined}");
    }

    #[test]
    fn the_split_never_steals_focus() {
        // The human is typing at the agent. A viewer that grabs the cursor
        // mid-sentence is worse than no viewer at all.
        let mut tmux = surface(Recorder::working());
        tmux.open(&request()).unwrap();
        assert!(tmux.runner.calls()[0].iter().any(|a| a == "-d"));
    }

    #[test]
    fn closing_targets_the_pane_id_tmux_gave_us() {
        // Not a title, which a human may rename, and not a position, which
        // moves as other panes come and go.
        let mut tmux = surface(Recorder::working());
        tmux.open(&request()).unwrap();
        tmux.close().unwrap();
        let joined = tmux.runner.calls()[1].join(" ");
        assert_eq!(joined, "tmux -S /tmp/tmux-1000/default kill-pane -t %7");
    }

    #[test]
    fn a_session_id_cannot_smuggle_a_second_command_into_the_shell() {
        // tmux runs shell-command through sh, and session ids are agent
        // supplied — the same reason every path goes through
        // `sanitize_component`. So this hands the command to a real `sh`,
        // with `thirdeye` shadowed by a function that prints its arguments:
        // a substring check on the string would pass a quoting bug and fail
        // a correct quote, since the payload text survives either way. Each
        // payload defeats a different wrong answer — no quoting, naive
        // single quotes, double quotes.
        //
        // Windows has an `sh` only where Git's `usr\bin` is on `PATH` — CI's
        // runner, not a PowerShell beside a stock Git for Windows — and
        // tmux reaches Windows only through one. Without it there is nothing
        // to quote for; on Unix a missing `sh` stays a failure.
        if cfg!(windows)
            && std::process::Command::new("sh")
                .args(["-c", ":"])
                .output()
                .is_err()
        {
            eprintln!("skipped: no sh on PATH");
            return;
        }
        for hostile in [
            "s1; echo SMUGGLED",
            "s1'; echo SMUGGLED; echo '",
            "s1$(echo SMUGGLED)",
            "s1`echo SMUGGLED`",
        ] {
            let script = format!(
                "thirdeye() {{ printf '%s\\n' \"$@\"; }}; {}",
                head_command(Path::new("thirdeye"), hostile)
            );
            let out = std::process::Command::new("sh")
                .args(["-c", &script])
                .output()
                .unwrap();
            let printed = String::from_utf8_lossy(&out.stdout);
            let args: Vec<&str> = printed.lines().collect();
            assert_eq!(
                args,
                ["head", "--launched-by-surface", "--session", hostile],
                "an unquoted session id would run a second command: {printed}"
            );
        }
    }

    #[test]
    fn the_head_says_the_surface_launched_it() {
        // Its leaving is how the daemon learns the human closed the pane:
        // nothing else reports a pane killed by hand.
        let mut tmux = surface(Recorder::working());
        tmux.open(&request()).unwrap();
        let command = tmux.runner.calls()[0].last().unwrap().clone();
        assert!(command.contains("--launched-by-surface"), "{command}");
    }

    #[test]
    fn opening_twice_splits_once() {
        // The visibility rules ask on every event, so open is called far more
        // often than it transitions.
        let mut tmux = surface(Recorder::working());
        tmux.open(&request()).unwrap();
        tmux.open(&request()).unwrap();
        assert_eq!(tmux.runner.calls().len(), 1);
    }

    #[test]
    fn a_split_that_prints_no_pane_id_leaves_the_surface_closed() {
        // Without an id there is no handle to close, and believing the pane
        // exists would leave the human with a viewer nothing can tear down.
        let mut tmux = surface(Recorder {
            stdout: String::new(),
            available: true,
            ..Default::default()
        });
        assert!(tmux.open(&request()).is_err());
        assert!(!tmux.is_open());
    }

    #[test]
    fn a_split_that_fails_leaves_the_surface_closed() {
        // Otherwise the daemon believes a pane exists, never retries, and
        // the human sees nothing for the rest of the session.
        let mut tmux = surface(Recorder {
            available: true,
            fail: true,
            ..Default::default()
        });
        assert!(tmux.open(&request()).is_err());
        assert!(!tmux.is_open());
    }

    #[test]
    fn a_pane_the_human_already_closed_does_not_trap_the_surface() {
        // kill-pane fails when the pane is gone. Refusing to forget it would
        // mean retrying forever and never reopening.
        let recorder = Recorder::working();
        let mut tmux = surface(recorder.clone());
        tmux.open(&request()).unwrap();

        tmux.runner = Recorder {
            fail: true,
            available: true,
            calls: recorder.calls.clone(),
            ..Default::default()
        };
        assert!(tmux.close().is_err());
        assert!(!tmux.is_open(), "a failed close must not look open");
    }

    #[test]
    fn an_agent_started_outside_tmux_has_no_tmux_surface() {
        // §12: absence is the signal, not an error to report.
        assert!(!detects(&BTreeMap::new()));
        assert!(TmuxSurface::from_env(&BTreeMap::new(), "s1", Recorder::working()).is_none());
    }

    #[test]
    fn tmux_without_a_pane_is_not_a_surface() {
        // $TMUX without $TMUX_PANE means the variable was inherited by a
        // process that is not itself in a pane. Splitting on a guess would
        // put the head in someone else's window.
        let mut env = BTreeMap::new();
        env.insert("TMUX".into(), "/tmp/tmux-1000/default,4242,0".into());
        assert!(!detects(&env));
    }
}
