mod agent_process;
mod cmd_doctor;
mod cmd_flag;
mod cmd_hook;
mod cmd_install;
mod cmd_ls;
mod cmd_mcp;
mod cmd_self;
mod cmd_units;
mod cmd_view;
mod daemon;
mod heads;
mod held_turn;
mod integration;
mod invocation;
mod local_socket;
mod notify;
mod observe;
mod published;
mod session_ref;
mod workspace;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "thirdeye",
    version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("THIRDEYE_BUILD"), ")"),
    about = "Companion viewer for CLI coding agents"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Consume an agent hook payload on stdin and append it to the log.
    Hook {
        /// Which agent's payload this is. Claude by default, because every
        /// installed Claude plugin runs a bare `thirdeye hook`.
        #[arg(long, value_enum, default_value = "claude")]
        agent: AgentArg,
        /// The event this hook is registered for, for an agent whose
        /// payloads do not name it (Copilot).
        #[arg(long)]
        event: Option<String>,
    },
    /// Serve the MCP tools Claude Code calls to flag files for attention.
    Mcp {
        /// Which agent's flags these are. Claude by default, as for `hook`.
        #[arg(long, value_enum, default_value = "claude")]
        agent: AgentArg,
    },
    /// Watch one session's log and drive the viewer.
    Daemon {
        #[arg(long)]
        session: String,
        #[arg(long)]
        workspace: Option<std::path::PathBuf>,
        /// Start the daemon in the background and return: what a hook
        /// runs where no service manager starts it.
        #[arg(long, hide = true)]
        detach: bool,
    },
    /// Run the terminal viewer for a session.
    Head {
        #[arg(long)]
        session: String,
        #[arg(long)]
        workspace: Option<std::path::PathBuf>,
        /// Set by the surface that launched this head, so the daemon knows
        /// its leaving means the window was closed.
        #[arg(long, hide = true)]
        launched_by_surface: bool,
    },
    /// Show the viewer now, whatever the policy says.
    Open {
        #[arg(long)]
        session: Option<String>,
    },
    /// Hide the viewer until an edit says it may come back.
    Close {
        #[arg(long)]
        session: Option<String>,
        /// next-mutation | next-turn | session. A flag reopens it regardless.
        #[arg(long, default_value = "next-turn")]
        until: String,
    },
    /// Set when the viewer may open itself.
    Policy {
        /// any | attention | not-this-turn | never
        mode: String,
        #[arg(long)]
        session: Option<String>,
    },
    /// Report what is installed and whether the pieces agree: thirdeye
    /// itself, what is wired for your user, and what is wired into the
    /// projects named.
    #[command(group(clap::ArgGroup::new("where").required(true).args(["projects", "user"])))]
    Doctor {
        /// The projects to check. thirdeye keeps no list of them.
        #[arg(value_name = "PATH")]
        projects: Vec<std::path::PathBuf>,
        /// Check no project, only thirdeye and your user's wiring.
        #[arg(short, long)]
        user: bool,
        /// Check only these agents; `<id>:<dir>` for one run with another
        /// configuration directory. Repeatable.
        #[arg(short, long = "agent", value_name = "AGENT[:DIR]")]
        agents: Vec<cmd_install::AgentSpec>,
    },
    /// Flag a file for the human's attention, as the MCP tool does.
    Flag {
        #[arg(long, value_enum)]
        agent: AgentArg,
        #[arg(long)]
        session: String,
        #[arg(long)]
        path: String,
        #[arg(long)]
        reason: String,
        /// First line worth reading, inclusive.
        #[arg(long)]
        start_line: Option<u32>,
        /// Last line worth reading, inclusive.
        #[arg(long)]
        end_line: Option<u32>,
    },
    /// Wire thirdeye into agents, for one project or for your user. Naming
    /// no agent, it finds the ones on this machine and asks about each.
    #[command(group(clap::ArgGroup::new("where").required(true).args(["path", "user"])))]
    Install {
        /// The project to wire.
        path: Option<std::path::PathBuf>,
        /// Every session the agent runs, rather than one project.
        #[arg(short, long)]
        user: bool,
        /// claude, codex, opencode, pi, copilot, antigravity or grok;
        /// `<id>:<dir>` for an agent run with another configuration
        /// directory (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, …). Repeatable.
        #[arg(short, long = "agent", value_name = "AGENT[:DIR]")]
        agents: Vec<cmd_install::AgentSpec>,
        /// Leave the systemd units alone; otherwise they are installed if
        /// missing.
        #[arg(long)]
        no_units: bool,
    },
    /// Remove exactly what `install` added, from one project or your user.
    /// Naming no agent, every one wired there.
    #[command(group(clap::ArgGroup::new("where").required(true).args(["path", "user"])))]
    Uninstall {
        /// The project to unwire. thirdeye keeps no list of them.
        path: Option<std::path::PathBuf>,
        /// What was wired for every session the agent runs.
        #[arg(short, long)]
        user: bool,
        /// The agents to unwire, as for `install`. Repeatable.
        #[arg(short, long = "agent", value_name = "AGENT[:DIR]")]
        agents: Vec<cmd_install::AgentSpec>,
    },
    /// Rewrite thirdeye's own directory for this binary, which every
    /// installed integration loads from. Touches no project.
    Refresh {
        /// Which tool installed this binary: script, cargo, winget or xtask.
        #[arg(long, hide = true)]
        installed_by: Option<String>,
    },
    /// Update or remove thirdeye itself.
    #[command(name = "self")]
    Itself {
        #[command(subcommand)]
        action: SelfAction,
    },
    /// Install or remove the systemd user units the daemon runs under.
    Units {
        #[command(subcommand)]
        action: UnitsAction,
    },
    /// Print the turn-grouped file set for a session.
    Ls {
        /// The project; the current one by default.
        path: Option<std::path::PathBuf>,
        #[arg(long)]
        session: Option<String>,
    },
}

