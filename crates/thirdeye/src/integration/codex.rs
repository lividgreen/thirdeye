//! Codex CLI.
//!
//! Codex has no scriptable plugin install for local plugins (its plugins are
//! installed from the desktop app), so thirdeye writes Codex's own config
//! instead: `hooks.json`, an `[mcp_servers.thirdeye]` block in `config.toml`,
//! and the flagging skill. The hooks and the block name only the binary,
//! whose path an update keeps; the skill links into thirdeye's directory
//! ([`super::skills`]).
//!
//! These files may already hold the user's own settings, so install merges
//! rather than overwrites, and uninstall removes exactly what install added.
//!
//! Codex skips hooks it has not been told to trust, and trust is keyed on a
//! hash of the hook's definition: **every install that changes `hooks.json`
//! must be approved again in `/hooks`.** Because the definitions name only
//! the binary, an update does not change them.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde_json::{Value, json};

use super::tree::{self, File};
use super::{AgentSays, Context, Inspection, Integration, Scope, Target, skills};
use crate::invocation;

const HOOKS_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/agents/codex/hooks.json"
));
const CONFIG_TEMPLATE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/agents/codex/config.toml"
));
const SKILL: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/agents/codex/skills/flagging-files/SKILL.md"
));

/// What the templates call this binary.
const PLACEHOLDER: &str = "{{THIRDEYE}}";

/// How a hook is recognized as ours, for replacing and removing it.
const OURS: &str = "hook --agent codex";

const BLOCK_START: &str = "# >>> thirdeye";
const BLOCK_END: &str = "# <<< thirdeye";

pub struct Codex;

impl Integration for Codex {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn program(&self) -> &'static str {
        "codex"
    }

    fn scopes(&self) -> &'static [Scope] {
        &[Scope::Project, Scope::User]
    }

    fn config_var(&self) -> Option<&'static str> {
        Some("CODEX_HOME")
    }

    fn publish(&self, cx: &Context) -> Result<()> {
        tree::publish(&cx.data_for(self.id()), &published())
    }

    fn install(&self, cx: &Context, target: &Target) -> Result<()> {
        let dir = codex_dir(cx, target)?;
        self.publish(cx)?;
        // A project's `.codex/` is usually committed: a bare `thirdeye`
        // there when PATH finds this very binary.
        let on_path = invocation::thirdeye_on_path();
        let named = invocation::invocation(
            &cx.binary,
            on_path.as_deref(),
            matches!(target, Target::Project(_)),
        );
        install(&dir, &named, &skill_source(cx))
    }

    fn uninstall(&self, cx: &Context, target: &Target) -> Result<()> {
        uninstall(&codex_dir(cx, target)?, &skill_source(cx))
    }

    fn inspect(&self, cx: &Context, target: &Target) -> Option<Inspection> {
        inspect(&codex_dir(cx, target).ok()?, &skill_source(cx))
    }

    fn next_steps(&self, _: &Target) -> String {
        "Codex skips hooks until trusted: open /hooks in Codex and approve them, then restart it"
            .into()
    }
}

/// What thirdeye's directory holds for Codex: the skill.
fn published() -> Vec<File> {
    vec![File::new(
        format!("skills/{}/SKILL.md", skills::NAME),
        SKILL,
    )]
}

fn skill_source(cx: &Context) -> PathBuf {
    cx.data_for("codex").join("skills").join(skills::NAME)
}

/// Where to install: `<project>/.codex`, or the user's `$CODEX_HOME`.
fn codex_dir(cx: &Context, target: &Target) -> Result<PathBuf> {
    match target {
        Target::Project(project) => Ok(project.join(".codex")),
        Target::User => cx
            .path_var("CODEX_HOME")
            .or_else(|| cx.home.as_ref().map(|h| h.join(".codex")))
            .context("neither $CODEX_HOME nor $HOME is set"),
    }
}

/// The skill is ours if it names the flag tool as thirdeye's skill does.
fn skill_is_ours(text: &str) -> bool {
    text.contains("thirdeye")
}

