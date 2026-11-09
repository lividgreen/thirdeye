//! `thirdeye install | uninstall | refresh`: the binary wiring itself into
//! agents, from the files it embeds.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, bail};

use crate::integration::{self, Context, Integration, Target};
use crate::published::{self, Published, Source};

/// thirdeye's own directory, or why there is none.
pub fn data_dir() -> Result<PathBuf> {
    thirdeye_core::location::data_root()
        .context("cannot place thirdeye's data directory: set $HOME or $THIRDEYE_DATA_DIR")
}

/// This binary, by the path hooks must name.
pub fn binary() -> Result<PathBuf> {
    let me = crate::cmd_units::own_path()?;
    // Not on Windows: `canonicalize` there returns a `\\?\` spelling no
    // agent uses, and `current_exe` is already absolute.
    if cfg!(windows) {
        return Ok(me);
    }
    Ok(me.canonicalize().unwrap_or(me))
}

fn env_var(key: &str) -> Option<OsString> {
    std::env::var_os(key)
}

/// Run an agent's CLI, returning its stdout. Its stderr is kept for the
/// error, so a refusal says why.
pub fn run_agent(program: &str, args: &[&str], dir: Option<&Path>) -> Result<String> {
    run_agent_with(program, args, dir, None)
}

/// [`run_agent`] with one variable set for the agent alone — never
/// `set_var`, which is forbidden here and would leak into every later call.
fn run_agent_with(
    program: &str,
    args: &[&str],
    dir: Option<&Path>,
    env: Option<(&str, &std::ffi::OsStr)>,
) -> Result<String> {
    let mut command = Command::new(resolve(program));
    command.args(args).stdin(Stdio::null());
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    if let Some((key, value)) = env {
        command.env(key, value);
    }
    let out = command
        .output()
        .with_context(|| format!("running {program} — is it on PATH?"))?;
    if !out.status.success() {
        bail!(
            "{program} {} failed ({}): {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// What npm installs on Windows is a `.cmd` beside its script, which
/// `Command` does not find by the bare name.
fn resolve(program: &str) -> String {
    if cfg!(windows) && !integration::detected_exact(program) {
        let cmd = format!("{program}.cmd");
        if integration::detected_exact(&cmd) {
            return cmd;
        }
    }
    program.to_string()
}

/// Build the real context and hand it to `f`.
pub fn with_context<T>(f: impl FnOnce(&Context) -> Result<T>) -> Result<T> {
    let cx = Context {
        binary: binary()?,
        data: data_dir()?,
        home: thirdeye_core::home_dir(),
        var: &env_var,
        run: &run_agent,
    };
    f(&cx)
}

/// Build the context for one agent, with its configuration directory moved
/// to `config` when one was named: the agent's own variable then answers
/// with it, for every path the integration derives and for the agent's CLI.
pub fn with_agent<T>(
    agent: &dyn Integration,
    config: Option<&Path>,
    f: impl FnOnce(&Context) -> Result<T>,
) -> Result<T> {
    let moved: Option<(&str, OsString)> = match config {
        None => None,
        Some(dir) => Some((
            agent.config_var().with_context(|| {
                format!(
                    "{} has no setting for another configuration directory",
                    agent.id()
                )
            })?,
            dir.as_os_str().to_owned(),
        )),
    };
    let var = |key: &str| match &moved {
        Some((k, v)) if *k == key => Some(v.clone()),
        _ => env_var(key),
    };
    let run = |program: &str, args: &[&str], dir: Option<&Path>| {
        run_agent_with(
            program,
            args,
            dir,
            moved.as_ref().map(|(k, v)| (*k, v.as_os_str())),
        )
    };
    let cx = Context {
        binary: binary()?,
        data: data_dir()?,
        home: thirdeye_core::home_dir(),
        var: &var,
        run: &run,
    };
    f(&cx)
}

/// `--agent <id>[:<config dir>]`: an agent, and optionally the
/// configuration directory it was started with, for an agent run with
/// `CLAUDE_CONFIG_DIR`, `CODEX_HOME` and the like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSpec {
    pub id: String,
    pub config: Option<PathBuf>,
}

impl std::str::FromStr for AgentSpec {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        // The first colon only: a Windows directory has one of its own.
        let (id, config) = match text.split_once(':') {
            Some((id, dir)) => (id, Some(dir)),
            None => (text, None),
        };
        if id.is_empty() {
            return Err("name an agent, e.g. claude or claude:~/.my-claude".into());
        }
        let config = match config {
            None => None,
            Some("") => return Err(format!("{id}: the directory after the colon is empty")),
            Some(dir) => Some(expand(
                dir,
                thirdeye_core::home_dir().as_deref(),
                std::env::current_dir().ok().as_deref(),
            )),
        };
        Ok(Self {
            id: id.to_string(),
            config,
        })
    }
}

/// `~` and relative paths, made absolute: the shell expands `~` only at
/// the start of a word, and `claude:~/x` is not one.
fn expand(dir: &str, home: Option<&Path>, cwd: Option<&Path>) -> PathBuf {
    let path = match (dir.strip_prefix('~'), home) {
        (Some(""), Some(home)) => home.to_path_buf(),
        (Some(rest), Some(home)) if rest.starts_with(['/', '\\']) => home.join(&rest[1..]),
        _ => PathBuf::from(dir),
    };
    match cwd {
        Some(cwd) if path.is_relative() => cwd.join(path),
        _ => path,
    }
}

/// A project, or `--user`: one of them, always named. Nothing records
/// where thirdeye was wired, so no command assumes.
pub fn target(path: Option<PathBuf>, user: bool) -> Result<Target> {
    match (path, user) {
        (Some(_), true) => bail!("name a project or --user, not both"),
        (None, false) => bail!("name a project, or --user"),
        (None, true) => Ok(Target::User),
        (Some(path), false) => {
            // A typo would otherwise become a project: every agent but
            // Claude creates its directory there.
            if !path.is_dir() {
                bail!("{} is not a directory", path.display());
            }
            let cwd = std::env::current_dir().ok();
            // Through workspace.rs, like every command: a project spelled
            // two ways would otherwise be two workspaces.
            let root = crate::workspace::resolve_workspace_root(Some(path), cwd.as_deref());
            refuse_home(&root, thirdeye_core::home_dir().as_deref())?;
            Ok(Target::Project(root))
        }
    }
}

/// `curl | sh` is usually run from `~`, which is not a project — and a
/// "project" install there writes the agents' *user* config (`~/.codex/`,
/// `~/.claude/settings.local.json`) while calling it a project.
fn refuse_home(root: &Path, home: Option<&Path>) -> Result<()> {
    let canonical = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    if home.is_some_and(|home| canonical(home) == canonical(root)) {
        bail!(
            "{} is your home directory, not a project: name a project, or \
             --user for every session",
            root.display()
        );
    }
    Ok(())
}

/// An agent to act on, and the configuration directory it was named with.
pub type Chosen = (Box<dyn Integration>, Option<PathBuf>);

/// Look every agent up, and check each can go to `target`, before anything
/// is written.
pub fn resolve_agents(specs: &[AgentSpec], target: &Target) -> Result<Vec<Chosen>> {
    specs
        .iter()
        .map(|spec| {
            let name = &spec.id;
            let agent = integration::by_id(name).with_context(|| {
                let known: Vec<&str> = integration::registry().iter().map(|a| a.id()).collect();
                format!("unknown agent {name:?}; known: {}", known.join(", "))
            })?;
            if agent.scopes().is_empty() {
                bail!("{name} is not supported: {}", agent.unsupported_reason());
            }
            if !agent.scopes().contains(&target.scope()) {
                bail!(
                    "{name} installs plugins per user, not per project; use --user \
                     (it then runs in every {name} session)"
                );
            }
            if spec.config.is_some() && agent.config_var().is_none() {
                bail!("{name} has no setting for another configuration directory");
            }
            Ok((agent, spec.config.clone()))
        })
        .collect()
}

/// `thirdeye install`: the agents named, or — naming none — the ones found
/// on this machine, each confirmed by the human. No agent is wired
/// unasked.
pub fn install(path: Option<PathBuf>, user: bool, specs: &[AgentSpec], units: bool) -> Result<()> {
    let target = target(path, user)?;
    let chosen = if specs.is_empty() {
        choose(&target)?
    } else {
        resolve_agents(specs, &target)?
    };
    let mut outcome = Ok(());
    let mut wired = 0;
    for (agent, config) in &chosen {
        let done = with_agent(agent.as_ref(), config.as_deref(), |cx| {
            warn_if_build_artifact(&cx.binary);
            agent.install(cx, &target)
        });
        if let Err(e) = done {
            outcome = Err(e.context(format!("wiring {} into {}", agent.id(), target.describe())));
            break;
        }
        println!("{}: wired into {}", agent.id(), target.describe());
        println!("  next: {}", agent.next_steps(&target));
        if cfg!(windows) && !agent.verified_on_windows() {
            eprintln!(
                "  warning: {} is not verified on native Windows: its hooks are written \
                 for sh and may record nothing here. It is supported from WSL.",
                agent.id()
            );
        }
        wired += 1;
    }
    if chosen.is_empty() {
        println!("nothing wired");
        return outcome;
    }
    let recorded = with_context(|cx| {
        // The record vouches for the whole directory, so every agent's part
        // is brought up to this build, not only the ones just wired.
        let complete = publish_all(cx);
        published::record(&cx.data, &cx.binary, None, complete)
    });
    // The viewer opens through the units, so they follow the first agent
    // wired — and nothing wired asks for nothing.
    if units && wired > 0 {
        ensure_units();
    }
    // The wiring's own error first: it is the one the human asked about.
    outcome.and(recorded)
}

/// The agents on this machine that fit `target`, each confirmed on the
/// terminal. The rest are named, with why they are not offered.
fn choose(target: &Target) -> Result<Vec<Chosen>> {
    let place = match target {
        Target::Project(dir) => dir.display().to_string(),
        Target::User => "--user".to_string(),
    };
    let mut fits = Vec::new();
    for agent in integration::registry() {
        if !integration::detected(agent.program()) {
            continue;
        }
        if agent.scopes().contains(&target.scope()) {
            fits.push(agent);
        } else if agent.scopes().is_empty() {
            println!("{}: found, but {}", agent.id(), agent.unsupported_reason());
        } else {
            println!(
                "{}: found; it installs per user only: thirdeye install --user -a {}",
                agent.id(),
                agent.id()
            );
        }
    }
    if fits.is_empty() {
        bail!(
            "no agent thirdeye can wire into {} was found on PATH",
            target.describe()
        );
    }
    let Some(mut tty) = Tty::open() else {
        // Under `curl … | sh` stdin is the script, and in CI there is
        // nobody to ask: say how, and wire nothing.
        let ids: Vec<String> = fits.iter().map(|a| format!("-a {}", a.id())).collect();
        bail!(
            "no terminal to ask on; name the agents: thirdeye install {} {place}",
            ids.join(" ")
        );
    };
    let mut chosen = Vec::new();
    for agent in fits {
        let question = match target {
            Target::Project(dir) => format!("wire {} into {}?", agent.id(), dir.display()),
            Target::User => format!(
                "wire {} for every {} session you run?",
                agent.id(),
                agent.id()
            ),
        };
        if tty.ask(&question, true)? {
            chosen.push((agent, None));
        }
    }
    Ok(chosen)
}

/// Unwire `specs` from one target — or, naming none, every agent thirdeye
/// finds wired there.
pub fn uninstall(path: Option<PathBuf>, user: bool, specs: &[AgentSpec]) -> Result<()> {
    let target = target(path, user)?;
    let chosen = if specs.is_empty() {
        with_context(|cx| Ok(wired(cx, &target)))?
            .into_iter()
            .map(|a| (a, None))
            .collect()
    } else {
        resolve_agents(specs, &target)?
    };
    if chosen.is_empty() {
        println!("nothing of thirdeye's is wired into {}", target.describe());
        return Ok(());
    }
    let mut failed = 0;
    for (agent, config) in &chosen {
        match with_agent(agent.as_ref(), config.as_deref(), |cx| {
            agent.uninstall(cx, &target)
        }) {
            Ok(()) => println!("{}: removed from {}", agent.id(), target.describe()),
            Err(e) => {
                failed += 1;
                eprintln!("{}: {e:#}", agent.id());
            }
        }
    }
    if failed > 0 {
        bail!("{failed} integration(s) could not be removed");
    }
    Ok(())
}

/// The agents with something of thirdeye's at `target`.
pub fn wired(cx: &Context, target: &Target) -> Vec<Box<dyn Integration>> {
    integration::registry()
        .into_iter()
        .filter(|a| a.scopes().contains(&target.scope()) && a.inspect(cx, target).is_some())
        .collect()
}

/// Rewrite thirdeye's directory for every agent that has a part of it, and
/// have the agents that copy what they load take a new copy. Touches no
/// project.
pub fn refresh(installed_by: Option<Source>) -> Result<()> {
    with_context(|cx| {
        warn_if_build_artifact(&cx.binary);
        let mut failed = 0;
        for agent in integration::registry() {
            if agent.scopes().is_empty() || !cx.data_for(agent.id()).is_dir() {
                continue;
            }
            match agent.refresh(cx) {
                Ok(()) => println!("{}: refreshed", agent.id()),
                Err(e) => {
                    failed += 1;
                    eprintln!("{}: {e:#}", agent.id());
                }
            }
        }
        refresh_units(&cx.binary);
        // Recorded even with nothing wired yet: `install.sh` runs this on a
        // fresh install, and `self update` needs to know who owns the binary.
        published::record(&cx.data, &cx.binary, installed_by, failed == 0)?;
        if failed > 0 {
            bail!("{failed} agent(s) could not be refreshed");
        }
        Ok(())
    })
}

/// Units that are installed are rewritten, so a changed template follows an
/// update; none are installed by a refresh.
fn refresh_units(binary: &Path) {
    let dir = crate::cmd_units::unit_dir();
    if cfg!(windows) || !dir.join("thirdeye@.service").exists() {
        return;
    }
    match crate::cmd_units::install(&dir, binary, &crate::cmd_units::UserSystemctl) {
        Ok(_) => println!("units: refreshed"),
        Err(e) => eprintln!("units: {e:#}"),
    }
}

/// The hook's half of an update by a tool that runs nothing of ours
/// afterwards: if thirdeye's directory was published by another build,
/// publish it again. Files only, no agent CLI — a hook must stay quick —
/// so an agent that copies catches up at `thirdeye refresh`.
pub fn publish_if_stale() {
    if let (Ok(data), Ok(binary)) = (data_dir(), binary()) {
        publish_if_stale_in(&data, &binary, thirdeye_core::home_dir());
    }
}

fn publish_if_stale_in(data: &Path, binary: &Path, home: Option<PathBuf>) -> bool {
    // Nothing published yet: nothing installed through this binary.
    let Some(before) = Published::load(data) else {
        return false;
    };
    // Only the binary that published it, replaced in place by a package
    // manager. A build run from `target/` would otherwise rewrite every
    // integration to name itself, and the next `cargo clean` silence them.
    if before.is_this_build() || before.binary != binary {
        return false;
    }
    let quiet = |_: &str, _: &[&str], _: Option<&Path>| -> Result<String> {
        bail!("no agent CLI from a hook")
    };
    let cx = Context {
        binary: binary.to_path_buf(),
        data: data.to_path_buf(),
        home,
        var: &env_var,
        run: &quiet,
    };
    // A failed publish — on Windows, a file another process holds — leaves
    // the build old, so the next hook tries again.
    publish_all(&cx) && published::record(data, binary, None, true).is_ok()
}

/// Publish every agent that has a part of thirdeye's directory: files only,
/// no agent CLI. Whether all of them succeeded.
fn publish_all(cx: &Context) -> bool {
    let mut ok = true;
    for agent in integration::registry() {
        if !agent.scopes().is_empty() && cx.data_for(agent.id()).is_dir() {
            ok &= agent.publish(cx).is_ok();
        }
    }
    ok
}

fn warn_if_build_artifact(binary: &Path) {
    if crate::cmd_units::is_build_artifact(binary) {
        // Worth interrupting for: every hook would name a build tree, and
        // the next `cargo clean` would silence them all.
        eprintln!(
            "warning: wiring agents to {} — a build tree. Install the binary \
             (`cargo xtask install`, or `cargo install --path crates/thirdeye`) \
             and run this from there, or the hooks break on the next `cargo clean`.",
            binary.display()
        );
    }
}

/// The units, where a systemd user manager runs and they are not there
/// yet: the viewer opens through them. A failure is reported, not fatal —
/// the wiring itself is done, and records whatever happens.
fn ensure_units() {
    let dir = crate::cmd_units::unit_dir();
    if cfg!(windows)
        || dir.join("thirdeye@.service").exists()
        || !crate::cmd_units::systemd_user_manager()
    {
        return;
    }
    let installed = binary().and_then(|binary| {
        crate::cmd_units::install(&dir, &binary, &crate::cmd_units::UserSystemctl)
    });
    match installed {
        Ok(_) => println!("units: installed in {}", dir.display()),
        Err(e) => eprintln!("units: {e:#}"),
    }
}

/// The terminal, read directly: stdin may be a pipe holding the very
/// script that ran this.
struct Tty {
    input: std::io::BufReader<std::fs::File>,
}

impl Tty {
    fn open() -> Option<Self> {
        let path = if cfg!(windows) { "CONIN$" } else { "/dev/tty" };
        let file = std::fs::File::open(path).ok()?;
        Some(Self {
            input: std::io::BufReader::new(file),
        })
    }

    fn ask(&mut self, question: &str, default: bool) -> Result<bool> {
        use std::io::{BufRead, Write};
        let hint = if default { "[Y/n]" } else { "[y/N]" };
        print!("{question} {hint} ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if self.input.read_line(&mut line)? == 0 {
            return Ok(false);
        }
        Ok(match line.trim().to_ascii_lowercase().as_str() {
            "" => default,
            "y" | "yes" => true,
            _ => false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(text: &str) -> AgentSpec {
        text.parse().unwrap()
    }

    fn refused(specs: &[&str], target: &Target) -> String {
        let specs: Vec<AgentSpec> = specs.iter().map(|s| spec(s)).collect();
        resolve_agents(&specs, target).err().unwrap().to_string()
    }

    #[test]
    fn a_user_only_agent_is_refused_a_project_before_anything_is_written() {
        let err = refused(&["claude", "copilot"], &Target::Project("/p".into()));
        assert!(err.contains("copilot") && err.contains("--user"), "{err}");
    }

    #[test]
    fn an_unknown_agent_names_the_known_ones() {
        let err = refused(&["cursor"], &Target::User);
        assert!(err.contains("cursor") && err.contains("claude"), "{err}");
    }

    #[test]
    fn an_agent_with_no_config_variable_refuses_a_config_directory() {
        let err = refused(&["antigravity:/x"], &Target::User);
        assert!(err.contains("no setting"), "{err}");
        assert!(resolve_agents(&[spec("codex:/x")], &Target::User).is_ok());
    }

    #[test]
    fn an_agent_spec_takes_its_directory_after_the_first_colon() {
        assert_eq!(spec("claude").config, None);
        let windows = spec(r"claude:C:\Users\me\.claude");
        assert_eq!(windows.id, "claude");
        // Absolute only on Windows; elsewhere it is joined to the cwd.
        let config = windows.config.unwrap().display().to_string();
        assert!(config.ends_with(r"C:\Users\me\.claude"), "{config}");
        assert!("claude:".parse::<AgentSpec>().is_err());
        assert!(":/x".parse::<AgentSpec>().is_err());
    }

    #[test]
    fn a_config_directory_is_made_absolute_and_tilde_is_home() {
        let home = Path::new("/home/me");
        let cwd = Path::new("/work");
        assert_eq!(
            expand("~/.my-claude", Some(home), Some(cwd)),
            home.join(".my-claude")
        );
        assert_eq!(expand("~", Some(home), Some(cwd)), home);
        assert_eq!(expand("cfg", Some(home), Some(cwd)), cwd.join("cfg"));
        assert_eq!(
            expand("~other/x", Some(home), Some(cwd)),
            cwd.join("~other/x")
        );
    }

    #[test]
    fn a_hook_republishes_only_for_the_binary_that_published() {
        let base = std::env::temp_dir().join(format!("thirdeye-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let data = base.join("data");
        let installed = base.join("bin/thirdeye");
        std::fs::create_dir_all(data.join("copilot")).unwrap();

        assert!(
            !publish_if_stale_in(&data, &installed, None),
            "nothing published yet"
        );
        std::fs::write(
            data.join(published::FILE),
            format!(
                "build = \"0.0.1 (old)\"\nbinary = {:?}\n",
                installed.display().to_string()
            ),
        )
        .unwrap();
        // A build tree's binary running a hook must not take the directory over.
        assert!(!publish_if_stale_in(
            &data,
            &base.join("target/debug/thirdeye"),
            None
        ));
        assert!(!data.join("copilot/thirdeye/hooks.json").exists());
        assert!(publish_if_stale_in(&data, &installed, None));
        let hooks = std::fs::read_to_string(data.join("copilot/thirdeye/hooks.json")).unwrap();
        // As JSON spells it: a Windows path's backslashes are escaped.
        let spelled = serde_json::to_string(&installed.display().to_string()).unwrap();
        assert!(hooks.contains(spelled.trim_matches('"')), "{hooks}");
        assert!(Published::load(&data).unwrap().is_this_build());
        assert!(!publish_if_stale_in(&data, &installed, None), "once");
    }

    #[test]
    fn the_home_directory_is_not_taken_for_a_project() {
        let home = std::env::temp_dir();
        assert!(refuse_home(&home, Some(&home)).is_err());
        assert!(refuse_home(&home.join("src/app"), Some(&home)).is_ok());
        assert!(refuse_home(&home, None).is_ok());
    }

    #[test]
    fn a_target_is_named_once_and_never_assumed() {
        assert!(target(Some("/p".into()), true).is_err());
        assert!(target(None, false).is_err(), "no project is assumed");
        let missing = std::env::temp_dir().join("thirdeye-no-such-project-ever");
        assert!(
            target(Some(missing), false).is_err(),
            "a typo is no project"
        );
        assert_eq!(target(None, true).unwrap(), Target::User);
    }
}
