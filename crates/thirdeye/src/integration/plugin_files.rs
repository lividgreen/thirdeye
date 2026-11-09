//! A script-and-skill integration, for an agent that discovers both by
//! directory: OpenCode's plugins, pi's extensions.
//!
//! The script itself is published into thirdeye's directory, naming the
//! binary. What goes into the agent's directory is a **stub** that imports
//! it by absolute path and is the same for every version, and a link to the
//! published skill ([`super::skills`]). So an update rewrites the published
//! script, and the next session loads it. Nothing is merged into the agent's
//! config.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use super::tree::{self, File};
use super::{AgentSays, Context, Inspection, skills};

/// Said after a project install: the stub and the skill link name paths in
/// this user's home, which mean nothing in a teammate's clone.
pub const PRIVATE: &str = ". The files it wrote name paths on this machine: keep them out \
                           of version control";

/// What the script templates call this binary.
pub const PLACEHOLDER: &str = "{{THIRDEYE}}";

/// The published script's name. `.mjs`, so it is a module to every loader
/// whatever `package.json` it finds above thirdeye's directory.
const SCRIPT: &str = "plugin.mjs";

/// Where an agent wants the integration, relative to its config directory.
pub struct Layout {
    /// The agent's id: its part of thirdeye's directory.
    pub agent: &'static str,
    /// Directory holding the stub, as `index.js`; its parent is the agent's
    /// plugin or extension directory.
    pub plugin: &'static str,
    /// The script, with [`PLACEHOLDER`] for the binary.
    pub template: &'static str,
    /// The `flagging-files` skill.
    pub skill: &'static str,
    /// How the agent names what it loads, for the stub's comment.
    pub noun: &'static str,
}

impl Layout {
    fn published(&self, cx: &Context) -> PathBuf {
        cx.data_for(self.agent)
    }

    fn skill_source(&self, published: &Path) -> PathBuf {
        published.join("skills").join(skills::NAME)
    }

    /// The published files, for `binary`.
    fn files(&self, binary: &Path) -> Vec<File> {
        vec![
            File::new(SCRIPT, render(self.template, binary)),
            File::new(format!("skills/{}/SKILL.md", skills::NAME), self.skill),
        ]
    }

    /// The stub, importing the script published under `published`.
    fn stub(&self, published: &Path) -> String {
        let url =
            serde_json::to_string(&file_url(&published.join(SCRIPT))).expect("a string serializes");
        format!(
            "// thirdeye's {noun} — installed by `thirdeye install {agent}`.\n\
             //\n\
             // The {noun} itself is in thirdeye's own directory, which an update\n\
             // rewrites; this file only points there, and stays the same.\n\
             export {{ default }} from {url};\n",
            noun = self.noun,
            agent = self.agent,
        )
    }
}

/// The script with the binary written in as a JS string literal: JSON's
/// string syntax is valid JavaScript, and it escapes whatever the path holds.
fn render(template: &str, binary: &Path) -> String {
    let literal =
        serde_json::to_string(&binary.display().to_string()).expect("a string serializes");
    template.replace(PLACEHOLDER, &literal)
}

/// A `file:` URL for an absolute path: an import specifier that is a bare
/// Windows path is read as a URL scheme `c:`.
fn file_url(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    let mut url = String::from("file://");
    if !text.starts_with('/') {
        url.push('/');
    }
    for c in text.chars() {
        match c {
            ' ' => url.push_str("%20"),
            '%' => url.push_str("%25"),
            '#' => url.push_str("%23"),
            '?' => url.push_str("%3F"),
            '"' => url.push_str("%22"),
            _ => url.push(c),
        }
    }
    url
}

pub fn publish(cx: &Context, layout: &Layout) -> Result<()> {
    tree::publish(&layout.published(cx), &layout.files(&cx.binary))
}

pub fn install(cx: &Context, root: &Path, layout: &Layout) -> Result<()> {
    let published = layout.published(cx);
    let skill = layout.skill_source(&published);
    // Both refusals before anything is written.
    skills::check(&root.join("skills"), &skill, ours)?;
    let plugin = root.join(layout.plugin);
    if plugin.exists() && !script_is_ours(&plugin) {
        anyhow::bail!(
            "{} exists and is not thirdeye's; rename it and install again",
            plugin.display()
        );
    }
    publish(cx, layout)?;
    std::fs::create_dir_all(&plugin).with_context(|| format!("creating {}", plugin.display()))?;
    tree::write_atomically(&plugin.join("index.js"), &layout.stub(&published), false)?;
    skills::place(&root.join("skills"), &skill, ours)
}

pub fn uninstall(cx: &Context, root: &Path, layout: &Layout) -> Result<()> {
    let plugin = root.join(layout.plugin);
    if script_is_ours(&plugin) {
        let _ = std::fs::remove_dir_all(&plugin);
    }
    skills::remove(
        &root.join("skills"),
        &layout.skill_source(&layout.published(cx)),
        ours,
    )?;
    // `remove_dir` only removes an empty directory, so anything of the
    // user's keeps these in place.
    if let Some(parent) = plugin.parent() {
        let _ = std::fs::remove_dir(parent);
    }
    let _ = std::fs::remove_dir(root);
    Ok(())
}

/// What is installed under `root`, if an integration of ours is.
pub fn inspect(cx: &Context, root: &Path, layout: &Layout) -> Option<Inspection> {
    let plugin = root.join(layout.plugin);
    let stub = std::fs::read_to_string(plugin.join("index.js")).ok()?;
    if !stub.contains("installed by `thirdeye ") {
        return None;
    }
    let published = layout.published(cx);
    // The binary the published script names: what the stub, and so the
    // agent, runs.
    let script = std::fs::read_to_string(published.join(SCRIPT)).unwrap_or_default();
    let binary = script
        .lines()
        .find_map(|l| l.strip_prefix("const THIRDEYE = "))
        .map(|l| l.trim_end_matches(';'))
        .and_then(|literal| serde_json::from_str::<String>(literal).ok())
        .map(PathBuf::from)
        .unwrap_or_else(|| cx.binary.clone());
    Some(Inspection {
        current: stub == layout.stub(&published)
            && tree::matches(&published, &layout.files(&binary))
            && skills::current(
                &root.join("skills"),
                &layout.skill_source(&published),
                layout.skill,
            ),
        binary,
        location: plugin,
        agent_says: AgentSays::Loaded,
    })
}

/// Whether the script directory holds a script of ours. Every stub, and
/// every script an older thirdeye copied there, says which install wrote it.
fn script_is_ours(plugin: &Path) -> bool {
    std::fs::read_to_string(plugin.join("index.js"))
        .is_ok_and(|s| s.contains("installed by `thirdeye "))
}

/// Whether a skill's text is thirdeye's, any version of it.
fn ours(text: &str) -> bool {
    text.contains("thirdeye_flag")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_becomes_a_url_an_import_resolves() {
        assert_eq!(
            file_url(Path::new("/home/u/my data/x#1.mjs")),
            "file:///home/u/my%20data/x%231.mjs"
        );
        assert_eq!(
            file_url(Path::new(r"C:\Users\u\AppData\x.mjs")),
            "file:///C:/Users/u/AppData/x.mjs"
        );
    }
}
