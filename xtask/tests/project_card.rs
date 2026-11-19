//! The gate for `docs/project-card.md`, the card www.livid.work copies at
//! each release.
//!
//! www's release workflow checks the card too, but a card it rejects only
//! keeps the old text and raises a warning in another repository's run, so a
//! broken card would ship unnoticed. These checks are the rules of the
//! project convention (§2 of `PROJECT-CONVENTION.md` in the www repository):
//! the keys and their limits, no H1, and every relative link resolving.

use std::path::{Path, PathBuf};

const CARD: &str = "docs/project-card.md";
const STATUSES: [&str; 3] = ["active", "shipped", "archived"];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask sits in the repository root")
        .to_path_buf()
}

/// Every problem with a card, given its text and the file it would be read
/// from. Empty means www would accept it.
fn problems(text: &str, card_path: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let Some((front, body)) = split_front_matter(text) else {
        return vec!["no frontmatter block between `---` lines".into()];
    };

    let mut title = None;
    let mut summary = None;
    for line in front.lines() {
        let Some((key, value)) = line.split_once(':') else {
            found.push(format!("frontmatter line is not `key: value`: {line:?}"));
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "title" => title = Some(unquote(value)),
            "summary" => {
                // An unquoted ": " makes YAML read a nested mapping and reject
                // the whole file.
                if value.contains(": ") && !is_quoted(value) {
                    found.push("summary contains \": \" and is not quoted".into());
                }
                summary = Some(unquote(value));
            }
            "status" if !STATUSES.contains(&value) => {
                found.push(format!("status {value:?} is not one of {STATUSES:?}"));
            }
            _ => {}
        }
    }
    match title {
        None => found.push("no title".into()),
        Some(t) if t.is_empty() => found.push("empty title".into()),
        Some(t) if t.chars().count() > 120 => found.push("title over 120 characters".into()),
        Some(_) => {}
    }
    match summary {
        None => found.push("no summary".into()),
        Some(s) if s.is_empty() => found.push("empty summary".into()),
        Some(s) if s.chars().count() > 500 => found.push("summary over 500 characters".into()),
        Some(_) => {}
    }

    let base = card_path.parent().expect("a card is a file in a directory");
    let mut in_fence = false;
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if line.starts_with("# ") {
            found.push(format!("level-1 heading: {line:?}"));
        }
        for dest in link_destinations(line) {
            if let Some(problem) = dangling(&dest, base, body) {
                found.push(problem);
            }
        }
    }
    found
}

fn split_front_matter(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    Some((&rest[..end], &rest[end + "\n---\n".len()..]))
}

fn is_quoted(value: &str) -> bool {
    value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
}

