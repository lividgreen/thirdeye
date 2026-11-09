//! pi.
//!
//! pi discovers extensions and skills by directory, so this writes only
//! files of thirdeye's own — `extensions/thirdeye/index.js` and
//! `skills/flagging-files` under `<project>/.pi/` or `~/.pi/agent/` — and
//! merges nothing. Both point into thirdeye's directory
//! ([`super::plugin_files`]).

use std::path::PathBuf;

use anyhow::{Context as _, Result};

use super::plugin_files::{self, Layout};
use super::{Context, Inspection, Integration, Scope, Target};

const LAYOUT: Layout = Layout {
    agent: "pi",
    plugin: "extensions/thirdeye",
    template: include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/agents/pi/extension.js"
    )),
    skill: include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/agents/pi/skills/flagging-files/SKILL.md"
    )),
    noun: "pi extension",
};

pub struct Pi;

impl Integration for Pi {
    fn id(&self) -> &'static str {
        "pi"
    }

    fn program(&self) -> &'static str {
        "pi"
    }

    fn scopes(&self) -> &'static [Scope] {
        &[Scope::Project, Scope::User]
    }

    fn config_var(&self) -> Option<&'static str> {
        Some("PI_CODING_AGENT_DIR")
    }

    fn publish(&self, cx: &Context) -> Result<()> {
        plugin_files::publish(cx, &LAYOUT)
    }

    fn install(&self, cx: &Context, target: &Target) -> Result<()> {
        plugin_files::install(cx, &pi_dir(cx, target)?, &LAYOUT)
    }

    fn uninstall(&self, cx: &Context, target: &Target) -> Result<()> {
        plugin_files::uninstall(cx, &pi_dir(cx, target)?, &LAYOUT)
    }

    fn inspect(&self, cx: &Context, target: &Target) -> Option<Inspection> {
        plugin_files::inspect(cx, &pi_dir(cx, target).ok()?, &LAYOUT)
    }

    fn next_steps(&self, target: &Target) -> String {
        let mut steps = "restart pi; a project's .pi/ loads only once you trust the project (pi \
                         asks, or pass --approve)"
            .to_string();
        if matches!(target, Target::Project(_)) {
            steps.push_str(plugin_files::PRIVATE);
        }
        steps
    }
}

