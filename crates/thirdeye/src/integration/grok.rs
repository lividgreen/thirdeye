//! Grok Build.
//!
//! Grok loads hooks, MCP servers and skills from plugins, which it installs
//! per user and **copies** (`grok plugin install <path>`; verified against
//! 1.0.50 — a directory merely placed in its `plugins/` was not loaded). So
//! the plugin's source lives in thirdeye's directory and Grok installs it
//! from there. A copy does not follow its source, and `grok plugin update`
//! does not help: on a local plugin it answers "local symlink, already live"
//! and leaves the copy as it was (verified against 1.0.50). So a refresh
//! removes the copy and has Grok take a new one — once, for the user.
//! Facts: `docs/grok-hook-payload.md`.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde_json::json;

use super::common::{shell_quote, user_only};
use super::tree::{self, File};
use super::{AgentSays, Context, Inspection, Integration, Scope, Target};

const MANIFEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/agents/grok/thirdeye/plugin.json"
));
const SKILL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/agents/grok/thirdeye/skills/flagging-files/SKILL.md"
));

/// The rule that lets the model flag without Grok asking first.
const ALLOW_FLAG: &str = "MCPTool(thirdeye__flag)";

/// The plugin's name, as its manifest gives it and `[plugins] enabled`
/// lists it.
const PLUGIN: &str = "thirdeye";

pub struct Grok;

impl Integration for Grok {
    fn id(&self) -> &'static str {
        "grok"
    }

    fn program(&self) -> &'static str {
        "grok"
    }

    fn scopes(&self) -> &'static [Scope] {
        &[Scope::User]
    }

    fn config_var(&self) -> Option<&'static str> {
        Some("GROK_HOME")
    }

    fn publish(&self, cx: &Context) -> Result<()> {
        tree::publish(&source(cx), &files(&cx.binary))
    }

    fn install(&self, cx: &Context, target: &Target) -> Result<()> {
        user_only("Grok", target)?;
        let config = config_path(cx).context("$HOME is not set")?;
        self.publish(cx)?;
        take_a_copy(cx)?;
        // Last: a config that will not parse fails the install loudly, with
        // the plugin in place and only the asking left to live with.
        edit_config(&config, with_flag_allowed)
    }

    fn uninstall(&self, cx: &Context, target: &Target) -> Result<()> {
        user_only("Grok", target)?;
        let grok = |args: &[&str]| (cx.run)("grok", args, None);
        remove_installed(&source(cx), &grok);
        let config = config_path(cx).context("$HOME is not set")?;
        edit_config(&config, without_flag_allowed)?;
        // Grok's own install adds the plugin to `[plugins] enabled` and its
        // uninstall leaves it there (1.0.50).
        edit_config(&config, without_plugin_enabled)?;
        // Not the published source: a Grok with another GROK_HOME may have
        // been installed from it too. `self uninstall` removes it.
        Ok(())
    }

    fn refresh(&self, cx: &Context) -> Result<()> {
        self.publish(cx)?;
        // Only a user who has it: a refresh must not install anything.
        let listed = (cx.run)("grok", &["plugin", "list"], None)
            .is_ok_and(|list| installed_name(&list, &source(cx)).is_some());
        if listed {
            take_a_copy(cx)?;
        }
        Ok(())
    }

    fn inspect(&self, cx: &Context, target: &Target) -> Option<Inspection> {
        if *target != Target::User {
            return None;
        }
        let root = source(cx);
        let manifest = std::fs::read_to_string(root.join("plugin.json")).ok()?;
        if !manifest.contains("installed by `thirdeye install grok`") {
            return None;
        }
        let mcp: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(root.join(".mcp.json")).ok()?).ok()?;
        let binary = PathBuf::from(mcp["mcpServers"]["thirdeye"]["command"].as_str()?);
        // Grok runs the copy, which only exists once installed — and
        // whether it is installed is Grok's to say: the source is shared
        // by every GROK_HOME, wired or not.
        let agent_says = match (cx.run)("grok", &["plugin", "list"], None) {
            Ok(list) if installed_name(&list, &root).is_some() => AgentSays::Loaded,
            Ok(_) => return None,
            Err(_) => AgentSays::Unknown(
                "`grok plugin list` could not be run to confirm it is installed".into(),
            ),
        };
        Some(Inspection {
            current: tree::matches(&root, &files(&binary)),
            binary,
            location: root,
            agent_says,
        })
    }

    fn next_steps(&self, _: &Target) -> String {
        "start a new grok session; the flag tool is allowed in Grok's config.toml so flags do \
         not ask"
            .into()
    }
}

