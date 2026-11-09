//! `thirdeye doctor` — what is actually installed, and does it agree.
//!
//! The binary, thirdeye's own directory of integration files, the wiring in
//! each agent and the units are separate artifacts, and an agent may load a
//! copy of its own. Nothing forces them to agree, so a partial update leaves
//! every component behaving correctly while the system is wrong.
//!
//! That is not a hypothesis. During milestone 7 the plugin was current, the
//! binary was eighteen minutes stale, the hook recorded no kitty address, the
//! daemon correctly concluded "no surface", and nothing reported a problem.
//! This command exists so that failure produces a sentence instead of an
//! afternoon.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use crate::cmd_install::AgentSpec;
use crate::integration::{self, AgentSays, Context, Inspection, Integration, Target};
use crate::published::{self, Published};

/// What this build was made from — see `build.rs`.
pub const BUILD: &str = env!("THIRDEYE_BUILD");

#[derive(Debug, PartialEq, Eq)]
pub enum Status {
    /// Checked and consistent.
    Ok(String),
    /// Checked and inconsistent — with what to do about it.
    Drift { detail: String, fix: String },
    /// Deliberately not checked, and why. A green that hides its own blind
    /// spots will eventually vouch for something it never examined.
    Unchecked(String),
    /// True and worth knowing, but not a disagreement between artifacts.
    ///
    /// Separate from `Drift` so the closing count means what it says: a
    /// contributor whose every run reports "1 thing disagrees" for an expected
    /// condition stops reading the number.
    Note(String),
}

#[derive(Debug)]
pub struct Finding {
    pub subject: &'static str,
    pub status: Status,
}

/// What the units' `ExecStart` binary reports as its build.
///
/// Comparing *paths* would be the wrong question: running `doctor` from
/// `target/debug` while the units correctly point at the installed copy is
/// normal, not drift. What matters is whether the binary systemd will actually
/// launch came from this source — which is exactly the milestone 7 failure,
/// where the installed binary was eighteen minutes behind the checkout.
fn build_of(binary: &Path) -> Option<String> {
    let out = std::process::Command::new(binary)
        .arg("--version")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    // `thirdeye 0.1.0 (abc1234-dirty)`
    let stamp = text.split('(').nth(1)?.split(')').next()?;
    Some(stamp.trim().to_string())
}

/// What puts a binary, and everything naming it, back in agreement.
const REINSTALL: &str =
    "`thirdeye refresh` from the thirdeye you mean to run (from a checkout: cargo xtask install)";

fn unit_status(target: &Path) -> Status {
    binary_status("the unit runs", target)
}

/// Whether `target` exists and is this very build. `what` names who runs it.
fn binary_status(what: &str, target: &Path) -> Status {
    if !target.exists() {
        return Status::Drift {
            detail: format!("{what} {}, which does not exist", target.display()),
            fix: REINSTALL.into(),
        };
    }
    match build_of(target) {
        Some(build) if build == BUILD => Status::Ok(format!(
            "{} runs a binary built from {build}",
            target.display()
        )),
        Some(build) => Status::Drift {
            detail: format!(
                "{what} {}, built from {build}; this source is {BUILD}",
                target.display()
            ),
            fix: REINSTALL.into(),
        },
        None => Status::Unchecked(format!(
            "{} could not report its build; it may predate the build stamp",
            target.display()
        )),
    }
}

