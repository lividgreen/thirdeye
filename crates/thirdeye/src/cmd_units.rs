//! `thirdeye units` — installing the systemd user units, rootless.
//!
//! Rootless is not a nicety here, it is the whole deployment model: user units
//! under `$XDG_CONFIG_HOME/systemd/user`, never `/etc/systemd/system`, never
//! `systemctl` without `--user`. Nothing thirdeye does should require a
//! password, and anything that did would be a reason not to install it.
//!
//! The units are embedded at build time rather than read from the checkout,
//! because the binary is installed separately from the plugin and may be run
//! from anywhere — an installer that needed the source tree beside it would
//! work only for the person who built it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The unit templates, verbatim from `units/`.
///
/// At the repository root, not inside an agent plugin: systemd units are how
/// *thirdeye* is deployed and have nothing to do with any agent. They used to
/// live under the Claude plugin and were duly copied into its cache, where
/// nothing ever read them.
const SOCKET_UNIT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/units/thirdeye@.socket"
));
const SERVICE_UNIT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/units/thirdeye@.service"
));

/// The path in the checked-in template that names the binary.
///
/// Replaced at install time with wherever this binary actually is, so a
/// `--root` other than `~/.local` works without editing anything. The
/// templates stay valid to copy by hand, which is why this is a real path
/// rather than a placeholder token.
const TEMPLATE_BIN: &str = "%h/.local/bin/thirdeye";

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Written,
    AlreadyCurrent,
}

/// Runs `systemctl --user`, behind a seam so tests never touch a real one.
pub trait Systemctl {
    fn reload(&self) -> Result<()>;
    fn stop_all(&self) -> Result<()>;
}

pub struct UserSystemctl;

impl Systemctl for UserSystemctl {
    fn reload(&self) -> Result<()> {
        run(&["daemon-reload"])
    }

    fn stop_all(&self) -> Result<()> {
        // Instances are per session and named at runtime, so they are stopped
        // by glob rather than enumerated.
        run(&["stop", "thirdeye@*.socket", "thirdeye@*.service"])
    }
}

fn run(args: &[&str]) -> Result<()> {
    let status = std::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .context("running systemctl --user")?;
    // A stop that matched nothing is a success for our purposes.
    if !status.success() && args.first() == Some(&"daemon-reload") {
        anyhow::bail!("systemctl --user daemon-reload failed");
    }
    Ok(())
}

/// Where user units live.
pub fn unit_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("systemd/user")
}

pub fn install(
    dir: &Path,
    binary: &Path,
    systemctl: &impl Systemctl,
) -> Result<Vec<(String, Outcome)>> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

    let mut outcomes = Vec::new();
    for (name, body) in units(binary) {
        let path = dir.join(&name);
        let outcome = if std::fs::read_to_string(&path).ok().as_deref() == Some(body.as_str()) {
            // Reporting "already current" rather than rewriting keeps the
            // mtime meaningful and makes a re-run obviously a no-op.
            Outcome::AlreadyCurrent
        } else {
            std::fs::write(&path, &body).with_context(|| format!("writing {}", path.display()))?;
            Outcome::Written
        };
        outcomes.push((name, outcome));
    }

    // systemd will not see a new or changed unit until it is told to look.
    systemctl.reload()?;
    Ok(outcomes)
}

pub fn uninstall(dir: &Path, systemctl: &impl Systemctl) -> Result<()> {
    // Stop first: removing a unit file out from under a running instance
    // leaves systemd holding a unit it can no longer describe.
    systemctl.stop_all()?;
    for (name, _) in units(Path::new(TEMPLATE_BIN)) {
        let path = dir.join(&name);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing {}", path.display())),
        }
    }
    systemctl.reload()
}

/// The units as they should appear on disk, with the binary path resolved.
fn units(binary: &Path) -> Vec<(String, String)> {
    let bin = binary.display().to_string();
    vec![
        ("thirdeye@.socket".to_string(), SOCKET_UNIT.to_string()),
        (
            "thirdeye@.service".to_string(),
            SERVICE_UNIT.replace(TEMPLATE_BIN, &bin),
        ),
    ]
}

