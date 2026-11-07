//! Port of `packages/tui/test/overlay-non-capturing.test.ts`: focus management, no-op guards,
//! focus cycle prevention and rendering order of capturing and non-capturing overlays.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use common::Harness;
use pi_tui::tui::{
    Component, ComponentRef, Container, OverlayHandle, OverlayOptions, OverlayUnfocusOptions, SizeValue, Tui,
    TuiStopOptions,
};

type Hook = Arc<dyn Fn(&str) + Send + Sync>;

/// `FocusableOverlay`: records input and tracks focus. A test may extend `handleInput`; the
/// hook runs after the input is recorded, like the TS overrides that push then act.
struct Focusable {
    lines: Vec<String>,
    focused: AtomicBool,
    inputs: Mutex<Vec<String>>,
    hook: Mutex<Option<Hook>>,
}

impl Focusable {
    fn new(label: &str) -> Arc<Self> {
        Arc::new(Self {
            lines: vec![label.to_string()],
            focused: AtomicBool::new(false),
            inputs: Mutex::new(Vec::new()),
            hook: Mutex::new(None),
        })
    }
    fn is_focused(&self) -> bool {
        self.focused.load(Ordering::SeqCst)
    }
    fn inputs(&self) -> Vec<String> {
        self.inputs.lock().unwrap().clone()
    }
    fn on_input(&self, hook: impl Fn(&str) + Send + Sync + 'static) {
        *self.hook.lock().unwrap() = Some(Arc::new(hook));
    }
}

impl Component for Focusable {
    fn render(&self, _width: usize) -> Vec<String> {
        self.lines.clone()
    }
    fn handles_input(&self) -> bool {
        true
    }
    fn handle_input(&self, data: &str) {
        self.inputs.lock().unwrap().push(data.to_string());
        let hook = self.hook.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(data);
        }
    }
    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focused(&self, focused: bool) {
        self.focused.store(focused, Ordering::SeqCst);
    }
    fn focused(&self) -> bool {
        self.is_focused()
    }
}

struct EmptyContent;

impl Component for EmptyContent {
    fn render(&self, _width: usize) -> Vec<String> {
        Vec::new()
    }
}

struct StaticOverlay(Vec<String>);

impl Component for StaticOverlay {
    fn render(&self, _width: usize) -> Vec<String> {
        self.0.clone()
    }
}

fn static_overlay(label: &str) -> ComponentRef {
    Arc::new(StaticOverlay(vec![label.to_string()]))
}

