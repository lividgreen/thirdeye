//! OpenCode.
//!
//! OpenCode discovers plugins and skills by directory, so this writes only
//! files of its own — `plugins/thirdeye/index.js` and
//! `skills/flagging-files` under `<project>/.opencode/` or
//! `~/.config/opencode/` — and merges nothing into the user's config. Both
//! point into thirdeye's directory ([`super::plugin_files`]).

use std::path::PathBuf;

use anyhow::{Context as _, Result};

use super::plugin_files::{self, Layout, PRIVATE};
use super::{Context, Inspection, Integration, Scope, Target};

const PLUGIN: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/agents/opencode/plugin.js"
));
const SKILL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/agents/opencode/skills/flagging-files/SKILL.md"
));

/// Where each file of the integration goes under the OpenCode directory.
const LAYOUT: Layout = Layout {
    agent: "opencode",
    plugin: "plugins/thirdeye",
    template: PLUGIN,
    skill: SKILL,
    noun: "OpenCode plugin",
};

pub struct OpenCode;

impl Integration for OpenCode {
    fn id(&self) -> &'static str {
        "opencode"
    }

    fn program(&self) -> &'static str {
        "opencode"
    }

    fn scopes(&self) -> &'static [Scope] {
        &[Scope::Project, Scope::User]
    }

    fn config_var(&self) -> Option<&'static str> {
        Some("OPENCODE_CONFIG_DIR")
    }

    fn publish(&self, cx: &Context) -> Result<()> {
        plugin_files::publish(cx, &LAYOUT)
    }

    fn install(&self, cx: &Context, target: &Target) -> Result<()> {
        plugin_files::install(cx, &opencode_dir(cx, target)?, &LAYOUT)
    }

    fn uninstall(&self, cx: &Context, target: &Target) -> Result<()> {
        plugin_files::uninstall(cx, &opencode_dir(cx, target)?, &LAYOUT)
    }

    fn inspect(&self, cx: &Context, target: &Target) -> Option<Inspection> {
        plugin_files::inspect(cx, &opencode_dir(cx, target).ok()?, &LAYOUT)
    }

    fn next_steps(&self, target: &Target) -> String {
        let mut steps = "restart OpenCode — and its background service, `opencode serve \
                         --service` — to load the plugin"
            .to_string();
        if matches!(target, Target::Project(_)) {
            steps.push_str(PRIVATE);
        }
        steps
    }
}