/// This binary's own path, for the `ExecStart` the units will carry.
/// Whether a systemd *user* manager is actually usable here.
///
/// `systemctl` existing is not enough: it is present in containers and on
/// machines where the user manager never started, and units installed there
/// would be files nothing ever reads.
pub fn systemd_user_manager() -> bool {
    std::process::Command::new("systemctl")
        .args(["--user", "show", "--property=Version"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

pub fn own_path() -> Result<PathBuf> {
    std::env::current_exe().context("locating this binary")
}

/// Whether `binary` is somewhere a unit file should not point.
///
/// A unit written from a `cargo run` build points into `target/`, which the
/// next `cargo clean` deletes — leaving units that fail to start with an error
/// naming a path the user never typed.
pub fn is_build_artifact(binary: &Path) -> bool {
    binary
        .components()
        .any(|c| c.as_os_str() == "target" || c.as_os_str() == "deps")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Recorder {
        calls: RefCell<Vec<&'static str>>,
    }

    impl Systemctl for Recorder {
        fn reload(&self) -> Result<()> {
            self.calls.borrow_mut().push("reload");
            Ok(())
        }
        fn stop_all(&self) -> Result<()> {
            self.calls.borrow_mut().push("stop");
            Ok(())
        }
    }

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("thirdeye-units-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_template_still_contains_the_path_the_installer_rewrites() {
        // If someone edits the unit file and changes how the binary is named,
        // the installer would silently write an untemplated ExecStart. This
        // is the check that makes that edit fail loudly instead.
        assert!(
            SERVICE_UNIT.contains(TEMPLATE_BIN),
            "the service template no longer contains {TEMPLATE_BIN}; the installer would \
             write a unit pointing at the wrong binary"
        );
    }

    #[test]
    fn install_writes_both_units_and_reloads() {
        let dir = tmp("install");
        let recorder = Recorder::default();
        let outcomes = install(&dir, Path::new("/opt/bin/thirdeye"), &recorder).unwrap();

        assert_eq!(outcomes.len(), 2);
        assert!(dir.join("thirdeye@.socket").exists());
        assert!(dir.join("thirdeye@.service").exists());
        assert_eq!(*recorder.calls.borrow(), vec!["reload"]);
    }

    #[test]
    fn the_installed_service_points_at_this_binary_not_the_template_default() {
        // `cargo install --root` elsewhere must work without hand-editing.
        let dir = tmp("path");
        install(&dir, Path::new("/opt/bin/thirdeye"), &Recorder::default()).unwrap();

        let service = std::fs::read_to_string(dir.join("thirdeye@.service")).unwrap();
        assert!(
            service.contains("/opt/bin/thirdeye daemon --session %i"),
            "{service}"
        );
        assert!(
            !service.contains(TEMPLATE_BIN),
            "the default path must be gone"
        );
    }

    #[test]
    fn installing_twice_reports_the_second_as_a_no_op() {
        let dir = tmp("idempotent");
        let bin = Path::new("/opt/bin/thirdeye");
        install(&dir, bin, &Recorder::default()).unwrap();
        let again = install(&dir, bin, &Recorder::default()).unwrap();
        assert!(
            again.iter().all(|(_, o)| *o == Outcome::AlreadyCurrent),
            "got {again:?}"
        );
    }

    #[test]
    fn a_changed_binary_path_rewrites_the_unit() {
        let dir = tmp("moved");
        install(&dir, Path::new("/opt/bin/thirdeye"), &Recorder::default()).unwrap();
        let moved = install(
            &dir,
            Path::new("/usr/local/bin/thirdeye"),
            &Recorder::default(),
        )
        .unwrap();

        assert!(
            moved
                .iter()
                .any(|(name, o)| name.ends_with(".service") && *o == Outcome::Written)
        );
        let service = std::fs::read_to_string(dir.join("thirdeye@.service")).unwrap();
        assert!(service.contains("/usr/local/bin/thirdeye"), "{service}");
    }

    #[test]
    fn uninstall_stops_before_it_removes() {
        // Removing a unit file out from under a running instance leaves
        // systemd holding one it can no longer describe.
        let dir = tmp("uninstall");
        install(&dir, Path::new("/opt/bin/thirdeye"), &Recorder::default()).unwrap();

        let recorder = Recorder::default();
        uninstall(&dir, &recorder).unwrap();
        assert_eq!(*recorder.calls.borrow(), vec!["stop", "reload"]);
        assert!(!dir.join("thirdeye@.socket").exists());
        assert!(!dir.join("thirdeye@.service").exists());
    }

    #[test]
    fn uninstalling_what_was_never_installed_is_not_an_error() {
        let dir = tmp("absent");
        std::fs::create_dir_all(&dir).unwrap();
        uninstall(&dir, &Recorder::default()).unwrap();
    }

    #[test]
    fn a_binary_inside_the_build_tree_is_recognized() {
        // Units written from `cargo run` point into target/, which the next
        // `cargo clean` deletes.
        assert!(is_build_artifact(Path::new(
            "/home/u/app/target/debug/thirdeye"
        )));
        assert!(is_build_artifact(Path::new(
            "/home/u/app/target/debug/deps/thirdeye-abc123"
        )));
        assert!(!is_build_artifact(Path::new("/home/u/.local/bin/thirdeye")));
    }

    #[test]
    fn the_units_never_reach_outside_the_user() {
        // The deployment model in one assertion: no system paths, no sudo.
        for (_, body) in units(Path::new("/home/u/.local/bin/thirdeye")) {
            assert!(!body.contains("/etc/systemd"), "{body}");
            assert!(!body.contains("sudo"), "{body}");
            assert!(!body.contains("User="), "a user unit must not set User=");
        }
    }
}
