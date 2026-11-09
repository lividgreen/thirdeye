//! `install.toml` — what `configure` decided, and `install` obeys.
//!
//! The autotools separation, and for the autotools reason: probing and acting
//! in one step means nothing can be reviewed before it happens, and a machine
//! where a guess is wrong has nowhere to correct it. `configure` writes this
//! file; `install` reads it and does exactly what it says, with no probing and
//! no defaults of its own.
//!
//! Gitignored, because it describes one machine and not the project.
//!
//! Parsed by hand rather than with a TOML crate: the grammar is `key = value`
//! and one repeated table, and the project has hand-rolled FNV-1a and JSON-RPC
//! for smaller reasons than avoiding a dependency in a build tool.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub const FILE: &str = "install.toml";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Where the binary goes: `<prefix>/bin/thirdeye`.
    pub prefix: PathBuf,
    /// `dev` or `release`.
    pub profile: String,
    /// Where systemd user units go, if units are wanted at all.
    pub unit_dir: Option<PathBuf>,
    /// How visible the wiring should be: `local`, `project` or `user`.
    ///
    /// A general notion each agent maps onto its own vocabulary. Defaults to
    /// `local` because `project` writes this machine's absolute paths into a
    /// committable file — fine for your own repo, wrong the moment you wire
    /// thirdeye into someone else's.
    pub scope: String,
    /// Agent id → the projects to wire it into. Empty is normal and means
    /// "install the tool, wire nothing".
    pub agents: Vec<(String, PathBuf)>,
}

impl Config {
    pub fn bin_dir(&self) -> PathBuf {
        self.prefix.join("bin")
    }

    pub fn binary(&self) -> PathBuf {
        self.bin_dir()
            .join(format!("thirdeye{}", std::env::consts::EXE_SUFFIX))
    }

    pub fn path_in(repo: &Path) -> PathBuf {
        repo.join(FILE)
    }

    pub fn load(repo: &Path) -> Result<Self> {
        let path = Self::path_in(repo);
        let raw = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "no {FILE} — run `cargo xtask configure` first (it probes this machine \
                 and records what it found)"
            )
        })?;
        Self::parse(&raw).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, repo: &Path) -> Result<PathBuf> {
        let path = Self::path_in(repo);
        std::fs::write(&path, self.render())
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(path)
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(
            "# Written by `cargo xtask configure`. Describes this machine, not the\n\
             # project — it is gitignored. Re-run configure to change it; `install`\n\
             # never decides anything this file does not say.\n\n",
        );
        let _ = writeln!(out, "prefix = \"{}\"", self.prefix.display());
        let _ = writeln!(out, "profile = \"{}\"", self.profile);
        let _ = writeln!(out, "scope = \"{}\"", self.scope);
        match &self.unit_dir {
            Some(dir) => {
                let _ = writeln!(out, "unit_dir = \"{}\"", dir.display());
            }
            None => {
                let _ = writeln!(
                    out,
                    "# unit_dir unset: no systemd user manager was found, so the daemon\n\
                     # must be started by hand (`thirdeye daemon --session <id>`)."
                );
            }
        }
        if self.agents.is_empty() {
            let _ = writeln!(
                out,
                "\n# No agent wired. Add one with:\n\
                 #   cargo xtask configure --agent claude=/path/to/your/project"
            );
        }
        for (id, project) in &self.agents {
            let _ = writeln!(out, "\n[[agent]]");
            let _ = writeln!(out, "id = \"{id}\"");
            let _ = writeln!(out, "project = \"{}\"", project.display());
        }
        out
    }

    pub fn parse(raw: &str) -> Result<Self> {
        let mut prefix = None;
        let mut profile = None;
        let mut scope = None;
        let mut unit_dir = None;
        let mut agents: Vec<(String, PathBuf)> = Vec::new();
        // `[[agent]]` accumulates a pair across the two lines that follow it.
        let mut pending: Option<(Option<String>, Option<PathBuf>)> = None;

        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line == "[[agent]]" {
                flush(&mut pending, &mut agents)?;
                pending = Some((None, None));
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                bail!("cannot read line: {line}");
            };
            let key = key.trim();
            let value = value.trim().trim_matches('"');
            match (key, pending.as_mut()) {
                ("id", Some(slot)) => slot.0 = Some(value.to_string()),
                ("project", Some(slot)) => slot.1 = Some(PathBuf::from(value)),
                ("prefix", _) => prefix = Some(PathBuf::from(value)),
                ("profile", _) => profile = Some(value.to_string()),
                ("scope", _) => scope = Some(value.to_string()),
                ("unit_dir", _) => unit_dir = Some(PathBuf::from(value)),
                (other, _) => bail!("unknown setting: {other}"),
            }
        }
        flush(&mut pending, &mut agents)?;

        Ok(Config {
            prefix: prefix.context("prefix is missing")?,
            profile: profile.context("profile is missing")?,
            // Files written before the setting existed predate multi-project
            // wiring, where the default was the only behaviour.
            scope: scope.unwrap_or_else(|| "project".to_string()),
            unit_dir,
            agents,
        })
    }
}