/// `<project>/.opencode`, or the user's: `$OPENCODE_CONFIG_DIR`, which
/// OpenCode reads in place of `~/.config/opencode`, else that.
fn opencode_dir(cx: &Context, target: &Target) -> Result<PathBuf> {
    match target {
        Target::Project(project) => Ok(project.join(".opencode")),
        Target::User => {
            if let Some(dir) = cx.path_var("OPENCODE_CONFIG_DIR") {
                return Ok(dir);
            }
            Ok(cx
                .path_var("XDG_CONFIG_HOME")
                .or_else(|| cx.home.as_ref().map(|h| h.join(".config")))
                .context("neither $XDG_CONFIG_HOME nor $HOME is set")?
                .join("opencode"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// A context whose home and data directory sit beside `root`.
    fn cx_for(root: &Path, binary: &Path) -> Context<'static> {
        let base = root.parent().unwrap();
        Context {
            binary: binary.to_path_buf(),
            data: base.join("data"),
            home: Some(base.join("home")),
            var: &super::super::testing::no_vars,
            run: &super::super::testing::no_run,
        }
    }

    fn install(root: &Path, binary: &Path) -> Result<()> {
        plugin_files::install(&cx_for(root, binary), root, &LAYOUT)
    }

    fn uninstall(root: &Path) -> Result<()> {
        plugin_files::uninstall(&cx_for(root, Path::new("/x")), root, &LAYOUT)
    }

    fn inspect(root: &Path) -> Option<Inspection> {
        plugin_files::inspect(&cx_for(root, Path::new("/x")), root, &LAYOUT)
    }

    /// The script as published, which the stub imports.
    fn published(root: &Path, agent: &str) -> PathBuf {
        root.parent()
            .unwrap()
            .join("data")
            .join(agent)
            .join("plugin.mjs")
    }

    fn root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("thirdeye-opencode-{tag}-{}", std::process::id()))
            .join(".opencode");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
        std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        dir
    }

    const BINARY: &str = r#"/opt/we"ird\dir/thirdeye"#;

    #[test]
    fn install_writes_the_plugin_and_the_skill() {
        let r = root("writes");
        install(&r, Path::new(BINARY)).unwrap();
        assert!(r.join("plugins/thirdeye/index.js").is_file());
        let skill = std::fs::read_to_string(r.join("skills/flagging-files/SKILL.md")).unwrap();
        assert!(skill.contains("thirdeye_flag"));
        assert!(inspect(&r).unwrap().current);
    }

    #[test]
    fn the_plugin_runs_this_binary_whatever_its_path() {
        // The path is written as a JS string literal; a quote or backslash
        // spliced in raw would break the plugin, and OpenCode with it.
        let r = root("binary");
        install(&r, Path::new(BINARY)).unwrap();
        let js = std::fs::read_to_string(published(&r, "opencode")).unwrap();
        let line = js
            .lines()
            .find(|l| l.starts_with("const THIRDEYE = "))
            .unwrap();
        let literal = line
            .trim_start_matches("const THIRDEYE = ")
            .trim_end_matches(';');
        let named: String = serde_json::from_str(literal).unwrap();
        assert_eq!(named, BINARY);
    }

    #[test]
    fn the_rendered_plugin_is_valid_javascript() {
        // Checked with node where it exists; OpenCode would refuse a plugin
        // that does not parse, silently from the human's point of view.
        let r = root("syntax");
        install(&r, Path::new(BINARY)).unwrap();
        let stub = r.join("plugins/thirdeye/index.mjs");
        std::fs::copy(r.join("plugins/thirdeye/index.js"), &stub).unwrap();
        for plugin in [stub, published(&r, "opencode")] {
            match std::process::Command::new("node")
                .arg("--check")
                .arg(&plugin)
                .output()
            {
                Ok(out) => assert!(
                    out.status.success(),
                    "{}",
                    String::from_utf8_lossy(&out.stderr)
                ),
                Err(_) => eprintln!("node not found; syntax unchecked"),
            }
        }
    }

    #[test]
    fn uninstall_leaves_nothing_where_there_was_nothing() {
        let r = root("nothing");
        install(&r, Path::new(BINARY)).unwrap();
        uninstall(&r).unwrap();
        assert!(!r.exists());
    }

    #[test]
    fn the_users_own_plugins_and_skills_survive() {
        let r = root("theirs");
        std::fs::create_dir_all(r.join("plugins/mine")).unwrap();
        std::fs::write(r.join("plugins/mine/index.js"), "x").unwrap();
        std::fs::create_dir_all(r.join("skills/review")).unwrap();
        std::fs::write(r.join("skills/review/SKILL.md"), "y").unwrap();
        install(&r, Path::new(BINARY)).unwrap();
        uninstall(&r).unwrap();
        assert!(r.join("plugins/mine/index.js").exists());
        assert!(r.join("skills/review/SKILL.md").exists());
        assert!(!r.join("plugins/thirdeye").exists());
    }

    #[test]
    fn a_skill_of_the_same_name_that_is_not_ours_is_left_alone() {
        // `flagging-files` is a plausible name for a skill the user copied
        // from Claude's. Overwriting it on install and deleting it on
        // uninstall would lose their file.
        let r = root("their-skill");
        std::fs::create_dir_all(r.join("skills/flagging-files")).unwrap();
        std::fs::write(r.join("skills/flagging-files/SKILL.md"), "mine").unwrap();
        assert!(install(&r, Path::new(BINARY)).is_err());
        uninstall(&r).unwrap();
        assert_eq!(
            std::fs::read_to_string(r.join("skills/flagging-files/SKILL.md")).unwrap(),
            "mine"
        );
    }

    /// Run the rendered plugin under node with a stand-in OpenCode context and
    /// a stand-in `thirdeye` (`fake` is its shell body), returning what the
    /// harness printed. `None` when node is absent.
    fn drive(tag: &str, fake: &str, harness: &str) -> Option<String> {
        let r = root(tag);
        let bin = r.parent().unwrap().join("fake-thirdeye");
        std::fs::write(&bin, format!("#!/bin/sh\n{fake}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // A shell script is not a program Windows can run.
        if cfg!(not(unix)) {
            return None;
        }
        install(&r, &bin).unwrap();
        let dir = r.join("plugins/thirdeye");
        std::fs::copy(dir.join("index.js"), dir.join("index.mjs")).unwrap();
        std::fs::write(
            dir.join("harness.mjs"),
            format!(
                r#"import plugin from "./index.mjs";
const hooks = {{}}; let tool;
const ctx = {{
  location: {{ directory: "/tmp" }},
  session: {{ hook: async (n, f) => {{ hooks["session." + n] = f; }} }},
  tool: {{ hook: async (n, f) => {{ hooks["tool." + n] = f; }},
          transform: async (fn) => fn({{ add: (t) => {{ tool = t; }} }}) }},
  event: {{ subscribe: () => (async function* () {{}})() }},
}};
await plugin.setup(ctx);
{harness}
process.exit(0);
"#
            ),
        )
        .unwrap();
        let out = std::process::Command::new("node")
            .arg(dir.join("harness.mjs"))
            .output()
            .ok()?;
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    #[test]
    fn a_hung_thirdeye_never_holds_up_opencode() {
        // OpenCode awaits the prompt and shell hooks. A stuck state directory
        // must cost a missed event, not a frozen agent.
        let Some(out) = drive(
            "hung",
            "sleep 30",
            r#"let t = Date.now();
await hooks["session.prompt"]({ sessionID: "s", messageID: "m", prompt: { text: "x" } });
const prompt = Date.now() - t; t = Date.now();
await hooks["tool.execute.before"]({ tool: "shell", sessionID: "s", id: "c" });
console.log(JSON.stringify({ prompt, shell: Date.now() - t }));"#,
        ) else {
            return eprintln!("node not found; plugin unchecked");
        };
        let t: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(t["prompt"].as_u64().unwrap() < 500, "{out}");
        assert!(t["shell"].as_u64().unwrap() < 3_000, "{out}");
    }

    #[test]
    fn a_flag_whose_words_start_with_a_dash_survives() {
        // `--reason "-1 is off by one"` read as a flag would reject it.
        let Some(out) = drive(
            "dash",
            r#"printf '%s|' "$@""#,
            r#"const r = await tool.execute({ path: "-x.rs", reason: "-1 is off by one" }, { sessionID: "s1" });
console.log(r.content);"#,
        ) else {
            return eprintln!("node not found; plugin unchecked");
        };
        assert!(out.contains("|--path=-x.rs|"), "{out}");
        assert!(out.contains("|--reason=-1 is off by one|"), "{out}");
    }

    #[test]
    fn a_flag_without_a_session_is_refused_not_misfiled() {
        let Some(out) = drive(
            "no-session",
            r#"printf 'RAN'"#,
            r#"const r = await tool.execute({ path: "a.rs", reason: "r" }, {});
console.log(r.content);"#,
        ) else {
            return eprintln!("node not found; plugin unchecked");
        };
        assert!(!out.contains("RAN"), "{out}");
    }

    #[test]
    fn an_unserializable_event_does_not_throw_into_opencode() {
        let Some(out) = drive(
            "bigint",
            "true",
            r#"hooks["tool.execute.after"]({ tool: "read", sessionID: "s", id: "c", result: { n: 1n } });
console.log("survived");"#,
        ) else {
            return eprintln!("node not found; plugin unchecked");
        };
        assert_eq!(out, "survived");
    }

    #[test]
    fn the_template_still_names_the_placeholder_the_installer_rewrites() {
        assert!(PLUGIN.contains(plugin_files::PLACEHOLDER));
    }

    #[test]
    fn publishing_a_new_version_leaves_the_project_byte_for_byte_alone() {
        // The stub imports the published script; an update rewrites only that.
        use super::super::testing::snapshot;
        let r = root("rule");
        install(&r, Path::new(BINARY)).unwrap();
        let before = snapshot(&r);
        let moved = cx_for(&r, Path::new("/elsewhere/thirdeye"));
        OpenCode.refresh(&moved).unwrap();
        assert_eq!(snapshot(&r), before);
        let found = OpenCode
            .inspect(&moved, &Target::Project(r.parent().unwrap().to_path_buf()))
            .unwrap();
        assert_eq!(found.binary, Path::new("/elsewhere/thirdeye"));
        assert!(found.current);
    }

    #[test]
    fn the_users_config_follows_opencode_config_dir() {
        // OpenCode reads it in place of ~/.config/opencode, not beside it.
        let base = std::path::Path::new("/b");
        let moved = |k: &str| (k == "OPENCODE_CONFIG_DIR").then(|| "/elsewhere/oc".into());
        let run = super::super::testing::no_run;
        let cx = super::super::testing::context(base, &moved, &run);
        assert_eq!(
            opencode_dir(&cx, &Target::User).unwrap(),
            std::path::Path::new("/elsewhere/oc")
        );
        let cx = super::super::testing::context(base, &super::super::testing::no_vars, &run);
        assert_eq!(
            opencode_dir(&cx, &Target::User).unwrap(),
            base.join("home/.config/opencode")
        );
    }
}
