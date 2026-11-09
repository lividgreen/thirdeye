//! Agents thirdeye can wire itself into: the installer's seam.
//!
//! Separate from the adapter seam: an adapter is how thirdeye *reads* an
//! agent, an integration is how it *installs into* one. An agent needs both.
//!
//! **Projects point, thirdeye's directory holds.** Everything about an
//! integration that changes between versions lives in one fixed directory of
//! thirdeye's own, `<data>/<agent>/` ([`Context::data`]), which [`publish`]
//! rewrites. A project, or an agent's user config, holds only a reference
//! that stays the same across versions: the binary's path, or the path of
//! that directory. So an update rewrites thirdeye's directory and nothing
//! else, and reaches every project without knowing where any of them are —
//! a list of projects to rewrite would break exactly when one moved.
//!
//! **Detection and support are separate facts.** Another agent being on the
//! machine is worth reporting; implying thirdeye could serve it would be a
//! claim about the product that is not true.
//!
//! [`publish`]: Integration::publish

pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod common;
pub mod copilot;
pub mod grok;
pub mod opencode;
pub mod pi;
pub mod plugin_files;
pub mod skills;
#[cfg(test)]
pub mod testing;
pub mod tree;
pub mod unsupported;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::Result;

/// Where an integration is recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// One project, privately where the agent has a private place for it.
    Project,
    /// Every session the user runs.
    User,
}

/// One place an integration is installed into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Project(PathBuf),
    User,
}

impl Target {
    pub fn scope(&self) -> Scope {
        match self {
            Target::Project(_) => Scope::Project,
            Target::User => Scope::User,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Target::Project(dir) => dir.display().to_string(),
            Target::User => "your user".into(),
        }
    }
}

/// Runs an agent's own CLI: `(program, args, working directory)` to its
/// stdout. Injected, so no test touches the user's agents.
pub type Runner<'a> = dyn Fn(&str, &[&str], Option<&Path>) -> Result<String> + 'a;

/// Everything an integration may learn about the machine, passed in rather
/// than read, so every path rule is tested without touching the user's
/// home — and because `std::env::set_var` is forbidden here.
pub struct Context<'a> {
    /// The thirdeye the integration must run: hooks name it exactly.
    pub binary: PathBuf,
    /// thirdeye's own directory, which [`Integration::publish`] owns.
    pub data: PathBuf,
    pub home: Option<PathBuf>,
    pub var: &'a dyn Fn(&str) -> Option<OsString>,
    pub run: &'a Runner<'a>,
}

impl Context<'_> {
    /// An environment variable as a path, empty treated as unset.
    pub fn path_var(&self, key: &str) -> Option<PathBuf> {
        (self.var)(key).filter(|v| !v.is_empty()).map(PathBuf::from)
    }

    /// This agent's part of thirdeye's directory.
    pub fn data_for(&self, agent: &str) -> PathBuf {
        self.data.join(agent)
    }

    /// The command that wires `id` at `target` again, as this context
    /// reaches it: `-a <id>:<dir>` when `config_var` names a moved
    /// configuration directory, or a fix would rewire the default one.
    pub fn install_command(&self, id: &str, config_var: Option<&str>, target: &Target) -> String {
        let agent = match config_var.and_then(|var| self.path_var(var)) {
            Some(dir) => format!("{id}:{}", dir.display()),
            None => id.to_string(),
        };
        match target {
            Target::Project(dir) => format!("thirdeye install -a {agent} {}", dir.display()),
            Target::User => format!("thirdeye install --user -a {agent}"),
        }
    }
}

/// What an installed integration runs, and whether it is current.
#[derive(Debug)]
pub struct Inspection {
    /// The binary its files name — possibly a bare `thirdeye`, which the
    /// agent resolves through its own `PATH`.
    pub binary: PathBuf,
    /// Whether the files are exactly what this build would install for that
    /// binary. A template that changed since the install is the drift a
    /// reinstall fixes and nothing else reports.
    pub current: bool,
    /// Where it was found, for the report.
    pub location: PathBuf,
    /// What the agent itself says, for one that keeps its own record of
    /// what it loads: the files can be in place while it has them disabled
    /// or forgotten, and then no hook runs.
    pub agent_says: AgentSays,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AgentSays {
    /// It confirms, or it keeps no record to ask.
    Loaded,
    /// It does not, with what to do about it.
    NotLoaded { detail: String, fix: String },
    /// It could not be asked.
    Unknown(String),
}

/// One agent thirdeye knows how to be installed into.
pub trait Integration {
    /// Stable id, as typed on the command line and named in thirdeye's directory.
    fn id(&self) -> &'static str;

    /// The executable that indicates this agent is present.
    fn program(&self) -> &'static str;

    /// The scopes it can be installed at; empty for an agent thirdeye
    /// detects but cannot serve.
    fn scopes(&self) -> &'static [Scope];

    /// Why it cannot be served, for an agent with no scopes.
    fn unsupported_reason(&self) -> &'static str {
        "thirdeye has no adapter for it yet"
    }

