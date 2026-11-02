//! Multi-line text with word wrapping. Port of `components/text.ts`.

use std::sync::{Arc, Mutex, MutexGuard};

use crate::StyleFn;
use crate::tui::Component;
use crate::utils::{apply_background_to_line, js_trim, visible_width, wrap_text_with_ansi};

struct State {
    text: String,
    padding_x: usize,
    padding_y: usize,
    custom_bg_fn: Option<StyleFn>,
    cache: Option<(String, usize, Vec<String>)>,
}

/// Displays multi-line text with word wrapping, padding and an optional background.
pub struct Text {
    st: Mutex<State>,
}

impl Default for Text {
    fn default() -> Self {
        Self::new("", 1, 1, None)
    }
}

impl Text {
    pub fn new(text: impl Into<String>, padding_x: usize, padding_y: usize, custom_bg_fn: Option<StyleFn>) -> Self {
        Self { st: Mutex::new(State { text: text.into(), padding_x, padding_y, custom_bg_fn, cache: None }) }
    }

    /// `Text::new` wrapped in an `Arc`.
    pub fn new_ref(text: impl Into<String>, padding_x: usize, padding_y: usize, custom_bg_fn: Option<StyleFn>) -> Arc<Self> {
        Arc::new(Self::new(text, padding_x, padding_y, custom_bg_fn))
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn text(&self) -> String {
        self.lock().text.clone()
    }

    pub fn set_text(&self, text: impl Into<String>) {
        let text = text.into();
        let mut s = self.lock();
        // Renderers often set the same text on every update; keep the wrapped lines then.
        if s.text == text {
            return;
        }
        s.text = text;
        s.cache = None;
    }

    pub fn set_custom_bg_fn(&self, f: Option<StyleFn>) {
        let mut s = self.lock();
        s.custom_bg_fn = f;
        s.cache = None;
    }

    pub fn set_padding_x(&self, padding_x: usize) {
        let mut s = self.lock();
        s.padding_x = padding_x;
        s.cache = None;
    }

    pub fn set_padding_y(&self, padding_y: usize) {
        let mut s = self.lock();
        s.padding_y = padding_y;
        s.cache = None;
    }
}

impl Component for Text {
    fn render(&self, width: usize) -> Vec<String> {
        let mut s = self.lock();
        if let Some((text, w, lines)) = &s.cache
            && *text == s.text && *w == width
        {
            return lines.clone();
        }
        if s.text.is_empty() || js_trim(&s.text).is_empty() {
            s.cache = Some((s.text.clone(), width, Vec::new()));
            return Vec::new();
        }
        let normalized = s.text.replace('\t', "   ");
        let padding_x = s.padding_x.min(width.saturating_sub(1) / 2);
        let content_width = width.saturating_sub(padding_x * 2).max(1);
        let wrapped = wrap_text_with_ansi(&normalized, content_width);
        let margin = " ".repeat(padding_x);
        let mut content_lines = Vec::with_capacity(wrapped.len());
        for line in wrapped {
            let with_margins = format!("{margin}{line}{margin}");
            if let Some(bg) = &s.custom_bg_fn {
                content_lines.push(apply_background_to_line(&with_margins, width, bg.as_ref()));
            } else {
                let pad = width.saturating_sub(visible_width(&with_margins));
                content_lines.push(format!("{with_margins}{}", " ".repeat(pad)));
            }
        }
        let empty_line = " ".repeat(width);
        let mut empty_lines = Vec::new();
        for _ in 0..s.padding_y {
            empty_lines.push(match &s.custom_bg_fn {
                Some(bg) => apply_background_to_line(&empty_line, width, bg.as_ref()),
                None => empty_line.clone(),
            });
        }
        let mut result = empty_lines.clone();
        result.extend(content_lines);
        result.extend(empty_lines);
        s.cache = Some((s.text.clone(), width, result.clone()));
        if result.is_empty() { vec![String::new()] } else { result }
    }

    fn invalidate(&self) {
        self.lock().cache = None;
    }
}