/// `<project>/.pi`, or the user's agent directory: `$PI_CODING_AGENT_DIR`,
/// else `~/.pi/agent` — not `~/.pi`.
fn pi_dir(cx: &Context, target: &Target) -> Result<PathBuf> {
    match target {
        Target::Project(project) => Ok(project.join(".pi")),
        Target::User => cx
            .path_var("PI_CODING_AGENT_DIR")
            .or_else(|| cx.home.as_ref().map(|h| h.join(".pi/agent")))
            .context("neither $PI_CODING_AGENT_DIR nor $HOME is set"),
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
            .join(format!("thirdeye-pi-{tag}-{}", std::process::id()))
            .join(".pi");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
        std::fs::create_dir_all(dir.parent().unwrap()).unwrap();
        dir
    }

    #[test]
    fn install_writes_the_extension_and_the_skill_where_pi_looks() {
        let r = root("writes");
        install(&r, Path::new("/opt/thirdeye")).unwrap();
        assert!(r.join("extensions/thirdeye/index.js").is_file());
        assert!(r.join("skills/flagging-files/SKILL.md").is_file());
        uninstall(&r).unwrap();
        assert!(!r.exists());
    }

    #[test]
    fn an_extension_of_the_same_name_that_is_not_ours_is_left_alone() {
        // `extensions/thirdeye/` could be the user's own; overwriting it on
        // install and deleting it on uninstall would lose their work.
        let r = root("their-extension");
        std::fs::create_dir_all(r.join("extensions/thirdeye")).unwrap();
        std::fs::write(r.join("extensions/thirdeye/index.js"), "mine").unwrap();
        assert!(install(&r, Path::new("/opt/thirdeye")).is_err());
        uninstall(&r).unwrap();
        assert_eq!(
            std::fs::read_to_string(r.join("extensions/thirdeye/index.js")).unwrap(),
            "mine"
        );
    }

    #[test]
    fn an_installed_extension_reports_its_binary_and_whether_it_is_current() {
        // What doctor needs: which thirdeye the extension runs, and whether
        // the file is still the one this build would write.
        let r = root("inspect");
        assert!(
            inspect(&r).is_none(),
            "nothing installed, nothing to report"
        );
        install(&r, Path::new("/opt/thirdeye")).unwrap();
        let found = inspect(&r).unwrap();
        assert_eq!(found.binary, PathBuf::from("/opt/thirdeye"));
        assert!(found.current);
        let script = published(&r, "pi");
        let old = std::fs::read_to_string(&script)
            .unwrap()
            .replace("2000", "1000");
        std::fs::write(&script, old).unwrap();
        assert!(
            !inspect(&r).unwrap().current,
            "an older extension must read as stale"
        );
    }

    #[test]
    fn the_user_directory_is_pis_agent_directory() {
        // pi's user-level config is ~/.pi/agent, not ~/.pi.
        let cx = Context {
            home: Some(PathBuf::from("/home/u")),
            ..cx_for(Path::new("/tmp/.pi"), Path::new("/x"))
        };
        assert_eq!(
            pi_dir(&cx, &Target::User).unwrap(),
            PathBuf::from("/home/u/.pi/agent")
        );
        assert_eq!(
            pi_dir(&cx, &Target::Project("/p".into())).unwrap(),
            PathBuf::from("/p/.pi")
        );
    }

    /// Run the rendered extension under node with a stand-in pi API and a
    /// stand-in `thirdeye` (`fake` is its shell body); `None` without node.
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
        let dir = r.join("extensions/thirdeye");
        std::fs::copy(dir.join("index.js"), dir.join("index.mjs")).unwrap();
        std::fs::write(
            dir.join("harness.mjs"),
            format!(
                r#"import factory from "./index.mjs";
const on = {{}}; let tool;
factory({{ on: (n, f) => {{ on[n] = f; }}, registerTool: (t) => {{ tool = t; }} }});
const ctx = {{ cwd: "/tmp", sessionManager: {{ getSessionId: () => "s1" }} }};
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
    fn a_hung_thirdeye_never_holds_up_pi() {
        let Some(out) = drive(
            "hung",
            "sleep 30",
            r#"let t = Date.now();
await on.before_agent_start({ prompt: "x" }, ctx);
const prompt = Date.now() - t; t = Date.now();
await on.tool_call({ toolName: "bash", toolCallId: "c", input: {} }, ctx);
console.log(JSON.stringify({ prompt, bash: Date.now() - t }));"#,
        ) else {
            return eprintln!("node not found; extension unchecked");
        };
        let t: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(t["prompt"].as_u64().unwrap() < 500, "{out}");
        assert!(t["bash"].as_u64().unwrap() < 3_000, "{out}");
    }

    #[test]
    fn a_flag_whose_words_start_with_a_dash_survives() {
        let Some(out) = drive(
            "dash",
            r#"printf '%s|' "$@""#,
            r#"const r = await tool.execute("c", { path: "-x.rs", reason: "-1 is off by one" }, undefined, undefined, ctx);
console.log(r.content[0].text);"#,
        ) else {
            return eprintln!("node not found; extension unchecked");
        };
        assert!(out.contains("|--session=s1|"), "{out}");
        assert!(out.contains("|--reason=-1 is off by one|"), "{out}");
    }

    #[test]
    fn a_flag_without_a_session_is_refused_not_misfiled() {
        let Some(out) = drive(
            "no-session",
            "printf RAN",
            r#"const r = await tool.execute("c", { path: "a.rs", reason: "r" }, undefined, undefined, { cwd: "/tmp" });
console.log(r.content[0].text);"#,
        ) else {
            return eprintln!("node not found; extension unchecked");
        };
        assert!(!out.contains("RAN"), "{out}");
    }

    #[test]
    fn a_reload_does_not_end_the_session() {
        // /reload rebuilds the extension; the session carries on. Ending it
        // would stop the daemon and close the viewer mid-session.
        let ran = root_ran("reload");
        let Some(out) = drive(
            "reload",
            &format!("echo RAN >> '{}'", ran.display()),
            r#"await on.session_shutdown({ reason: "reload" }, ctx);
console.log("done");"#,
        ) else {
            return eprintln!("node not found; extension unchecked");
        };
        assert_eq!(out, "done");
        assert!(!ran.exists(), "a reload ended the session");
    }

    #[test]
    fn quitting_does_end_the_session() {
        // The perturbation half: the guard must not swallow a real end.
        let ran = root_ran("quit");
        drive(
            "quit",
            &format!("echo RAN >> '{}'", ran.display()),
            r#"await on.session_shutdown({ reason: "quit" }, ctx);"#,
        );
        // `drive` runs nothing where a shell script is no program.
        if cfg!(unix) && which_node() {
            assert!(ran.exists(), "quitting did not end the session");
        }
    }

    /// Where a stand-in thirdeye leaves its trace; outside the directory
    /// `root` resets.
    fn root_ran(tag: &str) -> PathBuf {
        let ran =
            std::env::temp_dir().join(format!("thirdeye-pi-ran-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_file(&ran);
        ran
    }

    fn which_node() -> bool {
        std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_ok()
    }

    #[test]
    fn an_unserializable_event_does_not_throw_into_pi() {
        let Some(out) = drive(
            "bigint",
            "true",
            r#"on.tool_result({ toolName: "read", toolCallId: "c", details: { n: 1n } }, ctx);
console.log("survived");"#,
        ) else {
            return eprintln!("node not found; extension unchecked");
        };
        assert_eq!(out, "survived");
    }

    #[test]
    fn the_users_agent_directory_follows_pi_coding_agent_dir() {
        // What `-a pi:<dir>` sets, and what pi itself reads.
        let base = Path::new("/b");
        let moved = |k: &str| (k == "PI_CODING_AGENT_DIR").then(|| "/elsewhere/pi".into());
        let run = super::super::testing::no_run;
        let cx = super::super::testing::context(base, &moved, &run);
        assert_eq!(
            pi_dir(&cx, &Target::User).unwrap(),
            Path::new("/elsewhere/pi")
        );
        let cx = super::super::testing::context(base, &super::super::testing::no_vars, &run);
        assert_eq!(
            pi_dir(&cx, &Target::User).unwrap(),
            base.join("home/.pi/agent")
        );
    }
}
