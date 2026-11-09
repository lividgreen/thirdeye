//! Deciding which project a command is talking about.
//!
//! Every subcommand must answer this the same way. The workspace slug is
//! derived from the path *text* (see [`Workspace::new`]), so two spellings of
//! one directory silently become two workspaces: `thirdeye hook` would record
//! into one and `thirdeye ls` would read from another, reporting "no sessions
//! recorded" for a session that exists. This module is the single place that
//! answer is computed.

use std::path::{Path, PathBuf};

use thirdeye_core::Workspace;

/// Resolve the workspace root for any subcommand.
///
/// Precedence, most to least specific:
///
/// 1. `explicit` — a `--workspace` flag the user typed. An explicit request
///    wins over ambient state, otherwise the flag would be a no-op inside an
///    agent session.
/// 2. `$CLAUDE_PROJECT_DIR` — the agent's project root. Preferred over `cwd`
///    so an agent working in a subdirectory does not fork a second workspace.
/// 3. `cwd` — where the caller is, walked up to the enclosing repository root
///    if there is one. Running `thirdeye ls` from a subdirectory is the normal
///    case, and `docs/install.md` tells users to just run it.
///
/// The result is canonicalized, because the slug is path text: `.`,
/// `/repo/../repo` and a symlinked `/repo` must all name one workspace.
pub fn resolve_workspace_root(explicit: Option<PathBuf>, cwd: Option<&Path>) -> PathBuf {
    let project_dir = std::env::var_os("CLAUDE_PROJECT_DIR").map(PathBuf::from);
    resolve_with(explicit, project_dir, cwd)
}

/// [`resolve_workspace_root`] with the environment passed in, so the
/// precedence rules can be tested without mutating process-global state (and
/// so tests cannot be perturbed by an ambient `$CLAUDE_PROJECT_DIR`).
pub fn resolve_with(
    explicit: Option<PathBuf>,
    project_dir: Option<PathBuf>,
    cwd: Option<&Path>,
) -> PathBuf {
    let chosen = match (explicit, project_dir) {
        (Some(explicit), _) => explicit,
        (None, Some(project_dir)) => project_dir,
        // Only the `cwd` fallback ascends: an explicit path and
        // `$CLAUDE_PROJECT_DIR` are both already assertions about *which*
        // directory is the root, and overriding them would be surprising.
        (None, None) => match cwd {
            Some(cwd) => repo_root_of(cwd).unwrap_or_else(|| cwd.to_path_buf()),
            None => PathBuf::from("."),
        },
    };
    canonicalize_best_effort(chosen)
}

/// Nearest ancestor containing `.git`, `self` included.
///
/// `.git` is a file, not a directory, inside a worktree or submodule, so this
/// tests existence rather than directory-ness.
fn repo_root_of(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

/// Canonicalize, falling back to the path as given.
///
/// Canonicalization fails on a path that does not exist, which is routine in
/// tests and possible for a `cwd` recorded by an agent in a directory since
/// deleted. A non-canonical workspace root is worse than no root only if it
/// disagrees with another command's — and every command resolves through
/// here, so they agree either way.
fn canonicalize_best_effort(path: PathBuf) -> PathBuf {
    std::fs::canonicalize(&path).map(plain).unwrap_or(path)
}

/// Windows canonicalizes to a verbatim path, `\\?\C:\app`, which no agent
/// writes: the paths in its payloads would then lie outside a workspace they
/// are in, and a terminal or `git` handed it as a directory may refuse it.
/// The spelling everything else uses is the same path without the prefix.
#[cfg(windows)]
fn plain(path: PathBuf) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path;
    };
    if let Some(share) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{share}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\")
        && rest.as_bytes().get(1) == Some(&b':')
    {
        PathBuf::from(rest)
    } else {
        path
    }
}

#[cfg(not(windows))]
fn plain(path: PathBuf) -> PathBuf {
    path
}

/// The workspace a subcommand should operate on.
pub fn resolve(explicit: Option<PathBuf>, cwd: Option<&Path>) -> Workspace {
    Workspace::new(resolve_workspace_root(explicit, cwd))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("thirdeye-ws-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The temp dir itself may be a symlink (/tmp -> /private/tmp on
        // macOS), so compare against its canonical form throughout.
        canonicalize_best_effort(dir)
    }

    #[test]
    fn a_subdirectory_resolves_to_the_same_root_as_the_project_itself() {
        // The reported bug: `thirdeye hook` recorded under the project root
        // while `thirdeye ls`, run from `crates/`, looked under a second
        // workspace and reported no sessions.
        let repo = tmpdir("subdir");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let sub = repo.join("crates/thirdeye/src");
        std::fs::create_dir_all(&sub).unwrap();

        assert_eq!(resolve_with(None, None, Some(&repo)), repo);
        assert_eq!(resolve_with(None, None, Some(&sub)), repo);
    }

    #[cfg(windows)]
    #[test]
    fn a_root_is_spelled_the_way_an_agent_spells_it() {
        // The slug is path text and so is every prefix test: a root written
        // `\\?\C:\app` is a different workspace from the `C:\app` in the
        // agent's payloads, and contains none of the files they name.
        let dir = std::env::temp_dir().join(format!("thirdeye-ws-plain-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let root = resolve_with(Some(dir.clone()), None, None);
        assert_eq!(root, dir);
        assert!(dir.join("src").starts_with(&root));
    }

    #[test]
    fn a_relative_explicit_workspace_resolves_to_an_absolute_path() {
        // `--workspace .` used to slug as "root-<hash of ".">", a workspace
        // no producer ever writes to.
        let root = resolve_with(Some(PathBuf::from(".")), None, None);
        assert!(root.is_absolute(), "got {root:?}");
        assert_eq!(root, canonicalize_best_effort(".".into()));
    }

    #[test]
    fn two_spellings_of_one_directory_resolve_identically() {
        let repo = tmpdir("spellings");
        let sub = repo.join("crates");
        std::fs::create_dir_all(&sub).unwrap();

        let direct = resolve_with(Some(repo.clone()), None, None);
        let round_trip = resolve_with(Some(sub.join("..")), None, None);
        assert_eq!(direct, round_trip);
    }

    #[test]
    fn an_explicit_workspace_outranks_the_ambient_project_dir() {
        // Otherwise `--workspace` would be silently inert inside an agent
        // session, which is exactly where the variable is set.
        let a = tmpdir("explicit-a");
        let b = tmpdir("explicit-b");
        assert_eq!(resolve_with(Some(a.clone()), Some(b), None), a);
    }

    #[test]
    fn the_project_dir_outranks_the_working_directory() {
        // A hook fires with the agent's `cwd`, which may be a subdirectory
        // that is not itself a repository root.
        let repo = tmpdir("projdir");
        let sub = repo.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(resolve_with(None, Some(repo.clone()), Some(&sub)), repo);
    }

    #[test]
    fn a_nonexistent_root_is_used_verbatim_rather_than_discarded() {
        // Canonicalization fails on a deleted directory; losing the workspace
        // identity entirely would be worse than an uncanonical one.
        let root = resolve_with(Some(PathBuf::from("/no/such/dir")), None, None);
        assert_eq!(root, Path::new("/no/such/dir"));
    }

    #[test]
    fn a_cwd_outside_any_repository_is_used_as_is() {
        let dir = tmpdir("norepo");
        assert_eq!(resolve_with(None, None, Some(&dir)), dir);
    }
}
