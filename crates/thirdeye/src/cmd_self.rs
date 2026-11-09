//! `thirdeye self update | uninstall`.
//!
//! An update re-runs the official installer rather than downloading
//! anything itself: `curl` is what fetched the installer in the first
//! place, and Windows 10 ships PowerShell, so no HTTP client enters the
//! binary. The installer replaces the binary and runs the *new* binary's
//! `refresh` — on Linux this one's `current_exe` reads `… (deleted)` once
//! the file is swapped, so the refresh cannot be this binary's.
//!
//! A binary a package manager put there is that manager's to replace: one
//! that finds its file replaced behind its back is a support ticket.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, bail};

use crate::integration;
use crate::published::{Published, Source};

/// Where releases are published; `THIRDEYE_BASE_URL` overrides it, which is
/// how the installers are tested before there is a release to fetch.
const RELEASES: &str = "https://github.com/lividgreen/thirdeye/releases";

/// Where the installer script comes from: the one `THIRDEYE_BASE_URL`
/// names, else the requested release's own, else the latest. A versioned
/// install fetches that release's script, not the latest one's.
fn base_url(user_base: Option<String>, version: Option<&str>) -> String {
    match (user_base.filter(|v| !v.is_empty()), version) {
        (Some(base), _) => base.trim_end_matches('/').to_string(),
        (None, Some(version)) => format!("{RELEASES}/download/v{version}"),
        (None, None) => format!("{RELEASES}/latest/download"),
    }
}

/// The source recorded for this binary, or a guess for one nothing told.
fn source(data: &Path, binary: &Path) -> Source {
    match Published::load(data) {
        Some(p) if p.source != Source::Unknown && p.binary == binary => p.source,
        _ => Source::infer(binary),
    }
}

/// What owns the update of a binary thirdeye did not install, if anything.
fn owner(source: Source) -> Option<&'static str> {
    match source {
        Source::Script => None,
        Source::Cargo => Some("cargo install thirdeye --force"),
        Source::Winget => Some("winget upgrade thirdeye"),
        Source::Xtask => Some("cargo xtask install, from your checkout"),
        Source::Unknown => Some(
            "the tool that installed it; if that was install.sh or install.ps1, \
             `thirdeye refresh --installed-by script` once lets `self update` take over",
        ),
    }
}

pub fn update(version: Option<String>) -> Result<()> {
    let binary = crate::cmd_install::binary()?;
    let data = crate::cmd_install::data_dir()?;
    let source = source(&data, &binary);
    if let Some(owner) = owner(source) {
        bail!(
            "{} was installed by {}, so it updates with: {owner}",
            binary.display(),
            source.as_str()
        );
    }
    let dir = binary
        .parent()
        .context("the binary has no directory")?
        .to_path_buf();
    let version = version.map(|v| v.trim_start_matches('v').to_string());
    let base = base_url(std::env::var("THIRDEYE_BASE_URL").ok(), version.as_deref());
    // `THIRDEYE_BASE_URL`, if set, reaches the installer by inheritance;
    // it is not set for it, since the installer prefers it to the version.
    let mut command = installer_command(&base);
    // Into this binary's own directory: an update that put the new one
    // elsewhere would leave every hook running this one.
    command.env("THIRDEYE_INSTALL_DIR", &dir);
    if let Some(version) = &version {
        command.env("THIRDEYE_VERSION", version);
    }
    let status = command
        .status()
        .context("running the installer — is curl (or PowerShell) there?")?;
    if !status.success() {
        bail!(
            "the installer failed ({status}); {} is unchanged",
            binary.display()
        );
    }
    Ok(())
}

#[cfg(not(windows))]
fn installer_command(base: &str) -> Command {
    let mut command = Command::new("sh");
    // Downloaded first, not piped: in `curl | sh` the status is the second
    // sh's, which reads nothing from a failed download and succeeds.
    command
        .arg("-c")
        .arg(
            r#"t=$(mktemp) || exit 1
curl -fsSL "$1/install.sh" -o "$t" || { rm -f "$t"; exit 1; }
sh "$t"; s=$?; rm -f "$t"; exit $s"#,
        )
        .arg("sh")
        .arg(base);
    command
}

