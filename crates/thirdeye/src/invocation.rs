//! How an installed agent config names thirdeye — shared by every installer
//! that writes one (`thirdeye install -a codex`, `-a opencode`, …).

use std::path::{Path, PathBuf};

/// How the installed config should name thirdeye.
///
/// A project's `.codex/` is usually committed, and an absolute path there
/// names one machine's home directory. So a project install writes a bare
/// `thirdeye` — but only when `PATH` already resolves to this very binary;
/// otherwise the bare name would run a different thirdeye than the one
/// installed. A user install is private to the machine and always exact.
pub fn invocation(binary: &Path, on_path: Option<&Path>, project: bool) -> PathBuf {
    if project && on_path == Some(binary) {
        PathBuf::from("thirdeye")
    } else {
        binary.to_path_buf()
    }
}

/// The `thirdeye` that `PATH` resolves to, canonicalized so a symlinked
/// install directory compares equal to the binary's own path.
pub fn thirdeye_on_path() -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join("thirdeye"))
        .find(|candidate| candidate.is_file())
        .and_then(|found| found.canonicalize().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_install_names_thirdeye_portably_when_path_finds_this_binary() {
        // <project>/.codex/ is usually committed. An absolute path there names
        // one machine's home directory and breaks for everyone else.
        let me = Path::new("/home/u/.local/bin/thirdeye");
        assert_eq!(invocation(me, Some(me), true), PathBuf::from("thirdeye"));
    }

    #[test]
    fn a_project_install_keeps_the_absolute_path_when_path_finds_another() {
        // A bare name would then run a different thirdeye than was installed.
        let me = Path::new("/opt/dev/thirdeye");
        let other = Path::new("/usr/bin/thirdeye");
        assert_eq!(invocation(me, Some(other), true), me);
        assert_eq!(invocation(me, None, true), me);
    }

    #[test]
    fn a_user_install_always_names_the_binary_exactly() {
        // $CODEX_HOME is private to this machine; exact beats portable there.
        let me = Path::new("/home/u/.local/bin/thirdeye");
        assert_eq!(invocation(me, Some(me), false), me);
    }
}
