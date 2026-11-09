//! The Antigravity CLI (`agy`).
//!
//! Antigravity reads hooks, MCP servers and skills from files a user may
//! share with other tools, so this merges rather than overwrites: one key of
//! its own in `hooks.json` (whose top level is keyed by hook name for exactly
//! this), one server in `mcp_config.json`, one skill directory. Facts:
//! `docs/antigravity-hook-payload.md`.
//!
//! MCP tools ask before every call, and the rule that stops it lives only in
//! the user's `settings.json`, so both scopes add `mcp(thirdeye/flag)` there.
//! Only a user uninstall removes it: a project uninstall cannot know whether
//! another project still relies on it.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde_json::{Value, json};

use super::common::{read_object, shell_quote, write_object};
use super::tree::{self, File};
use super::{AgentSays, Context, Inspection, Integration, Scope, Target, skills};

const SKILL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/agents/antigravity/skills/flagging-files/SKILL.md"
));

/// The key thirdeye owns in `hooks.json`, and its MCP server's name.
const NAME: &str = "thirdeye";

/// The permission that lets the model flag without asking each time.
const ALLOW_FLAG: &str = "mcp(thirdeye/flag)";

/// `PostInvocation` is left out: it repeats `PreInvocation`'s fields.
const EVENTS: &[&str] = &["PreInvocation", "PreToolUse", "PostToolUse", "Stop"];

/// Where each piece goes for one scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Targets {
    pub hooks: PathBuf,
    pub mcp: PathBuf,
    /// The directory holding skill folders.
    pub skills: PathBuf,
    /// The user's settings, whatever the scope: permissions live nowhere else.
    pub settings: PathBuf,
    pub user: bool,
    /// The skill as thirdeye's directory holds it.
    pub skill_source: PathBuf,
}

pub struct Antigravity;

impl Integration for Antigravity {
    fn id(&self) -> &'static str {
        "antigravity"
    }

    fn program(&self) -> &'static str {
        "agy"
    }

    fn scopes(&self) -> &'static [Scope] {
        &[Scope::Project, Scope::User]
    }

    fn publish(&self, cx: &Context) -> Result<()> {
        tree::publish(
            &cx.data_for(self.id()),
            &[File::new(
                format!("skills/{}/SKILL.md", skills::NAME),
                SKILL,
            )],
        )
    }

    fn install(&self, cx: &Context, target: &Target) -> Result<()> {
        let t = targets_for(cx, target)?;
        self.publish(cx)?;
        install(&t, &cx.binary)
    }

    fn uninstall(&self, cx: &Context, target: &Target) -> Result<()> {
        uninstall(&targets_for(cx, target)?)
    }

    fn inspect(&self, cx: &Context, target: &Target) -> Option<Inspection> {
        inspect(&targets_for(cx, target).ok()?)
    }

    fn next_steps(&self, _: &Target) -> String {
        "start a new agy session; `mcp(thirdeye/flag)` is allowed in your Antigravity settings \
         so flags do not ask"
            .into()
    }
}

fn targets_for(cx: &Context, target: &Target) -> Result<Targets> {
    let project = match target {
        Target::Project(dir) => Some(dir.clone()),
        Target::User => None,
    };
    targets(
        project,
        cx.home.clone(),
        cx.data_for("antigravity").join("skills").join(skills::NAME),
    )
}

fn targets(
    project: Option<PathBuf>,
    home: Option<PathBuf>,
    skill_source: PathBuf,
) -> Result<Targets> {
    let home = home.context("$HOME is not set")?;
    let settings = home.join(".gemini/antigravity-cli/settings.json");
    match project {
        Some(project) => {
            let agents = project.join(".agents");
            Ok(Targets {
                hooks: agents.join("hooks.json"),
                mcp: agents.join("mcp_config.json"),
                skills: agents.join("skills"),
                settings,
                user: false,
                skill_source,
            })
        }
        None => Ok(Targets {
            hooks: home.join(".gemini/config/hooks.json"),
            mcp: home.join(".gemini/config/mcp_config.json"),
            skills: home.join(".gemini/antigravity-cli/skills"),
            settings,
            user: true,
            skill_source,
        }),
    }
}

/// Each event runs the hook with its name. **Nothing may reach stdout:** a
/// `PreToolUse` hook that prints an object without a `decision` denies the
/// call. Stderr goes where the Claude plugin's does; the exit is always 0.
fn hooks(binary: &Path) -> Value {
    let bin = shell_quote(binary);
    let mut events = serde_json::Map::new();
    for event in EVENTS {
        let command = format!(
            r#"d="${{XDG_STATE_HOME:-$HOME/.local/state}}/thirdeye"; mkdir -p "$d" 2>/dev/null; {bin} hook --agent antigravity --event {event} >/dev/null 2>>"$d/hook-errors.log"; exit 0"#
        );
        let handler = json!({ "type": "command", "command": command, "timeout": 10 });
        // Tool events take matcher groups; the others take handlers directly.
        let entry = if event.ends_with("ToolUse") {
            json!([{ "matcher": "*", "hooks": [handler] }])
        } else {
            json!([handler])
        };
        events.insert(event.to_string(), entry);
    }
    Value::Object(events)
}