#[cfg(windows)]
fn installer_command(base: &str) -> Command {
    let mut command = Command::new("powershell");
    command
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command"])
        // `Stop`, so a failed download is a failed update.
        .arg(format!(
            "$ErrorActionPreference = 'Stop'; iex (irm '{base}/install.ps1')"
        ));
    command
}

pub fn uninstall(purge: bool) -> Result<()> {
    let binary = crate::cmd_install::binary()?;
    let data = crate::cmd_install::data_dir()?;
    let source = source(&data, &binary);

    // Nothing records which projects are wired, so they are the human's to
    // unwire — said first, before the output scrolls with everything else.
    eprintln!(
        "warning: projects wired with `thirdeye install`, and agents wired in a \
         configuration directory you named (`-a claude:<dir>`), are not recorded, \
         so this cannot unwire them. Their agents load thirdeye from {} and run \
         {}, both removed here: they will report a missing plugin, hook or MCP \
         server. Run `thirdeye uninstall <path>` in each such project, and \
         `thirdeye uninstall --user -a <agent>:<dir>` for each such directory, \
         before this or after it.",
        data.display(),
        binary.display()
    );

    // The user's own wiring first, while the files it points at still
    // exist — and only onwards if all of it went: wiring left behind
    // pointing at a directory this then deleted would break the agent's
    // sessions with nothing saying why.
    crate::cmd_install::uninstall(None, true, &[])
        .context("some wiring could not be removed; fix that and run this again")?;

    // From here on every step is attempted, and failures reported at the end.
    let mut failed: Vec<String> = Vec::new();
    let _ = crate::cmd_install::with_context(|cx| {
        for agent in integration::registry() {
            if cx.data_for(agent.id()).is_dir()
                && let Err(e) = agent.retire(cx)
            {
                failed.push(format!("{}: {e:#}", agent.id()));
            }
        }
        Ok(())
    });
    let units = crate::cmd_units::unit_dir();
    if !cfg!(windows) && units.join("thirdeye@.service").exists() {
        match crate::cmd_units::uninstall(&units, &crate::cmd_units::UserSystemctl) {
            Ok(()) => println!("units: removed from {}", units.display()),
            Err(e) => failed.push(format!("units: {e:#}")),
        }
    }
    match remove_data(&data) {
        Ok(true) => println!("removed {}", data.display()),
        Ok(false) => println!(
            "removed thirdeye's files from {}; it holds others, so it stays",
            data.display()
        ),
        Err(e) => failed.push(format!("{}: {e:#}", data.display())),
    }
    if purge {
        match thirdeye_core::location::state_root() {
            Some(state) if named_thirdeye(&state) && state.exists() => {
                match std::fs::remove_dir_all(&state) {
                    Ok(()) => println!("removed the logs in {}", state.display()),
                    Err(e) => failed.push(format!("{}: {e}", state.display())),
                }
            }
            Some(state) if state.exists() => println!(
                "left the logs in {}: it is not named for thirdeye, so remove it yourself",
                state.display()
            ),
            _ => {}
        }
    }
    match owner(source) {
        // A script install is ours to remove.
        None => {
            if let Err(e) = remove_binary(&binary) {
                failed.push(format!("{e:#}"));
            }
        }
        Some(_) if source == Source::Xtask || source == Source::Unknown => println!(
            "the binary {} is left in place; delete it if you want it gone",
            binary.display()
        ),
        Some(_) => println!(
            "the binary belongs to {}: {}",
            source.as_str(),
            match source {
                Source::Cargo => "cargo uninstall thirdeye",
                _ => "winget uninstall thirdeye",
            }
        ),
    }
    if !failed.is_empty() {
        bail!("not everything was removed:\n  {}", failed.join("\n  "));
    }
    Ok(())
}

