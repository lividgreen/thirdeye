//! Writing a directory of thirdeye's own: what every `publish` does.
//!
//! An agent may be reading the directory while it is rewritten — Claude Code
//! and Copilot load their plugin from it live — so each file is replaced by a
//! rename, never truncated and refilled. And a file an older version wrote
//! that this one does not is removed, or it would be loaded beside the new
//! ones; but only a file the previous publish *recorded* writing, so a
//! directory variable pointed somewhere wrong costs nothing that was there.

use std::path::Path;

use anyhow::{Context, Result};

/// The list of files the last publish wrote, kept inside the directory.
pub const MANIFEST: &str = ".thirdeye-files";

/// One file of a published tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// Relative to the tree's root, `/`-separated.
    pub path: String,
    pub body: String,
    pub executable: bool,
}

impl File {
    pub fn new(path: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            body: body.into(),
            executable: false,
        }
    }

    pub fn executable(mut self) -> Self {
        self.executable = true;
        self
    }
}

/// Make `root` hold exactly `files`, as far as thirdeye's own files go.
pub fn publish(root: &Path, files: &[File]) -> Result<()> {
    std::fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;
    for file in files {
        write_atomically(&root.join(&file.path), &file.body, file.executable)?;
    }
    let previous = std::fs::read_to_string(root.join(MANIFEST)).unwrap_or_default();
    for stale in previous
        .lines()
        .filter(|old| !old.is_empty() && !files.iter().any(|f| f.path == *old))
    {
        let path = root.join(stale);
        let _ = std::fs::remove_file(&path);
        // Directories the stale file leaves empty; `remove_dir` refuses one
        // that still holds anything.
        for dir in path.ancestors().skip(1) {
            if dir == root || std::fs::remove_dir(dir).is_err() {
                break;
            }
        }
    }
    let listing: String = files.iter().map(|f| format!("{}\n", f.path)).collect();
    write_atomically(&root.join(MANIFEST), &listing, false)
}

/// Whether `root` holds every one of `files`, byte for byte.
pub fn matches(root: &Path, files: &[File]) -> bool {
    files
        .iter()
        .all(|f| std::fs::read_to_string(root.join(&f.path)).is_ok_and(|have| have == f.body))
}

/// Remove a tree [`publish`] wrote: its recorded files, then whatever
/// directories they leave empty. Anything else in it stays.
pub fn remove(root: &Path) -> Result<()> {
    let Ok(listing) = std::fs::read_to_string(root.join(MANIFEST)) else {
        return Ok(());
    };
    let mut dirs = Vec::new();
    for rel in listing.lines().filter(|l| !l.is_empty()) {
        let path = root.join(rel);
        let _ = std::fs::remove_file(&path);
        dirs.extend(
            path.ancestors()
                .skip(1)
                .take_while(|d| *d != root)
                .map(Path::to_path_buf),
        );
    }
    std::fs::remove_file(root.join(MANIFEST))
        .with_context(|| format!("removing {}", root.join(MANIFEST).display()))?;
    // Deepest first, so a parent is empty by the time it is tried.
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    dirs.dedup();
    for dir in dirs {
        let _ = std::fs::remove_dir(dir);
    }
    let _ = std::fs::remove_dir(root);
    Ok(())
}

/// Write `body` beside `path` and rename it into place, so a reader sees
/// the old file or the new one and never half of either.
pub fn write_atomically(path: &Path, body: &str, executable: bool) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".tmp-{}", std::process::id()));
    let temp = path.with_file_name(name);
    std::fs::write(&temp, body).with_context(|| format!("writing {}", temp.display()))?;
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(not(unix))]
    let _ = executable;
    std::fs::rename(&temp, path).with_context(|| format!("replacing {}", path.display()))
}

/// FNV-1a over every path and body, for a version string that changes
/// exactly when the files do.
pub fn content_hash(files: &[File]) -> u64 {
    let mut sorted: Vec<&File> = files.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut bytes = Vec::new();
    for f in sorted {
        bytes.extend(f.path.as_bytes());
        bytes.push(0);
        bytes.extend(f.body.as_bytes());
        bytes.push(0);
    }
    thirdeye_core::location::fnv1a64(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("thirdeye-tree-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_file_an_older_version_wrote_is_removed_on_the_next_publish() {
        // Loaded beside the new files, it would keep an old hook running.
        let root = scratch("stale");
        publish(
            &root,
            &[File::new("a.json", "1"), File::new("old/hooks.json", "x")],
        )
        .unwrap();
        publish(&root, &[File::new("a.json", "2")]).unwrap();
        assert_eq!(std::fs::read_to_string(root.join("a.json")).unwrap(), "2");
        assert!(!root.join("old").exists(), "stale file and its directory");
    }

    #[test]
    fn a_file_no_publish_wrote_is_never_removed() {
        // A data directory variable pointed at the wrong place must cost
        // nothing that was already there.
        let root = scratch("theirs");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("notes.txt"), "mine").unwrap();
        publish(&root, &[File::new("a.json", "1")]).unwrap();
        publish(&root, &[File::new("b.json", "1")]).unwrap();
        remove(&root).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("notes.txt")).unwrap(),
            "mine"
        );
    }

    #[test]
    fn remove_leaves_nothing_where_there_was_nothing() {
        let root = scratch("remove");
        publish(&root, &[File::new("x/y/z.md", "1")]).unwrap();
        remove(&root).unwrap();
        assert!(!root.exists());
    }

    #[test]
    fn matches_notices_any_changed_byte() {
        let root = scratch("matches");
        let files = [File::new("a", "one")];
        publish(&root, &files).unwrap();
        assert!(matches(&root, &files));
        assert!(!matches(&root, &[File::new("a", "two")]));
    }

    #[test]
    fn the_hash_follows_content_and_names_but_not_order() {
        let a = [File::new("a", "1"), File::new("b", "2")];
        let b = [File::new("b", "2"), File::new("a", "1")];
        assert_eq!(content_hash(&a), content_hash(&b));
        assert_ne!(content_hash(&a), content_hash(&[File::new("a", "1")]));
        assert_ne!(
            content_hash(&[File::new("a", "12")]),
            content_hash(&[File::new("a1", "2")])
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_executable_file_is_executable() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("exec");
        publish(&root, &[File::new("run.sh", "#!/bin/sh\n").executable()]).unwrap();
        let mode = std::fs::metadata(root.join("run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);
    }
}
