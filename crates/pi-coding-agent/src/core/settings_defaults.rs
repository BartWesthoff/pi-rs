//! Port of `packages/coding-agent/src/core/settings-defaults.ts`.
//!
//! Runtime defaults shared by settings accessors and the published JSON Schema. The TS object is
//! mirrored as nested constant modules; [`settings_defaults_json`] renders it as the TS object.

use pi_ai::utils::retry::DEFAULT_MAX_AGENT_RETRY_DELAY_MS;
use serde_json::{Value, json};

pub const TRANSPORT: &str = "auto";
pub const SESSION_SHARE_DESTINATION: &str = "auto";
pub const STEERING_MODE: &str = "one-at-a-time";
pub const FOLLOW_UP_MODE: &str = "one-at-a-time";

pub mod compaction {
    pub const ENABLED: bool = true;
    pub const RESERVE_TOKENS: u64 = 16384;
    pub const KEEP_RECENT_TOKENS: u64 = 20000;
}

pub mod branch_summary {
    pub const RESERVE_TOKENS: u64 = 16384;
    pub const SKIP_PROMPT: bool = false;
}

pub mod retry {
    pub const ENABLED: bool = true;
    pub const MAX_RETRIES: u64 = 3;
    pub const BASE_DELAY_MS: u64 = 2000;
    pub const MAX_AGENT_DELAY_MS: u64 = super::DEFAULT_MAX_AGENT_RETRY_DELAY_MS;
    pub mod provider {
        pub const MAX_RETRY_DELAY_MS: u64 = 60000;
    }
}

pub const HIDE_THINKING_BLOCK: bool = false;
pub const SHOW_CACHE_MISS_NOTICES: bool = false;
pub const QUIET_STARTUP: bool = false;
pub const DEFAULT_PROJECT_TRUST: &str = "ask";
pub const COLLAPSE_CHANGELOG: bool = false;
// pi-rs divergence from TS Pi: install telemetry is opt-in, so a default start makes no pi.dev request.
pub const ENABLE_INSTALL_TELEMETRY: bool = false;
pub const ENABLE_ANALYTICS: bool = false;
pub const ENABLE_SKILL_COMMANDS: bool = true;

pub mod terminal {
    pub const SHOW_IMAGES: bool = true;
    pub const IMAGE_WIDTH_CELLS: u64 = 60;
    pub const CLEAR_ON_SHRINK: bool = false;
    pub const SHOW_TERMINAL_PROGRESS: bool = false;
}

pub mod images {
    pub const AUTO_RESIZE: bool = true;
    pub const BLOCK_IMAGES: bool = false;
}

pub const DOUBLE_ESCAPE_ACTION: &str = "tree";
pub const TREE_FILTER_MODE: &str = "default";
pub const EDITOR_PADDING_X: u64 = 0;
pub const OUTPUT_PAD: u64 = 1;
pub const AUTOCOMPLETE_MAX_VISIBLE: u64 = 5;

pub mod markdown {
    pub const CODE_BLOCK_INDENT: &str = "  ";
    pub const MERMAID: &str = "streaming";
}

pub mod warnings {
    pub const ANTHROPIC_EXTRA_USAGE: bool = true;
}

pub mod codemode {
    pub const MODE: &str = "on";
    pub const INLINE_BUDGET: u64 = 3000;
}

pub const CACHE_WARMING: &str = "streaming";
pub const TUI_MODE: &str = "fullscreen";
pub const FULLSCREEN_EXIT_OUTPUT: &str = "transcript";
pub const FULLSCREEN_SCROLLBAR: &str = "auto";
pub const FULLSCREEN_COPY_ON_SELECT: bool = true;
pub const FULLSCREEN_WHEEL_SCROLL_LINES: &str = "auto";

/// `SETTINGS_DEFAULTS` as the JSON object of the TS source (key order preserved).
pub fn settings_defaults_json() -> Value {
    json!({
        "transport": TRANSPORT,
        "sessionShareDestination": SESSION_SHARE_DESTINATION,
        "steeringMode": STEERING_MODE,
        "followUpMode": FOLLOW_UP_MODE,
        "compaction": {
            "enabled": compaction::ENABLED,
            "reserveTokens": compaction::RESERVE_TOKENS,
            "keepRecentTokens": compaction::KEEP_RECENT_TOKENS,
        },
        "branchSummary": {
            "reserveTokens": branch_summary::RESERVE_TOKENS,
            "skipPrompt": branch_summary::SKIP_PROMPT,
        },
        "retry": {
            "enabled": retry::ENABLED,
            "maxRetries": retry::MAX_RETRIES,
            "baseDelayMs": retry::BASE_DELAY_MS,
            "maxAgentDelayMs": retry::MAX_AGENT_DELAY_MS,
            "provider": { "maxRetryDelayMs": retry::provider::MAX_RETRY_DELAY_MS },
        },
        "hideThinkingBlock": HIDE_THINKING_BLOCK,
        "showCacheMissNotices": SHOW_CACHE_MISS_NOTICES,
        "quietStartup": QUIET_STARTUP,
        "defaultProjectTrust": DEFAULT_PROJECT_TRUST,
        "collapseChangelog": COLLAPSE_CHANGELOG,
        "enableInstallTelemetry": ENABLE_INSTALL_TELEMETRY,
        "enableAnalytics": ENABLE_ANALYTICS,
        "enableSkillCommands": ENABLE_SKILL_COMMANDS,
        "terminal": {
            "showImages": terminal::SHOW_IMAGES,
            "imageWidthCells": terminal::IMAGE_WIDTH_CELLS,
            "clearOnShrink": terminal::CLEAR_ON_SHRINK,
            "showTerminalProgress": terminal::SHOW_TERMINAL_PROGRESS,
        },
        "images": {
            "autoResize": images::AUTO_RESIZE,
            "blockImages": images::BLOCK_IMAGES,
        },
        "doubleEscapeAction": DOUBLE_ESCAPE_ACTION,
        "treeFilterMode": TREE_FILTER_MODE,
        "editorPaddingX": EDITOR_PADDING_X,
        "outputPad": OUTPUT_PAD,
        "autocompleteMaxVisible": AUTOCOMPLETE_MAX_VISIBLE,
        "markdown": {
            "codeBlockIndent": markdown::CODE_BLOCK_INDENT,
            "mermaid": markdown::MERMAID,
        },
        "warnings": { "anthropicExtraUsage": warnings::ANTHROPIC_EXTRA_USAGE },
        "codemode": { "mode": codemode::MODE, "inlineBudget": codemode::INLINE_BUDGET },
        "cacheWarming": CACHE_WARMING,
        "tuiMode": TUI_MODE,
        "fullscreenExitOutput": FULLSCREEN_EXIT_OUTPUT,
        "fullscreenScrollbar": FULLSCREEN_SCROLLBAR,
        "fullscreenCopyOnSelect": FULLSCREEN_COPY_ON_SELECT,
        "fullscreenWheelScrollLines": FULLSCREEN_WHEEL_SCROLL_LINES,
    })
}
