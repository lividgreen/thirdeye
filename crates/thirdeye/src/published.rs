//! `published.toml`: which build last wrote thirdeye's directory, from which
//! binary, and what put that binary there. It describes the directory it
//! sits in, so it lives there.
//!
//! **It names no project, and nothing else does.** An update rewrites this
//! directory, which reaches every project by itself; `doctor` and
//! `uninstall` are told which project to look at. A list of projects would
//! only go stale — moved, deleted, a scratch project a test forgot.
//!
//! The build is what a hook compares: a binary updated by a tool that runs
//! nothing of ours afterwards — winget, `cargo install` — notices on its
//! first hook that the directory is not its own. `source` says which tool
//! owns the binary, so `self update` knows whether replacing it is
//! thirdeye's business at all.

use std::path::{Path, PathBuf};

use anyhow::Result;

pub const FILE: &str = "published.toml";

/// What put the binary where it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `install.sh` or `install.ps1`, which `self update` runs again.
    Script,
    Cargo,
    Winget,
    /// `cargo xtask install`, from a checkout.
    Xtask,
    Unknown,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Script => "script",
            Source::Cargo => "cargo",
            Source::Winget => "winget",
            Source::Xtask => "xtask",
            Source::Unknown => "unknown",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "script" => Source::Script,
            "cargo" => Source::Cargo,
            "winget" => Source::Winget,
            "xtask" => Source::Xtask,
            "unknown" => Source::Unknown,
            _ => return None,
        })
    }

    /// A guess from where the binary is, for a binary nothing told: `cargo
    /// install` and winget each put it somewhere recognizable.
    pub fn infer(binary: &Path) -> Self {
        let text = binary.display().to_string().replace('\\', "/");
        if text.contains("/.cargo/bin/") {
            Source::Cargo
        } else if text.contains("/WinGet/") || text.contains("/Microsoft/WinGet") {
            Source::Winget
        } else {
            Source::Unknown
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    /// The build whose files the directory holds, as `this_build` spells
    /// it; empty while no publish has fully succeeded.
    pub build: String,
    pub binary: PathBuf,
    pub source: Source,
}

impl Published {
    /// What `data` says about itself, if it says anything readable.
    pub fn load(data: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(data.join(FILE)).ok()?;
        let doc: toml_edit::DocumentMut = text.parse().ok()?;
        let string = |key: &str| doc.get(key).and_then(|v| v.as_str()).unwrap_or_default();
        Some(Self {
            build: string("build").to_string(),
            binary: PathBuf::from(string("binary")),
            source: Source::parse(string("source")).unwrap_or(Source::Unknown),
        })
    }

    /// Whether the directory holds this build's files.
    pub fn is_this_build(&self) -> bool {
        self.build == this_build()
    }

    fn render(&self) -> String {
        let quote = |s: &str| toml_edit::Value::from(s).to_string();
        format!(
            "# Who last published this directory. thirdeye keeps no list of projects.\n\
             build = {}\nbinary = {}\nsource = {}\n",
            quote(&self.build),
            quote(&self.binary.display().to_string()),
            quote(self.source.as_str()),
        )
    }
}

/// Record that `binary`, this build, published `data` — `complete` when
/// every part of it went. An incomplete publish keeps the old build, since
/// a stamp that vouched for a failed publish would stop anything from
/// retrying it; the binary and source are recorded either way.
pub fn record(
    data: &Path,
    binary: &Path,
    installed_by: Option<Source>,
    complete: bool,
) -> Result<()> {
    let before = Published::load(data);
    let moved = before.as_ref().is_none_or(|b| b.binary != binary);
    let source = match (installed_by, &before) {
        (Some(source), _) => source,
        // A binary somewhere else is not the one the recorded tool put there.
        (None, Some(b)) if !moved && b.source != Source::Unknown => b.source,
        (None, _) => Source::infer(binary),
    };
    let build = if complete {
        this_build()
    } else {
        before.map(|b| b.build).unwrap_or_default()
    };
    let now = Published {
        build,
        binary: binary.to_path_buf(),
        source,
    };
    crate::integration::tree::write_atomically(&data.join(FILE), &now.render(), false)
}

/// This build, as the file records it.
pub fn this_build() -> String {
    format!("{} ({})", env!("CARGO_PKG_VERSION"), env!("THIRDEYE_BUILD"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("thirdeye-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_record_reads_back_what_was_written() {
        let dir = scratch("published");
        assert_eq!(Published::load(&dir), None);
        let binary = PathBuf::from(r#"/home/me/"odd"\bin/thirdeye"#);
        record(&dir, &binary, Some(Source::Script), true).unwrap();
        let back = Published::load(&dir).unwrap();
        assert_eq!(back.binary, binary);
        assert_eq!(back.source, Source::Script);
        assert!(back.is_this_build());
    }

    #[test]
    fn an_incomplete_publish_does_not_vouch_for_this_build() {
        let dir = scratch("published-incomplete");
        let binary = PathBuf::from("/home/me/.local/bin/thirdeye");
        record(&dir, &binary, Some(Source::Script), false).unwrap();
        let first = Published::load(&dir).unwrap();
        assert!(!first.is_this_build(), "nothing ever completed");
        assert_eq!(first.source, Source::Script, "recorded anyway");

        std::fs::write(
            dir.join(FILE),
            "build = \"0.0.1 (old)\"\nbinary = \"/home/me/.local/bin/thirdeye\"\nsource = \"script\"\n",
        )
        .unwrap();
        record(&dir, &binary, None, false).unwrap();
        let after = Published::load(&dir).unwrap();
        assert_eq!(after.build, "0.0.1 (old)", "the old build stands");
        assert_eq!(after.source, Source::Script, "same binary, same owner");
    }

    #[test]
    fn a_binary_elsewhere_is_not_the_recorded_tools() {
        let dir = scratch("published-moved");
        record(
            &dir,
            Path::new("/home/me/.local/bin/thirdeye"),
            Some(Source::Script),
            true,
        )
        .unwrap();
        record(&dir, Path::new("/home/me/.cargo/bin/thirdeye"), None, true).unwrap();
        assert_eq!(Published::load(&dir).unwrap().source, Source::Cargo);
    }

    #[test]
    fn a_package_managers_binary_is_recognized_by_where_it_is() {
        assert_eq!(
            Source::infer(Path::new("/home/me/.cargo/bin/thirdeye")),
            Source::Cargo
        );
        assert_eq!(
            Source::infer(Path::new(
                r"C:\Users\me\AppData\Local\Microsoft\WinGet\Packages\x\thirdeye.exe"
            )),
            Source::Winget
        );
        assert_eq!(
            Source::infer(Path::new("/home/me/.local/bin/thirdeye")),
            Source::Unknown
        );
    }
}
