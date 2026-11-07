//! Allocation cost of streamed tool-call arguments (port of the cost tests in
//! `test/streaming-json.test.ts` and `test/streaming-tool-arguments.test.ts`, upstream #9265).
//!
//! A counting allocator records the bytes each test thread allocates, so tests running in parallel
//! do not disturb each other.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use pi_ai::{StreamingJsonParser, ToolCallArgumentsStream};
use serde_json::json;

thread_local! {
    static ALLOCATED: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// SAFETY: forwards to `System` unchanged; the thread-local counter is const-initialized and never
// allocates.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATED.try_with(|bytes| bytes.set(bytes.get() + layout.size()));
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: the caller upholds `GlobalAlloc::dealloc`'s contract.
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let _ =
            ALLOCATED.try_with(|bytes| bytes.set(bytes.get() + size.saturating_sub(layout.size())));
        // SAFETY: the caller upholds `GlobalAlloc::realloc`'s contract.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Bytes allocated on this thread while running `f`.
fn allocated_bytes<T>(f: impl FnOnce() -> T) -> usize {
    let before = ALLOCATED.with(Cell::get);
    let result = f();
    let bytes = ALLOCATED.with(Cell::get) - before;
    drop(result);
    bytes
}

/// `write`-style arguments with `size` bytes of file content.
fn write_arguments(size: usize) -> String {
    let content: String = (0..)
        .map(|line| format!("const line{line} = \"value {line}\"; // \\ done\n"))
        .take_while({
            let mut total = 0;
            move |line: &String| {
                total += line.len();
                total <= size + line.len()
            }
        })
        .collect();
    json!({"path": "/src/file.ts", "content": &content[..size]}).to_string()
}

/// Streams `text` through a `ToolCallArgumentsStream` in `delta`-byte pieces, keeping every partial
/// argument object the way a provider puts it on the content block.
fn stream_allocation(text: &str, delta: usize) -> usize {
    allocated_bytes(|| {
        let mut args = ToolCallArgumentsStream::new();
        let mut shown = None;
        for piece in text.as_bytes().chunks(delta) {
            if let Some(partial) = args.append(std::str::from_utf8(piece).expect("ASCII arguments"))
            {
                shown = Some(partial);
            }
        }
        (shown, args.finish())
    })
}

#[test]
fn allocates_linearly_in_the_streamed_text_never_per_accumulated_text() {
    let small = write_arguments(50_000);
    let large = write_arguments(500_000);
    for delta in [32, 3] {
        let small_bytes = stream_allocation(&small, delta);
        let large_bytes = stream_allocation(&large, delta);
        let per_byte = large_bytes / large.len();
        // Re-parsing the accumulated text on every delta cost hundreds of KB per byte.
        assert!(
            per_byte < 400,
            "{per_byte} bytes allocated per argument byte at {delta}-byte deltas"
        );
        // Ten times the text: linear growth is about 10x, quadratic about 100x.
        let ratio = large_bytes as f64 / small_bytes as f64;
        assert!(
            ratio < 20.0,
            "x{ratio:.1} allocation for x10 text at {delta}-byte deltas"
        );
    }
}

#[test]
fn keeps_very_large_open_arrays_linear() {
    let elements = |count: usize| json!({"items": (0..count).collect::<Vec<_>>()}).to_string();
    let small = elements(10_000);
    let large = elements(100_000);
    let small_bytes = stream_allocation(&small, 16);
    let large_bytes = stream_allocation(&large, 16);
    assert!(
        large_bytes / large.len() < 2000,
        "{} bytes per byte",
        large_bytes / large.len()
    );
    assert!((large_bytes as f64 / small_bytes as f64) < 20.0);
}

#[test]
fn reads_one_huge_piece_in_linear_time() {
    let text = write_arguments(500_000);
    let bytes = allocated_bytes(|| {
        let mut parser = StreamingJsonParser::new();
        parser.append(&text);
        parser.snapshot().cloned()
    });
    assert!(
        bytes / text.len() < 100,
        "{} bytes per byte",
        bytes / text.len()
    );
}
