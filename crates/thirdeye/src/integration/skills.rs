//! The flagging skill, for an agent that finds skills by directory.
//!
//! Codex, Antigravity, OpenCode and pi look for skills inside the project,
//! so the skill cannot simply live in thirdeye's directory. On Unix the
//! project's `skills/flagging-files` is a **symbolic link** to the copy
//! thirdeye publishes, which every one of them follows, so the skill updates
//! with the directory it points at. Windows makes a symbolic link a
//! privilege, so there it is a copy, and `doctor` reports one that an update
//! left behind.

use std::path::Path;

use anyhow::{Context as _, Result};

/// The directory a skill is placed under, inside a skills directory.
pub const NAME: &str = "flagging-files";

/// Put the skill at `skills/flagging-files`, pointing at `published` (the
/// directory holding the published `SKILL.md`). Refuses to replace a
/// directory that is not thirdeye's.
pub fn place(skills: &Path, published: &Path, ours: impl Fn(&str) -> bool) -> Result<()> {
    check(skills, published, &ours)?;
    let at = skills.join(NAME);
    std::fs::create_dir_all(skills).with_context(|| format!("creating {}", skills.display()))?;
    clear(&at)?;
    link_or_copy(published, &at)
}

/// Refuse, before anything is written, a skill directory that is not
/// thirdeye's.
pub fn check(skills: &Path, published: &Path, ours: impl Fn(&str) -> bool) -> Result<()> {
    let at = skills.join(NAME);
    if present(&at) && !is_ours(&at, published, &ours) {
        anyhow::bail!(
            "{} exists and is not thirdeye's; rename it and install again",
            at.display()
        );
    }
    Ok(())
}

/// Remove the skill if it is thirdeye's, and `skills` if that leaves it empty.
pub fn remove(skills: &Path, published: &Path, ours: impl Fn(&str) -> bool) -> Result<()> {
    let at = skills.join(NAME);
    if present(&at) && is_ours(&at, published, &ours) {
        clear(&at)?;
    }
    let _ = std::fs::remove_dir(skills);
    Ok(())
}

/// Whether the skill at `skills` is what an install would put there now: a
/// link to `published`, or a copy of `body`.
pub fn current(skills: &Path, published: &Path, body: &str) -> bool {
    let at = skills.join(NAME);
    match std::fs::read_link(&at) {
        Ok(to) => to == published,
        Err(_) => std::fs::read_to_string(at.join("SKILL.md")).is_ok_and(|have| have == body),
    }
}

fn present(at: &Path) -> bool {
    std::fs::symlink_metadata(at).is_ok()
}

/// A link to `published` is ours, wherever its text came from; a directory
/// is ours if its `SKILL.md` says so — `flagging-files` is a plausible name
/// for a skill the user wrote.
fn is_ours(at: &Path, published: &Path, ours: &impl Fn(&str) -> bool) -> bool {
    match std::fs::read_link(at) {
        Ok(to) => {
            to == published
                || ours(&std::fs::read_to_string(to.join("SKILL.md")).unwrap_or_default())
        }
        // A copy an older thirdeye made — and only that: one the user added
        // files to is theirs now, and replacing it would delete them.
        Err(_) => {
            std::fs::read_to_string(at.join("SKILL.md")).is_ok_and(|s| ours(&s))
                && std::fs::read_dir(at)
                    .is_ok_and(|entries| entries.flatten().all(|e| e.file_name() == "SKILL.md"))
        }
    }
}

/// Remove a link, or a directory of ours, at `at`.
fn clear(at: &Path) -> Result<()> {
    let Ok(meta) = std::fs::symlink_metadata(at) else {
        return Ok(());
    };
    if meta.is_dir() {
        std::fs::remove_dir_all(at)
    } else {
        // A link, even one to a directory: `remove_file` removes the link.
        std::fs::remove_file(at).or_else(|_| std::fs::remove_dir(at))
    }
    .with_context(|| format!("removing {}", at.display()))
}

