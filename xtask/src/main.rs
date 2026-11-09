//! Developer tasks: `configure`, `install`, `uninstall`, `doctor`.
//!
//! Modelled on `./configure && make install`, and for the same reason: probing
//! the machine and acting on it are different jobs. `configure` finds out what
//! is here and writes it down; `install` obeys that file and decides nothing.
//! A machine where a probe guessed wrong therefore has a place to be corrected,
//! and a run can be reviewed before it happens.
//!
//! The design, and the three criticisms that produced it, are recorded in
//! `docs/design/plans/2026-08-02-developer-protocol.md`.
//!
//! **Dev and production differ by profile and prefix, nothing else.** The
//! wiring is the binary's own: `install` builds and installs it, then hands
//! each agent to `thirdeye install`, exactly as a user's `thirdeye install`
//! would. If that stops being true, so does the promise that a development
//! build matches what ships.

mod config;

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use config::Config;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str).unwrap_or("help") {
        "configure" => configure(&args),
        "install" => install(),
        "uninstall" => uninstall(),
        "doctor" => doctor(),
        _ => {
            help();
            Ok(())
        }
    }
}

fn help() {
    println!(
        "cargo xtask <task>

  configure [flags]   probe this machine, report what was found, write install.toml
  install             build and install exactly what install.toml says
  uninstall           reverse it, using the same file
  doctor              compare what is installed against install.toml and the source

configure flags:
  --prefix <dir>          where the binary goes (default: $HOME/.local)
  --profile dev|release   build profile (default: dev)
  --scope project|user    how the wiring is recorded (default: project).
                          user wires every session the agent runs.
  --agent <id>=<project>  wire an agent into a project; repeatable.
                          Nothing is wired unless you say so. Copilot and
                          Grok are wired per user whatever the scope.
  --no-units              skip systemd even if a user manager is present

Restart your agent after installing: plugins, hooks, MCP servers and skills are
read at session start."
    );
}

fn repo() -> Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .context("locating the repository root")
}

/// `%USERPROFILE%` on Windows: where the agents themselves look, and set
/// whether or not this was started from a shell that invents a `HOME`.
pub(crate) fn home() -> Result<PathBuf> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(key)
        .map(PathBuf::from)
        .with_context(|| format!("{key} is not set"))
}

// --- configure -----------------------------------------------------------

fn configure(args: &[String]) -> Result<()> {
    let repo = repo()?;
    let mut prefix = home()?.join(".local");
    let mut profile = "dev".to_string();
    let mut scope = "project".to_string();
    let mut want_units = true;
    let mut agents: Vec<(String, PathBuf)> = Vec::new();

    let mut rest = args.iter().skip(1);
    while let Some(flag) = rest.next() {
        match flag.as_str() {
            "--prefix" => {
                prefix = PathBuf::from(rest.next().context("--prefix needs a directory")?)
            }
            "--profile" => profile = rest.next().context("--profile needs a value")?.clone(),
            "--scope" => scope = rest.next().context("--scope needs a value")?.clone(),
            "--no-units" => want_units = false,
            "--agent" => {
                let spec = rest.next().context("--agent needs <id>=<project>")?;
                let (id, project) = spec
                    .split_once('=')
                    .context("--agent expects <id>=<project>, e.g. claude=/path/to/project")?;
                agents.push((id.to_string(), PathBuf::from(project)));
            }
            other => bail!("unknown flag: {other}"),
        }
    }

    if profile != "dev" && profile != "release" {
        bail!("--profile must be dev or release, not {profile}");
    }
    if !["local", "project", "user"].contains(&scope.as_str()) {
        bail!("--scope must be project or user, not {scope}");
    }

    // Probe, and say what was found. Every line is a fact about *this* machine,
    // which is the whole reason the file exists.
    println!("probing this machine:");
    println!("  prefix                 {}", prefix.display());
    println!("  profile                {profile}");
    println!(
        "  wiring scope           {scope}{}",
        match scope.as_str() {
            "user" => "  (every session you run, everywhere)",
            _ => "  (the named projects)",
        }
    );

    let unit_dir = match (want_units, systemd_user_manager()) {
        // Nothing to install: the hook starts the daemon itself there.
        _ if cfg!(windows) => {
            println!("  systemd user manager   none on Windows — the hook starts the daemon");
            None
        }
        (false, _) => {
            println!("  systemd user manager   skipped (--no-units)");
            None
        }
        (true, false) => {
            println!("  systemd user manager   absent — start the daemon by hand");
            None
        }
        (true, true) => {
            let dir = unit_dir();
            println!("  systemd user manager   present");
            println!("  unit directory         {}", dir.display());
            Some(dir)
        }
    };

    // Which agents exist is the binary's knowledge (`thirdeye install <path>`
    // lists them); `install` refuses an unknown one before wiring anything.
    for (id, project) in &agents {
        if id.is_empty() || id.contains(char::is_whitespace) {
            bail!("{id:?} is not an agent id");
        }
        if !project.is_dir() {
            bail!("{} is not a directory", project.display());
        }
    }

    if agents.is_empty() {
        println!("  agent wiring           none requested");
    }
    for (id, project) in &agents {
        println!("  agent wiring           {id} -> {}", project.display());
    }

    let written = Config {
        prefix,
        profile,
        scope,
        unit_dir,
        agents,
    }
    .save(&repo)?;
    println!("\nwrote {}", written.display());
    println!("next: cargo xtask install");
    Ok(())
}