#[derive(Subcommand)]
enum SelfAction {
    /// Replace this binary with a released one, then refresh every
    /// integration. Only for a binary the install script put here.
    Update {
        /// A release, e.g. 0.2.0; the latest by default.
        #[arg(long)]
        version: Option<String>,
    },
    /// Remove your user's integrations, the units, thirdeye's directory and
    /// the binary. Projects are not recorded: unwire each with `thirdeye
    /// uninstall <path>`.
    Uninstall {
        /// Remove the logs too.
        #[arg(long)]
        purge: bool,
    },
}

#[derive(Subcommand)]
enum UnitsAction {
    /// Write the units and reload the user manager.
    Install,
    /// Stop any running viewers, then remove the units.
    Uninstall,
    /// Report where the units go and whether they are current.
    Status,
}

fn units(action: UnitsAction) -> anyhow::Result<()> {
    let dir = cmd_units::unit_dir();
    match action {
        UnitsAction::Install => {
            let binary = cmd_units::own_path()?;
            if cmd_units::is_build_artifact(&binary) {
                // Worth interrupting for: the units would work until the next
                // `cargo clean`, then fail naming a path the user never typed.
                eprintln!(
                    "warning: installing units that point at {} — a build tree.\n\
                     Run `cargo install --path crates/thirdeye --root ~/.local` and \
                     re-run this, or the units break on the next `cargo clean`.",
                    binary.display()
                );
            }
            for (name, outcome) in cmd_units::install(&dir, &binary, &cmd_units::UserSystemctl)? {
                let what = match outcome {
                    cmd_units::Outcome::Written => "installed",
                    cmd_units::Outcome::AlreadyCurrent => "already current",
                };
                println!("{what}: {}", dir.join(name).display());
            }
            println!("\nNothing needs enabling — the units are templates, started by the");
            println!("first tool call of a session. Restart your agent to pick them up.");
        }
        UnitsAction::Uninstall => {
            cmd_units::uninstall(&dir, &cmd_units::UserSystemctl)?;
            println!("removed the thirdeye units from {}", dir.display());
        }
        UnitsAction::Status => {
            println!("unit directory: {}", dir.display());
            for name in ["thirdeye@.socket", "thirdeye@.service"] {
                let path = dir.join(name);
                println!(
                    "  {name}: {}",
                    if path.exists() {
                        "installed"
                    } else {
                        "not installed"
                    }
                );
            }
        }
    }
    Ok(())
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Hook { agent, event } => {
            cmd_hook::run(&agent.into(), event.as_deref(), std::io::stdin().lock())?;
            Ok(())
        }
        Command::Mcp { agent } => cmd_mcp::run(agent.into()),
        Command::Daemon {
            session,
            workspace,
            detach,
        } => {
            if detach {
                daemon::run_detached(&session, workspace.as_deref())
            } else {
                daemon::run(session, workspace)
            }
        }
        Command::Head {
            session,
            workspace,
            launched_by_surface,
        } => heads::tui::run(session, workspace, launched_by_surface),
        Command::Open { session } => cmd_view::run(session, daemon::protocol::ClientMessage::Show),
        Command::Close { session, until } => {
            let until = cmd_view::parse_hide_until(&until)?;
            cmd_view::run(session, daemon::protocol::ClientMessage::Hide { until })
        }
        Command::Policy { mode, session } => {
            let mode = cmd_view::parse_policy(&mode)?;
            cmd_view::run(session, daemon::protocol::ClientMessage::SetPolicy { mode })
        }
        Command::Doctor {
            projects,
            user: _,
            agents,
        } => cmd_doctor::run(&projects, &agents),
        Command::Units { action } => units(action),
        Command::Install {
            path,
            user,
            agents,
            no_units,
        } => cmd_install::install(path, user, &agents, !no_units),
        Command::Uninstall { path, user, agents } => cmd_install::uninstall(path, user, &agents),
        Command::Itself { action } => match action {
            SelfAction::Update { version } => cmd_self::update(version),
            SelfAction::Uninstall { purge } => cmd_self::uninstall(purge),
        },
        Command::Refresh { installed_by } => {
            let source = match installed_by.as_deref() {
                None => None,
                Some(text) => Some(published::Source::parse(text).ok_or_else(|| {
                    anyhow::anyhow!("--installed-by: script, cargo, winget or xtask, not {text}")
                })?),
            };
            cmd_install::refresh(source)
        }
        Command::Flag {
            agent,
            session,
            path,
            reason,
            start_line,
            end_line,
        } => cmd_flag::run(agent.into(), session, path, reason, start_line, end_line),
        Command::Ls { path, session } => cmd_ls::run(session, path),
    }
}