/// Remove what thirdeye wrote in its directory — every published tree and
/// `published.toml` — and the directory itself if that empties it. Only
/// files thirdeye recorded writing: a `$THIRDEYE_DATA_DIR` pointed at a
/// directory of the user's must cost them nothing.
fn remove_data(data: &Path) -> Result<bool> {
    fn trees(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                if path.join(integration::tree::MANIFEST).is_file() {
                    out.push(path.clone());
                }
                trees(&path, out);
            }
        }
    }
    let mut found = Vec::new();
    trees(data, &mut found);
    // Deepest first, so a tree's parent is tried once it is empty.
    found.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for root in &found {
        integration::tree::remove(root)?;
        for dir in root.ancestors().skip(1).take_while(|d| *d != data) {
            if std::fs::remove_dir(dir).is_err() {
                break;
            }
        }
    }
    let _ = std::fs::remove_file(data.join(crate::published::FILE));
    Ok(std::fs::remove_dir(data).is_ok() || !data.exists())
}

fn named_thirdeye(dir: &Path) -> bool {
    dir.file_name()
        .is_some_and(|n| n.to_string_lossy().contains("thirdeye"))
}

/// Remove the running binary. Unix unlinks it in place; Windows cannot
/// delete a running executable but can rename one, so it is moved aside for
/// the human to delete.
fn remove_binary(binary: &Path) -> Result<()> {
    if cfg!(windows) {
        let aside = aside(binary);
        std::fs::rename(binary, &aside)
            .with_context(|| format!("moving {} aside", binary.display()))?;
        println!(
            "removed {}; delete {} once this has exited",
            binary.display(),
            aside.display()
        );
    } else {
        std::fs::remove_file(binary).with_context(|| format!("removing {}", binary.display()))?;
        println!("removed {}", binary.display());
    }
    Ok(())
}

fn aside(binary: &Path) -> PathBuf {
    let mut name = binary.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    binary.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_package_managers_binary_is_never_replaced_behind_its_back() {
        for source in [
            Source::Cargo,
            Source::Winget,
            Source::Xtask,
            Source::Unknown,
        ] {
            assert!(owner(source).is_some(), "{source:?}");
        }
        assert_eq!(owner(Source::Script), None);
        assert!(owner(Source::Cargo).unwrap().contains("cargo install"));
    }

    #[test]
    fn a_record_naming_another_binary_says_nothing_about_this_one() {
        let dir = std::env::temp_dir().join(format!("thirdeye-self-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        crate::published::record(
            &dir,
            Path::new("/home/me/.local/bin/thirdeye"),
            Some(Source::Script),
            true,
        )
        .unwrap();
        assert_eq!(
            source(&dir, Path::new("/home/me/.local/bin/thirdeye")),
            Source::Script
        );
        assert_eq!(
            source(&dir, Path::new("/home/me/.cargo/bin/thirdeye")),
            Source::Cargo
        );
    }

    #[test]
    fn only_what_thirdeye_wrote_is_removed_from_its_directory() {
        // Found by review: a data directory pointed at the user's own lost
        // everything in it, because a record of ours alone made it "ours".
        use crate::integration::tree::{File, publish};
        let dir = std::env::temp_dir().join(format!("thirdeye-self-ours-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.txt"), "mine").unwrap();
        publish(&dir.join("copilot"), &[File::new("a/b.json", "1")]).unwrap();
        publish(&dir.join("grok/thirdeye"), &[File::new("plugin.json", "1")]).unwrap();
        crate::published::record(&dir, Path::new("/bin/thirdeye"), None, true).unwrap();
        assert!(!remove_data(&dir).unwrap(), "the user's file keeps it");
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(left, ["notes.txt"]);

        std::fs::remove_file(dir.join("notes.txt")).unwrap();
        publish(&dir.join("pi"), &[File::new("plugin.mjs", "1")]).unwrap();
        assert!(remove_data(&dir).unwrap());
        assert!(!dir.exists());
        assert!(named_thirdeye(Path::new("/h/.local/state/thirdeye")));
        assert!(!named_thirdeye(Path::new("/home/me")));
    }

    #[test]
    fn a_versioned_update_fetches_that_releases_installer() {
        assert!(base_url(None, Some("0.2.0")).ends_with("/download/v0.2.0"));
        assert!(base_url(None, None).ends_with("/latest/download"));
        assert_eq!(
            base_url(Some("http://h:1/".into()), Some("0.2.0")),
            "http://h:1"
        );
        assert!(base_url(Some(String::new()), None).ends_with("/latest/download"));
    }
}