fn install(codex_dir: &Path, binary: &Path, skill: &Path) -> Result<()> {
    // Before anything is written: a refusal must leave nothing half installed.
    skills::check(&codex_dir.join("skills"), skill, skill_is_ours)?;
    std::fs::create_dir_all(codex_dir)
        .with_context(|| format!("creating {}", codex_dir.display()))?;

    // Checked before anything is written. A thirdeye table outside our
    // markers — the user's own, or ours with the markers deleted — would be
    // defined twice once ours is appended, and Codex refuses such a config.
    let config = without_block(&read_config(codex_dir)?);
    if config
        .lines()
        .any(|l| l.trim_start().starts_with("[mcp_servers.thirdeye"))
    {
        anyhow::bail!(
            "{} already defines [mcp_servers.thirdeye] outside thirdeye's markers; \
             remove that table and run install again",
            codex_dir.join("config.toml").display()
        );
    }

    let mut ours: Value =
        serde_json::from_str(HOOKS_TEMPLATE).context("the embedded hooks template is not JSON")?;
    // Substituted into the parsed strings, not the JSON text: a `"` or `\` in
    // the path would otherwise break the file or be read as an escape.
    // Shell-quoted, because Codex hands each command to a shell.
    render(&mut ours, &shell_quote(binary));
    let mut hooks = without_ours(read_hooks(codex_dir)?);
    for (event, groups) in ours["hooks"].as_object().into_iter().flatten() {
        let slot = hooks["hooks"][event].as_array_mut();
        match slot {
            Some(existing) => existing.extend(groups.as_array().cloned().unwrap_or_default()),
            None => hooks["hooks"][event] = groups.clone(),
        }
    }
    write_hooks(codex_dir, &hooks)?;

    let block = CONFIG_TEMPLATE.replace(PLACEHOLDER, &toml_string(binary));
    let separator = if config.is_empty() || config.ends_with("\n\n") {
        ""
    } else if config.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    std::fs::write(
        codex_dir.join("config.toml"),
        format!("{config}{separator}{block}"),
    )?;

    skills::place(&codex_dir.join("skills"), skill, skill_is_ours)
}

/// What is installed in `codex_dir`, for `doctor`: the binary our hooks
/// run, and whether hooks, config block and skill are exactly what this
/// build would write for it.
fn inspect(codex_dir: &Path, skill: &Path) -> Option<Inspection> {
    let installed = read_hooks(codex_dir).ok()?;
    let ours = |hooks: &Value| -> Vec<Value> {
        hooks["hooks"]
            .as_object()
            .into_iter()
            .flatten()
            .flat_map(|(event, groups)| {
                groups
                    .as_array()
                    .into_iter()
                    .flatten()
                    .flat_map(|g| g["hooks"].as_array().cloned().unwrap_or_default())
                    .filter(|h| h["command"].as_str().is_some_and(|c| c.contains(OURS)))
                    .map(move |h| json!({ "event": event, "hook": h }))
            })
            .collect()
    };
    let found = ours(&installed);
    let command = found.first()?["hook"]["command"].as_str()?.to_string();
    let binary = binary_in(&command)?;

    let mut expected: Value = serde_json::from_str(HOOKS_TEMPLATE).ok()?;
    render(&mut expected, &shell_quote(&binary));
    let block = CONFIG_TEMPLATE.replace(PLACEHOLDER, &toml_string(&binary));
    let current = found == ours(&expected)
        && read_config(codex_dir).is_ok_and(|c| c.contains(block.trim_end()))
        && skills::current(&codex_dir.join("skills"), skill, SKILL);
    Some(Inspection {
        binary,
        current,
        location: codex_dir.to_path_buf(),
        agent_says: AgentSays::Loaded,
    })
}

/// The binary a hook command runs: the shell word just before `hook
/// --agent codex`, single-quoted as install writes it, or a bare name.
fn binary_in(command: &str) -> Option<PathBuf> {
    let before = command.split(OURS).next()?.trim_end();
    let word = before.rsplit("; ").next()?.trim();
    let word = match word.strip_prefix('\'').and_then(|w| w.strip_suffix('\'')) {
        Some(quoted) => quoted.replace("'\\''", "'"),
        None => word.to_string(),
    };
    (!word.is_empty()).then(|| PathBuf::from(word))
}

