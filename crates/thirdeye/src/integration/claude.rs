//! Claude Code.
//!
//! The plugin lives in a local marketplace in thirdeye's directory, and a
//! project — or the user's settings — declares that marketplace by path and
//! enables the plugin. Neither changes between versions.
//!
//! **Claude Code loads a directory marketplace's plugin live** from its
//! folder at every session start, with `CLAUDE_PLUGIN_ROOT` naming the
//! folder: `claude plugin list --json` reports it as `readFromFolder`, and a
//! hook added to the folder alone ran in the next `claude -p` (verified
//! against 2.1.300). So an update is a rewrite of the folder. The copy
//! Claude still makes in its plugin cache is not what runs; for a Claude
//! that does run it, the version carries a hash of the files, and
//! `claude plugin update` acts on any change of version string, build
//! metadata included.
//!
//! A project install uses Claude's `local` scope, `.claude/settings.local.json`:
//! the marketplace is declared by an absolute path into this user's home,
//! which in the committed `.claude/settings.json` would point every teammate
//! at a directory they do not have.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

use super::common::{allow_in_settings, read_object, shell_quote, write_object};
use super::tree::{self, File};
use super::{AgentSays, Context, Inspection, Integration, Scope, Target};

macro_rules! template {
    ($path:literal) => {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/agents/claude/",
            $path
        ))
    };
}

/// Every file of the marketplace that is not generated, as published.
const STATIC: &[(&str, &str)] = &[
    (
        ".claude-plugin/marketplace.json",
        template!(".claude-plugin/marketplace.json"),
    ),
    (
        "thirdeye/hooks/hooks.json",
        template!("thirdeye/hooks/hooks.json"),
    ),
    (
        "thirdeye/commands/thirdeye-open.md",
        template!("thirdeye/commands/thirdeye-open.md"),
    ),
    (
        "thirdeye/commands/thirdeye-close.md",
        template!("thirdeye/commands/thirdeye-close.md"),
    ),
    (
        "thirdeye/commands/thirdeye-policy.md",
        template!("thirdeye/commands/thirdeye-policy.md"),
    ),
    (
        "thirdeye/skills/flagging-files/SKILL.md",
        template!("thirdeye/skills/flagging-files/SKILL.md"),
    ),
];

const MANIFEST: &str = template!("thirdeye/.claude-plugin/plugin.json");
const HOOK: &str = template!("thirdeye/scripts/thirdeye-hook.sh");

/// What the hook script calls this binary.
const PLACEHOLDER: &str = "{{THIRDEYE}}";
const VERSION: &str = "{{VERSION}}";

const PLUGIN: &str = "thirdeye@thirdeye";

/// The flag tool as Claude Code names a plugin's MCP tool:
/// `mcp__plugin_<plugin>_<server>__<tool>`.
const FLAG_TOOL: &str = "mcp__plugin_thirdeye_thirdeye__flag";

pub struct Claude;

