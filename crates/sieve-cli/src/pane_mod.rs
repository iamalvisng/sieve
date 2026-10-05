//! The sieve-pane Claude Code mod that `sieve init --mod` installs.
//!
//! The mod source lives in `crates/sieve-cli/assets/claude-mod/sieve-pane/`. The build embeds
//! each file, so the Rust code holds none of their text.

use std::io;
use std::path::{Path, PathBuf};

use crate::hosts::{self, WriteAction};
use crate::ojson::OJson;

/// The key that `enabledPlugins` holds for the mod.
pub const PLUGIN_KEY: &str = "sieve-pane@skills-dir";

// cargo package cannot include these files; publishing sieve-cli needs them moved into the crate (open question Q2 in the handover).
/// The mod files that `init` owns, as a path inside the mod dir and the
/// embedded content. Every file that `register.tsx` imports is here. The README, the tests,
/// the scripts and the assets stay out.
const FILES: [(&str, &str); 5] = [
    (
        ".claude-plugin/plugin.json",
        include_str!("../assets/claude-mod/sieve-pane/.claude-plugin/plugin.json"),
    ),
    (
        "hooks/hooks.json",
        include_str!("../assets/claude-mod/sieve-pane/hooks/hooks.json"),
    ),
    (
        "hooks/register.tsx",
        include_str!("../assets/claude-mod/sieve-pane/hooks/register.tsx"),
    ),
    (
        "hooks/mascots.ts",
        include_str!("../assets/claude-mod/sieve-pane/hooks/mascots.ts"),
    ),
    (
        "types/index.d.ts",
        include_str!("../assets/claude-mod/sieve-pane/types/index.d.ts"),
    ),
];

/// The mod dir inside a project.
pub fn mod_dir(root: &Path) -> PathBuf {
    root.join(".claude").join("skills").join("sieve-pane")
}

/// The paths of the files that `init` owns, for the plan and for uninstall.
pub fn owned_paths(root: &Path) -> Vec<PathBuf> {
    let dir = mod_dir(root);
    FILES.iter().map(|(rel, _)| dir.join(rel)).collect()
}

/// Writes the mod files and the `enabledPlugins` key of the project
/// `settings.json`. Returns the report lines.
pub fn install(root: &Path) -> io::Result<Vec<String>> {
    let mut lines = Vec::new();
    for (path, (_, content)) in owned_paths(root).into_iter().zip(FILES) {
        let word = match hosts::write_owned(&path, content)? {
            WriteAction::Created => "created",
            WriteAction::Unchanged => "unchanged",
            _ => "updated",
        };
        lines.push(format!(
            "\u{2713} mod sieve-pane: {} ({word})",
            path.display()
        ));
    }
    let settings = root.join(".claude").join("settings.json");
    // A value the user set stays. A `false` means the user turned the mod off.
    let user_value = std::fs::read_to_string(&settings)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.get("enabledPlugins")?.get(PLUGIN_KEY).cloned());
    if let Some(value) = user_value {
        if value != serde_json::Value::Bool(true) {
            lines.push(format!(
                "\u{26a0} mod sieve-pane: disabled in the project settings ({}): files written, key left as it is",
                settings.display()
            ));
            return Ok(lines);
        }
    }
    let action =
        hosts::merge_json_entry(&settings, "enabledPlugins", PLUGIN_KEY, OJson::Bool(true))?;
    lines.push(if action == WriteAction::SkippedUnparseable {
        format!(
            "\u{26a0} mod sieve-pane: {} left unchanged (not valid JSON or wrong shape) \u{2014} add enabledPlugins.{PLUGIN_KEY} by hand",
            settings.display()
        )
    } else {
        format!(
            "\u{2713} mod sieve-pane: {} enabledPlugins ({})",
            settings.display(),
            action.word()
        )
    });
    Ok(lines)
}
