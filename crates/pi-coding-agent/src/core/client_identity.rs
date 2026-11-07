//! Port of `packages/coding-agent/src/core/client-identity.ts`.

use crate::config::VERSION;
use crate::core::settings_manager::SettingsManager;
use crate::core::telemetry::is_install_telemetry_enabled;
use pi_ai::utils::identity::{ClientIdentity, UserAgentMode, UserAgentNamed, pi_attribution};
use std::sync::Arc;

/// How pi's requests identify it to providers: a detailed `User-Agent` with pi's version, and attribution to pi as a
/// CLI agent while install telemetry is enabled.
pub fn get_client_identity(settings_manager: Arc<SettingsManager>) -> ClientIdentity {
    let telemetry_env = std::env::var("PI_TELEMETRY").ok();
    let attribution = if is_install_telemetry_enabled(settings_manager, telemetry_env.as_deref()) {
        Some(pi_attribution())
    } else {
        None
    };
    ClientIdentity {
        user_agent: Some(UserAgentMode::Named(UserAgentNamed::Detailed)),
        version: Some(VERSION.to_string()),
        attribution: Some(attribution),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Map, Value, json};

    fn settings(json: Value) -> Arc<SettingsManager> {
        let Value::Object(map) = json else { unreachable!() };
        SettingsManager::in_memory_json(map as Map<String, Value>, None)
    }

    // pi-rs divergence from TS Pi: with the default (off) setting no provider attribution is sent.
    #[test]
    fn attribution_follows_the_telemetry_setting() {
        if std::env::var_os("PI_TELEMETRY").is_some() {
            return; // the env var overrides the setting
        }
        assert_eq!(get_client_identity(settings(json!({}))).attribution, Some(None));
        let on = get_client_identity(settings(json!({ "enableInstallTelemetry": true })));
        assert!(on.attribution.is_some_and(|attribution| attribution.is_some()));
    }
}
