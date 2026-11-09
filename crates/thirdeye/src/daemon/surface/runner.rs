//! Running an external program, behind a seam.
//!
//! Shared by every surface that drives one — `kitten @` for kitty, `tmux` for
//! a pane. It sits in its own file rather than in `mod.rs` because `mod.rs`
//! holds the seam and nothing else, and a `Runner` is not a `Surface`.
//!
//! Not a seam for mocking's sake: neither `kitten @` nor `tmux` can run in
//! CI, and a surface whose only test was "we called something" would be
//! worthless. Asserting the exact argv is the part that *can* be checked, and
//! it is the part that is easy to get subtly wrong.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Debug;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

pub trait Runner: Debug + Send {
    /// Run `program`, returning its captured stdout, trimmed.
    ///
    /// Returning stdout rather than discarding it is what makes the tmux
    /// surface possible: `split-window -P` prints the new pane's id, and
    /// nothing else ever tells us what it is. kitty ignores the value.
    fn run(&self, program: &str, args: &[String]) -> Result<String>;

    /// [`Self::run`], with `vars` set for `program` alone.
    ///
    /// Through `env` by default, so what a test records is the command a
    /// human would type. A runner of real processes sets them itself:
    /// Windows has no `env`, except where Git's happens to be on `PATH`.
    fn run_with_env(
        &self,
        vars: &[(&str, &str)],
        program: &str,
        args: &[String],
    ) -> Result<String> {
        let mut argv: Vec<String> = vars.iter().map(|(k, v)| format!("{k}={v}")).collect();
        argv.push(program.to_string());
        argv.extend(args.iter().cloned());
        self.run("env", &argv)
    }

    /// Whether `program` can be executed at all.
    fn available(&self, program: &str) -> bool;
}

/// Runs real processes, found where the *agent's* shell would find them.
///
/// The daemon's own `PATH` is the systemd user manager's, which holds
/// `~/.local/bin` or `~/.cargo/bin` only when a login session exported them
/// into it — and WSL's never does. A Zellij installed with `cargo install`
/// was then "not installed", and the session ran with no surface beside a
/// terminal that could have shown one. The human started the agent from a
/// shell that finds their terminal's CLI, so that shell's `PATH`, recorded
/// by the hook, is the one to search.
#[derive(Debug, Default)]
pub struct ProcessRunner {
    /// The agent's `PATH`; `None` leaves the daemon's own in force.
    path: Option<OsString>,
}

impl ProcessRunner {
    pub fn for_env(env: &BTreeMap<String, String>) -> Self {
        Self {
            path: env.get("PATH").map(OsString::from),
        }
    }

    fn search_path(&self) -> Option<OsString> {
        self.path.clone().or_else(|| std::env::var_os("PATH"))
    }
}

impl Runner for ProcessRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<String> {
        self.run_with_env(&[], program, args)
    }

    fn run_with_env(
        &self,
        vars: &[(&str, &str)],
        program: &str,
        args: &[String],
    ) -> Result<String> {
        let mut command = Command::new(program);
        command.envs(vars.iter().copied());
        // Set on the child rather than used to resolve `program` here: std
        // then searches it for `program`, and a program that runs another
        // (`env … zellij`) finds that one too.
        if let Some(path) = &self.path {
            command.env("PATH", path);
        }
        let out = command
            .args(args)
            // stdout is captured rather than inherited, and stderr dropped: a
            // surface's chatter belongs in neither the journal nor the agent's
            // terminal. Failures are reported through the exit status.
            .stderr(Stdio::null())
            .output()
            .with_context(|| format!("running {program}"))?;
        if !out.status.success() {
            bail!("{program} exited with {}", out.status);
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn available(&self, program: &str) -> bool {
        // A PATH walk rather than spawning `program --version`: `available` is
        // asked on every refresh, and a process spawn per event is a cost the
        // hot path should not carry.
        self.search_path()
            .map(|paths| {
                std::env::split_paths(&paths).any(|dir| {
                    file_names(program).any(|name| {
                        let candidate = dir.join(name);
                        candidate.is_file() && is_executable(&candidate)
                    })
                })
            })
            .unwrap_or(false)
    }
}

/// The files `program` may be. Windows runs `zellij` from `zellij.exe`, as
/// `Command` does; looking for the bare name found no Zellij at all.
fn file_names(program: &str) -> impl Iterator<Item = String> {
    let suffixed = (cfg!(windows) && Path::new(program).extension().is_none())
        .then(|| format!("{program}.exe"));
    std::iter::once(program.to_string()).chain(suffixed)
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn a_variable_reaches_the_program_without_an_env_program() {
        // Zellij places a pane by ZELLIJ_PANE_ID and WezTerm finds its GUI by
        // WEZTERM_UNIX_SOCKET, both set for one call. Windows has no `env`.
        let out = ProcessRunner::default()
            .run_with_env(
                &[("THIRDEYE_RUNNER_PROBE", "pane-3")],
                "cmd",
                &[
                    "/d".into(),
                    "/c".into(),
                    "echo %THIRDEYE_RUNNER_PROBE%".into(),
                ],
            )
            .unwrap();
        assert_eq!(out, "pane-3");
    }

    #[test]
    fn a_program_is_found_by_the_name_it_is_run_by() {
        // Surfaces ask for `zellij` and `wezterm`, which are `.exe` files.
        let runner = ProcessRunner::default();
        assert!(runner.available("cmd"));
        assert!(runner.available("cmd.exe"));
        assert!(!runner.available("thirdeye-no-such-program"));
    }
}

// Every test here runs a shell.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn stdout_comes_back_because_a_pane_id_is_only_ever_printed() {
        // tmux's `split-window -P` prints the id of the pane it just made,
        // and that id is the only precise handle for closing it later. A
        // runner that discarded stdout would make the tmux surface impossible
        // to write, which is why this is the seam's contract and not an extra.
        let out = ProcessRunner::default()
            .run("sh", &["-c".into(), "printf %s %3".into()])
            .unwrap();
        assert_eq!(out, "%3");
    }

    #[test]
    fn a_nonzero_exit_is_an_error_not_an_empty_string() {
        // A surface that read a failed launch as "it printed nothing" would
        // believe a window exists and never retry.
        assert!(
            ProcessRunner::default()
                .run("sh", &["-c".into(), "exit 3".into()])
                .is_err()
        );
    }

    #[test]
    fn a_program_that_is_not_installed_is_not_available() {
        let runner = ProcessRunner::default();
        assert!(!runner.available("thirdeye-no-such-program"));
        assert!(runner.available("sh"));
    }

    #[test]
    fn a_program_is_looked_for_on_the_agents_path_not_the_daemons() {
        // Under systemd the daemon's PATH lacks the directories a human
        // installs into; the program must be found, and run, through the
        // PATH the hook recorded from the agent's shell.
        let dir = std::env::temp_dir().join(format!("thirdeye-runner-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let tool = dir.join("thirdeye-test-tool");
        std::fs::write(&tool, "#!/bin/sh\nprintf found\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(!ProcessRunner::default().available("thirdeye-test-tool"));

        let recorded = format!("{}:/usr/bin:/bin", dir.display());
        let env = BTreeMap::from([("PATH".to_string(), recorded)]);
        let runner = ProcessRunner::for_env(&env);
        assert!(runner.available("thirdeye-test-tool"));
        assert_eq!(runner.run("thirdeye-test-tool", &[]).unwrap(), "found");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