/// An installed agent integration: current files, naming this build, and
/// loaded by the agent.
///
/// The milestone-7 failure in a new place — every part installed, and the
/// agent's hooks running a thirdeye that is gone or older — so it gets the
/// same check as the units. A bare `thirdeye` is resolved the way the agent
/// would, through `PATH`.
pub fn integration_status(found: &Inspection, fix: String) -> Status {
    if !found.current {
        return Status::Drift {
            detail: "the installed files are older than, or differ from, what this build \
                     installs"
                .into(),
            fix,
        };
    }
    let binary = if found.binary.is_absolute() {
        Some(found.binary.clone())
    } else {
        crate::invocation::thirdeye_on_path()
    };
    let status = match binary {
        Some(binary) => match binary_status("it runs", &binary) {
            Status::Drift { detail, .. } => Status::Drift { detail, fix },
            other => other,
        },
        None => Status::Drift {
            detail: format!("it runs `{}`, which is not on PATH", found.binary.display()),
            fix: format!("{fix}, or put thirdeye on PATH"),
        },
    };
    match (status, &found.agent_says) {
        (Status::Ok(_), AgentSays::NotLoaded { detail, fix }) => Status::Drift {
            detail: detail.clone(),
            fix: fix.clone(),
        },
        (Status::Ok(detail), AgentSays::Unknown(why)) => {
            Status::Unchecked(format!("{detail}; {why}"))
        }
        (other, _) => other,
    }
}

/// What `agent` has installed for the user and in `projects` — the ones
/// named, since nothing records where else thirdeye was wired.
fn integrations(cx: &Context, agent: &dyn Integration, projects: &[PathBuf]) -> Vec<Finding> {
    let mut findings = Vec::new();
    let targets =
        std::iter::once(Target::User).chain(projects.iter().cloned().map(Target::Project));
    for target in targets.filter(|t| agent.scopes().contains(&t.scope())) {
        let Some(found) = agent.inspect(cx, &target) else {
            continue;
        };
        // What lives in thirdeye's directory is a refresh away; what is in
        // a project or the agent's config needs the install again.
        let fix = if found.location.starts_with(&cx.data) {
            "thirdeye refresh".to_string()
        } else {
            cx.install_command(agent.id(), agent.config_var(), &target)
        };
        let place = match &target {
            Target::User => format!("for your user ({})", found.location.display()),
            Target::Project(dir) => dir.display().to_string(),
        };
        let status = match integration_status(&found, fix) {
            Status::Ok(detail) => Status::Ok(format!("{place} — {detail}")),
            Status::Drift { detail, fix } => Status::Drift {
                detail: format!("{place}: {detail}"),
                fix,
            },
            Status::Unchecked(why) => Status::Unchecked(format!("{place}: {why}")),
            other => other,
        };
        findings.push(Finding {
            subject: agent.id(),
            status,
        });
    }
    findings
}

/// Whether thirdeye's directory was published by this build. A binary a
/// package manager replaced runs nothing of ours afterwards; its first hook
/// republishes, and until then this says so.
pub fn published_finding(data: &Path) -> Option<Finding> {
    let found = Published::load(data)?;
    let status = if found.is_this_build() {
        Status::Ok(format!("{} published by this build", data.display()))
    } else {
        let by = match found.build.as_str() {
            "" => "not completely published by any build".to_string(),
            build => format!("published by thirdeye {build}"),
        };
        Status::Drift {
            detail: format!(
                "{} was {by}; this is {}",
                data.display(),
                published::this_build()
            ),
            fix: "thirdeye refresh".into(),
        }
    };
    Some(Finding {
        subject: "integrations",
        status,
    })
}

pub fn diagnose(binary: &Path, unit_dir: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();

    findings.push(Finding {
        subject: "binary",
        status: Status::Ok(format!(
            "{} — {} ({BUILD})",
            binary.display(),
            env!("CARGO_PKG_VERSION")
        )),
    });

    if BUILD.ends_with("-dirty") {
        findings.push(Finding {
            subject: "source",
            status: Status::Note(
                "built from a working tree with uncommitted changes — you are running \
                 your edits, not your last commit"
                    .into(),
            ),
        });
    }

    // --- units ------------------------------------------------------------
    let service = unit_dir.join("thirdeye@.service");
    match std::fs::read_to_string(&service) {
        // No service manager to install into, so nothing is missing.
        Err(_) if cfg!(windows) => findings.push(Finding {
            subject: "units",
            status: Status::Note(
                "none on Windows — the hook starts the daemon and the end of the \
                 session stops it"
                    .into(),
            ),
        }),
        Err(_) => findings.push(Finding {
            subject: "units",
            status: Status::Unchecked(format!(
                "not installed at {} — the daemon then only runs when started by hand",
                unit_dir.display()
            )),
        }),
        Ok(body) => {
            let target = body
                .lines()
                .find_map(|l| l.strip_prefix("ExecStart="))
                .and_then(|l| l.split_whitespace().next())
                .unwrap_or("");
            findings.push(Finding {
                subject: "units",
                status: unit_status(Path::new(target)),
            });
        }
    }

    findings.push(Finding {
        subject: "session",
        status: Status::Unchecked(
            "nothing on disk records when a session read its plugin config, so an \
             outstanding restart cannot be detected — restart the agent after any install"
                .into(),
        ),
    });

    findings
}