    /// Whether its hooks have been seen to work on native Windows. Every
    /// installer but Claude's writes them in `sh` syntax, and how each agent
    /// runs a hook there — `cmd`, PowerShell or Git Bash — has not been
    /// measured yet (README, "The other agents on Windows"). Installing one
    /// is still allowed, since that is how it gets measured, but it says so.
    fn verified_on_windows(&self) -> bool {
        false
    }

    /// The environment variable the agent itself reads for its own
    /// configuration directory, if it has one: `--agent <id>:<dir>` sets it,
    /// for the agent's CLI and for every path the integration derives.
    fn config_var(&self) -> Option<&'static str> {
        None
    }

    /// Write everything that changes between versions into
    /// `<data>/<id>/`, and nothing anywhere else. Idempotent.
    fn publish(&self, cx: &Context) -> Result<()>;

    /// [`publish`](Self::publish), then write into `target` only what stays
    /// the same across versions.
    fn install(&self, cx: &Context, target: &Target) -> Result<()>;

    /// Remove exactly what `install` added to `target`.
    fn uninstall(&self, cx: &Context, target: &Target) -> Result<()>;

    /// After an update: [`publish`](Self::publish), and for an agent that
    /// copies what it loads, have it take a new copy — once, per user, never
    /// once per project.
    fn refresh(&self, cx: &Context) -> Result<()> {
        self.publish(cx)
    }

    /// Once nothing is wired any more: remove what the agent itself keeps
    /// per user about thirdeye's directory, before that directory goes.
    fn retire(&self, _cx: &Context) -> Result<()> {
        Ok(())
    }

    /// What is installed at `target`, for `doctor`.
    fn inspect(&self, cx: &Context, target: &Target) -> Option<Inspection>;

    /// What the human must still do before it works, after an install.
    fn next_steps(&self, target: &Target) -> String;
}

/// Every agent this build knows about, supported or not.
pub fn registry() -> Vec<Box<dyn Integration>> {
    vec![
        Box::new(claude::Claude),
        Box::new(codex::Codex),
        Box::new(opencode::OpenCode),
        Box::new(pi::Pi),
        Box::new(copilot::Copilot),
        Box::new(antigravity::Antigravity),
        Box::new(grok::Grok),
        // The targeting research: no external process lifecycle hooks.
        Box::new(unsupported::Unsupported {
            id: "gemini",
            program: "gemini",
        }),
    ]
}

pub fn by_id(id: &str) -> Option<Box<dyn Integration>> {
    registry().into_iter().find(|a| a.id() == id)
}

/// Whether `program` is on `PATH`.
pub fn detected(program: &str) -> bool {
    // Windows runs a program by a name it does not have: `claude` is
    // `claude.exe`, and what npm installs is a `.cmd` beside its script.
    let names: Vec<String> = if cfg!(windows) {
        ["", ".exe", ".cmd"]
            .map(|ext| format!("{program}{ext}"))
            .to_vec()
    } else {
        vec![program.to_string()]
    };
    names.iter().any(|name| detected_exact(name))
}

/// Whether a file of exactly this name is on `PATH`.
pub fn detected_exact(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(name).is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fix_names_the_configuration_directory_it_was_found_in() {
        // Found by review: a hint without the `:<dir>` rewires the default.
        let base = Path::new("/b");
        let moved = |k: &str| (k == "GROK_HOME").then(|| "/elsewhere".into());
        let cx = testing::context(base, &moved, &testing::no_run);
        assert_eq!(
            cx.install_command("grok", Some("GROK_HOME"), &Target::User),
            "thirdeye install --user -a grok:/elsewhere"
        );
        assert_eq!(
            cx.install_command("codex", Some("CODEX_HOME"), &Target::Project("/p".into())),
            "thirdeye install -a codex /p"
        );
    }

    #[test]
    fn every_agent_has_a_distinct_id() {
        // Ids name the directories under thirdeye's own; a collision would
        // silently make one agent publish over another.
        let mut ids: Vec<&str> = registry().iter().map(|a| a.id()).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "duplicate agent id");
    }

    #[test]
    fn only_claude_installs_on_windows_without_a_warning() {
        // Pins the honest claim, as below: an agent joins this list when
        // its hooks have been measured on Windows, and README's platform
        // table says so in the same change.
        let verified: Vec<&str> = registry()
            .iter()
            .filter(|a| a.verified_on_windows())
            .map(|a| a.id())
            .collect();
        assert_eq!(verified, ["claude"]);
    }

    #[test]
    fn every_adapted_agent_is_supported() {
        // Pins the honest claim. Another adapter means this test, the
        // installer and the docs all have to say so.
        let supported: Vec<&str> = registry()
            .iter()
            .filter(|a| !a.scopes().is_empty())
            .map(|a| a.id())
            .collect();
        assert_eq!(
            supported,
            [
                "claude",
                "codex",
                "opencode",
                "pi",
                "copilot",
                "antigravity",
                "grok"
            ]
        );
    }

    #[test]
    fn agents_without_project_plugins_say_so() {
        // Copilot and Grok install plugins per user only; offering a project
        // install would wire every session while claiming to wire one.
        for id in ["copilot", "grok"] {
            assert_eq!(by_id(id).unwrap().scopes(), [Scope::User], "{id}");
        }
    }
}