impl Integration for Claude {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn program(&self) -> &'static str {
        "claude"
    }

    fn scopes(&self) -> &'static [Scope] {
        &[Scope::Project, Scope::User]
    }

    fn config_var(&self) -> Option<&'static str> {
        Some("CLAUDE_CONFIG_DIR")
    }

    // `docs/native-windows.md`: Claude Code runs a plugin's hooks through
    // Git Bash, which the hook script is written for.
    fn verified_on_windows(&self) -> bool {
        true
    }

    fn publish(&self, cx: &Context) -> Result<()> {
        let server = crate::shadow::run_path(&cx.data, &cx.binary);
        tree::publish(&cx.data_for(self.id()), &files(&cx.binary, &server))
    }

    fn install(&self, cx: &Context, target: &Target) -> Result<()> {
        let settings = settings(cx, target)?;
        self.publish(cx)?;
        let root = cx.data_for(self.id()).display().to_string();
        let (scope, dir) = claude_scope(cx, target);
        // Both are idempotent: adding a marketplace of the same name from
        // another directory re-points it, and installing an installed
        // plugin says so.
        (cx.run)(
            "claude",
            &["plugin", "marketplace", "add", &root, "--scope", scope],
            dir,
        )?;
        (cx.run)(
            "claude",
            &["plugin", "install", PLUGIN, "--scope", scope],
            dir,
        )?;
        // Claude Code asks before an MCP tool's first use; a flag the human
        // must first approve is a flag that arrives late.
        allow_in_settings(&settings, FLAG_TOOL, true)
    }

    fn uninstall(&self, cx: &Context, target: &Target) -> Result<()> {
        let (scope, dir) = claude_scope(cx, target);
        let _ = (cx.run)(
            "claude",
            &["plugin", "uninstall", PLUGIN, "--scope", scope],
            dir,
        );
        // Never `claude plugin marketplace remove`, whatever the scope: it
        // forgets the marketplace for every project, and with it every
        // other project's install (verified against 2.1.300). Only this
        // scope's declaration goes, from its own settings file.
        let settings = settings(cx, target)?;
        forget_marketplace(&settings)?;
        // `plugin uninstall` removes this too, when `claude` runs at all.
        forget_enabled(&settings)?;
        allow_in_settings(&settings, FLAG_TOOL, false)
    }

    fn retire(&self, cx: &Context) -> Result<()> {
        // Nothing is wired any more, so forgetting it everywhere is right.
        let _ = (cx.run)(
            "claude",
            &["plugin", "marketplace", "remove", "thirdeye"],
            cx.home.as_deref(),
        );
        Ok(())
    }

    fn inspect(&self, cx: &Context, target: &Target) -> Option<Inspection> {
        let settings_file = settings(cx, target).ok()?;
        let settings = read_object(&settings_file).ok()?;
        // Not enabled here, not installed here.
        settings.get("enabledPlugins")?.get(PLUGIN)?;
        let root = cx.data_for(self.id());
        let (scope, dir) = claude_scope(cx, target);
        let Some(binary) = published_binary(&root) else {
            // Enabled, with nothing of ours to load: an install from before
            // thirdeye's directory existed — its marketplace was a checkout —
            // or a deleted directory. Every session loads nothing.
            return Some(Inspection {
                binary: cx.binary.clone(),
                current: true,
                location: settings_file,
                agent_says: AgentSays::NotLoaded {
                    detail: format!(
                        "Claude Code has {PLUGIN} enabled, but {} holds no plugin of thirdeye's",
                        root.display()
                    ),
                    fix: cx.install_command("claude", self.config_var(), target),
                },
            });
        };
        let files = files(&binary, &server_for(cx, &binary));
        let agent_says = match (cx.run)("claude", &["plugin", "list", "--json"], dir) {
            Ok(list) => loaded(
                &list,
                scope,
                &root.join("thirdeye"),
                &version(&files),
                &cx.install_command("claude", self.config_var(), target),
            ),
            Err(_) => AgentSays::Unknown(
                "`claude plugin list` could not be run to confirm what loads".into(),
            ),
        };
        Some(Inspection {
            current: tree::matches(&root, &files),
            binary,
            location: root,
            agent_says,
        })
    }

    fn next_steps(&self, _: &Target) -> String {
        "start a new claude session: plugins load at session start".into()
    }
}

/// Claude's own scope for a target, and where to run `claude` for it.
fn claude_scope<'a>(cx: &'a Context, target: &'a Target) -> (&'static str, Option<&'a Path>) {
    match target {
        Target::Project(dir) => ("local", Some(dir.as_path())),
        Target::User => ("user", cx.home.as_deref()),
    }
}

/// The settings file Claude records a scope in — the one `claude plugin
/// install --scope` writes the enablement to.
fn settings(cx: &Context, target: &Target) -> Result<PathBuf> {
    Ok(match target {
        Target::Project(dir) => dir.join(".claude/settings.local.json"),
        Target::User => cx
            .path_var("CLAUDE_CONFIG_DIR")
            .or_else(|| cx.home.as_ref().map(|h| h.join(".claude")))
            .context("$HOME is not set")?
            .join("settings.json"),
    })
}

