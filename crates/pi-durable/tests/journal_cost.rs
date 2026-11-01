//! Journal representation cost — testable checks.
//!
//! The bench (`benches/journal.rs`) measures live heap bytes; this test covers
//! the same representation without a counting allocator: serialization round-trip,
//! typed-vs-untyped serialization identity, and a per-entry size budget.
use pi_durable::codemode::{GlobalEntry, JournalEntry, JournalState};
use pi_json::strict::JsonString;

fn build(n: usize) -> JournalState {
    let mut entries = Vec::with_capacity(n);
    for i in 0..n {
        entries.push(JournalEntry {
            id: i as u64,
            at: i as f64,
            issued_after: None,
            global: if i % 10 == 0 {
                Some(GlobalEntry {
                    name: JsonString::from("global"),
                    args: None,
                    outcome: pi_durable::JsonObject::default(),
                })
            } else {
                None
            },
        });
    }
    JournalState {
        entries,
        first_issued: Some(0),
    }
}

/// The typed document must round-trip through JSON losslessly.
#[test]
fn journal_round_trips() {
    let journal = build(100);
    let s = serde_json::to_string(&journal).expect("serialize");
    let parsed: JournalState = serde_json::from_str(&s).expect("parse");
    let s2 = serde_json::to_string(&parsed).expect("serialize");
    assert_eq!(s, s2, "journal did not round-trip stably");
}

/// The typed form and an untyped `serde_json::Value` of the same data serialize
/// to identical JSON — the persisted representation is the same either way.
#[test]
fn typed_and_untyped_serialize_identically() {
    let journal = build(100);
    let untyped = serde_json::to_value(&journal).expect("to_value");
    assert_eq!(
        serde_json::to_string(&journal).unwrap(),
        serde_json::to_string(&untyped).unwrap(),
    );
}

/// Per-entry serialized cost must stay within a generous budget.
#[test]
fn journal_ser_size_budget() {
    let n = 1_000;
    let journal = build(n);
    let s = serde_json::to_string(&journal).expect("serialize");
    let per_entry = s.len() as f64 / n as f64;
    assert!(
        per_entry < 100.0,
        "per-entry serialized bytes too high: {per_entry}"
    );
}
