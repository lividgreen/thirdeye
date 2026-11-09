//! Small pieces more than one integration needs.

use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

use super::Target;

/// POSIX single quotes: end the run, escape the quote, start a new run.
pub fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

/// Refuse a project target for an agent whose plugins are per user, with
/// the reason, rather than silently wiring every session instead.
pub fn user_only(agent: &str, target: &Target) -> Result<()> {
    match target {
        Target::User => Ok(()),
        Target::Project(_) => {
            anyhow::bail!("{agent} installs plugins per user, not per project; use --user")
        }
    }
}

/// A JSON object from `path`, empty if the file does not exist. Anything
/// else unreadable is an error, never "empty": the result is written back,
/// and a config read as empty would be replaced with ours alone.
pub fn read_object(path: &Path) -> Result<serde_json::Map<String, Value>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Default::default()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    match serde_json::from_str(&text).with_context(|| format!("{} is not JSON", path.display()))? {
        Value::Object(map) => Ok(map),
        _ => anyhow::bail!("{} is not a JSON object", path.display()),
    }
}

pub fn write_object(path: &Path, doc: &serde_json::Map<String, Value>) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(doc)?;
    std::fs::write(path, text + "\n").with_context(|| format!("writing {}", path.display()))
}

/// Add `entry` to the string list at `permissions.allow` in a JSON settings
/// file, or take it out, beside whatever else is there.
pub fn allow_in_settings(settings: &Path, entry: &str, allow: bool) -> Result<()> {
    use serde_json::json;
    if !allow && !settings.exists() {
        return Ok(());
    }
    let mut doc = read_object(settings)?;
    if allow {
        let list = doc
            .entry("permissions")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .with_context(|| format!("permissions in {} is not an object", settings.display()))?
            .entry("allow")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .with_context(|| {
                format!("permissions.allow in {} is not a list", settings.display())
            })?;
        if list.iter().any(|v| v == entry) {
            return Ok(());
        }
        list.push(json!(entry));
    } else {
        let Some(list) = doc
            .get_mut("permissions")
            .and_then(|p| p.get_mut("allow"))
            .and_then(Value::as_array_mut)
        else {
            return Ok(());
        };
        let before = list.len();
        list.retain(|v| v != entry);
        if list.len() == before {
            return Ok(());
        }
        // Nothing left behind that only this rule made: an empty list, and
        // an empty `permissions` once that is gone.
        if list.is_empty()
            && let Some(permissions) = doc.get_mut("permissions").and_then(Value::as_object_mut)
        {
            permissions.remove("allow");
            if permissions.is_empty() {
                doc.remove("permissions");
            }
        }
    }
    write_object(settings, &doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_permission_is_added_once_beside_the_users_and_removed_alone() {
        let dir = std::env::temp_dir().join(format!("thirdeye-allow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let settings = dir.join("settings.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &settings,
            r#"{"enabledPlugins":{"x@y":true},"permissions":{"allow":["Bash(ls:*)"]}}"#,
        )
        .unwrap();
        allow_in_settings(&settings, "flag", true).unwrap();
        allow_in_settings(&settings, "flag", true).unwrap();
        let doc = read_object(&settings).unwrap();
        assert_eq!(
            doc["permissions"]["allow"],
            serde_json::json!(["Bash(ls:*)", "flag"])
        );
        assert_eq!(doc["enabledPlugins"]["x@y"], true);
        allow_in_settings(&settings, "flag", false).unwrap();
        let doc = read_object(&settings).unwrap();
        assert_eq!(
            doc["permissions"]["allow"],
            serde_json::json!(["Bash(ls:*)"])
        );
    }

    #[test]
    fn removing_from_settings_that_do_not_exist_creates_nothing() {
        let settings = std::env::temp_dir().join(format!(
            "thirdeye-allow-none-{}/settings.json",
            std::process::id()
        ));
        allow_in_settings(&settings, "flag", false).unwrap();
        assert!(!settings.exists());
    }
}
