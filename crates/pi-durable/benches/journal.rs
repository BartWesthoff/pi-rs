//! Journal representation cost benchmark.
//!
//! Measures the in-memory heap cost of the typed `pi.codemode.journal` document
//! (`JournalState`) against an untyped `serde_json::Value` holding the same data
//! — the shape the reference (TypeScript) stores the journal as. Reports live heap
//! bytes, the typed/untyped ratio, and serialized byte cost, then checks the
//! budget: the typed representation must not exceed 2.0x the untyped baseline.
//!
//!   cargo bench -p pi-durable --bench journal
//!   PI_DURABLE_BENCH_QUICK=1 cargo bench -p pi-durable --bench journal
mod support;
use support::*;

use pi_chord::json::Map;
use pi_durable::{
    JsonObject, JsonString, JsonValue,
    codemode::{GlobalEntry, JournalEntry, JournalState},
};

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// A small realistic `JsonObject` for `GlobalEntry.outcome`.
fn outcome() -> JsonObject {
    let mut m = Map::new();
    m.insert(
        JsonString::from("status"),
        JsonValue::String(JsonString::from("ok")),
    );
    JsonObject::new(m)
}

/// Build a realistic journal: `n` entries, 1 in 10 carrying a `GlobalEntry`.
fn journal(n: usize) -> JournalState {
    let mut entries = Vec::with_capacity(n);
    for i in 0..n {
        let global = if i % 10 == 0 {
            Some(GlobalEntry {
                name: JsonString::from("global"),
                args: Some(JsonValue::Bool(true)),
                outcome: outcome(),
            })
        } else {
            None
        };
        entries.push(JournalEntry {
            id: i as u64,
            at: i as f64,
            issued_after: None,
            global,
        });
    }
    JournalState {
        entries,
        first_issued: Some(0),
    }
}

/// Live-heap deltas (bytes) for the typed and untyped forms of an `n`-entry
/// journal, plus the serialized byte cost of each.
fn measure(n: usize) -> (isize, isize, usize, usize) {
    let before = live_bytes() as isize;
    let typed = journal(n);
    let typed_heap = live_bytes() as isize - before;
    let after_typed = live_bytes() as isize;
    let untyped = serde_json::to_value(&typed).expect("to_value");
    let untyped_heap = live_bytes() as isize - after_typed;
    let typed_ser = serde_json::to_string(&typed).unwrap().len();
    let untyped_ser = serde_json::to_string(&untyped).unwrap().len();
    (typed_heap, untyped_heap, typed_ser, untyped_ser)
}

fn main() {
    println!(
        "{:<7} | {:>12} | {:>12} | {:>8} | {:>10} | {:>10} | {:>8.1} | budget",
        "entries", "typed heap", "untyped heap", "ratio", "typed ser", "untyped ser", "typed/entry"
    );
    let mut ok = true;
    for n in sizes(&[100, 1_000, 10_000], &[10, 100]) {
        let (typed_heap, untyped_heap, typed_ser, untyped_ser) = measure(n);
        let ratio = typed_heap as f64 / untyped_heap.max(1) as f64;
        let budget_ok = typed_heap as f64 / untyped_heap.max(1) as f64 <= 2.0;
        if !budget_ok {
            ok = false;
        }
        println!(
            "{:<7} | {:>12} | {:>12} | {:>8.2} | {:>10} | {:>10} | {:>8.1} | {}",
            n,
            typed_heap,
            untyped_heap,
            ratio,
            typed_ser,
            untyped_ser,
            typed_heap as f64 / n as f64,
            if budget_ok { "PASS" } else { "FAIL" }
        );
    }
    if !ok {
        panic!("typed journal representation exceeds 2.0x the untyped baseline");
    }
}
