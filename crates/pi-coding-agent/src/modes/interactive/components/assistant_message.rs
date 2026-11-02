//! Port of `packages/coding-agent/src/modes/interactive/components/assistant-message.ts`.

use crate::compat::MarkdownTheme;
use crate::core::extensions::types::MarkdownTransformer;
use crate::modes::interactive::components::markdown_transform::{
    CreateMarkdownTransformMessageType, IsStreamingFn, markdown_transform_fn,
};
use crate::modes::interactive::theme::theme::{ThemeColor, get_markdown_theme, theme};
use parking_lot::Mutex;
use pi_ai::types::{AssistantContent, AssistantMessage, StopReason};
use pi_tui::components::markdown::{DefaultTextStyle, Markdown, MarkdownOptions};
use pi_tui::components::mouse_region::MouseRegion;
use pi_tui::components::spacer::Spacer;
use pi_tui::components::text::Text;
use pi_tui::tui::{
    Component, ComponentRef, Container, MouseButton, MouseEvent, MouseEventType, MouseResult,
};
use pi_tui::utils::js_trim;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

pub(crate) const OSC133_ZONE_START: &str = "\x1b]133;A\x07";
pub(crate) const OSC133_ZONE_END: &str = "\x1b]133;B\x07";
pub(crate) const OSC133_ZONE_FINAL: &str = "\x1b]133;C\x07";

/// The kind of content a [`MarkdownSlot`] shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotKind {
    Text,
    Thinking,
}

/// A Markdown block of the last update. The next update reuses it at the same position for the same
/// kind of content, so a streamed answer keeps the blocks it already rendered, also once the stream ends.
struct MarkdownSlot {
    kind: SlotKind,
    markdown: Arc<Markdown>,
}

struct State {
    hide_thinking_block: bool,
    hidden_thinking_label: String,
    output_pad: usize,
    last_message: Option<AssistantMessage>,
    has_tool_calls: bool,
    thinking_visibility_overrides: HashMap<usize, bool>,
    markdown_slots: Vec<MarkdownSlot>,
}

struct Inner {
    weak_self: Weak<Inner>,
    container: Container,
    content_container: Arc<Container>,
    markdown_theme: MarkdownTheme,
    markdown_transformers: Vec<MarkdownTransformer>,
    message_id: Option<String>,
    timestamp: Option<String>,
    /// Read by the Markdown transforms on every render, so a reused component sees a stream end.
    is_streaming: Arc<AtomicBool>,
    st: Mutex<State>,
}

/// Component that renders a complete assistant message
/// TS: `extends Container`
pub struct AssistantMessageComponent {
    inner: Arc<Inner>,
}

fn has_visible(c: &AssistantContent) -> bool {
    match c {
        AssistantContent::Text(t) => !js_trim(&t.text).is_empty(),
        AssistantContent::Thinking(t) => !js_trim(&t.thinking).is_empty(),
        AssistantContent::ToolCall(_) => false,
    }
}

impl AssistantMessageComponent {
    /// Defaults: `hide_thinking_block` false, `markdown_theme` [`get_markdown_theme`],
    /// `hidden_thinking_label` "Thinking...", `output_pad` 1, no transformers.
    #[allow(clippy::too_many_arguments)] // mirrors the TS constructor's positional parameters
    pub fn new(
        message: Option<AssistantMessage>,
        hide_thinking_block: Option<bool>,
        markdown_theme: Option<&MarkdownTheme>,
        hidden_thinking_label: Option<&str>,
        output_pad: Option<i64>,
        markdown_transformers: Option<&[MarkdownTransformer]>,
        message_id: Option<&str>,
        timestamp: Option<&str>,
    ) -> Self {
        let content_container = Arc::new(Container::new());
        let container = Container::new();
        container.add_child(content_container.clone() as ComponentRef);
        let inner = Arc::new_cyclic(|weak| Inner {
            weak_self: weak.clone(),
            container,
            content_container,
            markdown_theme: markdown_theme.cloned().unwrap_or_else(get_markdown_theme),
            markdown_transformers: markdown_transformers.map(<[_]>::to_vec).unwrap_or_default(),
            message_id: message_id.map(str::to_owned),
            timestamp: timestamp.map(str::to_owned),
            is_streaming: Arc::new(AtomicBool::new(false)),
            st: Mutex::new(State {
                hide_thinking_block: hide_thinking_block.unwrap_or(false),
                hidden_thinking_label: hidden_thinking_label.unwrap_or("Thinking...").to_string(),
                output_pad: output_pad.unwrap_or(1).max(0) as usize,
                last_message: None,
                has_tool_calls: false,
                thinking_visibility_overrides: HashMap::new(),
                markdown_slots: Vec::new(),
            }),
        });
        if let Some(message) = message {
            inner.update_content(message, None);
        }
        Self { inner }
    }