/// Which surface an agent started from this environment would get.
///
/// Takes the captured map rather than reading the environment, both because
/// `std::env::set_var` is unsafe in Rust 2024 and because this must report on
/// the same allowlist the hook records — not on a second, drifting idea of
/// what matters. Always a `Note`: no surface is a state (§12), not a
/// disagreement between artifacts, and counting it as drift would teach the
/// reader to ignore the count.
pub fn surface_finding(env: &std::collections::BTreeMap<String, String>) -> Finding {
    use crate::daemon::surface::{ORDER, chosen, detected, resolve_detached};
    let found = detected(env).join(", ");
    // What gives a split instead. tmux has no Windows build; WezTerm's
    // split works there, and it is a window of its own on every platform.
    let splits = if cfg!(windows) {
        "zellij, or in WezTerm,"
    } else {
        "tmux or zellij"
    };
    let what = match (chosen(env), env.get("THIRDEYE_SURFACE")) {
        (Some(id), _) if resolve_detached(id) => format!(
            "detected [{found}], would use {id}: it cannot put a split beside the \
             agent, so the view opens in a window of its own — unasked only when the agent \
             flags a file, unless you set a policy; run the agent in {splits} for a split"
        ),
        (Some(id), _) => format!("detected [{found}], would use {id}"),
        (None, Some(named)) => {
            format!("detected [{found}], but THIRDEYE_SURFACE={named} selects none of them")
        }
        (None, None) => {
            let known: Vec<&str> = ORDER.iter().map(|(id, _)| *id).collect();
            format!(
                "no surface — this shell is in none of {}, so the view opens only via \
                 `thirdeye head`",
                known.join(", ")
            )
        }
    };
    Finding {
        subject: "surface",
        status: Status::Note(what),
    }
}

/// GNOME Terminal closes a window when its command exits only if the
/// profile says so; otherwise every viewer the daemon closes stays on screen,
/// holding a finished head. `exit_action` is the default profile's setting.
pub fn gnome_exit_finding(exit_action: Option<&str>) -> Option<Finding> {
    let action = exit_action?.trim().trim_matches('\'');
    (action != "close").then(|| Finding {
        subject: "gnome-terminal",
        status: Status::Drift {
            detail: format!(
                "the default profile's \"when command exits\" is {action}, so a closed viewer's \
                 window stays open"
            ),
            fix: "Preferences → profile → Command → When command exits: Exit the terminal".into(),
        },
    })
}