/// `<data>/grok/thirdeye`: the plugin's source, named `thirdeye` because
/// Grok names an installed plugin after its directory.
fn source(cx: &Context) -> PathBuf {
    cx.data_for("grok").join("thirdeye")
}

/// Replace whatever copy Grok has with a new one of the source.
fn take_a_copy(cx: &Context) -> Result<()> {
    let grok = |args: &[&str]| (cx.run)("grok", args, None);
    let root = source(cx);
    remove_installed(&root, &grok);
    let text = root.display().to_string();
    grok(&["plugin", "install", &text, "--trust"])?;
    Ok(())
}

/// Grok's own config file: `$GROK_HOME/config.toml`, or `~/.grok/`.
fn config_path(cx: &Context) -> Option<PathBuf> {
    cx.path_var("GROK_HOME")
        .or_else(|| cx.home.as_ref().map(|h| h.join(".grok")))
        .map(|dir| dir.join("config.toml"))
}

/// `config` with the flag tool allowed: one entry in `[permission] allow`,
/// beside whatever is there. Edited in place rather than rewritten, so the
/// user's comments and layout survive.
fn with_flag_allowed(config: &str) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = config.parse().context("parsing Grok's config.toml")?;
    let allow = doc
        .entry("permission")
        .or_insert(toml_edit::table())
        .as_table_like_mut()
        .context("[permission] in Grok's config.toml is not a table")?
        .entry("allow")
        .or_insert(toml_edit::value(toml_edit::Array::new()))
        .as_array_mut()
        .context("permission.allow in Grok's config.toml is not a list")?;
    if !allow.iter().any(|v| v.as_str() == Some(ALLOW_FLAG)) {
        allow.push(ALLOW_FLAG);
    }
    Ok(doc.to_string())
}

/// `config` without the rule, and without a list or table it leaves empty.
fn without_flag_allowed(config: &str) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = config.parse().context("parsing Grok's config.toml")?;
    let Some(permission) = doc
        .get_mut("permission")
        .and_then(|p| p.as_table_like_mut())
    else {
        return Ok(config.to_string());
    };
    if let Some(allow) = permission.get_mut("allow").and_then(|a| a.as_array_mut()) {
        allow.retain(|v| v.as_str() != Some(ALLOW_FLAG));
        if allow.is_empty() {
            permission.remove("allow");
        }
    }
    if permission.is_empty() {
        doc.remove("permission");
    }
    Ok(doc.to_string())
}

/// `config` without the plugin in `[plugins] enabled`, and without a list
/// or table that leaves empty.
fn without_plugin_enabled(config: &str) -> Result<String> {
    let mut doc: toml_edit::DocumentMut = config.parse().context("parsing Grok's config.toml")?;
    let Some(plugins) = doc.get_mut("plugins").and_then(|p| p.as_table_like_mut()) else {
        return Ok(config.to_string());
    };
    if let Some(enabled) = plugins.get_mut("enabled").and_then(|a| a.as_array_mut()) {
        enabled.retain(|v| v.as_str() != Some(PLUGIN));
        if enabled.is_empty() {
            plugins.remove("enabled");
        }
    }
    if plugins.is_empty() {
        doc.remove("plugins");
    }
    Ok(doc.to_string())
}

/// Apply `change` to the config file, treating a missing one as empty. A
/// file that does not parse is left alone and reported: the result is
/// written back, and a config read as empty would be replaced by ours.
fn edit_config(config: &Path, change: impl Fn(&str) -> Result<String>) -> Result<()> {
    let before = match std::fs::read_to_string(config) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", config.display())),
    };
    let after = change(&before)?;
    if after != before {
        if let Some(parent) = config.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(config, after).with_context(|| format!("writing {}", config.display()))?;
    }
    Ok(())
}

/// Grok's shell tool, under both names it goes by.
const SHELL: &str = "run_terminal_command|run_terminal_cmd";

