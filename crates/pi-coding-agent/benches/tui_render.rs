//! Per-frame TUI render cost while a reply streams (PERF-03, PERF-04).
//!
//! Two scenarios, both driven the way interactive mode drives the components: every delta calls the
//! component's update method and every frame renders it once.
//!
//! - `markdown`: a ~30 KB Markdown answer with 10 fenced code blocks, streamed into an
//!   `AssistantMessageComponent` (`update_content` per delta).
//! - `write`: a ~200 KB `write` tool call streamed into a `ToolExecutionComponent` with the built-in
//!   write renderer (`update_args` per delta).
//!
//! For each scenario it prints the frame count, the total and per-frame time (mean of the first and
//! last tenth of the frames, which shows whether frame time grows with the answer, plus p50 and max)
//! and the heap bytes and allocations per frame. Building the delta inputs is outside the measured
//! region; only the component calls are timed and counted.
//!
//! Run with `cargo bench -p pi-coding-agent --bench tui_render`. Environment overrides:
//! `PI_TUI_BENCH_MARKDOWN_BYTES`, `PI_TUI_BENCH_WRITE_BYTES`, `PI_TUI_BENCH_DELTA_BYTES` (bytes per
//! delta), `PI_TUI_BENCH_DELTAS_PER_FRAME` and `PI_TUI_BENCH_WIDTH`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use pi_ai::providers::faux::faux_text_message;
use pi_coding_agent::core::keybindings::KeybindingsManager;
use pi_coding_agent::core::tools::renderers::write_renderers;
use pi_coding_agent::modes::interactive::components::assistant_message::AssistantMessageComponent;
use pi_coding_agent::modes::interactive::components::tool_execution::{
    ToolExecutionComponent, ToolExecutionComponentToolDefinition,
};
use pi_coding_agent::modes::interactive::theme::theme::init_theme;
use pi_tui::event_loop::EventLoop;
use pi_tui::testing::VirtualTerminal;
use pi_tui::tui::{Component, Tui, TuiOptions};
use serde_json::json;

static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

/// Counts every allocation and the bytes it requests (cumulative, not live).
struct Counting;

// SAFETY: forwards every call to the system allocator unchanged and only adds relaxed counters.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract, which `System` shares.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: `pointer` came from `System` through this allocator with this layout.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATED_BYTES.fetch_add(size as u64, Ordering::Relaxed);
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: the caller upholds `GlobalAlloc::realloc`'s contract, which `System` shares.
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Time and allocations of one frame (its deltas plus its render).
#[derive(Default, Clone, Copy)]
struct Frame {
    time: Duration,
    bytes: u64,
    allocations: u64,
}

/// Accumulates the measured parts of a frame.
#[derive(Default)]
struct Meter {
    frame: Frame,
}