fn uninstall(codex_dir: &Path, skill: &Path) -> Result<()> {
    // A file left holding nothing is removed rather than kept empty: install
    // may have created it, and an empty one is clutter either way.
    if codex_dir.join("hooks.json").exists() {
        let hooks = without_ours(read_hooks(codex_dir)?);
        if hooks == json!({ "hooks": {} }) {
            std::fs::remove_file(codex_dir.join("hooks.json"))?;
        } else {
            write_hooks(codex_dir, &hooks)?;
        }
    }
    if codex_dir.join("config.toml").exists() {
        let config = without_block(&read_config(codex_dir)?);
        if config.trim().is_empty() {
            std::fs::remove_file(codex_dir.join("config.toml"))?;
        } else {
            std::fs::write(codex_dir.join("config.toml"), config)?;
        }
    }
    skills::remove(&codex_dir.join("skills"), skill, skill_is_ours)?;
    // `remove_dir` only removes an empty directory, so anything of the
    // user's keeps it in place.
    let _ = std::fs::remove_dir(codex_dir);
    Ok(())
}

/// Replace the placeholder in every string inside `value`.
fn render(value: &mut Value, with: &str) {
    match value {
        Value::String(text) => *text = text.replace(PLACEHOLDER, with),
        Value::Array(items) => items.iter_mut().for_each(|v| render(v, with)),
        Value::Object(fields) => fields.values_mut().for_each(|v| render(v, with)),
        _ => {}
    }
}

fn read_hooks(codex_dir: &Path) -> Result<Value> {
    let path = codex_dir.join("hooks.json");
    match read_existing(&path)? {
        Some(raw) => {
            serde_json::from_str(&raw).with_context(|| format!("{} is not JSON", path.display()))
        }
        None => Ok(json!({ "hooks": {} })),
    }
}