fn unquote(value: &str) -> String {
    if is_quoted(value) {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

/// Destinations of inline links and images, `[t](dest)`, and of
/// reference definitions, `[t]: dest`.
fn link_destinations(line: &str) -> Vec<String> {
    let mut dests = Vec::new();
    let mut rest = line;
    while let Some(at) = rest.find("](") {
        let after = &rest[at + 2..];
        let Some(close) = after.find(')') else { break };
        // A title after the destination, `[t](dest "title")`, is not part of it.
        let dest = after[..close].split_whitespace().next().unwrap_or("");
        dests.push(dest.to_string());
        rest = &after[close..];
    }
    let trimmed = line.trim_start();
    if trimmed.starts_with('[')
        && let Some((_, dest)) = trimmed.split_once("]: ")
    {
        dests.push(dest.split_whitespace().next().unwrap_or("").to_string());
    }
    dests
}

fn dangling(dest: &str, base: &Path, card_body: &str) -> Option<String> {
    if dest.is_empty() {
        return Some("empty link destination".into());
    }
    if dest.contains("://") || dest.starts_with("mailto:") {
        return None;
    }
    if dest.starts_with('/') {
        return Some(format!("absolute path {dest:?}: links must be relative"));
    }
    let (path, anchor) = match dest.split_once('#') {
        Some((p, a)) => (p, Some(a)),
        None => (dest, None),
    };
    let target_text = if path.is_empty() {
        card_body.to_string()
    } else {
        let target = base.join(path);
        if !target.exists() {
            return Some(format!("link {dest:?} names a file that does not exist"));
        }
        match anchor {
            Some(_) => std::fs::read_to_string(&target).unwrap_or_default(),
            None => return None,
        }
    };
    let anchor = anchor?;
    let known = target_text
        .lines()
        .filter(|l| l.starts_with('#'))
        .filter_map(|l| l.trim_start_matches('#').strip_prefix(' '))
        .any(|heading| github_slug(heading) == anchor);
    (!known).then(|| format!("link {dest:?} names an anchor with no matching heading"))
}

/// The anchor GitHub gives a heading: lowercase, punctuation dropped,
/// spaces as hyphens.
fn github_slug(heading: &str) -> String {
    heading
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-' || *c == '_')
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

#[test]
fn the_project_card_is_one_www_would_accept() {
    let path = repo_root().join(CARD);
    let text = std::fs::read_to_string(&path).expect("docs/project-card.md exists");
    let found = problems(&text, &path);
    assert!(found.is_empty(), "{CARD}:\n  {}", found.join("\n  "));
}

// Perturbations: each check goes red on a planted defect, so its green on
// the real card means something.

fn real_card() -> (String, PathBuf) {
    let path = repo_root().join(CARD);
    let text = std::fs::read_to_string(&path).expect("docs/project-card.md exists");
    (text, path)
}

fn assert_caught(planted: &str, path: &Path, expect: &str) {
    let found = problems(planted, path);
    assert!(
        found.iter().any(|p| p.contains(expect)),
        "expected a problem containing {expect:?}, got {found:?}"
    );
}

#[test]
fn a_level_one_heading_is_caught() {
    let (text, path) = real_card();
    let planted = text.replacen("## What it is", "# What it is", 1);
    assert_caught(&planted, &path, "level-1 heading");
}

#[test]
fn a_heading_inside_a_code_block_is_not_a_heading() {
    let (text, path) = real_card();
    let planted = format!("{text}\n```sh\n# a comment\n```\n");
    assert!(problems(&planted, &path).is_empty());
}

#[test]
fn a_link_to_a_missing_file_is_caught() {
    let (text, path) = real_card();
    let planted = text.replacen("(install.md)", "(instal.md)", 1);
    assert_caught(&planted, &path, "does not exist");
}

#[test]
fn a_link_to_a_missing_anchor_is_caught() {
    let (text, path) = real_card();
    let planted = text.replacen("#what-is-supported", "#what-is-supportd", 1);
    assert_caught(&planted, &path, "no matching heading");
}

#[test]
fn a_reference_definition_is_checked_too() {
    let (text, path) = real_card();
    let planted = format!("{text}\n[gone]: missing.md\n");
    assert_caught(&planted, &path, "does not exist");
}

#[test]
fn an_unquoted_summary_with_a_colon_is_caught() {
    let (text, path) = real_card();
    // Whatever the summary says, with its quotes dropped and a colon in it.
    let planted: String = text
        .lines()
        .map(|line| match line.strip_prefix("summary: ") {
            Some(value) => format!("summary: {}: unquoted\n", value.trim_matches('"')),
            None => format!("{line}\n"),
        })
        .collect();
    assert_caught(&planted, &path, "not quoted");
}

#[test]
fn a_summary_over_the_limit_is_caught() {
    let (text, path) = real_card();
    let long = "x".repeat(501);
    let planted = text.replacen("summary: \"", &format!("summary: \"{long}"), 1);
    assert_caught(&planted, &path, "summary over 500");
}

#[test]
fn a_missing_title_is_caught() {
    let (text, path) = real_card();
    let planted = text.replacen("title: thirdeye\n", "", 1);
    assert_caught(&planted, &path, "no title");
}

#[test]
fn an_unknown_status_is_caught() {
    let (text, path) = real_card();
    let planted = text.replacen("status: active", "status: beta", 1);
    assert_caught(&planted, &path, "status");
}

#[test]
fn github_slugs_match_the_readme_headings_the_card_links() {
    assert_eq!(github_slug("What is supported"), "what-is-supported");
    assert_eq!(github_slug("Linux with systemd"), "linux-with-systemd");
    assert_eq!(github_slug("In the viewer"), "in-the-viewer");
}