/// The agents `thirdeye hook` has an adapter for.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum AgentArg {
    Claude,
    Codex,
    Opencode,
    Pi,
    Copilot,
    Antigravity,
    Grok,
}

impl From<AgentArg> for thirdeye_core::AgentId {
    fn from(agent: AgentArg) -> Self {
        match agent {
            AgentArg::Claude => Self::Claude,
            AgentArg::Codex => Self::Codex,
            AgentArg::Opencode => Self::OpenCode,
            AgentArg::Pi => Self::Pi,
            AgentArg::Copilot => Self::Copilot,
            AgentArg::Antigravity => Self::Antigravity,
            AgentArg::Grok => Self::Grok,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_hook_is_a_claude_hook() {
        // Every installed Claude plugin runs `thirdeye hook` with no flag.
        let cli = Cli::try_parse_from(["thirdeye", "hook"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Hook {
                agent: AgentArg::Claude,
                ..
            }
        ));
    }

    #[test]
    fn every_embedded_file_is_inside_the_crate() {
        // `cargo package` carries only the crate's own directory, so a file
        // embedded from beside it builds here and fails on release day.
        fn walk(dir: &std::path::Path, found: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, found);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    // The whole call, which formatting may spread over lines.
                    for macro_name in [concat!("include_", "str!("), concat!("include_", "bytes!(")]
                    {
                        for (at, _) in text.match_indices(macro_name) {
                            let call: String =
                                text[at..].chars().take_while(|c| *c != ';').collect();
                            if call.contains("../") {
                                let line = text[..at].lines().count();
                                found.push(format!("{}:{line}", path.display()));
                            }
                        }
                    }
                }
            }
        }
        let mut found = Vec::new();
        walk(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut found,
        );
        assert!(
            found.is_empty(),
            "embedded from outside the crate: {found:?}"
        );
    }

    #[test]
    fn every_command_is_told_where_to_look() {
        // Nothing records where thirdeye was wired, so neither may assume.
        let parses = |args: &[&str]| Cli::try_parse_from(args).is_ok();
        assert!(!parses(&["thirdeye", "doctor"]));
        assert!(parses(&["thirdeye", "doctor", "--user"]));
        assert!(parses(&["thirdeye", "doctor", "/p", "/q"]));
        assert!(!parses(&["thirdeye", "doctor", "/p", "--user"]));
        assert!(!parses(&["thirdeye", "uninstall", "-a", "claude"]));
        assert!(parses(&["thirdeye", "uninstall", "/p"]));
        assert!(parses(&["thirdeye", "uninstall", "-u", "-a", "copilot"]));
        assert!(!parses(&["thirdeye", "uninstall", "--all"]));
        assert!(!parses(&["thirdeye", "install"]));
        assert!(parses(&["thirdeye", "install", "-u"]));
        assert!(parses(&[
            "thirdeye",
            "install",
            "--agent=claude",
            "--agent=codex",
            "/p"
        ]));
        assert!(parses(&[
            "thirdeye",
            "install",
            "-u",
            "--agent=claude:~/.my-claude"
        ]));
        assert!(!parses(&["thirdeye", "install", "/p", "-u"]));
        assert!(!parses(&["thirdeye", "setup"]));
    }

    #[test]
    fn codex_is_named_explicitly() {
        let cli = Cli::try_parse_from(["thirdeye", "hook", "--agent", "codex"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Hook {
                agent: AgentArg::Codex,
                ..
            }
        ));
    }
}
