//! Port of `packages/coding-agent/src/core/telemetry.ts`.

use crate::core::settings_manager::SettingsManager;
use std::sync::Arc;

fn is_truthy_env_flag(value: Option<&str>) -> bool {
    match value {
        None | Some("") => false,
        Some(value) => {
            let lower = value.to_lowercase();
            value == "1" || lower == "true" || lower == "yes"
        }
    }
}

/// `telemetry_env` defaults to `PI_TELEMETRY`; pass `Some(..)` to override it (tests).
pub fn is_install_telemetry_enabled(
    settings_manager: Arc<SettingsManager>,
    telemetry_env: Option<&str>,
) -> bool {
    let from_env = std::env::var("PI_TELEMETRY").ok();
    let telemetry_env = telemetry_env.or(from_env.as_deref());
    resolve_install_telemetry(settings_manager.get_enable_install_telemetry(), telemetry_env)
}

/// An explicit environment value wins over the setting (an empty value forces it off).
fn resolve_install_telemetry(settings_enabled: bool, telemetry_env: Option<&str>) -> bool {
    match telemetry_env {
        Some(value) => is_truthy_env_flag(Some(value)),
        None => settings_enabled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn settings(value: Option<serde_json::Value>) -> Arc<SettingsManager> {
        let map = match value {
            Some(serde_json::Value::Object(map)) => map,
            _ => Default::default(),
        };
        SettingsManager::in_memory_json(map, None)
    }

    // pi-rs divergence from TS Pi: telemetry is opt-in, so default settings must leave it off.
    #[test]
    fn default_settings_leave_telemetry_off() {
        assert!(!settings(None).get_enable_install_telemetry());
        assert!(!resolve_install_telemetry(settings(None).get_enable_install_telemetry(), None));
    }

    #[test]
    fn opted_in_settings_or_env_enable_telemetry() {
        let on = settings(Some(json!({ "enableInstallTelemetry": true })));
        assert!(on.get_enable_install_telemetry());
        assert!(resolve_install_telemetry(true, None));
        assert!(resolve_install_telemetry(false, Some("1")));
        assert!(!resolve_install_telemetry(true, Some("0")));
    }

    #[test]
    fn truthy_flags() {
        assert!(is_truthy_env_flag(Some("1")));
        assert!(is_truthy_env_flag(Some("TRUE")));
        assert!(is_truthy_env_flag(Some("Yes")));
        assert!(!is_truthy_env_flag(Some("0")));
        assert!(!is_truthy_env_flag(Some("")));
        assert!(!is_truthy_env_flag(None));
    }
}