/// A file's text, `None` only when it does not exist.
///
/// Every other failure is an error, never "empty": install and uninstall
/// write the result back, and an unreadable config read as empty would be
/// replaced with ours alone.
fn read_existing(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn write_hooks(codex_dir: &Path, hooks: &Value) -> Result<()> {
    let text = serde_json::to_string_pretty(hooks)?;
    std::fs::write(codex_dir.join("hooks.json"), text + "\n")?;
    Ok(())
}

/// The user's hooks with every one of ours removed, and any matcher group
/// or event left empty by that dropped too.
fn without_ours(mut hooks: Value) -> Value {
    let Some(events) = hooks["hooks"].as_object_mut() else {
        return hooks;
    };
    for groups in events.values_mut() {
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        for group in groups.iter_mut() {
            if let Some(list) = group["hooks"].as_array_mut() {
                list.retain(|h| !h["command"].as_str().is_some_and(|c| c.contains(OURS)));
            }
        }
        groups.retain(|g| g["hooks"].as_array().is_none_or(|l| !l.is_empty()));
    }
    events.retain(|_, groups| groups.as_array().is_none_or(|g| !g.is_empty()));
    hooks
}

fn read_config(codex_dir: &Path) -> Result<String> {
    Ok(read_existing(&codex_dir.join("config.toml"))?.unwrap_or_default())
}

/// `config` with our block removed, and the blank line install put before
/// it, so install then uninstall is byte-identical.
///
/// The block ends at our end marker **or at the first table that is not
/// ours**, whichever comes first. Codex appends tables at the end of this
/// file, and a writer that keeps the closing comment last puts them inside
/// the markers; deleting everything between the markers would delete them.
fn without_block(config: &str) -> String {
    let lines: Vec<&str> = config.split_inclusive('\n').collect();
    let Some(start) = lines.iter().position(|l| l.starts_with(BLOCK_START)) else {
        return config.to_string();
    };
    let ours = |l: &str| !l.starts_with('[') || l.starts_with("[mcp_servers.thirdeye");
    let mut end = start + 1;
    while end < lines.len() && !lines[end].starts_with(BLOCK_END) && ours(lines[end]) {
        end += 1;
    }

    let mut before: Vec<&str> = lines[..start].to_vec();
    // The blank line install added before the block, if there was one.
    if before.len() >= 2 && before[before.len() - 1] == "\n" {
        before.pop();
    }
    let after = lines[end..]
        .iter()
        .copied()
        // Our closing marker, wherever a foreign table left it.
        .filter(|l| !l.starts_with(BLOCK_END));
    before.into_iter().chain(after).collect()
}

/// POSIX single quotes: end the run, escape the quote, start a new run.
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

/// A TOML literal string where possible, a basic string otherwise.
fn toml_string(path: &Path) -> String {
    let text = path.display().to_string();
    if text.contains('\'') {
        format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        format!("'{text}'")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codex_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("thirdeye-codex-{tag}-{}", std::process::id()))
            .join(".codex");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const BINARY: &str = "/opt/thirdeye bin/thirdeye";

    /// The skill as thirdeye's directory holds it, beside the test's `.codex`.
    fn skill(dir: &Path) -> PathBuf {
        let published = dir.parent().unwrap().join("data/skills/flagging-files");
        std::fs::create_dir_all(&published).unwrap();
        std::fs::write(published.join("SKILL.md"), SKILL).unwrap();
        published
    }

    fn install(dir: &Path, binary: &Path) -> Result<()> {
        super::install(dir, binary, &skill(dir))
    }

    fn uninstall(dir: &Path) -> Result<()> {
        super::uninstall(dir, &skill(dir))
    }

    fn inspect(dir: &Path) -> Option<Inspection> {
        super::inspect(dir, &skill(dir))
    }

    fn hooks(dir: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(dir.join("hooks.json")).unwrap()).unwrap()
    }

    /// Every hook command under `event`, across all its matcher groups.
    fn commands(dir: &Path, event: &str) -> Vec<String> {
        hooks(dir)["hooks"][event]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|group| group["hooks"].as_array().cloned().unwrap_or_default())
            .filter_map(|h| h["command"].as_str().map(str::to_owned))
            .collect()
    }

    #[test]
    fn installing_twice_leaves_one_set_of_hooks() {
        let dir = codex_dir("twice");
        install(&dir, Path::new(BINARY)).unwrap();
        install(&dir, Path::new(BINARY)).unwrap();
        assert_eq!(commands(&dir, "PostToolUse").len(), 1);
        let config = std::fs::read_to_string(dir.join("config.toml")).unwrap();
        assert_eq!(config.matches("[mcp_servers.thirdeye]").count(), 1);
    }

    #[test]
    fn the_users_own_hooks_survive_install_and_uninstall() {
        let dir = codex_dir("own-hooks");
        std::fs::write(
            dir.join("hooks.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"my-linter"}]}]}}"#,
        )
        .unwrap();
        install(&dir, Path::new(BINARY)).unwrap();
        assert!(commands(&dir, "PreToolUse").contains(&"my-linter".to_string()));
        uninstall(&dir).unwrap();
        assert_eq!(commands(&dir, "PreToolUse"), vec!["my-linter".to_string()]);
    }

    #[test]
    fn the_users_own_config_survives_install_and_uninstall() {
        let dir = codex_dir("own-config");
        let mine = "model = \"x\"\n\n[features]\nhooks = true\n";
        std::fs::write(dir.join("config.toml"), mine).unwrap();
        install(&dir, Path::new(BINARY)).unwrap();
        uninstall(&dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("config.toml")).unwrap(),
            mine
        );
    }

    #[test]
    fn the_hooks_run_this_binary() {
        // Shell-quoted: the path has a space, and the command goes to a shell.
        let dir = codex_dir("binary");
        install(&dir, Path::new(BINARY)).unwrap();
        for event in [
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "Stop",
            "SessionEnd",
        ] {
            let cmds = commands(&dir, event);
            assert_eq!(cmds.len(), 1, "{event}");
            assert!(
                cmds[0].contains("'/opt/thirdeye bin/thirdeye' hook --agent codex"),
                "{event}: {}",
                cmds[0]
            );
        }
        let config = std::fs::read_to_string(dir.join("config.toml")).unwrap();
        assert!(config.contains("/opt/thirdeye bin/thirdeye"), "{config}");
        assert!(
            config.contains(r#"args = ["mcp", "--agent", "codex"]"#),
            "{config}"
        );
    }

    #[test]
    fn any_binary_path_yields_valid_hooks_that_run_it() {
        // Spliced into the JSON text, a `"` broke the file and a `\` was
        // read as an escape. The path must reach the shell exactly.
        let dir = codex_dir("odd-path");
        let odd = r#"/opt/we"ird\dir/thirdeye"#;
        install(&dir, Path::new(odd)).unwrap();
        let cmds = commands(&dir, "PostToolUse");
        assert!(
            cmds[0].contains(r#"'/opt/we"ird\dir/thirdeye' hook --agent codex"#),
            "{}",
            cmds[0]
        );
    }

    #[test]
    fn the_template_still_names_the_placeholder_the_installer_rewrites() {
        // Otherwise an edited template would install hooks that run nothing.
        assert!(HOOKS_TEMPLATE.contains(PLACEHOLDER));
        assert!(CONFIG_TEMPLATE.contains(PLACEHOLDER));
    }

    #[test]
    fn pre_tool_use_is_synchronous_and_session_end_has_its_budget() {
        // PreToolUse must land before the command runs or its writes fall
        // outside the window; SessionEnd gets Codex's maximum of 3 s.
        let dir = codex_dir("timing");
        install(&dir, Path::new(BINARY)).unwrap();
        let h = hooks(&dir);
        let first = |event: &str| h["hooks"][event][0]["hooks"][0].clone();
        assert_eq!(first("PreToolUse")["async"], Value::Null);
        assert_eq!(first("PostToolUse")["async"], Value::Bool(true));
        // Synchronous: under `codex exec` the process exits straight after
        // the turn, and an async Stop still running dies with it — the turn
        // then never closes.
        assert_eq!(first("Stop")["async"], Value::Null);
        assert_eq!(first("SessionEnd")["timeout"], Value::from(3));
    }

    #[test]
    fn the_flag_tool_runs_without_asking() {
        // Found live: Codex asks before every MCP call unless told otherwise,
        // and `codex exec` refuses outright. A viewer that interrupts the
        // human to approve each pointer at a file defeats itself.
        let dir = codex_dir("approve");
        install(&dir, Path::new(BINARY)).unwrap();
        let config = std::fs::read_to_string(dir.join("config.toml")).unwrap();
        let tool = config
            .split("[mcp_servers.thirdeye.tools.flag]")
            .nth(1)
            .expect("a per-tool table for flag");
        assert!(tool.contains(r#"approval_mode = "approve""#), "{config}");
    }

    #[test]
    fn the_skill_is_installed_and_removed() {
        let dir = codex_dir("skill");
        install(&dir, Path::new(BINARY)).unwrap();
        let skill = dir.join("skills/flagging-files/SKILL.md");
        assert!(
            std::fs::read_to_string(&skill)
                .unwrap()
                .contains("name: flagging-files")
        );
        uninstall(&dir).unwrap();
        assert!(!skill.exists());
    }

    #[test]
    fn a_table_codex_appends_after_our_block_survives() {
        // Codex appends tables at the end of config.toml — trusting a project
        // writes one — which puts them after our end marker's line but, once
        // its writer keeps our closing comment last, inside the block.
        let dir = codex_dir("appended");
        install(&dir, Path::new(BINARY)).unwrap();
        let config = std::fs::read_to_string(dir.join("config.toml")).unwrap();
        let trusted = "[projects.\"/p\"]\ntrust_level = \"trusted\"\n";
        let swallowed = config.replace(BLOCK_END, &format!("{trusted}{BLOCK_END}"));
        std::fs::write(dir.join("config.toml"), swallowed).unwrap();

        install(&dir, Path::new(BINARY)).unwrap();
        assert!(
            read_config(&dir).unwrap().contains(trusted),
            "lost on reinstall"
        );
        uninstall(&dir).unwrap();
        let left = read_config(&dir).unwrap();
        assert!(left.contains(trusted), "lost on uninstall: {left}");
        assert!(!left.contains("mcp_servers.thirdeye"), "{left}");
    }

    #[test]
    fn a_thirdeye_table_we_did_not_mark_is_refused_not_duplicated() {
        // Codex fails to parse a config defining one table twice. Whether the
        // user wrote it or deleted our markers, install must stop and say so.
        for (tag, mine) in [
            (
                "theirs",
                "[mcp_servers.thirdeye]\ncommand = \"elsewhere\"\n".to_string(),
            ),
            ("unmarked", {
                let dir = codex_dir("unmarked-source");
                install(&dir, Path::new(BINARY)).unwrap();
                read_config(&dir)
                    .unwrap()
                    .lines()
                    .filter(|l| !l.starts_with(BLOCK_START))
                    .map(|l| format!("{l}\n"))
                    .collect()
            }),
        ] {
            let dir = codex_dir(tag);
            std::fs::write(dir.join("config.toml"), &mine).unwrap();
            let err = install(&dir, Path::new(BINARY)).unwrap_err().to_string();
            assert!(err.contains("[mcp_servers.thirdeye]"), "{tag}: {err}");
            assert_eq!(
                read_config(&dir).unwrap(),
                mine,
                "{tag}: file must be untouched"
            );
        }
    }

    #[test]
    fn an_unreadable_config_is_an_error_not_an_empty_one() {
        // Read as empty, it would be overwritten with only our block.
        let dir = codex_dir("unreadable");
        std::fs::write(dir.join("config.toml"), b"model = \"\xff\"\n").unwrap();
        assert!(install(&dir, Path::new(BINARY)).is_err());
        assert!(uninstall(&dir).is_err());
        assert_eq!(
            std::fs::read(dir.join("config.toml")).unwrap(),
            b"model = \"\xff\"\n"
        );
    }

    #[test]
    fn unreadable_hooks_are_an_error_not_an_empty_set() {
        let dir = codex_dir("unreadable-hooks");
        std::fs::write(dir.join("hooks.json"), b"\xff").unwrap();
        assert!(install(&dir, Path::new(BINARY)).is_err());
        assert_eq!(std::fs::read(dir.join("hooks.json")).unwrap(), b"\xff");
    }

    #[test]
    fn uninstall_leaves_nothing_where_there_was_nothing() {
        // A project that never had Codex config should not keep an empty
        // hooks.json, config.toml and skills/ — or a .codex/ — afterwards.
        let dir = codex_dir("nothing");
        std::fs::remove_dir(&dir).unwrap();
        install(&dir, Path::new(BINARY)).unwrap();
        uninstall(&dir).unwrap();
        assert!(
            !dir.exists(),
            "left behind: {:?}",
            std::fs::read_dir(&dir).map(|d| d.count())
        );
    }

    #[test]
    fn uninstall_keeps_a_directory_that_holds_the_users_files() {
        let dir = codex_dir("keeps");
        std::fs::write(dir.join("notes.txt"), "mine").unwrap();
        install(&dir, Path::new(BINARY)).unwrap();
        uninstall(&dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("notes.txt")).unwrap(),
            "mine"
        );
    }

    #[test]
    fn installed_hooks_report_their_binary_and_whether_they_are_current() {
        let dir = codex_dir("inspect");
        assert!(inspect(&dir).is_none());
        install(&dir, Path::new(BINARY)).unwrap();
        let found = inspect(&dir).unwrap();
        assert_eq!(found.binary, PathBuf::from(BINARY));
        assert!(found.current);

        // A bare name, as a portable project install writes it.
        install(&dir, Path::new("thirdeye")).unwrap();
        assert_eq!(inspect(&dir).unwrap().binary, PathBuf::from("thirdeye"));

        let hooks = std::fs::read_to_string(dir.join("hooks.json")).unwrap();
        std::fs::write(
            dir.join("hooks.json"),
            hooks.replace("\"timeout\": 3", "\"timeout\": 1"),
        )
        .unwrap();
        assert!(
            !inspect(&dir).unwrap().current,
            "changed hooks must read as stale"
        );
    }

    #[test]
    fn uninstalling_what_was_never_installed_is_not_an_error() {
        uninstall(&codex_dir("never")).unwrap();
    }

    #[test]
    fn publishing_a_new_version_leaves_the_project_byte_for_byte_alone() {
        use super::super::testing::{context, no_vars, scratch, snapshot};
        let base = scratch("codex-rule");
        let run = |_: &str, _: &[&str], _: Option<&Path>| Ok(String::new());
        let cx = context(&base, &no_vars, &run);
        let project = base.join("p");
        Codex
            .install(&cx, &Target::Project(project.clone()))
            .unwrap();
        let before = snapshot(&project);
        std::fs::write(
            skill_source(&cx).join("SKILL.md"),
            "thirdeye, a later skill",
        )
        .unwrap();
        Codex.refresh(&cx).unwrap();
        assert_eq!(snapshot(&project), before);
        assert!(
            Codex
                .inspect(&cx, &Target::Project(project))
                .unwrap()
                .current
        );
    }

    #[test]
    fn a_skill_of_the_users_stops_the_install_before_anything_is_written() {
        let dir = codex_dir("their-skill");
        std::fs::create_dir_all(dir.join("skills/flagging-files")).unwrap();
        std::fs::write(dir.join("skills/flagging-files/SKILL.md"), "mine").unwrap();
        assert!(install(&dir, Path::new(BINARY)).is_err());
        assert!(!dir.join("hooks.json").exists() && !dir.join("config.toml").exists());
    }
}
