//! Port of `packages/coding-agent/src/core/slash-commands.ts`.

use crate::core::source_info::SourceInfo;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SlashCommandSource {
    #[serde(rename = "extension")]
    Extension,
    #[serde(rename = "prompt")]
    Prompt,
    #[serde(rename = "skill")]
    Skill,
}

impl SlashCommandSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Extension => "extension",
            Self::Prompt => "prompt",
            Self::Skill => "skill",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlashCommandInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub source: SlashCommandSource,
    pub source_info: SourceInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuiltinSlashCommand {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argument_hint: Option<String>,
}

pub static BUILTIN_SLASH_COMMANDS: LazyLock<Vec<BuiltinSlashCommand>> = LazyLock::new(|| {
    let cmd = |name: &str, description: &str, argument_hint: Option<&str>| BuiltinSlashCommand {
        name: name.to_string(),
        description: description.to_string(),
        argument_hint: argument_hint.map(str::to_string),
    };
    vec![
        cmd("settings", "Open settings menu", None),
        cmd(
            "model",
            "Select model (opens selector UI)",
            Some("<provider/model>"),
        ),
        cmd("tree", "Navigate session tree (switch branches)", None),
        cmd("thinking", "Set thinking level", Some("<level>")),
        cmd(
            "scoped-models",
            "Enable/disable models for Ctrl+P cycling",
            None,
        ),
        cmd(
            "refresh-models",
            "Refresh model catalogs now (no startup fetch)",
            None,
        ),
        cmd(
            "export",
            "Export session (HTML default, or specify path: .html/.jsonl)",
            None,
        ),
        cmd(
            "import",
            "Import and resume a session from a JSONL file",
            None,
        ),
        cmd(
            "share",
            "Share session via a Radius artifact or a GitHub gist",
            None,
        ),
        cmd(
            "bug",
            "Report a bug to the Pi developers",
            Some("<description>"),
        ),
        cmd("copy", "Copy last agent message to clipboard", None),
        cmd("name", "Set session display name", None),
        cmd("session", "Show session info and stats", None),
        cmd("changelog", "Show changelog entries", None),
        cmd("hotkeys", "Show all keyboard shortcuts", None),
        cmd(
            "fork",
            "Create a new fork from a previous user message",
            None,
        ),
        cmd(
            "clone",
            "Duplicate the current session at the current position",
            None,
        ),
        cmd(
            "trust",
            "Save project trust decision for future sessions",
            None,
        ),
        cmd(
            "login",
            "Configure provider authentication",
            Some("<provider>"),
        ),
        cmd("logout", "Remove provider authentication", None),
        cmd("new", "Start a new session", None),
        cmd("compact", "Manually compact the session context", None),
        cmd("resume", "Resume a different session", None),
        cmd(
            "reload",
            "Reload keybindings, extensions, skills, prompts, themes, and context files",
            None,
        ),
        cmd("quit", &format!("Quit {}", crate::config::APP_NAME), None),
    ]
});