/// The default GNOME Terminal profile's `exit-action`, if `gsettings` can say.
fn gnome_exit_action() -> Option<String> {
    let run = |args: &[&str]| {
        std::process::Command::new("gsettings")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let profile = run(&["get", "org.gnome.Terminal.ProfilesList", "default"])?;
    let schema = format!(
        "org.gnome.Terminal.Legacy.Profile:/org/gnome/terminal/legacy/profiles:/:{}/",
        profile.trim_matches('\'')
    );
    run(&["get", &schema, "exit-action"])
}

/// `projects` empty is `--user`: thirdeye and the user's wiring only;
/// `specs` empty is every agent, each in its default configuration.
pub fn run(projects: &[PathBuf], specs: &[AgentSpec]) -> Result<()> {
    let projects = projects
        .iter()
        .map(|dir| {
            if !dir.is_dir() {
                anyhow::bail!("{} is not a directory", dir.display());
            }
            match crate::cmd_install::target(Some(dir.clone()), false)? {
                Target::Project(root) => Ok(root),
                Target::User => unreachable!("a directory was named"),
            }
        })
        .collect::<Result<Vec<_>>>()?;
    let binary = crate::cmd_units::own_path()?;
    let mut findings = diagnose(&binary, &crate::cmd_units::unit_dir());
    crate::cmd_install::with_context(|cx| {
        findings.extend(published_finding(&cx.data));
        Ok(())
    })?;
    let agents: Vec<(Box<dyn Integration>, Option<PathBuf>)> = if specs.is_empty() {
        integration::registry()
            .into_iter()
            .map(|a| (a, None))
            .collect()
    } else {
        specs
            .iter()
            .map(|spec| {
                let agent = integration::by_id(&spec.id)
                    .with_context(|| format!("unknown agent {:?}", spec.id))?;
                Ok((agent, spec.config.clone()))
            })
            .collect::<Result<_>>()?
    };
    for (agent, config) in &agents {
        findings.extend(crate::cmd_install::with_agent(
            agent.as_ref(),
            config.as_deref(),
            |cx| Ok(integrations(cx, agent.as_ref(), &projects)),
        )?);
    }
    // From doctor's own environment: an agent started from this shell would
    // carry the same variables into its hook.
    let env = crate::daemon::surface::capture(|key| std::env::var(key).ok());
    findings.push(surface_finding(&env));
    if crate::daemon::surface::chosen(&env) == Some("gnome-terminal") {
        findings.extend(gnome_exit_finding(gnome_exit_action().as_deref()));
    }

    let mut drifted = 0;
    for finding in &findings {
        match &finding.status {
            Status::Ok(detail) => println!("  ok        {}: {detail}", finding.subject),
            Status::Unchecked(why) => println!("  unchecked {}: {why}", finding.subject),
            Status::Note(what) => println!("  note      {}: {what}", finding.subject),
            Status::Drift { detail, fix } => {
                drifted += 1;
                println!("  DRIFT     {}: {detail}", finding.subject);
                println!("            fix: {fix}");
            }
        }
    }

    if drifted == 0 {
        println!("\nEverything checkable agrees.");
    } else {
        println!("\n{drifted} thing(s) disagree with the source.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn inspection(binary: &str, current: bool) -> Inspection {
        Inspection {
            binary: binary.into(),
            current,
            location: "/x".into(),
            agent_says: AgentSays::Loaded,
        }
    }

    #[test]
    fn a_directory_another_build_published_is_drift_until_refreshed() {
        let dir = tmp("stamp");
        assert!(published_finding(&dir).is_none(), "nothing installed");
        std::fs::write(dir.join(published::FILE), "build = \"0.0.1 (old)\"\n").unwrap();
        assert!(matches!(
            published_finding(&dir).unwrap().status,
            Status::Drift { ref fix, .. } if fix == "thirdeye refresh"
        ));
        published::record(&dir, Path::new("/bin/thirdeye"), None, true).unwrap();
        assert!(matches!(
            published_finding(&dir).unwrap().status,
            Status::Ok(_)
        ));
    }

    fn note(finding: Finding) -> String {
        match finding.status {
            Status::Note(what) => what,
            other => panic!("a surface finding is never a verdict: {other:?}"),
        }
    }

    #[test]
    fn an_integration_naming_a_missing_binary_is_drift() {
        // The milestone-7 failure in a new place: every part looks installed
        // and the agent's hooks run a thirdeye that is not there.
        let found = inspection("/nowhere/thirdeye", true);
        assert!(matches!(
            integration_status(&found, "fix".into()),
            Status::Drift { .. }
        ));
    }

    #[test]
    fn a_terminal_that_cannot_split_is_reported_as_opening_a_window() {
        let line = note(surface_finding(&env_of(&[("TERM", "xterm-ghostty")])));
        assert!(
            line.contains("would use ghostty") && line.contains("window"),
            "{line}"
        );
    }

    #[test]
    fn a_gnome_profile_that_keeps_finished_windows_is_drift() {
        assert!(gnome_exit_finding(Some("'close'")).is_none());
        assert!(gnome_exit_finding(None).is_none(), "unknown is not drift");
        let finding = gnome_exit_finding(Some("'hold'")).unwrap();
        assert!(
            matches!(finding.status, Status::Drift { ref detail, .. } if detail.contains("hold"))
        );
    }

    #[test]
    fn an_agent_that_does_not_load_the_files_is_drift_even_when_they_are_right() {
        // Copilot with the plugin disabled: every file current, no hook runs.
        let mut found = inspection(
            &std::env::current_exe().unwrap().display().to_string(),
            true,
        );
        found.agent_says = AgentSays::NotLoaded {
            detail: "disabled".into(),
            fix: "enable it".into(),
        };
        match integration_status(&found, "x".into()) {
            Status::Drift { detail, fix } => {
                assert_eq!((detail.as_str(), fix.as_str()), ("disabled", "enable it"))
            }
            // The test binary is not this build's thirdeye, so it may read as
            // drift of its own; never as fine.
            Status::Ok(detail) => panic!("loaded nothing, reported ok: {detail}"),
            _ => {}
        }
    }

    #[test]
    fn a_stale_integration_is_drift_whatever_binary_it_names() {
        let found = inspection(
            &std::env::current_exe().unwrap().display().to_string(),
            false,
        );
        match integration_status(&found, "fix".into()) {
            Status::Drift { detail, .. } => assert!(detail.contains("older"), "{detail}"),
            other => panic!("expected drift, got {other:?}"),
        }
    }

    #[test]
    fn the_surface_line_names_what_was_detected_and_what_won() {
        // The milestone 7 failure this command exists for, in its next form:
        // every component correct, no surface, and nothing saying so. A user
        // in tmux inside kitty needs to see *both* were found and which one
        // was chosen, or "why did it open in the wrong place" has no answer.
        let line = note(surface_finding(&env_of(&[
            ("TMUX", "/tmp/tmux-1000/default,7,0"),
            ("TMUX_PANE", "%3"),
            ("KITTY_LISTEN_ON", "unix:/tmp/kitty-4242"),
        ])));
        assert!(line.contains("tmux, kitty"), "{line}");
        assert!(line.contains("would use tmux"), "{line}");
    }

    #[test]
    fn no_surface_is_reported_as_a_state_not_a_problem() {
        // §12: an agent outside any known container is normal. Printing it as
        // drift would send someone hunting a bug that is not there — which is
        // why `note` panics on anything but a Note.
        let line = note(surface_finding(&env_of(&[])));
        assert!(line.contains("no surface"), "{line}");
    }

    #[test]
    fn an_override_that_selects_nothing_says_so() {
        // Otherwise a stray THIRDEYE_SURFACE reads exactly like "not in a
        // terminal thirdeye knows", and the human checks the wrong thing.
        let line = note(surface_finding(&env_of(&[
            ("THIRDEYE_SURFACE", "kitty"),
            ("TMUX", "/tmp/tmux-1000/default,7,0"),
            ("TMUX_PANE", "%3"),
        ])));
        assert!(line.contains("THIRDEYE_SURFACE=kitty"), "{line}");
    }

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("thirdeye-doc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Serializes "write a fake binary, then execute it".
    ///
    /// `execve` fails with `ETXTBSY` while any process holds that file open for
    /// writing, and a `fork` in another thread hands *that thread's* open write
    /// descriptor to the child, which holds it until its own `exec` completes.
    /// So one test writing its fake binary while a sibling spawns one is enough
    /// to make the first unrunnable — the tests below are the only code here
    /// that both writes an executable and runs one, so serializing them removes
    /// the overlap entirely.
    ///
    /// The race is not new; Rust 1.97 merely made it reproducible, at roughly
    /// three runs in twenty. Under 1.94 it never surfaced in sixty.
    #[cfg(unix)]
    static EXEC: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Take [`EXEC`], ignoring poisoning.
    ///
    /// A test that panicked while holding it has already reported its own
    /// failure; turning that into a second failure in every later test would
    /// bury the one that mattered.
    #[cfg(unix)]
    fn exec_guard() -> std::sync::MutexGuard<'static, ()> {
        EXEC.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn a_unit_pointing_at_a_binary_that_is_gone_is_drift() {
        // What a `cargo clean` leaves behind after units were installed from
        // a build tree.
        let dir = tmp("units-missing");
        std::fs::write(
            dir.join("thirdeye@.service"),
            "[Service]\nExecStart=/no/such/thirdeye daemon --session %i\n",
        )
        .unwrap();

        let findings = diagnose(Path::new("/home/u/.local/bin/thirdeye"), &dir);
        let units = findings.iter().find(|f| f.subject == "units").unwrap();
        match &units.status {
            Status::Drift { detail, .. } => assert!(detail.contains("does not exist"), "{detail}"),
            other => panic!("expected drift, got {other:?}"),
        }
    }

    // The stand-in binary is a shell script.
    #[cfg(unix)]
    #[test]
    fn a_unit_running_a_stale_binary_is_drift_even_at_the_right_path() {
        // The milestone 7 failure exactly: the path was right, the binary was
        // eighteen minutes old, and nothing noticed. A path comparison passes
        // here; only comparing what the binary reports catches it.
        let _exec = exec_guard();
        let dir = tmp("units-stale");
        let fake = dir.join("thirdeye");
        std::fs::write(&fake, "#!/bin/sh\necho 'thirdeye 0.1.0 (deadbeef)'\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(
            dir.join("thirdeye@.service"),
            format!(
                "[Service]\nExecStart={} daemon --session %i\n",
                fake.display()
            ),
        )
        .unwrap();

        let findings = diagnose(&fake, &dir);
        let units = findings.iter().find(|f| f.subject == "units").unwrap();
        match &units.status {
            Status::Drift { detail, fix } => {
                assert!(
                    detail.contains("deadbeef"),
                    "must name the stale build: {detail}"
                );
                assert!(detail.contains(BUILD), "and this one: {detail}");
                assert!(fix.contains("xtask install"), "{fix}");
            }
            other => panic!("expected drift, got {other:?}"),
        }
    }

    // The stand-in binary is a shell script.
    #[cfg(unix)]
    #[test]
    fn a_unit_running_this_very_build_is_ok() {
        let _exec = exec_guard();
        let dir = tmp("units-ok");
        let fake = dir.join("thirdeye");
        std::fs::write(
            &fake,
            format!("#!/bin/sh\necho 'thirdeye 0.1.0 ({BUILD})'\n"),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(
            dir.join("thirdeye@.service"),
            format!(
                "[Service]\nExecStart={} daemon --session %i\n",
                fake.display()
            ),
        )
        .unwrap();

        let findings = diagnose(&fake, &dir);
        let units = findings.iter().find(|f| f.subject == "units").unwrap();
        assert!(matches!(units.status, Status::Ok(_)), "{:?}", units.status);
    }

    #[test]
    fn what_cannot_be_checked_is_said_out_loud() {
        // A report that only listed passes would imply coverage it does not
        // have — the failure mode this whole command exists to prevent.
        let dir = tmp("unchecked");
        let findings = diagnose(Path::new("/home/u/.local/bin/thirdeye"), &dir);

        assert!(
            findings
                .iter()
                .any(|f| f.subject == "session" && matches!(f.status, Status::Unchecked(_))),
            "an outstanding restart is undetectable and must be declared"
        );
    }
}
