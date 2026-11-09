//! GitHub Copilot CLI.
//!
//! Copilot loads hooks, MCP servers and skills from plugins, and installs
//! plugins per user only. A plugin in a *local marketplace* is loaded live
//! from its own directory rather than copied (verified against 1.0.94: "It is
//! loaded live … nothing was copied"), so the marketplace lives in thirdeye's
//! directory and an update needs nothing beyond rewriting it. Install
//! registers it through the `copilot` CLI, once.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::common::{shell_quote, user_only};
use super::tree::{self, File};
use super::{AgentSays, Context, Inspection, Integration, Scope, Target};

const MARKETPLACE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/agents/copilot/.github/plugin/marketplace.json"
));
const MANIFEST: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/agents/copilot/thirdeye/plugin.json"
));
const SKILL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/agents/copilot/thirdeye/skills/flagging-files/SKILL.md"
));

/// What Copilot calls the plugin, in its marketplace.
const PLUGIN: &str = "thirdeye@thirdeye";

/// The events thirdeye reads. Copilot's payloads do not name their event,
/// so each is registered with its name on the command line. `sessionStart`
/// is left out: it arrives after the prompt and carries nothing a turn
/// needs.
const EVENTS: &[&str] = &[
    "userPromptSubmitted",
    "preToolUse",
    "postToolUse",
    "postToolUseFailure",
    "agentStop",
    "sessionEnd",
];

pub struct Copilot;

impl Integration for Copilot {
    fn id(&self) -> &'static str {
        "copilot"
    }

    fn program(&self) -> &'static str {
        "copilot"
    }

    fn scopes(&self) -> &'static [Scope] {
        // A repository can switch a plugin off, but not install one.
        &[Scope::User]
    }

    fn config_var(&self) -> Option<&'static str> {
        Some("COPILOT_HOME")
    }

    fn publish(&self, cx: &Context) -> Result<()> {
        tree::publish(&cx.data_for(self.id()), &files(&cx.binary))
    }

    fn install(&self, cx: &Context, target: &Target) -> Result<()> {
        user_only("Copilot", target)?;
        self.publish(cx)?;
        let root = cx.data_for(self.id());
        let copilot = |args: &[&str]| (cx.run)("copilot", args, None);
        // A previous install may be registered already, and re-adding a
        // marketplace of the same name is refused. Neither removal matters
        // if nothing was there.
        let _ = copilot(&["plugin", "uninstall", PLUGIN]);
        let _ = copilot(&["plugin", "marketplace", "remove", "thirdeye"]);
        let root_text = root.display().to_string();
        copilot(&["plugin", "marketplace", "add", &root_text])?;
        copilot(&["plugin", "install", PLUGIN])?;
        Ok(())
    }

    fn uninstall(&self, cx: &Context, target: &Target) -> Result<()> {
        user_only("Copilot", target)?;
        let copilot = |args: &[&str]| (cx.run)("copilot", args, None);
        let _ = copilot(&["plugin", "uninstall", PLUGIN]);
        let _ = copilot(&["plugin", "marketplace", "remove", "thirdeye"]);
        // Not the published marketplace: every Copilot configuration
        // directory loads the same one, and another may still be wired to
        // it. `self uninstall` removes it with the rest.
        Ok(())
    }

    fn inspect(&self, cx: &Context, target: &Target) -> Option<Inspection> {
        if *target != Target::User {
            return None;
        }
        let root = cx.data_for(self.id());
        let binary = installed_binary(&root)?;
        // Whether it is installed is Copilot's to say: the published files
        // are shared by every configuration directory, wired or not.
        let agent_says = match (cx.run)("copilot", &["plugin", "list"], None) {
            Ok(list) => match registration(&list) {
                Registration::Enabled => AgentSays::Loaded,
                Registration::Disabled => AgentSays::NotLoaded {
                    detail: "Copilot has the plugin disabled, so no hook runs".into(),
                    fix: "copilot plugin enable thirdeye@thirdeye".into(),
                },
                Registration::Missing => return None,
            },
            Err(_) => AgentSays::Unknown(
                "`copilot plugin list` could not be run to confirm it is enabled".into(),
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
        "start a new copilot session: plugins load at session start".into()
    }
}

/// Every file of the plugin, relative to the marketplace root.
fn files(binary: &Path) -> Vec<File> {
    vec![
        File::new(".github/plugin/marketplace.json", MARKETPLACE),
        File::new("thirdeye/plugin.json", MANIFEST),
        File::new("thirdeye/hooks.json", hooks(binary)),
        File::new("thirdeye/.mcp.json", mcp(binary)),
        File::new("thirdeye/skills/flagging-files/SKILL.md", SKILL),
    ]
}

/// Each event runs the hook with its own name. Synchronous — Copilot has no
/// other kind — so the hook is as quick as for every agent. Its stderr goes
/// where the Claude plugin's does, and the exit is always 0: a broken viewer
/// must never fail the agent's tool call.
fn hooks(binary: &Path) -> String {
    let bin = shell_quote(binary);
    let handlers: serde_json::Map<String, serde_json::Value> = EVENTS
        .iter()
        .map(|event| {
            let command = format!(
                r#"d="${{XDG_STATE_HOME:-$HOME/.local/state}}/thirdeye"; mkdir -p "$d" 2>/dev/null; {bin} hook --agent copilot --event {event} 2>>"$d/hook-errors.log"; exit 0"#
            );
            let handler = serde_json::json!([{ "type": "command", "bash": command, "timeoutSec": 10 }]);
            (event.to_string(), handler)
        })
        .collect();
    let doc = serde_json::json!({ "version": 1, "hooks": handlers });
    serde_json::to_string_pretty(&doc).expect("a JSON value serializes") + "\n"
}

/// The binary itself, never a wrapper: an MCP server's stdout is the
/// protocol, and nothing may write to it but the server.
fn mcp(binary: &Path) -> String {
    let doc = serde_json::json!({
        "mcpServers": {
            "thirdeye": {
                "command": binary.display().to_string(),
                "args": ["mcp", "--agent", "copilot"],
            }
        }
    });
    serde_json::to_string_pretty(&doc).expect("a JSON value serializes") + "\n"
}

/// The binary a published marketplace runs, if one of ours is there.
fn installed_binary(root: &Path) -> Option<PathBuf> {
    let marketplace = std::fs::read_to_string(root.join(".github/plugin/marketplace.json")).ok()?;
    if !marketplace.contains("installed by `thirdeye install copilot`") {
        return None;
    }
    let mcp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("thirdeye/.mcp.json")).ok()?)
            .ok()?;
    Some(PathBuf::from(
        mcp["mcpServers"]["thirdeye"]["command"].as_str()?,
    ))
}

