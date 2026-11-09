//! A per-build copy of this binary, for the processes that live as long as a
//! session: the daemon and the MCP server.
//!
//! **Windows will not let a package manager replace a running exe.** A Claude
//! session holds the daemon and the MCP server for its whole life, and winget
//! deletes rather than renames: the upgrade fails half-done and still records
//! the new version (`docs/design/plans/2026-11-10-shadow-copy.md`). So those
//! two run from `<data>/bin/<build>/thirdeye.exe`, and the package manager's
//! file is held for the ~50 ms of a hook only.
//!
//! Elsewhere an exe in use can be unlinked and replaced, so nothing here
//! applies and [`run_path`] returns the binary it was given.

use std::path::{Path, PathBuf};

const EXE: &str = if cfg!(windows) {
    "thirdeye.exe"
} else {
    "thirdeye"
};

/// A build as a directory name: `this_build` has spaces and parentheses.
fn dir_name(build: &str) -> String {
    build
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn root(data: &Path) -> PathBuf {
    data.join("bin")
}

/// Where `build`'s copy lives.
pub fn path(data: &Path, build: &str) -> PathBuf {
    root(data).join(dir_name(build)).join(EXE)
}

/// The copy of `binary` for `build`, made if absent.
///
/// Copied under a name of its own and renamed into place, so a daemon
/// started by a concurrent hook never executes half a file; losing that race
/// is fine, the winner's copy is the same bytes.
pub fn ensure(data: &Path, binary: &Path, build: &str) -> std::io::Result<PathBuf> {
    let target = path(data, build);
    if target.is_file() {
        return Ok(target);
    }
    let dir = target.parent().expect("a copy has a directory");
    std::fs::create_dir_all(dir)?;
    let partial = dir.join(format!("{EXE}.{}.part", std::process::id()));
    std::fs::copy(binary, &partial)?;
    match std::fs::rename(&partial, &target) {
        Ok(()) => Ok(target),
        Err(_) if target.is_file() => {
            let _ = std::fs::remove_file(&partial);
            Ok(target)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&partial);
            Err(e)
        }
    }
}

/// Delete every build's copy but `keep`'s. A copy something still runs
/// cannot be deleted, and stays for the next prune; none of this is an error.
pub fn prune(data: &Path, keep: &str) {
    let Ok(entries) = std::fs::read_dir(root(data)) else {
        return;
    };
    let keep = dir_name(keep);
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy() != keep {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// What a long-lived process should be started from: this build's copy of
/// `binary` on Windows, `binary` itself anywhere else — or when the copy
/// cannot be made, since a daemon from the package manager's file still
/// works, and only the upgrade suffers.
pub fn run_path(data: &Path, binary: &Path) -> PathBuf {
    if !cfg!(windows) {
        return binary.to_path_buf();
    }
    let build = crate::published::this_build();
    match ensure(data, binary, &build) {
        Ok(copy) => {
            prune(data, &build);
            copy
        }
        Err(_) => binary.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("thirdeye-shadow-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_build_names_a_directory_with_no_spaces_or_parentheses() {
        let p = path(Path::new("/d"), "0.1.0 (abc 123)");
        assert!(p.ends_with(Path::new("0.1.0__abc_123_").join(EXE)), "{p:?}");
    }

    #[test]
    fn the_copy_is_the_binary_and_survives_its_replacement() {
        let dir = scratch("copy");
        let binary = dir.join("package-thirdeye");
        std::fs::write(&binary, b"v1").unwrap();
        let copy = ensure(&dir, &binary, "1 (a)").unwrap();
        assert_eq!(std::fs::read(&copy).unwrap(), b"v1");

        // The package manager replaces its file; the running copy is untouched
        // and a second call for the same build copies nothing.
        std::fs::write(&binary, b"v2").unwrap();
        assert_eq!(ensure(&dir, &binary, "1 (a)").unwrap(), copy);
        assert_eq!(std::fs::read(&copy).unwrap(), b"v1");
        let strays = std::fs::read_dir(copy.parent().unwrap()).unwrap().count();
        assert_eq!(strays, 1, "no partial file is left beside the copy");
    }

    #[test]
    fn prune_removes_other_builds_and_keeps_this_one() {
        let dir = scratch("prune");
        let binary = dir.join("t");
        std::fs::write(&binary, b"x").unwrap();
        let old = ensure(&dir, &binary, "1 (old)").unwrap();
        let new = ensure(&dir, &binary, "2 (new)").unwrap();
        prune(&dir, "2 (new)");
        assert!(!old.exists());
        assert!(new.is_file());
    }

    #[test]
    fn a_missing_binary_is_an_error_not_a_panic() {
        let dir = scratch("missing");
        assert!(ensure(&dir, &dir.join("nope"), "1 (a)").is_err());
        assert!(!path(&dir, "1 (a)").exists());
    }

    #[cfg(not(windows))]
    #[test]
    fn elsewhere_the_binary_runs_where_it_is() {
        let dir = scratch("elsewhere");
        let binary = dir.join("t");
        assert_eq!(run_path(&dir, &binary), binary);
        assert!(!root(&dir).exists());
    }

    #[cfg(windows)]
    #[test]
    fn on_windows_the_long_lived_process_runs_from_the_copy() {
        let dir = scratch("windows");
        let binary = dir.join("t.exe");
        std::fs::write(&binary, b"x").unwrap();
        let run = run_path(&dir, &binary);
        assert_ne!(run, binary);
        assert!(run.starts_with(root(&dir)) && run.is_file());
    }
}