/// Take `extraKnownMarketplaces.thirdeye` out of a settings file.
fn forget_marketplace(settings: &Path) -> Result<()> {
    if !settings.exists() {
        return Ok(());
    }
    let mut doc = read_object(settings)?;
    let Some(known) = doc
        .get_mut("extraKnownMarketplaces")
        .and_then(|v| v.as_object_mut())
    else {
        return Ok(());
    };
    if known.remove("thirdeye").is_none() {
        return Ok(());
    }
    if known.is_empty() {
        doc.remove("extraKnownMarketplaces");
    }
    write_object(settings, &doc)
}

/// Take `enabledPlugins["thirdeye@thirdeye"]` out of a settings file, and
/// the object with it if that empties it.
fn forget_enabled(settings: &Path) -> Result<()> {
    if !settings.exists() {
        return Ok(());
    }
    let mut doc = read_object(settings)?;
    let Some(enabled) = doc
        .get_mut("enabledPlugins")
        .and_then(|v| v.as_object_mut())
    else {
        return Ok(());
    };
    let removed = enabled.remove(PLUGIN).is_some();
    if enabled.is_empty() {
        doc.remove("enabledPlugins");
    } else if !removed {
        return Ok(());
    }
    write_object(settings, &doc)
}

/// Every published file, for `binary`.
fn files(binary: &Path, server: &Path) -> Vec<File> {
    let mut files: Vec<File> = STATIC
        .iter()
        .map(|(path, body)| File::new(*path, *body))
        .collect();
    files.push(File::new("thirdeye/scripts/thirdeye-hook.sh", hook(binary)).executable());
    files.push(File::new("thirdeye/.mcp.json", mcp(binary, server)));
    // Last, and over everything else: the version is how a Claude that runs
    // its cached copy learns there is a new one.
    let version = format!(
        "{}+{:08x}",
        env!("CARGO_PKG_VERSION"),
        tree::content_hash(&files) as u32
    );
    files.push(File::new(
        "thirdeye/.claude-plugin/plugin.json",
        MANIFEST.replace(VERSION, &version),
    ));
    files
}

/// The manifest's version among `files`.
fn version(files: &[File]) -> String {
    files
        .iter()
        .find(|f| f.path.ends_with("plugin.json"))
        .and_then(|f| serde_json::from_str::<serde_json::Value>(&f.body).ok())
        .and_then(|doc| doc["version"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The hook script, naming `binary` exactly. On Windows the script runs
/// under Git Bash, which takes `C:/…` where a backslash would be an escape.
fn hook(binary: &Path) -> String {
    let text = binary.display().to_string();
    let text = if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text
    };
    HOOK.replace(PLACEHOLDER, &shell_quote(Path::new(&text)))
}

/// The binary itself, never a wrapper: an MCP server's stdout is the
/// protocol, and nothing may write to it but the server.
///
/// The server runs `server`, which on Windows is a per-build copy of
/// `binary` (`shadow.rs`) so a package manager can replace `binary` while a
/// session holds the server. `binary` is then recorded in `env`, because
/// `published_binary` could not otherwise learn which one hooks run.
fn mcp(binary: &Path, server: &Path) -> String {
    let mut entry = serde_json::json!({
        "command": server.display().to_string(),
        "args": ["mcp"],
    });
    if server != binary {
        entry["env"] = serde_json::json!({ PUBLISHED_BINARY: binary.display().to_string() });
    }
    let doc = serde_json::json!({ "mcpServers": { "thirdeye": entry } });
    serde_json::to_string_pretty(&doc).expect("a JSON value serializes") + "\n"
}

/// The `.mcp.json` variable naming the binary hooks run, when the server runs
/// a copy.
const PUBLISHED_BINARY: &str = "THIRDEYE_PUBLISHED_BINARY";

/// What the MCP server of a plugin directory should run, without making it.
fn server_for(cx: &Context, binary: &Path) -> PathBuf {
    if cfg!(windows) {
        crate::shadow::path(&cx.data, &crate::published::this_build())
    } else {
        binary.to_path_buf()
    }
}

fn published_binary(root: &Path) -> Option<PathBuf> {
    let marketplace = std::fs::read_to_string(root.join(".claude-plugin/marketplace.json")).ok()?;
    if !marketplace.contains("installed by `thirdeye install claude`") {
        return None;
    }
    let mcp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("thirdeye/.mcp.json")).ok()?)
            .ok()?;
    let server = &mcp["mcpServers"]["thirdeye"];
    Some(PathBuf::from(
        server["env"][PUBLISHED_BINARY]
            .as_str()
            .or_else(|| server["command"].as_str())?,
    ))
}