/// The binary itself: an MCP server's stdout is the protocol.
fn mcp_server(binary: &Path) -> Value {
    json!({ "command": binary.display().to_string(), "args": ["mcp", "--agent", "antigravity"] })
}

fn install(t: &Targets, binary: &Path) -> Result<()> {
    // Checked before anything is written: a refusal must leave nothing half
    // installed.
    skills::check(&t.skills, &t.skill_source, skill_is_ours)?;

    let mut hooks_doc = read_object(&t.hooks)?;
    hooks_doc.insert(NAME.into(), hooks(binary));
    write_object(&t.hooks, &hooks_doc)?;

    let mut mcp_doc = read_object(&t.mcp)?;
    let servers = mcp_doc
        .entry("mcpServers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .with_context(|| format!("mcpServers in {} is not an object", t.mcp.display()))?;
    servers.insert(NAME.into(), mcp_server(binary));
    write_object(&t.mcp, &mcp_doc)?;

    skills::place(&t.skills, &t.skill_source, skill_is_ours)?;

    let mut settings = read_object(&t.settings)?;
    let allow = settings
        .entry("permissions")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("permissions in settings.json is not an object")?
        .entry("allow")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .context("permissions.allow in settings.json is not a list")?;
    if !allow.iter().any(|v| v == ALLOW_FLAG) {
        allow.push(json!(ALLOW_FLAG));
    }
    write_object(&t.settings, &settings)
}

fn uninstall(t: &Targets) -> Result<()> {
    let mut hooks_doc = read_object(&t.hooks)?;
    if hooks_doc.remove(NAME).is_some() {
        write_or_remove(&t.hooks, &hooks_doc)?;
    }

    let mut mcp_doc = read_object(&t.mcp)?;
    let removed = mcp_doc
        .get_mut("mcpServers")
        .and_then(Value::as_object_mut)
        .and_then(|servers| servers.remove(NAME))
        .is_some();
    if removed {
        if mcp_doc["mcpServers"]
            .as_object()
            .is_some_and(|s| s.is_empty())
        {
            mcp_doc.remove("mcpServers");
        }
        write_or_remove(&t.mcp, &mcp_doc)?;
    }

    skills::remove(&t.skills, &t.skill_source, skill_is_ours)?;
    // `remove_dir` removes only empty directories: anything of the user's
    // keeps it.
    if !t.user
        && let Some(agents) = t.hooks.parent()
    {
        let _ = std::fs::remove_dir(agents);
    }

    if t.user {
        let mut settings = read_object(&t.settings)?;
        if let Some(allow) = settings
            .get_mut("permissions")
            .and_then(|p| p.get_mut("allow"))
            .and_then(Value::as_array_mut)
        {
            let before = allow.len();
            allow.retain(|v| v != ALLOW_FLAG);
            if allow.len() != before {
                write_object(&t.settings, &settings)?;
            }
        }
    }
    Ok(())
}

/// What is installed for `t`, for `doctor`.
fn inspect(t: &Targets) -> Option<Inspection> {
    let hooks_doc = read_object(&t.hooks).ok()?;
    let ours = hooks_doc.get(NAME)?;
    let mcp_doc = read_object(&t.mcp).ok()?;
    let server = &mcp_doc["mcpServers"][NAME];
    let binary = PathBuf::from(server["command"].as_str()?);
    Some(Inspection {
        current: *ours == hooks(&binary)
            && *server == mcp_server(&binary)
            && skills::current(&t.skills, &t.skill_source, SKILL),
        binary,
        location: t.hooks.clone(),
        agent_says: AgentSays::Loaded,
    })
}

fn skill_is_ours(text: &str) -> bool {
    text.contains("**thirdeye**")
}

/// Write `doc` back, or remove the file it came from if nothing is left.
fn write_or_remove(path: &Path, doc: &serde_json::Map<String, Value>) -> Result<()> {
    if doc.is_empty() {
        std::fs::remove_file(path).with_context(|| format!("removing {}", path.display()))
    } else {
        write_object(path, doc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real `targets`, with the skill published beside the scratch home.
    fn targets(project: Option<PathBuf>, user: bool, home: Option<PathBuf>) -> Result<Targets> {
        let home = home.unwrap();
        let published = home.parent().unwrap().join("data/skills/flagging-files");
        std::fs::create_dir_all(&published).unwrap();
        std::fs::write(published.join("SKILL.md"), SKILL).unwrap();
        assert_eq!(project.is_none(), user);
        super::targets(project, Some(home), published)
    }

    fn scratch(tag: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("thirdeye-agy-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        (base.join("project"), base.join("home"))
    }

    #[test]
    fn a_project_install_merges_beside_what_is_there_and_leaves_it_on_uninstall() {
        let (project, home) = scratch("merge");
        let t = targets(Some(project.clone()), false, Some(home)).unwrap();
        write_object(
            &t.hooks,
            &json!({ "linter": { "Stop": [] } })
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
        write_object(
            &t.mcp,
            &json!({ "mcpServers": { "db": { "command": "db" } } })
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();

        install(&t, Path::new("/opt/thirdeye")).unwrap();
        let hooks_doc = read_object(&t.hooks).unwrap();
        assert!(hooks_doc.contains_key("linter") && hooks_doc.contains_key("thirdeye"));
        let mcp_doc = read_object(&t.mcp).unwrap();
        assert_eq!(
            mcp_doc["mcpServers"]["thirdeye"]["command"],
            "/opt/thirdeye"
        );
        assert!(
            project
                .join(".agents/skills/flagging-files/SKILL.md")
                .is_file()
        );
        let settings = read_object(&t.settings).unwrap();
        assert_eq!(settings["permissions"]["allow"], json!([ALLOW_FLAG]));

        uninstall(&t).unwrap();
        assert_eq!(
            read_object(&t.hooks).unwrap().keys().collect::<Vec<_>>(),
            ["linter"]
        );
        assert!(
            read_object(&t.mcp).unwrap()["mcpServers"]
                .get("thirdeye")
                .is_none()
        );
        assert!(!project.join(".agents/skills").exists());
        assert_eq!(
            read_object(&t.settings).unwrap()["permissions"]["allow"],
            json!([ALLOW_FLAG]),
            "another project may still rely on it"
        );
    }

    #[test]
    fn an_install_leaves_the_users_keys_in_their_order() {
        // Rewriting someone's settings sorted would be a diff of every line
        // for one added key.
        let (project, home) = scratch("order");
        let t = targets(Some(project), false, Some(home)).unwrap();
        std::fs::create_dir_all(t.settings.parent().unwrap()).unwrap();
        std::fs::write(&t.settings, r#"{"zeta": 1, "alpha": 2}"#).unwrap();
        install(&t, Path::new("/opt/thirdeye")).unwrap();
        let keys: Vec<String> = read_object(&t.settings).unwrap().keys().cloned().collect();
        assert_eq!(keys, ["zeta", "alpha", "permissions"]);
    }

    #[test]
    fn installing_twice_leaves_one_of_everything() {
        let (project, home) = scratch("twice");
        let t = targets(Some(project), false, Some(home)).unwrap();
        install(&t, Path::new("/opt/thirdeye")).unwrap();
        install(&t, Path::new("/opt/thirdeye")).unwrap();
        let settings = read_object(&t.settings).unwrap();
        assert_eq!(
            settings["permissions"]["allow"].as_array().unwrap().len(),
            1
        );
    }

    #[test]
    fn a_user_uninstall_removes_everything_it_added() {
        let (_, home) = scratch("user");
        let t = targets(None, true, Some(home.clone())).unwrap();
        install(&t, Path::new("/opt/thirdeye")).unwrap();
        assert!(
            home.join(".gemini/antigravity-cli/skills/flagging-files/SKILL.md")
                .is_file()
        );
        uninstall(&t).unwrap();
        assert!(!t.hooks.exists() && !t.mcp.exists());
        assert_eq!(
            read_object(&t.settings).unwrap()["permissions"]["allow"],
            json!([])
        );
    }

    #[test]
    fn no_hook_writes_to_stdout_or_fails_the_agent() {
        // `{}` on stdout from a PreToolUse hook denied every tool call.
        let doc = hooks(Path::new("/opt/it's/thirdeye"));
        let tool = doc["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(tool.contains("--agent antigravity --event PreToolUse >/dev/null"));
        assert!(tool.contains(r"'/opt/it'\''s/thirdeye'") && tool.ends_with("exit 0"));
        assert!(
            doc["Stop"][0]["command"].is_string(),
            "handlers directly, no matcher"
        );
    }

    #[test]
    fn a_skill_of_the_same_name_that_is_not_ours_stops_the_install() {
        let (project, home) = scratch("their-skill");
        let t = targets(Some(project), false, Some(home)).unwrap();
        std::fs::create_dir_all(t.skills.join("flagging-files")).unwrap();
        std::fs::write(t.skills.join("flagging-files/SKILL.md"), "mine").unwrap();
        assert!(install(&t, Path::new("/opt/thirdeye")).is_err());
        assert!(!t.hooks.exists(), "nothing half installed");
    }

    #[test]
    fn doctor_sees_the_binary_and_notices_drift() {
        let (project, home) = scratch("inspect");
        let t = targets(Some(project), false, Some(home)).unwrap();
        install(&t, Path::new("/opt/thirdeye")).unwrap();
        let found = inspect(&t).unwrap();
        assert_eq!(found.binary, Path::new("/opt/thirdeye"));
        assert!(found.current);
        let mut doc = read_object(&t.hooks).unwrap();
        doc.insert(NAME.into(), json!({}));
        write_object(&t.hooks, &doc).unwrap();
        assert!(!inspect(&t).unwrap().current);
    }
}