/// The directory is linked, not `SKILL.md`: Codex 0.162.1 follows a linked
/// skill directory but leaves out a skill whose `SKILL.md` is a link.
#[cfg(unix)]
fn link_or_copy(published: &Path, at: &Path) -> Result<()> {
    std::os::unix::fs::symlink(published, at)
        .with_context(|| format!("linking {} to {}", at.display(), published.display()))
}

#[cfg(not(unix))]
fn link_or_copy(published: &Path, at: &Path) -> Result<()> {
    std::fs::create_dir_all(at).with_context(|| format!("creating {}", at.display()))?;
    let body = std::fs::read_to_string(published.join("SKILL.md"))
        .with_context(|| format!("reading {}", published.join("SKILL.md").display()))?;
    std::fs::write(at.join("SKILL.md"), body)
        .with_context(|| format!("writing {}", at.join("SKILL.md").display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let base = super::super::testing::scratch(tag);
        let published = base.join("data/skills/flagging-files");
        std::fs::create_dir_all(&published).unwrap();
        std::fs::write(published.join("SKILL.md"), "thirdeye skill v1").unwrap();
        (base.join("project/.agent/skills"), published)
    }

    fn ours(text: &str) -> bool {
        text.contains("thirdeye")
    }

    #[test]
    fn a_placed_skill_follows_what_thirdeye_publishes() {
        let (skills, published) = setup("skill-follows");
        place(&skills, &published, ours).unwrap();
        assert!(current(&skills, &published, "thirdeye skill v1"));
        std::fs::write(published.join("SKILL.md"), "thirdeye skill v2").unwrap();
        if cfg!(unix) {
            assert_eq!(
                std::fs::read_to_string(skills.join(NAME).join("SKILL.md")).unwrap(),
                "thirdeye skill v2",
                "the project reads the new text without being touched"
            );
        } else {
            assert!(
                !current(&skills, &published, "thirdeye skill v2"),
                "a stale copy is reported, not hidden"
            );
        }
    }

    #[test]
    fn placing_twice_replaces_rather_than_nests() {
        let (skills, published) = setup("skill-twice");
        place(&skills, &published, ours).unwrap();
        place(&skills, &published, ours).unwrap();
        assert!(!published.join(NAME).exists(), "no link inside the target");
        assert!(current(&skills, &published, "thirdeye skill v1"));
    }

    #[test]
    fn a_users_skill_of_the_same_name_is_left_alone() {
        let (skills, published) = setup("skill-theirs");
        std::fs::create_dir_all(skills.join(NAME)).unwrap();
        std::fs::write(skills.join(NAME).join("SKILL.md"), "mine").unwrap();
        assert!(place(&skills, &published, ours).is_err());
        remove(&skills, &published, ours).unwrap();
        assert_eq!(
            std::fs::read_to_string(skills.join(NAME).join("SKILL.md")).unwrap(),
            "mine"
        );
    }

    #[test]
    fn an_older_copy_of_ours_is_replaced_and_removal_leaves_nothing() {
        let (skills, published) = setup("skill-old");
        std::fs::create_dir_all(skills.join(NAME)).unwrap();
        std::fs::write(skills.join(NAME).join("SKILL.md"), "thirdeye skill v0").unwrap();
        place(&skills, &published, ours).unwrap();
        assert!(current(&skills, &published, "thirdeye skill v1"));
        remove(&skills, &published, ours).unwrap();
        assert!(!skills.exists());
        assert!(
            published.join("SKILL.md").is_file(),
            "the published copy stays"
        );
    }

    #[test]
    fn a_copy_of_ours_the_user_added_files_to_is_theirs() {
        let (skills, published) = setup("skill-added");
        std::fs::create_dir_all(skills.join(NAME)).unwrap();
        std::fs::write(skills.join(NAME).join("SKILL.md"), "thirdeye skill v0").unwrap();
        std::fs::write(skills.join(NAME).join("examples.md"), "mine").unwrap();
        assert!(place(&skills, &published, ours).is_err());
        assert!(skills.join(NAME).join("examples.md").is_file());
    }
}