/// What Copilot itself says about the plugin.
#[derive(Debug, PartialEq, Eq)]
enum Registration {
    Enabled,
    Disabled,
    Missing,
}

/// Read `copilot plugin list`, whose lines look like
/// `• thirdeye@thirdeye (v0.1.0) (enabled)`.
fn registration(list: &str) -> Registration {
    match list.lines().find(|l| l.contains(PLUGIN)) {
        Some(line) if line.contains("(enabled)") => Registration::Enabled,
        Some(_) => Registration::Disabled,
        None => Registration::Missing,
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::{Calls, context, no_vars, scratch, snapshot};
    use super::*;

    #[test]
    fn install_publishes_a_marketplace_and_registers_it() {
        let base = scratch("copilot-install");
        let calls = Calls::default();
        let run = |p: &str, a: &[&str], _: Option<&Path>| {
            calls.record(p, a);
            Ok(String::new())
        };
        let cx = context(&base, &no_vars, &run);
        Copilot.install(&cx, &Target::User).unwrap();
        let root = cx.data_for("copilot");
        assert!(root.join(".github/plugin/marketplace.json").is_file());
        assert!(
            root.join("thirdeye/skills/flagging-files/SKILL.md")
                .is_file()
        );
        assert_eq!(
            calls.take()[2..],
            [
                format!("copilot plugin marketplace add {}", root.display()),
                "copilot plugin install thirdeye@thirdeye".to_string()
            ]
        );
    }

    #[test]
    fn a_refresh_rewrites_thirdeyes_directory_and_asks_copilot_nothing() {
        // Copilot loads the marketplace live, so a new version is a rewrite
        // of the files and nothing else.
        let base = scratch("copilot-refresh");
        let calls = Calls::default();
        let run = |p: &str, a: &[&str], _: Option<&Path>| {
            calls.record(p, a);
            Ok("  • thirdeye@thirdeye (v0.1.0) (enabled)\n".to_string())
        };
        let cx = context(&base, &no_vars, &run);
        Copilot.install(&cx, &Target::User).unwrap();
        calls.take();
        std::fs::write(cx.data_for("copilot").join("thirdeye/hooks.json"), "{}").unwrap();
        Copilot.refresh(&cx).unwrap();
        assert!(calls.take().is_empty());
        // Asked, here, by `inspect` alone.
        assert!(
            Copilot
                .inspect(&cx, &Target::User)
                .is_some_and(|found| found.current)
        );
    }

    #[test]
    fn every_hook_names_its_event_and_never_fails_the_agent() {
        let doc: serde_json::Value =
            serde_json::from_str(&hooks(Path::new("/opt/it's/thirdeye"))).unwrap();
        for event in EVENTS {
            let command = doc["hooks"][event][0]["bash"].as_str().unwrap();
            assert!(command.contains(&format!("--agent copilot --event {event} ")));
            assert!(
                command.contains(r"'/opt/it'\''s/thirdeye'"),
                "quoted: {command}"
            );
            assert!(command.ends_with("exit 0"));
        }
    }

    #[test]
    fn the_mcp_server_is_the_binary_itself() {
        let doc: serde_json::Value =
            serde_json::from_str(&mcp(Path::new("/opt/thirdeye"))).unwrap();
        let server = &doc["mcpServers"]["thirdeye"];
        assert_eq!(server["command"], "/opt/thirdeye");
        assert_eq!(
            server["args"],
            serde_json::json!(["mcp", "--agent", "copilot"])
        );
    }

    #[test]
    fn doctor_sees_the_binary_and_notices_drift() {
        let base = scratch("copilot-inspect");
        let run = |_: &str, _: &[&str], _: Option<&Path>| {
            Ok("  • thirdeye@thirdeye (v0.1.0) (enabled)\n".to_string())
        };
        let cx = context(&base, &no_vars, &run);
        Copilot.install(&cx, &Target::User).unwrap();
        let found = Copilot.inspect(&cx, &Target::User).unwrap();
        assert_eq!(found.binary, cx.binary);
        assert!(found.current);
        assert_eq!(found.agent_says, AgentSays::Loaded);
        std::fs::write(cx.data_for("copilot").join("thirdeye/hooks.json"), "{}").unwrap();
        assert!(!Copilot.inspect(&cx, &Target::User).unwrap().current);
    }

    #[test]
    fn copilot_saying_the_plugin_is_off_is_not_taken_for_installed() {
        let on = "Live Plugins (loaded from a local marketplace directory, never copied):\n  \
                  • thirdeye@thirdeye (v0.1.0) (enabled)\n      from /h/.local/share/thirdeye/copilot";
        assert_eq!(registration(on), Registration::Enabled);
        let off = on.replace("(enabled)", "(disabled)");
        assert_eq!(registration(&off), Registration::Disabled);
        assert_eq!(registration("No plugins installed."), Registration::Missing);
    }

    #[test]
    fn a_project_target_is_refused_with_the_reason() {
        let base = scratch("copilot-project");
        let run = |_: &str, _: &[&str], _: Option<&Path>| Ok(String::new());
        let cx = context(&base, &no_vars, &run);
        let err = Copilot
            .install(&cx, &Target::Project("/p".into()))
            .unwrap_err();
        assert!(err.to_string().contains("per user"), "{err}");
        assert!(!cx.data_for("copilot").exists(), "refused before writing");
    }

    #[test]
    fn uninstall_leaves_the_marketplace_another_copilot_home_may_load() {
        // Found by review: every COPILOT_HOME loads the one published
        // marketplace, so unwiring one deleted what the others ran.
        let base = scratch("copilot-uninstall");
        let calls = Calls::default();
        let run = |p: &str, a: &[&str], _: Option<&Path>| {
            calls.record(p, a);
            Ok(String::new())
        };
        let cx = context(&base, &no_vars, &run);
        Copilot.install(&cx, &Target::User).unwrap();
        let before = snapshot(&cx.data_for("copilot"));
        calls.take();
        Copilot.uninstall(&cx, &Target::User).unwrap();
        assert_eq!(snapshot(&cx.data_for("copilot")), before);
        assert!(calls.take().iter().any(|c| c.contains("plugin uninstall")));
        // And Copilot not listing it is not installed, whatever is published.
        assert!(Copilot.inspect(&cx, &Target::User).is_none());
    }
}
