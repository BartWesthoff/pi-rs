//! Per-package "loads and registers its tools and commands" smoke tests, plus the harness's own
//! checks: a well-behaved local package is observed completely and deliberately broken packages
//! are reported.

use serde_json::Value;

use crate::support::{
    Observation, SlashCommand, Sandbox, assert_scenario, captain_package, captain_packages, fixtures, observe,
    pins_dir, skip_without_fixtures,
};

fn local_package(name: &str) -> Value {
    Value::String(pins_dir().join(name).display().to_string())
}

/// Runs one captain package alone, from the fixture tree.
fn package_scenario(test: &str, source: &str) {
    let Some(fixtures) = fixtures() else {
        skip_without_fixtures(test);
        return;
    };
    let sandbox = Sandbox::new(vec![captain_package(source)], Some(fixtures));
    assert_scenario(test, &observe(&sandbox));
}

#[test]
fn ponytail() {
    package_scenario("ponytail", "git:github.com/DietrichGebert/ponytail");
}

#[test]
fn pi_mcp_adapter() {
    package_scenario("pi_mcp_adapter", "npm:pi-mcp-adapter");
}

#[test]
fn pi_subagents() {
    package_scenario("pi_subagents", "npm:pi-subagents");
}

#[test]
fn billion_context() {
    package_scenario("billion_context", "npm:billion-context");
}

#[test]
fn pi_web_access() {
    package_scenario("pi_web_access", "npm:pi-web-access");
}

#[test]
fn rpiv_ask_user_question() {
    package_scenario("rpiv_ask_user_question", "npm:@juicesharp/rpiv-ask-user-question");
}

#[test]
fn rpiv_todo() {
    package_scenario("rpiv_todo", "npm:@juicesharp/rpiv-todo");
}

#[test]
fn context_mode() {
    package_scenario("context_mode", "npm:context-mode");
}

/// All eight packages together, as the captain's settings list them.
#[test]
fn all_packages() {
    let Some(fixtures) = fixtures() else {
        skip_without_fixtures("all_packages");
        return;
    };
    let sandbox = Sandbox::new(captain_packages(), Some(fixtures));
    assert_scenario("all_packages", &observe(&sandbox));
}

// ---------------------------------------------------------------------------
// Harness self-checks (no fixtures needed)
// ---------------------------------------------------------------------------

/// The observation sees a package's tool, extension command and skill command.
#[test]
fn probe_package_is_observed() {
    let sandbox = Sandbox::new(vec![local_package("probe")], None);
    let observation = observe(&sandbox);
    let registered = &observation.registered;
    assert!(registered.tools.iter().any(|tool| tool == "probe_tool"), "{observation:#?}");
    for (source, name) in [("extension", "probe-command"), ("skill", "skill:probe-skill")] {
        let command = SlashCommand { source: source.to_string(), name: name.to_string() };
        assert!(registered.commands.contains(&command), "missing {command:?}: {observation:#?}");
    }
    assert_scenario("probe", &observation);
}

fn assert_detected(observation: &Observation, marker: &str) {
    assert!(
        observation.diagnostics.iter().any(|line| line.contains(marker)),
        "the broken fixture was not reported: {observation:#?}"
    );
}

/// A package whose module throws on import is reported as a load failure.
#[test]
fn broken_load_fixture_is_detected() {
    let sandbox = Sandbox::new(vec![local_package("broken-load")], None);
    assert_detected(&observe(&sandbox), "pkg-harness: broken-load fixture");
}

/// A package whose `session_start` handler throws is reported as an extension error.
#[test]
fn broken_handler_fixture_is_detected() {
    let sandbox = Sandbox::new(vec![local_package("broken-handler")], None);
    assert_detected(&observe(&sandbox), "pkg-harness: broken-handler fixture");
}