/// What `claude plugin list --json` says loads for `scope`: the folder
/// itself, or a cached copy — current only if its version is `version`.
fn loaded(list: &str, scope: &str, folder: &Path, version: &str, reinstall: &str) -> AgentSays {
    let fix = format!("{reinstall}, or claude plugin update {PLUGIN} --scope {scope}");
    let Ok(serde_json::Value::Array(entries)) = serde_json::from_str(list) else {
        return AgentSays::Unknown("`claude plugin list --json` printed no list".into());
    };
    let Some(entry) = entries
        .iter()
        .find(|e| e["id"] == PLUGIN && e["scope"] == scope)
    else {
        return AgentSays::NotLoaded {
            detail: format!("Claude Code does not list {PLUGIN} at {scope} scope"),
            fix,
        };
    };
    if entry["enabled"] == false {
        return AgentSays::NotLoaded {
            detail: "Claude Code has the plugin disabled, so no hook runs".into(),
            fix: format!("claude plugin enable {PLUGIN} --scope {scope}"),
        };
    }
    match entry["readFromFolder"].as_str() {
        Some(read) if Path::new(read) == folder => AgentSays::Loaded,
        Some(read) => AgentSays::NotLoaded {
            detail: format!("Claude Code loads the plugin from {read}, not thirdeye's directory"),
            fix,
        },
        // A Claude Code that runs its cached copy.
        None if entry["version"] == version => AgentSays::Loaded,
        None => AgentSays::NotLoaded {
            detail: format!(
                "Claude Code runs a cached copy, version {}; thirdeye's is {version}",
                entry["version"]
            ),
            fix,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::{BINARY, Calls, context, no_vars, scratch, snapshot};
    use super::*;

    fn recording(calls: &Calls) -> impl Fn(&str, &[&str], Option<&Path>) -> Result<String> + '_ {
        move |p, a, dir| {
            calls.record(p, a);
            if let Some(dir) = dir {
                assert!(dir.is_absolute(), "claude runs somewhere definite");
            }
            Ok(String::new())
        }
    }

    #[test]
    fn a_project_install_declares_thirdeyes_directory_privately_and_allows_the_flag() {
        let base = scratch("claude-install");
        let calls = Calls::default();
        let run = recording(&calls);
        let cx = context(&base, &no_vars, &run);
        let project = base.join("p");
        Claude
            .install(&cx, &Target::Project(project.clone()))
            .unwrap();
        let root = cx.data_for("claude");
        assert_eq!(
            calls.take(),
            [
                format!(
                    "claude plugin marketplace add {} --scope local",
                    root.display()
                ),
                "claude plugin install thirdeye@thirdeye --scope local".to_string(),
            ]
        );
        let settings = read_object(&project.join(".claude/settings.local.json")).unwrap();
        assert_eq!(
            settings["permissions"]["allow"],
            serde_json::json!([FLAG_TOOL])
        );
        assert!(root.join("thirdeye/hooks/hooks.json").is_file());
    }

    #[test]
    fn publishing_a_new_version_leaves_the_project_byte_for_byte_alone() {
        // The rule: an update rewrites thirdeye's directory and nothing else.
        let base = scratch("claude-rule");
        let calls = Calls::default();
        let run = recording(&calls);
        let cx = context(&base, &no_vars, &run);
        let project = base.join("p");
        Claude
            .install(&cx, &Target::Project(project.clone()))
            .unwrap();
        let before = snapshot(&project);
        let moved = context(&base, &no_vars, &run);
        let moved = Context {
            binary: PathBuf::from("/elsewhere/thirdeye"),
            ..moved
        };
        Claude.refresh(&moved).unwrap();
        assert_eq!(snapshot(&project), before);
        assert!(
            std::fs::read_to_string(cx.data_for("claude").join("thirdeye/.mcp.json"))
                .unwrap()
                .contains("/elsewhere/thirdeye")
        );
    }

    #[test]
    fn the_version_changes_exactly_when_the_files_do() {
        // `claude plugin update` acts on any change of version string, and
        // only on one; a Claude running its cached copy relies on it.
        let of = |p: &str| version(&files(Path::new(p), Path::new(p)));
        let a = of("/a/thirdeye");
        assert_eq!(a, of("/a/thirdeye"));
        assert_ne!(a, of("/b/thirdeye"));
        assert!(
            a.starts_with(concat!(env!("CARGO_PKG_VERSION"), "+")),
            "{a}"
        );
    }

    #[test]
    fn the_hook_names_the_binary_and_never_fails_the_agent() {
        let script = hook(Path::new("/opt/it's/thirdeye"));
        assert!(script.contains(r"bin='/opt/it'\''s/thirdeye'"), "{script}");
        assert!(script.trim_end().ends_with("exit 0"));
        assert!(!script.contains("command -v"), "no PATH lookup");
        let doc: serde_json::Value =
            serde_json::from_str(&mcp(Path::new(BINARY), Path::new(BINARY))).unwrap();
        assert_eq!(doc["mcpServers"]["thirdeye"]["command"], BINARY);
    }

    #[test]
    fn a_server_run_from_a_copy_still_tells_which_binary_hooks_run() {
        let text = mcp(
            Path::new("/pkg/thirdeye"),
            Path::new("/data/bin/b/thirdeye"),
        );
        let doc: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            doc["mcpServers"]["thirdeye"]["command"],
            "/data/bin/b/thirdeye"
        );

        let root = scratch("claude-copy");
        std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
        std::fs::create_dir_all(root.join("thirdeye")).unwrap();
        std::fs::write(
            root.join(".claude-plugin/marketplace.json"),
            "installed by `thirdeye install claude`",
        )
        .unwrap();
        std::fs::write(root.join("thirdeye/.mcp.json"), text).unwrap();
        assert_eq!(
            published_binary(&root),
            Some(PathBuf::from("/pkg/thirdeye"))
        );
    }

    #[test]
    fn the_templates_still_name_the_placeholders_the_installer_rewrites() {
        assert!(HOOK.contains(PLACEHOLDER));
        assert!(MANIFEST.contains(VERSION));
    }

    #[test]
    fn every_hook_runs_the_published_script() {
        let hooks: serde_json::Value =
            serde_json::from_str(template!("thirdeye/hooks/hooks.json")).unwrap();
        for (event, groups) in hooks["hooks"].as_object().unwrap() {
            let command = groups[0]["hooks"][0]["command"].as_str().unwrap();
            assert!(
                command.contains("${CLAUDE_PLUGIN_ROOT}/scripts/thirdeye-hook.sh"),
                "{event}: {command}"
            );
        }
    }

    #[test]
    fn uninstall_takes_out_this_projects_flag_and_marketplace_and_nobody_elses() {
        let base = scratch("claude-uninstall");
        let calls = Calls::default();
        let run = recording(&calls);
        let cx = context(&base, &no_vars, &run);
        let project = base.join("p");
        let settings = project.join(".claude/settings.local.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(&settings, r#"{"permissions":{"allow":["Bash(ls:*)"]}}"#).unwrap();
        let target = Target::Project(project);
        Claude.install(&cx, &target).unwrap();
        // What `claude plugin marketplace add --scope local` writes.
        let mut doc = read_object(&settings).unwrap();
        doc.insert(
            "extraKnownMarketplaces".into(),
            serde_json::json!({ "thirdeye": { "source": { "source": "directory", "path": "/d" } } }),
        );
        write_object(&settings, &doc).unwrap();
        calls.take();
        Claude.uninstall(&cx, &target).unwrap();
        // Never `marketplace remove`: it would unwire every other project.
        assert_eq!(
            calls.take(),
            ["claude plugin uninstall thirdeye@thirdeye --scope local"]
        );
        let doc = read_object(&settings).unwrap();
        assert_eq!(
            doc["permissions"]["allow"],
            serde_json::json!(["Bash(ls:*)"])
        );
        assert!(doc.get("extraKnownMarketplaces").is_none(), "{doc:?}");
    }

    #[test]
    fn uninstall_leaves_no_enable_and_no_empty_lists_when_claude_does_not_run() {
        // Found by review: the enable went only through `claude plugin
        // uninstall`, whose failure was ignored.
        let base = scratch("claude-uninstall-alone");
        let failing = |_: &str, _: &[&str], _: Option<&Path>| -> Result<String> {
            anyhow::bail!("no claude")
        };
        let cx = context(&base, &no_vars, &failing);
        let project = base.join("p");
        let settings = project.join(".claude/settings.local.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(
            &settings,
            format!(
                r#"{{"enabledPlugins":{{"{PLUGIN}":true}},"permissions":{{"allow":["{FLAG_TOOL}"]}},"model":"x"}}"#
            ),
        )
        .unwrap();
        Claude.uninstall(&cx, &Target::Project(project)).unwrap();
        let doc = read_object(&settings).unwrap();
        assert_eq!(
            serde_json::Value::Object(doc),
            serde_json::json!({ "model": "x" })
        );
    }

    #[test]
    fn the_user_scope_follows_claude_config_dir() {
        let base = scratch("claude-config-dir");
        let dir = base.join("cfg");
        let var = |k: &str| (k == "CLAUDE_CONFIG_DIR").then(|| dir.clone().into_os_string());
        let run = |_: &str, _: &[&str], _: Option<&Path>| Ok(String::new());
        let cx = context(&base, &var, &run);
        assert_eq!(
            settings(&cx, &Target::User).unwrap(),
            dir.join("settings.json")
        );
    }

    #[test]
    fn what_loads_is_read_from_claudes_own_list() {
        let folder = Path::new("/d/claude/thirdeye");
        let entry = |extra: &str| {
            format!(
                r#"[{{"id":"thirdeye@thirdeye","scope":"local","enabled":true,"version":"0.1.0+1"{extra}}}]"#
            )
        };
        let live = entry(r#","readFromFolder":"/d/claude/thirdeye""#);
        assert_eq!(
            loaded(&live, "local", folder, "0.2.0+2", "R"),
            AgentSays::Loaded
        );
        let checkout = entry(r#","readFromFolder":"/src/thirdeye/agents/claude""#);
        assert!(matches!(
            loaded(&checkout, "local", folder, "0.2.0+2", "R"),
            AgentSays::NotLoaded { .. }
        ));
        // An older Claude Code: what runs is the cache, current by version.
        assert_eq!(
            loaded(&entry(""), "local", folder, "0.1.0+1", "R"),
            AgentSays::Loaded
        );
        assert!(matches!(
            loaded(&entry(""), "local", folder, "0.2.0+2", "R"),
            AgentSays::NotLoaded { .. }
        ));
        assert!(matches!(
            loaded("[]", "local", folder, "x", "R"),
            AgentSays::NotLoaded { .. }
        ));
    }

    #[test]
    fn enabled_with_nothing_of_ours_to_load_is_reported() {
        // An install from before thirdeye's directory, whose marketplace was
        // a checkout: enabled, and loading nothing.
        let base = scratch("claude-legacy");
        let run = |_: &str, _: &[&str], _: Option<&Path>| Ok(String::new());
        let cx = context(&base, &no_vars, &run);
        let project = base.join("p");
        let settings = project.join(".claude/settings.local.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(
            &settings,
            r#"{"enabledPlugins":{"thirdeye@thirdeye":true}}"#,
        )
        .unwrap();
        let found = Claude.inspect(&cx, &Target::Project(project)).unwrap();
        assert!(
            matches!(found.agent_says, AgentSays::NotLoaded { ref fix, .. } if fix.contains("install -a claude"))
        );
    }
}