/// Every tool whose result thirdeye reads.
const READ: &str =
    "read_file|search_replace|write|grep|list_dir|run_terminal_command|run_terminal_cmd";

/// `(event, matcher)`. Tool events are narrowed to the tools thirdeye reads:
/// Grok's hooks are synchronous, so a hook it need not run is time the agent
/// gets back. A matcher on the turn and session events is ignored by Grok.
const EVENTS: &[(&str, Option<&str>)] = &[
    ("UserPromptSubmit", None),
    ("PreToolUse", Some(SHELL)),
    ("PostToolUse", Some(READ)),
    ("PostToolUseFailure", Some(SHELL)),
    ("Stop", None),
    ("StopFailure", None),
    ("StopCancelled", None),
    ("SessionEnd", None),
];

fn files(binary: &Path) -> Vec<File> {
    vec![
        File::new("plugin.json", MANIFEST),
        File::new("hooks/hooks.json", hooks(binary)),
        File::new(".mcp.json", mcp(binary)),
        File::new("skills/flagging-files/SKILL.md", SKILL),
    ]
}

/// **Nothing may reach stdout:** Grok reads a hook's stdout as a decision on
/// `PreToolUse` and `Stop`, and as feedback for the model on `PostToolUse`.
/// Stderr goes where the Claude plugin's does; the exit is always 0.
fn hooks(binary: &Path) -> String {
    let bin = shell_quote(binary);
    let command = format!(
        r#"d="${{XDG_STATE_HOME:-$HOME/.local/state}}/thirdeye"; mkdir -p "$d" 2>/dev/null; {bin} hook --agent grok >/dev/null 2>>"$d/hook-errors.log"; exit 0"#
    );
    let mut events = serde_json::Map::new();
    for (event, matcher) in EVENTS {
        let mut group = serde_json::Map::new();
        if let Some(matcher) = matcher {
            group.insert("matcher".into(), json!(matcher));
        }
        group.insert(
            "hooks".into(),
            json!([{ "type": "command", "command": command, "timeout": 10 }]),
        );
        events.insert(event.to_string(), json!([group]));
    }
    serde_json::to_string_pretty(&json!({ "hooks": events })).expect("a JSON value serializes")
        + "\n"
}

/// The binary itself: an MCP server's stdout is the protocol.
fn mcp(binary: &Path) -> String {
    let doc = json!({
        "mcpServers": {
            "thirdeye": {
                "command": binary.display().to_string(),
                "args": ["mcp", "--agent", "grok"],
            }
        }
    });
    serde_json::to_string_pretty(&doc).expect("a JSON value serializes") + "\n"
}