fn cr<T: Component>(c: &Arc<T>) -> ComponentRef {
    c.clone()
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn non_capturing() -> Option<OverlayOptions> {
    Some(OverlayOptions { non_capturing: true, ..Default::default() })
}

/// `{ row: 0, col: 0, width: 1, nonCapturing }`
fn corner(non_capturing: bool) -> Option<OverlayOptions> {
    Some(OverlayOptions {
        row: Some(SizeValue::Abs(0)),
        col: Some(SizeValue::Abs(0)),
        width: Some(SizeValue::Abs(1)),
        non_capturing,
        ..Default::default()
    })
}

fn visible_when(flag: &Arc<AtomicBool>) -> Option<OverlayOptions> {
    let flag = flag.clone();
    Some(OverlayOptions { visible: Some(Arc::new(move |_, _| flag.load(Ordering::SeqCst))), ..Default::default() })
}

fn target(c: Option<ComponentRef>) -> Option<OverlayUnfocusOptions> {
    Some(OverlayUnfocusOptions { target: c })
}

struct Setup {
    h: Harness,
}

impl Setup {
    /// A started main-screen TUI with empty content; `focus` is focused before start.
    fn new(cols: usize, rows: usize, base: Option<ComponentRef>, focus: Option<ComponentRef>) -> Self {
        let h = Harness::new(cols, rows);
        h.tui.add_child(base.unwrap_or_else(|| Arc::new(EmptyContent)));
        if focus.is_some() {
            h.tui.set_focus(focus);
        }
        h.tui.start();
        Self { h }
    }
    fn tui(&self) -> &Arc<Tui> {
        &self.h.tui
    }
    fn weak(&self) -> Weak<Tui> {
        Arc::downgrade(&self.h.tui)
    }
    fn show(&self, c: ComponentRef, options: Option<OverlayOptions>) -> OverlayHandle {
        self.h.tui.show_overlay(c, options)
    }
    /// `renderAndFlush`
    fn flush(&self) {
        self.h.tui.request_render(true);
        self.h.wait_for_render();
    }
    fn send(&self, data: &str) {
        self.h.terminal.send_input(data);
        self.h.lp.run_pending();
    }
    fn first_char(&self) -> Option<char> {
        self.h.terminal.get_viewport().first().and_then(|l| l.chars().next())
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        self.h.tui.stop(TuiStopOptions::default());
    }
}

fn with_editor() -> (Setup, Arc<Focusable>) {
    let editor = Focusable::new("EDITOR");
    (Setup::new(80, 24, None, Some(cr(&editor))), editor)
}

// ---- focus management ---------------------------------------------------------------------

#[test]
fn non_capturing_overlay_preserves_focus_on_creation() {
    let (s, editor) = with_editor();
    let overlay = Focusable::new("OVERLAY");
    s.show(cr(&overlay), non_capturing());
    s.flush();
    assert!(editor.is_focused());
    assert!(!overlay.is_focused());
}

#[test]
fn focus_transfers_focus_to_the_overlay() {
    let (s, editor) = with_editor();
    let overlay = Focusable::new("OVERLAY");
    let handle = s.show(cr(&overlay), non_capturing());
    handle.focus();
    s.flush();
    assert!(!editor.is_focused());
    assert!(overlay.is_focused());
    assert!(handle.is_focused());
}

#[test]
fn unfocus_restores_previous_focus() {
    let (s, editor) = with_editor();
    let overlay = Focusable::new("OVERLAY");
    let handle = s.show(cr(&overlay), non_capturing());
    handle.focus();
    handle.unfocus(None);
    s.flush();
    assert!(editor.is_focused());
    assert!(!overlay.is_focused());
    assert!(!handle.is_focused());
}

#[test]
fn set_hidden_false_on_non_capturing_overlay_does_not_auto_focus() {
    let (s, editor) = with_editor();
    let overlay = Focusable::new("OVERLAY");
    let handle = s.show(cr(&overlay), non_capturing());
    handle.set_hidden(true);
    handle.set_hidden(false);
    s.flush();
    assert!(editor.is_focused());
    assert!(!overlay.is_focused());
}

#[test]
fn hide_when_overlay_is_not_focused_does_not_change_focus() {
    let (s, editor) = with_editor();
    let overlay = Focusable::new("OVERLAY");
    let handle = s.show(cr(&overlay), non_capturing());
    handle.hide();
    s.flush();
    assert!(editor.is_focused());
}

#[test]
fn hide_when_focused_restores_focus_correctly() {
    let (s, editor) = with_editor();
    let overlay = Focusable::new("OVERLAY");
    let handle = s.show(cr(&overlay), non_capturing());
    handle.focus();
    handle.hide();
    s.flush();
    assert!(editor.is_focused());
    assert!(!overlay.is_focused());
}

#[test]
fn capturing_overlay_removed_with_non_capturing_below_restores_focus_to_editor() {
    let (s, editor) = with_editor();
    let nc = Focusable::new("NC");
    let capturing = Focusable::new("CAP");
    s.show(cr(&nc), non_capturing());
    let handle = s.show(cr(&capturing), None);
    assert!(capturing.is_focused());
    handle.hide();
    s.flush();
    assert!(editor.is_focused());
    assert!(!nc.is_focused());
}

#[test]
fn sub_overlay_cleanup_then_hide_overlay_restores_focus_and_input_to_editor() {
    let (s, editor) = with_editor();
    let timer = Focusable::new("TIMER");
    let controller = Focusable::new("CTRL");
    let timer_handle = s.show(cr(&timer), non_capturing());
    s.show(cr(&controller), None);
    assert!(controller.is_focused());
    assert!(!editor.is_focused());
    timer_handle.hide();
    s.tui().hide_overlay();
    s.flush();
    assert!(editor.is_focused());
    assert!(!controller.is_focused());
    assert!(!timer.is_focused());
    s.send("x");
    s.flush();
    assert_eq!(editor.inputs(), strings(&["x"]));
    assert!(controller.inputs().is_empty());
    assert!(timer.inputs().is_empty());
}

#[test]
fn removed_focused_child_overlay_does_not_become_parent_overlay_fallback() {
    let (s, editor) = with_editor();
    let child = Focusable::new("CHILD");
    let parent = Focusable::new("PARENT");
    let child_handle = s.show(cr(&child), non_capturing());
    child_handle.focus();
    let parent_handle = s.show(cr(&parent), None);
    assert!(parent.is_focused());

    child_handle.hide();
    parent_handle.hide();
    s.send("x");
    s.flush();

    assert_eq!(editor.inputs(), strings(&["x"]));
    assert!(child.inputs().is_empty());
    assert!(parent.inputs().is_empty());
    assert!(editor.is_focused());
}

#[test]
fn microtask_deferred_sub_overlay_pattern_restores_focus() {
    // showExtensionCustom simulation: the factory shows the timer synchronously, then a
    // continuation (a microtask in TS, a posted task here) shows the controller.
    let (s, editor) = with_editor();
    let timer = Focusable::new("TIMER");
    let controller = Focusable::new("CTRL");
    let timer_handle = s.show(cr(&timer), non_capturing());
    let weak = s.weak();
    let deferred = cr(&controller);
    s.h.lp.post(move || {
        if let Some(tui) = weak.upgrade() {
            tui.show_overlay(deferred, None);
        }
    });
    s.h.lp.run_pending();
    s.flush();

    assert!(controller.is_focused());
    assert!(!editor.is_focused());

    // Esc: cleanup + close.
    timer_handle.hide();
    s.tui().hide_overlay();
    s.flush();

    assert!(editor.is_focused(), "editor should regain focus");
    assert!(!controller.is_focused());
    assert!(!timer.is_focused());

    s.send("x");
    s.flush();
    assert_eq!(editor.inputs(), strings(&["x"]), "editor should receive input after close");
    assert!(controller.inputs().is_empty());
}

#[test]
fn handle_input_redirection_skips_non_capturing_overlays_when_focused_overlay_becomes_invisible() {
    let (s, _editor) = with_editor();
    let fallback = Focusable::new("FALLBACK");
    let nc = Focusable::new("NC");
    let primary = Focusable::new("PRIMARY");
    let visible = Arc::new(AtomicBool::new(true));
    s.show(cr(&fallback), None);
    s.show(cr(&nc), non_capturing());
    s.show(cr(&primary), visible_when(&visible));
    assert!(primary.is_focused());
    visible.store(false, Ordering::SeqCst);
    s.send("x");
    s.flush();
    assert!(primary.inputs().is_empty());
    assert!(nc.inputs().is_empty());
    assert_eq!(fallback.inputs(), strings(&["x"]));
    assert!(fallback.is_focused());
}

#[test]
fn active_base_focus_replacement_receives_close_input_before_overlay_restore() {
    let (s, editor) = with_editor();
    let replacement = Focusable::new("REPLACEMENT");
    let overlay = Focusable::new("OVERLAY");
    let (w, r) = (s.weak(), cr(&replacement));
    overlay.on_input(move |d| {
        if d == "b" {
            w.upgrade().unwrap().set_focus(Some(r.clone()));
        }
    });
    let (w, e) = (s.weak(), cr(&editor));
    replacement.on_input(move |d| {
        if d == "\r" {
            w.upgrade().unwrap().set_focus(Some(e.clone()));
        }
    });

    s.show(cr(&overlay), None);
    assert!(overlay.is_focused());
    s.send("b");
    s.flush();
    assert!(replacement.is_focused());

    s.send("\r");
    s.flush();
    assert_eq!(replacement.inputs(), strings(&["\r"]));
    assert_eq!(overlay.inputs(), strings(&["b"]));
    assert!(overlay.is_focused());

    s.send("x");
    s.flush();
    assert_eq!(overlay.inputs(), strings(&["b", "x"]));
}

#[test]
fn active_replacement_still_receives_input_when_it_is_another_overlay_pre_focus() {
    let (s, editor) = with_editor();
    let replacement = Focusable::new("REPLACEMENT");
    let passive = Focusable::new("PASSIVE");
    let overlay = Focusable::new("OVERLAY");
    let (w, r) = (s.weak(), cr(&replacement));
    overlay.on_input(move |d| {
        if d == "b" {
            w.upgrade().unwrap().set_focus(Some(r.clone()));
        }
    });
    let (w, e) = (s.weak(), cr(&editor));
    replacement.on_input(move |d| {
        if d == "\r" {
            w.upgrade().unwrap().set_focus(Some(e.clone()));
        }
    });

    s.tui().set_focus(Some(cr(&replacement)));
    s.show(cr(&passive), non_capturing());
    s.tui().set_focus(Some(cr(&editor)));
    s.show(cr(&overlay), None);
    s.send("b");
    s.flush();
    assert!(replacement.is_focused());

    s.send("1");
    s.send("\r");
    s.flush();
    assert_eq!(replacement.inputs(), strings(&["1", "\r"]));
    assert_eq!(overlay.inputs(), strings(&["b"]));
    assert!(overlay.is_focused());
}

#[test]
fn blocked_replacement_can_move_focus_internally_before_overlay_restore() {
    let base = Container::new_ref();
    let editor = Focusable::new("EDITOR");
    let first = Focusable::new("FIRST");
    let second = Focusable::new("SECOND");
    let overlay = Focusable::new("OVERLAY");
    base.add_child(cr(&editor));
    base.add_child(cr(&first));
    base.add_child(cr(&second));
    let s = Setup::new(80, 24, Some(base.clone() as ComponentRef), Some(cr(&editor)));
    let (w, f) = (s.weak(), cr(&first));
    overlay.on_input(move |d| {
        if d == "b" {
            w.upgrade().unwrap().set_focus(Some(f.clone()));
        }
    });
    let (w, sc) = (s.weak(), cr(&second));
    first.on_input(move |d| {
        if d == "n" {
            w.upgrade().unwrap().set_focus(Some(sc.clone()));
        }
    });
    let (w, e, b) = (s.weak(), cr(&editor), base.clone());
    second.on_input(move |d| {
        if d == "\r" {
            b.clear();
            b.add_child(e.clone());
            w.upgrade().unwrap().set_focus(Some(e.clone()));
        }
    });

    s.show(cr(&overlay), None);
    s.send("b");
    s.flush();
    s.send("n");
    s.flush();
    s.send("2");
    s.send("\r");
    s.flush();

    assert_eq!(overlay.inputs(), strings(&["b"]));
    assert_eq!(first.inputs(), strings(&["n"]));
    assert_eq!(second.inputs(), strings(&["2", "\r"]));
    assert!(overlay.is_focused());
}

#[test]
fn removed_replacement_restores_overlay_even_when_overlay_pre_focus_differs_from_next_focus() {
    let base = Container::new_ref();
    let editor = Focusable::new("EDITOR");
    let palette = Focusable::new("PALETTE");
    let replacement = Focusable::new("REPLACEMENT");
    let overlay = Focusable::new("OVERLAY");
    base.add_child(cr(&editor));
    base.add_child(cr(&palette));
    base.add_child(cr(&replacement));
    let s = Setup::new(80, 24, Some(base.clone() as ComponentRef), Some(cr(&palette)));
    let (w, r) = (s.weak(), cr(&replacement));
    overlay.on_input(move |d| {
        if d == "b" {
            w.upgrade().unwrap().set_focus(Some(r.clone()));
        }
    });
    let (w, e, b) = (s.weak(), cr(&editor), base.clone());
    replacement.on_input(move |d| {
        if d == "\r" {
            b.clear();
            b.add_child(e.clone());
            w.upgrade().unwrap().set_focus(Some(e.clone()));
        }
    });

    s.show(cr(&overlay), None);
    s.send("b");
    s.flush();
    s.send("\r");
    s.send("x");
    s.flush();

    assert_eq!(overlay.inputs(), strings(&["b", "x"]));
    assert_eq!(replacement.inputs(), strings(&["\r"]));
    assert!(editor.inputs().is_empty());
    assert!(overlay.is_focused());
}

#[test]
fn unfocus_target_releases_a_blocked_overlay_while_replacement_remains_focused() {
    let s = Setup::new(80, 24, None, None);
    let fallback = Focusable::new("FALLBACK");
    let target_component = Focusable::new("TARGET");
    let replacement = Focusable::new("REPLACEMENT");
    let overlay = Focusable::new("OVERLAY");
    let (w, f) = (s.weak(), cr(&fallback));
    replacement.on_input(move |d| {
        if d == "\r" {
            w.upgrade().unwrap().set_focus(Some(f.clone()));
        }
    });
    let overlay_handle = s.show(cr(&overlay), None);
    let (w, r, t, handle) = (s.weak(), cr(&replacement), cr(&target_component), overlay_handle.clone());
    overlay.on_input(move |d| {
        if d == "b" {
            w.upgrade().unwrap().set_focus(Some(r.clone()));
            handle.unfocus(target(Some(t.clone())));
        }
    });

    s.send("b");
    s.flush();
    assert!(replacement.is_focused());
    s.send("\r");
    s.send("x");
    s.flush();

    assert_eq!(overlay.inputs(), strings(&["b"]));
    assert_eq!(replacement.inputs(), strings(&["\r"]));
    assert!(fallback.inputs().is_empty());
    assert_eq!(target_component.inputs(), strings(&["x"]));
}

#[test]
fn handle_input_restores_focus_to_a_visible_focused_overlay_after_base_focus_steal() {
    let (s, editor) = with_editor();
    let replacement = Focusable::new("REPLACEMENT");
    let overlay = Focusable::new("OVERLAY");
    s.show(cr(&overlay), None);
    assert!(overlay.is_focused());
    s.tui().set_focus(Some(cr(&replacement)));
    s.tui().set_focus(Some(cr(&editor)));
    s.send("x");
    s.flush();
    assert_eq!(overlay.inputs(), strings(&["x"]));
    assert!(editor.inputs().is_empty());
    assert!(overlay.is_focused());
}

#[test]
fn handle_input_restores_focus_to_explicitly_focused_raw_sub_overlay_after_base_focus_steal() {
    let (s, editor) = with_editor();
    let controller = Focusable::new("CONTROLLER");
    let sub = Focusable::new("SUB");
    s.show(cr(&controller), None);
    let sub_handle = s.show(cr(&sub), non_capturing());
    sub_handle.focus();
    s.tui().set_focus(Some(cr(&editor)));
    s.send("x");
    s.flush();
    assert_eq!(sub.inputs(), strings(&["x"]));
    assert!(controller.inputs().is_empty());
    assert!(editor.inputs().is_empty());
}

#[test]
fn passive_non_capturing_overlay_does_not_regain_input_after_base_focus() {
    let (s, editor) = with_editor();
    let passive = Focusable::new("PASSIVE");
    s.show(cr(&passive), non_capturing());
    s.send("x");
    s.flush();
    assert_eq!(editor.inputs(), strings(&["x"]));
    assert!(passive.inputs().is_empty());
    assert!(editor.is_focused());
}

#[test]
fn explicitly_focused_non_capturing_overlay_regains_input_after_base_focus_steal() {
    let (s, editor) = with_editor();
    let overlay = Focusable::new("NC");
    let handle = s.show(cr(&overlay), non_capturing());
    handle.focus();
    s.tui().set_focus(Some(cr(&editor)));
    s.send("x");
    s.flush();
    assert_eq!(overlay.inputs(), strings(&["x"]));
    assert!(editor.inputs().is_empty());
}

#[test]
fn unfocus_prevents_visible_overlay_from_regaining_input() {
    let (s, editor) = with_editor();
    let overlay = Focusable::new("OVERLAY");
    let handle = s.show(cr(&overlay), None);
    handle.unfocus(None);
    s.send("x");
    s.flush();
    assert_eq!(editor.inputs(), strings(&["x"]));
    assert!(overlay.inputs().is_empty());
    assert!(editor.is_focused());
}

#[test]
fn set_focus_null_explicitly_clears_visible_overlay_restore() {
    let s = Setup::new(80, 24, None, None);
    let overlay = Focusable::new("OVERLAY");
    s.show(cr(&overlay), None);
    s.tui().set_focus(None);
    s.send("x");
    s.flush();
    assert!(overlay.inputs().is_empty());
    assert!(!overlay.is_focused());
}

#[test]
fn blocked_replacement_set_focus_null_resumes_the_visible_overlay() {
    let s = Setup::new(80, 24, None, None);
    let replacement = Focusable::new("REPLACEMENT");
    let overlay = Focusable::new("OVERLAY");
    let w = s.weak();
    replacement.on_input(move |d| {
        if d == "\r" {
            w.upgrade().unwrap().set_focus(None);
        }
    });
    let (w, r) = (s.weak(), cr(&replacement));
    overlay.on_input(move |d| {
        if d == "b" {
            w.upgrade().unwrap().set_focus(Some(r.clone()));
        }
    });
    s.show(cr(&overlay), None);
    s.send("b");
    s.flush();
    s.send("\r");
    s.send("x");
    s.flush();
    assert_eq!(replacement.inputs(), strings(&["\r"]));
    assert_eq!(overlay.inputs(), strings(&["b", "x"]));
    assert!(overlay.is_focused());
}

#[test]
fn temporarily_invisible_focused_overlay_falls_back_without_losing_restore_eligibility() {
    let (s, editor) = with_editor();
    let overlay = Focusable::new("OVERLAY");
    let visible = Arc::new(AtomicBool::new(true));
    s.show(cr(&overlay), visible_when(&visible));
    s.tui().set_focus(Some(cr(&editor)));
    visible.store(false, Ordering::SeqCst);
    s.send("x");
    s.flush();
    assert_eq!(editor.inputs(), strings(&["x"]));
    assert!(overlay.inputs().is_empty());
    visible.store(true, Ordering::SeqCst);
    s.send("y");
    s.flush();
    assert_eq!(editor.inputs(), strings(&["x"]));
    assert_eq!(overlay.inputs(), strings(&["y"]));
}

#[test]
fn temporarily_invisible_focused_overlay_with_null_pre_focus_restores_when_visible_again() {
    let s = Setup::new(80, 24, None, None);
    let overlay = Focusable::new("OVERLAY");
    let visible = Arc::new(AtomicBool::new(true));
    s.show(cr(&overlay), visible_when(&visible));
    visible.store(false, Ordering::SeqCst);
    s.send("x");
    s.flush();
    assert!(overlay.inputs().is_empty());
    visible.store(true, Ordering::SeqCst);
    s.send("y");
    s.flush();
    assert_eq!(overlay.inputs(), strings(&["y"]));
}

#[test]
fn cyclic_overlay_pre_focus_ancestry_does_not_hang_focus_changes() {
    let editor = Focusable::new("EDITOR");
    let overlay = Focusable::new("OVERLAY");
    let s = Setup::new(80, 24, None, Some(cr(&overlay)));
    let handle = s.show(cr(&overlay), non_capturing());
    handle.focus();
    s.tui().set_focus(Some(cr(&editor)));
    s.send("x");
    s.flush();
    assert_eq!(editor.inputs(), strings(&["x"]));
    assert!(overlay.inputs().is_empty());
}

#[test]
fn handle_input_restores_the_focus_order_top_overlay_after_base_focus_steal() {
    let (s, editor) = with_editor();
    let lower = Focusable::new("LOWER");
    let upper = Focusable::new("UPPER");
    let lower_handle = s.show(cr(&lower), None);
    s.show(cr(&upper), None);
    lower_handle.focus();
    s.tui().set_focus(Some(cr(&editor)));
    s.send("x");
    s.flush();
    assert_eq!(lower.inputs(), strings(&["x"]));
    assert!(upper.inputs().is_empty());
    assert!(editor.inputs().is_empty());
}

#[test]
fn hide_overlay_does_not_reassign_focus_when_topmost_overlay_is_non_capturing() {
    let (s, _editor) = with_editor();
    let capturing = Focusable::new("CAP");
    let nc = Focusable::new("NC");
    s.show(cr(&capturing), None);
    s.show(cr(&nc), non_capturing());
    assert!(capturing.is_focused());
    s.tui().hide_overlay();
    s.flush();
    assert!(capturing.is_focused());
}

#[test]
fn multiple_capturing_and_non_capturing_overlays_restore_focus_through_removals() {
    let (s, editor) = with_editor();
    let c1 = Focusable::new("C1");
    let n1 = Focusable::new("N1");
    let c2 = Focusable::new("C2");
    let n2 = Focusable::new("N2");
    let c1_handle = s.show(cr(&c1), None);
    s.show(cr(&n1), non_capturing());
    let c2_handle = s.show(cr(&c2), None);
    s.show(cr(&n2), non_capturing());
    assert!(c2.is_focused());
    c2_handle.hide();
    s.flush();
    assert!(c1.is_focused());
    c1_handle.hide();
    s.flush();
    assert!(editor.is_focused());
}

#[test]
fn capturing_overlay_unfocus_on_topmost_capturing_overlay_falls_back_to_pre_focus() {
    let (s, editor) = with_editor();
    let capturing = Focusable::new("CAP");
    let handle = s.show(cr(&capturing), None);
    assert!(capturing.is_focused());
    handle.unfocus(None);
    s.flush();
    assert!(editor.is_focused());
    assert!(!capturing.is_focused());
}

// ---- topmost visible overlay --------------------------------------------------------------

#[test]
fn topmost_visible_overlay_is_none_without_overlays() {
    let s = Setup::new(20, 6, None, None);
    assert!(s.tui().get_topmost_visible_overlay().is_none());
}

#[test]
fn topmost_visible_overlay_skips_hidden_non_capturing_and_invisible_overlays() {
    let s = Setup::new(20, 6, None, None);
    let invisible_flag = Arc::new(AtomicBool::new(false));
    let capturing = static_overlay("CAPTURING");
    s.show(static_overlay("NON-CAPTURING"), non_capturing());
    s.show(static_overlay("INVISIBLE"), visible_when(&invisible_flag));
    s.show(capturing.clone(), None);
    let hidden = s.show(static_overlay("HIDDEN"), None);
    hidden.set_hidden(true);
    s.flush();
    let topmost = s.tui().get_topmost_visible_overlay().expect("a visible capturing overlay");
    assert!(std::sync::Arc::ptr_eq(&topmost.component, &capturing));
}

#[test]
fn topmost_visible_overlay_follows_focus_order() {
    let s = Setup::new(20, 6, None, None);
    let lower = static_overlay("LOWER");
    let lower_handle = s.show(lower.clone(), None);
    s.show(static_overlay("UPPER"), None);
    s.flush();
    assert!(!std::sync::Arc::ptr_eq(
        &s.tui().get_topmost_visible_overlay().expect("topmost").component,
        &lower
    ));
    lower_handle.focus();
    s.flush();
    let topmost = s.tui().get_topmost_visible_overlay().expect("topmost");
    assert!(std::sync::Arc::ptr_eq(&topmost.component, &lower));
    assert!(topmost.focus_order > 0);
}

// ---- no-op guards -------------------------------------------------------------------------

#[test]
fn focus_on_hidden_overlay_is_a_no_op() {
    let (s, editor) = with_editor();
    let overlay = Focusable::new("OVERLAY");
    let handle = s.show(cr(&overlay), non_capturing());
    handle.set_hidden(true);
    handle.focus();
    s.flush();
    assert!(editor.is_focused());
    assert!(!handle.is_focused());
}

#[test]
fn focus_after_hide_is_a_no_op() {
    let (s, editor) = with_editor();
    let overlay = Focusable::new("OVERLAY");
    let handle = s.show(cr(&overlay), non_capturing());
    handle.hide();
    handle.focus();
    s.flush();
    assert!(editor.is_focused());
    assert!(!handle.is_focused());
}

#[test]
fn unfocus_when_overlay_does_not_have_focus_is_a_no_op() {
    let (s, editor) = with_editor();
    let overlay = Focusable::new("OVERLAY");
    let handle = s.show(cr(&overlay), non_capturing());
    handle.unfocus(None);
    s.flush();
    assert!(editor.is_focused());
    assert!(!overlay.is_focused());
}

#[test]
fn unfocus_with_null_pre_focus_clears_focus_and_does_not_route_input_back_to_overlay() {
    let s = Setup::new(80, 24, None, None);
    let overlay = Focusable::new("OVERLAY");
    let handle = s.show(cr(&overlay), None);
    assert!(overlay.is_focused());
    handle.unfocus(None);
    assert!(!overlay.is_focused());
    s.send("x");
    s.flush();
    assert!(overlay.inputs().is_empty());
    assert!(!handle.is_focused());
}

// ---- focus cycle prevention ---------------------------------------------------------------

#[test]
fn toggle_focus_between_non_capturing_overlays_then_unfocus_returns_to_editor() {
    let (s, editor) = with_editor();
    let a = Focusable::new("A");
    let b = Focusable::new("B");
    let a_handle = s.show(cr(&a), non_capturing());
    let b_handle = s.show(cr(&b), non_capturing());
    a_handle.focus();
    b_handle.focus();
    a_handle.focus();
    a_handle.unfocus(None);
    s.flush();
    assert!(editor.is_focused());
    assert!(!a.is_focused());
    assert!(!b.is_focused());
}

#[test]
fn explicit_unfocus_target_supports_cycling_between_three_overlays_and_editor() {
    let (s, editor) = with_editor();
    let a = Focusable::new("A");
    let b = Focusable::new("B");
    let c = Focusable::new("C");
    let a_handle = s.show(cr(&a), None);
    let b_handle = s.show(cr(&b), None);
    let c_handle = s.show(cr(&c), None);

    a_handle.focus();
    s.send("a");
    s.flush();
    b_handle.focus();
    s.send("b");
    s.flush();
    c_handle.focus();
    s.send("c");
    s.flush();
    c_handle.unfocus(target(Some(cr(&editor))));
    s.send("e");
    s.flush();
    a_handle.focus();
    s.send("A");
    s.flush();
    a_handle.unfocus(target(Some(cr(&editor))));
    s.send("E");
    s.flush();

    assert_eq!(a.inputs(), strings(&["a", "A"]));
    assert_eq!(b.inputs(), strings(&["b"]));
    assert_eq!(c.inputs(), strings(&["c"]));
    assert_eq!(editor.inputs(), strings(&["e", "E"]));
    assert!(editor.is_focused());
}

#[test]
fn explicit_null_unfocus_target_clears_focus_without_restoring_overlays() {
    let s = Setup::new(80, 24, None, None);
    let overlay = Focusable::new("OVERLAY");
    let handle = s.show(cr(&overlay), None);
    handle.unfocus(target(None));
    s.send("x");
    s.flush();
    assert!(overlay.inputs().is_empty());
    assert!(!handle.is_focused());
}

#[test]
fn hiding_focused_overlay_falls_back_to_next_visual_frontmost_overlay() {
    let (s, _editor) = with_editor();
    let a = Focusable::new("A");
    let b = Focusable::new("B");
    let c = Focusable::new("C");
    let a_handle = s.show(cr(&a), None);
    let b_handle = s.show(cr(&b), None);
    s.show(cr(&c), None);
    a_handle.focus();
    b_handle.focus();
    b_handle.set_hidden(true);
    s.send("x");
    s.flush();
    assert_eq!(a.inputs(), strings(&["x"]));
    assert!(c.inputs().is_empty());
    assert!(a.is_focused());
}

// ---- rendering order ----------------------------------------------------------------------

#[test]
fn focus_on_already_focused_overlay_bumps_visual_order() {
    let editor = Focusable::new("EDITOR");
    let s = Setup::new(20, 6, None, Some(cr(&editor)));
    let a_handle = s.show(static_overlay("A"), corner(true));
    s.show(static_overlay("B"), corner(true));
    a_handle.focus();
    s.show(static_overlay("C"), corner(true));
    s.flush();
    assert_eq!(s.first_char(), Some('C'));
    a_handle.focus();
    s.flush();
    assert_eq!(s.first_char(), Some('A'));
    assert!(a_handle.is_focused());
}

#[test]
fn default_rendering_order_for_overlapping_overlays_follows_creation_order() {
    let s = Setup::new(20, 6, None, None);
    s.show(static_overlay("A"), corner(true));
    s.show(static_overlay("B"), corner(true));
    s.flush();
    assert_eq!(s.first_char(), Some('B'));
}

#[test]
fn focus_on_lower_overlay_renders_it_on_top() {
    let s = Setup::new(20, 6, None, None);
    let lower = s.show(static_overlay("A"), corner(true));
    s.show(static_overlay("B"), corner(true));
    s.flush();
    assert_eq!(s.first_char(), Some('B'));
    lower.focus();
    s.flush();
    assert_eq!(s.first_char(), Some('A'));
}

#[test]
fn focusing_middle_overlay_places_it_on_top_while_preserving_others_relative_order() {
    let s = Setup::new(20, 6, None, None);
    s.show(static_overlay("A"), corner(true));
    let middle = s.show(static_overlay("B"), corner(true));
    let top = s.show(static_overlay("C"), corner(true));
    s.flush();
    assert_eq!(s.first_char(), Some('C'));
    middle.focus();
    s.flush();
    assert_eq!(s.first_char(), Some('B'));
    middle.hide();
    s.flush();
    assert_eq!(s.first_char(), Some('C'));
    top.hide();
    s.flush();
    assert_eq!(s.first_char(), Some('A'));
}

#[test]
fn capturing_overlay_hidden_and_shown_again_renders_on_top_after_unhide() {
    let s = Setup::new(20, 6, None, None);
    s.show(static_overlay("A"), corner(true));
    let capturing = s.show(static_overlay("B"), corner(false));
    s.flush();
    assert_eq!(s.first_char(), Some('B'));
    capturing.set_hidden(true);
    s.show(static_overlay("C"), corner(true));
    s.flush();
    assert_eq!(s.first_char(), Some('C'));
    capturing.set_hidden(false);
    s.flush();
    assert_eq!(s.first_char(), Some('B'));
}

#[test]
fn unfocus_does_not_change_visual_order_until_another_overlay_is_focused() {
    let editor = Focusable::new("EDITOR");
    let s = Setup::new(20, 6, None, Some(cr(&editor)));
    let a = s.show(static_overlay("A"), corner(true));
    let b = s.show(static_overlay("B"), corner(true));
    s.flush();
    assert_eq!(s.first_char(), Some('B'));
    a.focus();
    s.flush();
    assert_eq!(s.first_char(), Some('A'));
    a.unfocus(None);
    s.flush();
    assert_eq!(s.first_char(), Some('A'));
    b.focus();
    s.flush();
    assert_eq!(s.first_char(), Some('B'));
}