/// Whether a systemd *user* manager is actually usable here.
///
/// `systemctl` existing is not enough: it is present in containers and on
/// machines where the user manager never started, and units installed there
/// would be files nothing ever reads.
fn systemd_user_manager() -> bool {
    Command::new("systemctl")
        .args(["--user", "show", "--property=Version"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn unit_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("systemd/user")
}

// --- install -------------------------------------------------------------

fn install() -> Result<()> {
    let repo = repo()?;
    let config = Config::load(&repo)?;

    step("building");
    run(
        Command::new("cargo").args(["build", "-p", "thirdeye", "--profile", &config.profile]),
        &repo,
    )?;

    step(&format!(
        "installing the binary into {}",
        config.bin_dir().display()
    ));
    // `cargo install` rather than copying out of `target/`: it is what a user
    // runs, and a different mechanism here would mean development exercises a
    // path production never takes. `--target-dir target` keeps one build cache
    // rather than two.
    run(
        Command::new("cargo").args([
            "install",
            "--path",
            "crates/thirdeye",
            "--root",
            &config.prefix.display().to_string(),
            "--target-dir",
            "target",
            "--force",
            "--profile",
            &config.profile,
        ]),
        &repo,
    )?;

    match &config.unit_dir {
        Some(dir) => {
            step(&format!("installing systemd units into {}", dir.display()));
            run(
                Command::new(config.binary()).args(["units", "install"]),
                &repo,
            )?;
        }
        None if cfg!(windows) => {}
        None => println!("==> systemd units: not configured, skipping"),
    }

    if config.agents.is_empty() {
        println!("==> agents: none configured");
        println!("    to wire one:  cargo xtask configure --agent claude=/path/to/project");
    }
    for (id, project) in &config.agents {
        step(&format!("wiring {id} into {}", project.display()));
        run(
            Command::new(config.binary()).args(wiring("install", id, project, &config.scope)),
            &repo,
        )?;
    }

    step("recording the install");
    run(
        Command::new(config.binary()).args(["refresh", "--installed-by", "xtask"]),
        &repo,
    )?;

    step("checking");
    run(&mut doctor_command(&config), &repo)?;

    println!(
        "\n{} build installed. Restart your agent — plugins, hooks, MCP and skills\nare read at session start.",
        config.profile
    );
    Ok(())
}

fn uninstall() -> Result<()> {
    let repo = repo()?;
    let config = Config::load(&repo)?;

    for (id, project) in &config.agents {
        step(&format!("unwiring {id} from {}", project.display()));
        if config.binary().exists() {
            let _ = run(
                Command::new(config.binary()).args(wiring("uninstall", id, project, &config.scope)),
                &repo,
            );
        }
    }
    if config.unit_dir.is_some() && config.binary().exists() {
        step("removing the systemd units");
        let _ = run(
            Command::new(config.binary()).args(["units", "uninstall"]),
            &repo,
        );
    }
    // The binary stays deliberately: removing something another project may be
    // using is a bigger decision than this command is making.
    println!(
        "\nremoved. The binary at {} is left in place; delete it if you want it gone.",
        config.binary().display()
    );
    Ok(())
}

fn doctor() -> Result<()> {
    let repo = repo()?;
    let config = Config::load(&repo)?;
    let binary = config.binary();
    if !binary.exists() {
        bail!(
            "no thirdeye at {} — run `cargo xtask install`",
            binary.display()
        );
    }
    run(&mut doctor_command(&config), &repo)
}

/// `thirdeye doctor` on every configured project, so the integrations
/// there are checked against this build too — or `--user` with none: the
/// binary keeps no list of projects, so it is told them.
fn doctor_command(config: &Config) -> Command {
    let mut doctor = Command::new(config.binary());
    doctor.arg("doctor");
    let mut projects: Vec<&PathBuf> = config.agents.iter().map(|(_, p)| p).collect();
    projects.sort();
    projects.dedup();
    if projects.is_empty() {
        doctor.arg("--user");
    }
    doctor.args(projects);
    doctor
}

/// `thirdeye <action> -a <id> <p>`, or `--user` — which Copilot and Grok
/// always get: they have no per-project plugins, and the developer named
/// the agent, so refusing would wire nothing for a distinction the agent
/// does not make. The units are xtask's own step, per `install.toml`.
fn wiring(action: &str, id: &str, project: &Path, scope: &str) -> Vec<String> {
    let mut args = vec![action.to_string(), "-a".into(), id.to_string()];
    if scope == "user" || USER_ONLY.contains(&id) {
        if scope != "user" {
            println!("    {id}: wired for your user — it has no per-project plugins");
        }
        args.push("--user".into());
    } else {
        args.push(project.display().to_string());
    }
    if action == "install" {
        args.push("--no-units".into());
    }
    args
}

/// Agents whose plugins are per user only.
const USER_ONLY: &[&str] = &["copilot", "grok"];

fn step(what: &str) {
    println!("==> {what}");
}

fn run(command: &mut Command, dir: &Path) -> Result<()> {
    let status = command
        .current_dir(dir)
        .status()
        .with_context(|| format!("running {:?}", command.get_program()))?;
    if !status.success() {
        bail!("{:?} exited with {status}", command.get_program());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_is_wired_by_the_binary_into_the_named_project() {
        assert_eq!(
            wiring("install", "codex", Path::new("/p"), "project"),
            ["install", "-a", "codex", "/p", "--no-units"]
        );
        // An install.toml from before scopes were renamed.
        assert_eq!(
            wiring("install", "claude", Path::new("/p"), "local"),
            ["install", "-a", "claude", "/p", "--no-units"]
        );
        assert_eq!(
            wiring("uninstall", "pi", Path::new("/p"), "user"),
            ["uninstall", "-a", "pi", "--user"]
        );
        assert_eq!(
            wiring("install", "copilot", Path::new("/p"), "project"),
            ["install", "-a", "copilot", "--user", "--no-units"]
        );
    }
}