/// The name of the plugin Grok installed from `source`, read from `grok
/// plugin list`, whose lines look like `  <id>: <name> [local: <path>]`.
/// The name, not the id before the colon: `grok plugin uninstall` takes the
/// name and answers "not found" to the id (verified against 1.0.50).
fn installed_name(list: &str, source: &Path) -> Option<String> {
    let marker = format!("[local: {}]", source.display());
    let line = list.lines().find(|l| l.contains(&marker))?;
    let (_, rest) = line.split_once(": ")?;
    let name = rest.split(" [").next()?.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Uninstall whatever Grok has installed from `root`. Best-effort: nothing
/// installed is the usual case on a first run. Repeated, because Grok
/// removes one copy per call and an earlier install can have left another
/// under the same name; bounded, because a refusal must not spin.
fn remove_installed(root: &Path, grok: &dyn Fn(&[&str]) -> Result<String>) {
    for _ in 0..4 {
        let Some(name) = grok(&["plugin", "list"])
            .ok()
            .and_then(|list| installed_name(&list, root))
        else {
            return;
        };
        if grok(&["plugin", "uninstall", &name]).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::{Calls, context, no_vars, scratch, snapshot};
    use super::*;
    use std::cell::Cell;
    use std::ffi::OsString;

    /// A `grok` that lists one copy of `source` until it is uninstalled.
    fn grok_with_copy<'a>(
        calls: &'a Calls,
        source: PathBuf,
        installed: &'a Cell<bool>,
    ) -> impl Fn(&str, &[&str], Option<&Path>) -> Result<String> + 'a {
        move |p, args, _| {
            calls.record(p, args);
            Ok(match args {
                ["plugin", "list"] if installed.get() => {
                    format!("  thirdeye-3a14: thirdeye [local: {}]\n", source.display())
                }
                ["plugin", "uninstall", _] => {
                    installed.set(false);
                    String::new()
                }
                ["plugin", "install", ..] => {
                    installed.set(true);
                    String::new()
                }
                _ => String::new(),
            })
        }
    }

    #[test]
    fn install_replaces_the_copy_grok_already_has() {
        // Grok copies a plugin; reinstalling without removing the copy
        // would leave the old one answering. It is uninstalled by name.
        let base = scratch("grok-install");
        let calls = Calls::default();
        let installed = Cell::new(true);
        let src = base.join("data").join("grok").join("thirdeye");
        let run = grok_with_copy(&calls, src.clone(), &installed);
        let cx = context(&base, &no_vars, &run);
        Grok.install(&cx, &Target::User).unwrap();
        assert!(src.join("hooks/hooks.json").is_file());
        assert_eq!(
            calls.take(),
            [
                "grok plugin list".to_string(),
                "grok plugin uninstall thirdeye".to_string(),
                "grok plugin list".to_string(),
                format!("grok plugin install {} --trust", src.display()),
            ]
        );
        let config = std::fs::read_to_string(base.join("home/.grok/config.toml")).unwrap();
        assert!(config.contains(ALLOW_FLAG), "{config}");
    }

    #[test]
    fn a_refresh_takes_a_new_copy_only_for_a_user_who_has_one() {
        // `grok plugin update` leaves a local copy stale, so a refresh
        // reinstalls; and installing for someone who never did is not a
        // refresh's business.
        let base = scratch("grok-refresh");
        let calls = Calls::default();
        let installed = Cell::new(false);
        let src = base.join("data").join("grok").join("thirdeye");
        let run = grok_with_copy(&calls, src.clone(), &installed);
        let cx = context(&base, &no_vars, &run);
        Grok.refresh(&cx).unwrap();
        assert_eq!(calls.take(), ["grok plugin list"]);

        installed.set(true);
        Grok.refresh(&cx).unwrap();
        assert!(
            calls
                .take()
                .contains(&format!("grok plugin install {} --trust", src.display()))
        );
    }

    #[test]
    fn grok_home_moves_the_config_it_edits() {
        let base = scratch("grok-home");
        let grok_home = base.join("gh");
        let var = |k: &str| (k == "GROK_HOME").then(|| OsString::from(&grok_home));
        let run = |_: &str, _: &[&str], _: Option<&Path>| Ok(String::new());
        let cx = context(&base, &var, &run);
        Grok.install(&cx, &Target::User).unwrap();
        assert!(grok_home.join("config.toml").is_file());
    }

    #[test]
    fn a_config_that_does_not_parse_is_reported_and_left_alone() {
        let base = scratch("grok-badconfig");
        let run = |_: &str, _: &[&str], _: Option<&Path>| Ok(String::new());
        let cx = context(&base, &no_vars, &run);
        let config = base.join("home/.grok/config.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "[ui\nyolo = ").unwrap();
        assert!(Grok.install(&cx, &Target::User).is_err());
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "[ui\nyolo = ");
    }

    #[test]
    fn doctor_sees_the_binary_and_notices_drift() {
        let base = scratch("grok-inspect");
        let calls = Calls::default();
        let installed = Cell::new(false);
        let run = grok_with_copy(
            &calls,
            base.join("data").join("grok").join("thirdeye"),
            &installed,
        );
        let cx = context(&base, &no_vars, &run);
        Grok.install(&cx, &Target::User).unwrap();
        let found = Grok.inspect(&cx, &Target::User).unwrap();
        assert_eq!(found.binary, cx.binary);
        assert!(found.current);
        assert_eq!(found.agent_says, AgentSays::Loaded);
        std::fs::write(found.location.join("hooks/hooks.json"), "{}").unwrap();
        assert!(!Grok.inspect(&cx, &Target::User).unwrap().current);
    }

    #[test]
    fn a_project_target_is_refused_and_uninstall_leaves_the_shared_source() {
        // Every GROK_HOME installs from the one published source, so
        // unwiring one must not delete what the others were copied from.
        let base = scratch("grok-theirs");
        let run = |_: &str, _: &[&str], _: Option<&Path>| Ok(String::new());
        let cx = context(&base, &no_vars, &run);
        assert!(Grok.install(&cx, &Target::Project("/p".into())).is_err());
        Grok.install(&cx, &Target::User).unwrap();
        let before = snapshot(&source(&cx));
        Grok.uninstall(&cx, &Target::User).unwrap();
        assert_eq!(snapshot(&source(&cx)), before);
        // And Grok not listing it is not installed, whatever is published.
        assert!(Grok.inspect(&cx, &Target::User).is_none());
    }

    #[test]
    fn a_copy_that_will_not_go_does_not_spin_the_installer() {
        let r = PathBuf::from("/d/grok/thirdeye");
        let calls = Cell::new(0);
        let listed = format!("  thirdeye-3a14: thirdeye [local: {}]\n", r.display());
        remove_installed(&r, &|_: &[&str]| {
            calls.set(calls.get() + 1);
            Ok(listed.clone())
        });
        assert!(calls.get() <= 8, "bounded: {}", calls.get());
    }

    #[test]
    fn the_plugin_is_named_as_uninstall_wants_it() {
        // Captured: `thirdeye-b8012f88: thirdeye [local: …]`, and uninstall
        // of `thirdeye-b8012f88` answers "not found".
        let list = "  other-1: other [local: /elsewhere]\n  \
                    thirdeye-b8012f88: thirdeye [local: /d/thirdeye/grok/thirdeye]\n";
        assert_eq!(
            installed_name(list, Path::new("/d/thirdeye/grok/thirdeye")).as_deref(),
            Some("thirdeye")
        );
        assert_eq!(
            installed_name("No plugins installed.", Path::new("/d")),
            None
        );
    }

    #[test]
    fn no_hook_writes_to_stdout_and_tool_hooks_are_narrowed() {
        let doc: serde_json::Value =
            serde_json::from_str(&hooks(Path::new("/opt/it's/thirdeye"))).unwrap();
        for (event, matcher) in EVENTS {
            let group = &doc["hooks"][event][0];
            let command = group["hooks"][0]["command"].as_str().unwrap();
            assert!(command.contains(r"'/opt/it'\''s/thirdeye' hook --agent grok >/dev/null"));
            assert!(command.ends_with("exit 0"));
            assert_eq!(group["matcher"].as_str(), *matcher, "{event}");
        }
        assert_eq!(doc["hooks"]["PreToolUse"][0]["matcher"], SHELL);
    }

    #[test]
    fn the_flag_is_allowed_beside_the_users_own_rules_and_comments() {
        let mine = "# my grok\n[ui]\nyolo = false\n\n[permission]\n# things I trust\nallow = [\"Bash(git *)\"]\n";
        let allowed = with_flag_allowed(mine).unwrap();
        assert!(allowed.contains("# my grok") && allowed.contains("# things I trust"));
        assert!(
            allowed.contains(r#"allow = ["Bash(git *)", "MCPTool(thirdeye__flag)"]"#),
            "{allowed}"
        );
        assert_eq!(
            with_flag_allowed(&allowed).unwrap(),
            allowed,
            "once is enough"
        );
        assert_eq!(
            without_flag_allowed(&allowed).unwrap(),
            mine,
            "and it comes out clean"
        );
    }

    #[test]
    fn a_config_with_no_permissions_gains_and_loses_exactly_one_table() {
        let mine = "[ui]\nyolo = false\n";
        let allowed = with_flag_allowed(mine).unwrap();
        assert!(allowed.contains("[permission]") && allowed.contains(ALLOW_FLAG));
        assert_eq!(without_flag_allowed(&allowed).unwrap(), mine);
    }

    #[test]
    fn uninstall_takes_out_the_enable_grok_leaves_behind() {
        let config = "[plugins]\nenabled = [\"other\", \"thirdeye\"]\n";
        let after = without_plugin_enabled(config).unwrap();
        assert!(
            after.contains("\"other\"") && !after.contains("thirdeye"),
            "{after}"
        );
        let only =
            without_plugin_enabled("x = 1\n\n[plugins]\nenabled = [\"thirdeye\"]\n").unwrap();
        assert_eq!(only.trim(), "x = 1");
        assert_eq!(without_plugin_enabled("x = 1\n").unwrap(), "x = 1\n");
    }
}