impl Meter {
    fn measure<R>(&mut self, f: impl FnOnce() -> R) -> R {
        let bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);
        let allocations = ALLOCATIONS.load(Ordering::Relaxed);
        let start = Instant::now();
        let result = f();
        self.frame.time += start.elapsed();
        self.frame.bytes += ALLOCATED_BYTES.load(Ordering::Relaxed) - bytes;
        self.frame.allocations += ALLOCATIONS.load(Ordering::Relaxed) - allocations;
        result
    }

    fn finish(&mut self) -> Frame {
        std::mem::take(&mut self.frame)
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn mean_ms(frames: &[Frame]) -> f64 {
    if frames.is_empty() {
        return 0.0;
    }
    frames.iter().map(|f| ms(f.time)).sum::<f64>() / frames.len() as f64
}

fn report(name: &str, source_bytes: usize, frames: &[Frame]) {
    let total: Duration = frames.iter().map(|f| f.time).sum();
    let tenth = (frames.len() / 10).max(1);
    let mut times: Vec<Duration> = frames.iter().map(|f| f.time).collect();
    times.sort();
    let p50 = times.get(times.len() / 2).copied().unwrap_or_default();
    let max = times.last().copied().unwrap_or_default();
    let bytes: u64 = frames.iter().map(|f| f.bytes).sum();
    let allocations: u64 = frames.iter().map(|f| f.allocations).sum();
    let n = frames.len().max(1) as f64;
    println!(
        "| {name} | {source_bytes} | {} | {:.1} | {:.3} | {:.3} | {:.3} | {:.3} | {:.0} | {:.0} | {:.1} |",
        frames.len(),
        ms(total),
        mean_ms(&frames[..tenth.min(frames.len())]),
        mean_ms(&frames[frames.len().saturating_sub(tenth)..]),
        ms(p50),
        ms(max),
        bytes as f64 / n / 1024.0,
        allocations as f64 / n,
        bytes as f64 / (1024.0 * 1024.0),
    );
}

/// A ~`target` byte Markdown answer with 10 fenced code blocks spread through prose, lists and headings.
fn markdown_answer(target: usize) -> String {
    const LANGS: [&str; 5] = ["rust", "typescript", "python", "bash", "json"];
    let code = |lang: &str, n: usize| -> String {
        let body: Vec<String> = (0..20)
            .map(|i| match lang {
                "rust" => format!("    let value_{n}_{i}: Vec<u32> = (0..{i}).map(|x| x * {n}).collect();"),
                "typescript" => format!("  const value{n}_{i}: number[] = Array.from({{ length: {i} }}, (_, x) => x * {n});"),
                "python" => format!("    value_{n}_{i} = [x * {n} for x in range({i})]  # comment {i}"),
                "bash" => format!("for f in src/*.rs; do echo \"$f {n} {i}\" | grep -c pattern; done"),
                _ => format!("  \"key_{n}_{i}\": {{ \"value\": {i}, \"enabled\": true, \"name\": \"item {n}\" }},"),
            })
            .collect();
        format!("```{lang}\n{}\n```\n\n", body.join("\n"))
    };
    let blocks = 10;
    let mut sections = Vec::new();
    for n in 0..blocks {
        sections.push(format!(
            "## Step {n}: configure the **module** and its `options`\n\n"
        ));
        sections.push(code(LANGS[n % LANGS.len()], n));
    }
    let code_bytes: usize = sections.iter().map(String::len).sum();
    let prose_per_section = target.saturating_sub(code_bytes) / blocks;
    let mut out = String::new();
    for (n, pair) in sections.chunks(2).enumerate() {
        out.push_str(&pair[0]);
        let mut prose = String::new();
        let mut i = 0;
        while prose.len() < prose_per_section {
            if i % 4 == 3 {
                prose.push_str(&format!(
                    "- item {i} of step {n} with *emphasis* and a [link](https://example.com/{n}/{i})\n- another item with `code` in it\n\n"
                ));
            } else {
                prose.push_str(&format!(
                    "Paragraph {i} of step {n} explains why the configuration matters, which value to pick and \
                     what happens when the **default** changes; it keeps going for a while so lines wrap at the \
                     terminal width like a real answer does.\n\n"
                ));
            }
            i += 1;
        }
        out.push_str(&prose);
        out.push_str(&pair[1]);
    }
    out
}

/// A ~`target` byte TypeScript file.
fn write_content(target: usize) -> String {
    let mut out = String::new();
    let mut i = 0;
    while out.len() < target {
        out.push_str(&format!(
            "export function compute{i}(input: number[], factor = {i}): number {{ return input.reduce((a, b) => a + b * factor, 0); }}\n"
        ));
        i += 1;
    }
    out
}

/// Byte offsets that split `text` into `delta`-sized pieces on char boundaries.
fn delta_ends(text: &str, delta: usize) -> Vec<usize> {
    let mut ends = Vec::new();
    let mut end = 0;
    while end < text.len() {
        end = (end + delta).min(text.len());
        while !text.is_char_boundary(end) {
            end += 1;
        }
        ends.push(end);
    }
    ends
}

fn bench_markdown(bytes: usize, delta: usize, per_frame: usize, width: usize) {
    let answer = markdown_answer(bytes);
    let component = AssistantMessageComponent::new(None, None, None, None, None, None, None, None);
    let mut meter = Meter::default();
    let mut frames = Vec::new();
    for chunk in delta_ends(&answer, delta).chunks(per_frame) {
        for &end in chunk {
            let message = faux_text_message(&answer[..end]);
            meter.measure(|| component.update_content(message, Some(true)));
        }
        meter.measure(|| black_box(component.render(width)));
        frames.push(meter.finish());
    }
    report("markdown", answer.len(), &frames);
}

fn bench_write(bytes: usize, delta: usize, per_frame: usize, width: usize) {
    let content = write_content(bytes);
    let tui = Tui::main_screen_with(
        Arc::new(VirtualTerminal::new(width, 40)),
        TuiOptions {
            event_loop: Some(EventLoop::new_manual()),
            ..Default::default()
        },
    );
    let definition = ToolExecutionComponentToolDefinition::ToolRenderers(write_renderers());
    let component = ToolExecutionComponent::new(
        "write",
        "bench",
        json!({}),
        None,
        Some(&definition),
        tui,
        "/",
    );
    let mut meter = Meter::default();
    let mut frames = Vec::new();
    for chunk in delta_ends(&content, delta).chunks(per_frame) {
        for &end in chunk {
            let args = json!({ "path": "src/generated.ts", "content": &content[..end] });
            meter.measure(|| component.update_args(args));
        }
        meter.measure(|| black_box(component.render(width)));
        frames.push(meter.finish());
    }
    report("write", content.len(), &frames);
}

fn main() {
    // `cargo test --benches` runs bench targets with `--bench`-less args; keep that cheap.
    let quick = std::env::args().any(|a| a == "--test");
    init_theme(Some("dark"), Some(false));
    pi_tui::keybindings::set_keybindings(KeybindingsManager::new(None, None).tui_manager());
    let scale = if quick { 20 } else { 1 };
    let markdown_bytes = env_usize("PI_TUI_BENCH_MARKDOWN_BYTES", 30 * 1024) / scale;
    let write_bytes = env_usize("PI_TUI_BENCH_WRITE_BYTES", 200 * 1024) / scale;
    let delta = env_usize("PI_TUI_BENCH_DELTA_BYTES", 16).max(1);
    let per_frame = env_usize("PI_TUI_BENCH_DELTAS_PER_FRAME", 4).max(1);
    let width = env_usize("PI_TUI_BENCH_WIDTH", 120).max(20);
    println!("delta {delta} B, {per_frame} deltas per frame, width {width}");
    println!(
        "| scenario | source bytes | frames | total ms | first-10% ms/frame | last-10% ms/frame | p50 ms | max ms | KiB alloc/frame | allocs/frame | MiB alloc total |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|---|");
    bench_markdown(markdown_bytes, delta, per_frame, width);
    bench_write(write_bytes, delta, per_frame, width);
}