fn flush(
    pending: &mut Option<(Option<String>, Option<PathBuf>)>,
    agents: &mut Vec<(String, PathBuf)>,
) -> Result<()> {
    let Some((id, project)) = pending.take() else {
        return Ok(());
    };
    let id = id.context("an [[agent]] block has no id")?;
    let project = project.context("an [[agent]] block has no project")?;
    agents.push((id, project));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Config {
        Config {
            prefix: PathBuf::from("/home/u/.local"),
            profile: "dev".into(),
            scope: "local".into(),
            unit_dir: Some(PathBuf::from("/home/u/.config/systemd/user")),
            agents: vec![
                ("claude".into(), PathBuf::from("/home/u/work/app")),
                ("claude".into(), PathBuf::from("/home/u/work/other")),
            ],
        }
    }

    #[test]
    fn a_configuration_survives_the_round_trip() {
        assert_eq!(Config::parse(&sample().render()).unwrap(), sample());
    }

    #[test]
    fn the_binary_path_follows_the_prefix() {
        // The one thing every other step depends on.
        assert_eq!(
            sample().binary(),
            PathBuf::from(format!(
                "/home/u/.local/bin/thirdeye{}",
                std::env::consts::EXE_SUFFIX
            ))
        );
    }

    #[test]
    fn a_machine_without_systemd_round_trips_as_no_unit_dir() {
        // Absent, not empty: "no user manager here" is a normal state, and the
        // rendered file explains it rather than leaving a blank value.
        let mut config = sample();
        config.unit_dir = None;
        let rendered = config.render();
        assert!(rendered.contains("no systemd user manager"), "{rendered}");
        assert_eq!(Config::parse(&rendered).unwrap().unit_dir, None);
    }

    #[test]
    fn no_agents_round_trips_and_says_how_to_add_one() {
        // The default after the revision: wiring an agent is opt-in, so the
        // empty case must not look like a mistake.
        let mut config = sample();
        config.agents.clear();
        let rendered = config.render();
        assert!(rendered.contains("--agent claude="), "{rendered}");
        assert!(Config::parse(&rendered).unwrap().agents.is_empty());
    }

    #[test]
    fn several_projects_for_one_agent_are_kept_in_order() {
        let parsed = Config::parse(&sample().render()).unwrap();
        assert_eq!(parsed.agents.len(), 2);
        assert_eq!(parsed.agents[0].1, PathBuf::from("/home/u/work/app"));
        assert_eq!(parsed.agents[1].1, PathBuf::from("/home/u/work/other"));
    }

    #[test]
    fn a_file_written_before_scope_existed_still_loads() {
        // Old install.toml files predate the setting; refusing them would make
        // a pull break someone's checkout for no reason.
        let parsed = Config::parse("prefix = \"/x\"\nprofile = \"dev\"\n").unwrap();
        assert_eq!(parsed.scope, "project");
    }

    #[test]
    fn a_typo_is_refused_rather_than_ignored() {
        // Silently dropping an unknown key would let `install` do something
        // other than what the file appears to say.
        let err = Config::parse("prefix = \"/x\"\nprofile = \"dev\"\npreffix = \"/y\"\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("preffix"), "{err}");
    }

    #[test]
    fn a_half_written_agent_block_is_an_error() {
        let err = Config::parse("prefix = \"/x\"\nprofile = \"dev\"\n[[agent]]\nid = \"claude\"\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("project"), "{err}");
    }

    #[test]
    fn a_missing_file_names_the_command_that_creates_it() {
        // The error is the only place someone learns the configure step exists.
        let err = Config::load(Path::new("/no/such/repo"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("cargo xtask configure"), "{err}");
    }
}