    pub fn set_hide_thinking_block(&self, hide: bool) {
        let last = {
            let mut st = self.inner.st.lock();
            st.hide_thinking_block = hide;
            st.thinking_visibility_overrides.clear();
            st.last_message.clone()
        };
        if let Some(message) = last {
            self.inner.update_content(message, None);
        }
    }

    pub fn set_hidden_thinking_label(&self, label: &str) {
        let last = {
            let mut st = self.inner.st.lock();
            st.hidden_thinking_label = label.to_string();
            st.last_message.clone()
        };
        if let Some(message) = last {
            self.inner.update_content(message, None);
        }
    }

    pub fn set_output_pad(&self, padding: i64) {
        let last = {
            let mut st = self.inner.st.lock();
            st.output_pad = padding.max(0) as usize;
            // Markdown takes its padding at construction.
            st.markdown_slots.clear();
            st.last_message.clone()
        };
        if let Some(message) = last {
            self.inner.update_content(message, None);
        }
    }

    /// `is_streaming` defaults to the previous streaming state.
    pub fn update_content(&self, message: AssistantMessage, is_streaming: Option<bool>) {
        self.inner.update_content(message, is_streaming);
    }
}

impl Inner {
    fn update_content(&self, message: AssistantMessage, is_streaming: Option<bool>) {
        if let Some(streaming) = is_streaming {
            self.is_streaming.store(streaming, Ordering::Relaxed);
        }
        let (hide_thinking_block, hidden_label, output_pad, overrides, previous_slots) = {
            let mut st = self.st.lock();
            st.last_message = Some(message.clone());
            (
                st.hide_thinking_block,
                st.hidden_thinking_label.clone(),
                st.output_pad,
                st.thinking_visibility_overrides.clone(),
                std::mem::take(&mut st.markdown_slots),
            )
        };
        let t = theme();
        let streaming_flag = self.is_streaming.clone();
        let is_streaming: IsStreamingFn = Arc::new(move || streaming_flag.load(Ordering::Relaxed));
        let mut slots: Vec<MarkdownSlot> = Vec::new();
        let mut markdown_block =
            |kind: SlotKind, text: String, create: &dyn Fn(String) -> Markdown| -> Arc<Markdown> {
                let markdown = match previous_slots.get(slots.len()) {
                    Some(slot) if slot.kind == kind => {
                        slot.markdown.set_text(text);
                        slot.markdown.clone()
                    }
                    _ => Arc::new(create(text)),
                };
                slots.push(MarkdownSlot {
                    kind,
                    markdown: markdown.clone(),
                });
                markdown
            };

        // Clear content container
        let content = &self.content_container;
        content.clear();

        let has_visible_content = message.content.iter().any(has_visible);
        if has_visible_content {
            content.add_child(Arc::new(Spacer::new(1)));
        }

        // Render content in order
        let mut thinking_run_index = 0usize;
        let mut i = 0usize;
        while i < message.content.len() {
            match &message.content[i] {
                AssistantContent::Text(text) if !js_trim(&text.text).is_empty() => {
                    // Assistant text messages with no background - trim the text
                    // Set paddingY=0 to avoid extra spacing before tool executions
                    let markdown =
                        markdown_block(SlotKind::Text, js_trim(&text.text).to_string(), &|text| {
                            Markdown::new(
                                text,
                                output_pad,
                                0,
                                self.markdown_theme.clone(),
                                None,
                                Some(MarkdownOptions {
                                    transform: Some(markdown_transform_fn(
                                        CreateMarkdownTransformMessageType::Assistant,
                                        is_streaming.clone(),
                                        &self.markdown_transformers,
                                        self.message_id.as_deref(),
                                        self.timestamp.as_deref(),
                                    )),
                                    ..Default::default()
                                }),
                            )
                        });
                    content.add_child(markdown);
                }
                AssistantContent::Thinking(_) => {
                    let mut thinking_blocks: Vec<String> = Vec::new();
                    while i < message.content.len() {
                        let AssistantContent::Thinking(thinking_content) = &message.content[i] else {
                            break;
                        };
                        let thinking = js_trim(&thinking_content.thinking);
                        if !thinking.is_empty() {
                            thinking_blocks.push(thinking.to_string());
                        }
                        i += 1;
                    }
                    // `i` is the index after the run; the loop increment below is skipped.
                    if thinking_blocks.is_empty() {
                        continue;
                    }

                    // Add spacing only when another visible assistant content block follows.
                    // This avoids a superfluous blank line before separately-rendered tool execution blocks.
                    let has_visible_content_after = message.content[i..].iter().any(has_visible);

                    let run_index = thinking_run_index;
                    thinking_run_index += 1;
                    let hidden = overrides.get(&run_index).copied().unwrap_or(hide_thinking_block);
                    let thinking_component: ComponentRef = if hidden {
                        Arc::new(Text::new(
                            t.italic(&t.fg(&ThemeColor::ThinkingText, &hidden_label)),
                            output_pad,
                            0,
                            None,
                        ))
                    } else {
                        markdown_block(
                            SlotKind::Thinking,
                            thinking_blocks.join("\n\n"),
                            &|thinking| {
                                Markdown::new(
                                    thinking,
                                    output_pad,
                                    0,
                                    self.markdown_theme.clone(),
                                    Some(DefaultTextStyle {
                                        color: Some(Arc::new(|text: &str| {
                                            theme().fg(&ThemeColor::ThinkingText, text)
                                        })),
                                        italic: true,
                                        ..Default::default()
                                    }),
                                    Some(MarkdownOptions {
                                        transform: Some(markdown_transform_fn(
                                            CreateMarkdownTransformMessageType::AssistantThinking,
                                            is_streaming.clone(),
                                            &self.markdown_transformers,
                                            self.message_id.as_deref(),
                                            self.timestamp.as_deref(),
                                        )),
                                        ..Default::default()
                                    }),
                                )
                            },
                        )
                    };
                    let weak = self.weak_self.clone();
                    content.add_child(Arc::new(MouseRegion::new(
                        thinking_component,
                        Arc::new(move |event: &MouseEvent| {
                            if event.kind != MouseEventType::Click || event.button != MouseButton::Left {
                                return None;
                            }
                            let inner = weak.upgrade()?;
                            let last = {
                                let mut st = inner.st.lock();
                                st.thinking_visibility_overrides.insert(run_index, !hidden);
                                st.last_message.clone()
                            };
                            if let Some(message) = last {
                                inner.update_content(message, None);
                            }
                            Some(MouseResult::handled())
                        }),
                    )));
                    if has_visible_content_after {
                        content.add_child(Arc::new(Spacer::new(1)));
                    }
                    continue;
                }
                _ => {}
            }
            i += 1;
        }

        // Check if incomplete/failed - show after partial content.
        // For aborted/error tool calls, tool execution components show the error.
        // Length stops can happen before a tool call is complete, so surface them here too.
        let has_tool_calls = message.content.iter().any(|c| matches!(c, AssistantContent::ToolCall(_)));
        {
            let mut st = self.st.lock();
            st.has_tool_calls = has_tool_calls;
            st.markdown_slots = slots;
        }
        if message.stop_reason == StopReason::Length {
            content.add_child(Arc::new(Spacer::new(1)));
            content.add_child(Arc::new(Text::new(
                t.fg(&ThemeColor::Error, "Response was truncated before completion."),
                output_pad,
                0,
                None,
            )));
        } else if !has_tool_calls {
            if message.stop_reason == StopReason::Aborted {
                let abort_message = match message.error_message.as_deref() {
                    Some(m) if !m.is_empty() && m != "Request was aborted" => m.to_string(),
                    _ => "Operation aborted".to_string(),
                };
                content.add_child(Arc::new(Spacer::new(1)));
                content.add_child(Arc::new(Text::new(t.fg(&ThemeColor::Error, &abort_message), output_pad, 0, None)));
            } else if message.stop_reason == StopReason::Error {
                let error_msg = match message.error_message.as_deref() {
                    Some(m) if !m.is_empty() => m,
                    _ => "Unknown error",
                };
                content.add_child(Arc::new(Spacer::new(1)));
                content.add_child(Arc::new(Text::new(
                    t.fg(&ThemeColor::Error, &format!("Error: {error_msg}")),
                    output_pad,
                    0,
                    None,
                )));
            }
        }
    }
}

impl Component for AssistantMessageComponent {
    fn render(&self, width: usize) -> Vec<String> {
        let mut lines = self.inner.container.render(width);
        if self.inner.st.lock().has_tool_calls || lines.is_empty() {
            return lines;
        }
        lines[0] = format!("{OSC133_ZONE_START}{}", lines[0]);
        let last = lines.len() - 1;
        lines[last] = format!("{OSC133_ZONE_END}{OSC133_ZONE_FINAL}{}", lines[last]);
        lines
    }

    fn invalidate(&self) {
        self.inner.container.invalidate();
        let last = self.inner.st.lock().last_message.clone();
        if let Some(message) = last {
            self.inner.update_content(message, None);
        }
    }

    fn handle_mouse(&self, event: &MouseEvent) -> Option<MouseResult> {
        self.inner.container.handle_mouse(event)
    }

    fn child_components(&self) -> Option<Vec<ComponentRef>> {
        self.inner.container.child_components()
    }
}
