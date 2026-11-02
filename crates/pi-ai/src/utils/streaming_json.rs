//! Incremental, tolerant parsing of JSON text that arrives in pieces, such as streamed tool-call
//! arguments (port of `utils/streaming-json.ts`, upstream #9265).
//!
//! [`StreamingJsonParser::append`] reads each piece once, so a stream costs time linear in its
//! length, and [`StreamingJsonParser::snapshot`] returns the value of the text read so far. The
//! rules for incomplete text follow [`parse_streaming_json`](super::json_parse::parse_streaming_json):
//!
//! - An unterminated string shows the characters read so far; an unterminated escape is held back
//!   until complete.
//! - An unterminated container shows the members and elements read so far.
//! - A member whose key or value has not started is omitted.
//! - An unterminated literal shows its value once it is a prefix of `true`, `false` or `null`; an
//!   unterminated number shows its value when the digits read so far form a number (an exponent
//!   without digits is ignored).
//!
//! Malformed text is read leniently, the way [`repair_json`](super::json_parse::repair_json) and
//! `parse_streaming_json` treat it: raw control characters in strings are kept, an invalid escape
//! such as `\d` stays literal text, missing commas and colons are tolerated, values in invalid
//! positions and unknown words are dropped, and text after the root value is ignored. The parser
//! never fails. Callers that need the exact value of the finished text parse it once, e.g. with
//! [`parse_tool_call_arguments`].
//!
//! `append` also returns the changes the piece made, for a writer that keeps its own copy of the
//! value and applies them instead of re-reading snapshots: [`JsonChange::Set`] when a key, an
//! array item or a finished value appears (a new string is set once with its first text, a
//! container empty), [`JsonChange::Append`] when the piece extended an open string. Unterminated
//! numbers and literals are not reported until they end. A container nested more than 32 levels
//! deep is reported as one `Set` of its whole value when it closes, so paths stay short.
//!
//! Cost: a snapshot is a fresh value that copies the document read so far (unlike JavaScript,
//! Rust cannot share the strings of earlier snapshots), so it costs the bytes read. Each `append`
//! earns copy credit proportional to its length plus a constant, as in TypeScript; `snapshot`
//! spends it. A document up to [`ALWAYS_FRESH_BYTES`] is fresh after every piece. A larger one is
//! copied when the credit pays for it or when [`REFRESH_INTERVAL`] has passed since the last copy,
//! and otherwise `snapshot` returns the previous one: a stream that arrives slower than that
//! interval sees every piece, a fast one sees a preview that lags a little behind the text, and the
//! total work stays linear in the text plus one copy per interval. Once the root value is complete
//! the next snapshot is always exact.
//!
//! Differences from the TypeScript parser, both from Rust values: lone UTF-16 surrogate escapes
//! become U+FFFD, as in [`js_json_parse`](pi_json::js_json_parse), and an escaped high surrogate is
//! held back until the next character shows whether it pairs; containers nested deeper than
//! [`MAX_NESTING`] levels are dropped.

use std::time::Duration;

use serde_json::{Map, Value};
use web_time::Instant;

use crate::types::JsonObject;
use crate::utils::json_parse::{ParsedToolCallArguments, parse_tool_call_arguments};

/// One step of a [`JsonPath`]: a member key or an array index.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum JsonPathSegment {
    Key(String),
    Index(usize),
}

/// A path from the root value: member keys and array indices.
pub type JsonPath = Vec<JsonPathSegment>;

/// One change a piece of text made to the value; see the module documentation.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonChange {
    /// A value appeared or was replaced at `path`.
    Set { path: JsonPath, value: Value },
    /// The open string at `path` grew by `text`.
    Append { path: JsonPath, text: String },
}

/// Where a value sits in its parent.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Slot {
    Root,
    Member(String),
    Element(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Key,
    Colon,
    Value,
    Comma,
}

#[derive(Debug)]
enum Container {
    Object(Map<String, Value>),
    Array(Vec<Value>),
}

impl Container {
    fn is_array(&self) -> bool {
        matches!(self, Container::Array(_))
    }

    fn to_value(&self) -> Value {
        match self {
            Container::Object(members) => Value::Object(members.clone()),
            Container::Array(elements) => Value::Array(elements.clone()),
        }
    }

    fn into_value(self) -> Value {
        match self {
            Container::Object(members) => Value::Object(members),
            Container::Array(elements) => Value::Array(elements),
        }
    }

    fn empty_like(&self) -> Value {
        match self {
            Container::Object(_) => Value::Object(Map::new()),
            Container::Array(_) => Value::Array(Vec::new()),
        }
    }
}

/// An open object or array. Its value is never handed out while open; snapshots get copies.
#[derive(Debug)]
struct Frame {
    container: Container,
    /// Where the container sits; `None` for a container in an invalid position (read, dropped).
    slot: Option<Slot>,
    /// Where the container sits, when its changes are reported.
    path: Option<JsonPath>,
    /// Whether changes inside it are reported; when not, its whole value is reported as it closes.
    reports_inside: bool,
    /// Objects: the key read last, where the member value goes.
    key: Option<String>,
    phase: Phase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Key,
    Value,
}

/// An unterminated string. A value string is placed into its container only when it ends;
/// snapshots add its text so far.
#[derive(Debug)]
struct OpenString {
    target: Target,
    slot: Option<Slot>,
    text: String,
    /// Escape read so far: empty for none, otherwise a backslash and up to five more characters.
    escape: String,
    /// An escaped high surrogate waiting for its low half.
    high_surrogate: Option<u32>,
    /// Where a value string sits, when its changes are reported.
    path: Option<JsonPath>,
    /// Index of the change this string extends, while it is the newest change of the piece.
    change: Option<usize>,
}

/// Changes are reported at most this deep, so a path costs O(1) to build.
const MAX_REPORTED_DEPTH: usize = 32;
/// Copy credit earned per `append` and per appended byte.
const CREDIT_PER_APPEND: usize = 256;
const CREDIT_PER_BYTE: usize = 8;
/// Documents up to this many bytes get a fresh snapshot after every piece.
pub const ALWAYS_FRESH_BYTES: usize = 4 * 1024;
/// A larger document gets a fresh snapshot at least this often while it changes.
pub const REFRESH_INTERVAL: Duration = Duration::from_millis(16);
/// An unterminated number longer than this is not shown until it ends, so `snapshot` stays cheap.
const MAX_PENDING_SCALAR_LENGTH: usize = 64;
/// Containers nested deeper than this are read but dropped: a Rust value that deep would overflow
/// the stack when copied or dropped. `JSON.parse` in pi (serde_json) stops at 128 levels anyway.
pub const MAX_NESTING: usize = 512;

/// Incremental, tolerant JSON parser; see the module documentation for its rules and costs.
#[derive(Debug, Default)]
pub struct StreamingJsonParser {
    /// The root value once it started; a placeholder while the root container is open.
    root: Option<Value>,
    done: bool,
    stack: Vec<Frame>,
    string: Option<OpenString>,
    scalar: String,
    scalar_slot: Option<Slot>,
    /// Bytes read: what a fresh snapshot copies, at most.
    read: usize,
    credit: usize,
    dirty: bool,
    snapshot: Option<Value>,
    /// Bumped whenever `snapshot` takes a fresh copy.
    version: u64,
    /// When the last fresh copy was taken.
    copied_at: Option<Instant>,
    /// Changes of the piece being read.
    changes: Vec<JsonChange>,
}

impl StreamingJsonParser {
    /// A parser that has read nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the next piece of text; returns the changes it made, in order. Costs time linear in
    /// `text.len()`.
    pub fn append(&mut self, text: &str) -> &[JsonChange] {
        self.credit = self
            .credit
            .saturating_add(CREDIT_PER_APPEND + CREDIT_PER_BYTE * text.len());
        self.read += text.len();
        self.changes.clear();
        if let Some(open) = &mut self.string {
            open.change = None;
        }
        let mut index = 0;
        while index < text.len() && !self.done {
            if self.string.is_some() {
                index = self.read_string(text, index);
            } else if !self.scalar.is_empty() {
                index = self.read_scalar(text, index);
            } else {
                let c = text[index..]
                    .chars()
                    .next()
                    .expect("index is on a char boundary");
                index += c.len_utf8();
                self.structural(c);
            }
        }
        &self.changes
    }

    /// The value of the text read so far: `None` before a value starts. Returns the previous
    /// snapshot while the text read since did not change the value, or while a fresh copy is not
    /// yet paid for (see the module documentation).
    pub fn snapshot(&mut self) -> Option<&Value> {
        if let Some(fresh) = self.fresh_snapshot() {
            self.snapshot = fresh;
        }
        self.snapshot.as_ref()
    }

    /// Counts the fresh copies `snapshot` has taken; equal versions mean an unchanged snapshot.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// A fresh snapshot when the value changed and the copy is paid for; `None` otherwise. Unlike
    /// [`Self::snapshot`] it keeps no copy, for callers that take ownership of every snapshot.
    pub fn fresh_snapshot(&mut self) -> Option<Option<Value>> {
        if !self.dirty {
            return None;
        }
        let cost = self.read.max(1);
        // A complete root value is copied once, so the final snapshot is always exact.
        let complete = self.stack.is_empty() && self.string.is_none() && self.scalar.is_empty();
        let paid = complete
            || cost <= ALWAYS_FRESH_BYTES
            || cost <= self.credit
            || self
                .copied_at
                .is_none_or(|at| at.elapsed() >= REFRESH_INTERVAL);
        if !paid {
            return None;
        }
        self.credit = self.credit.saturating_sub(cost);
        self.dirty = false;
        self.version += 1;
        self.copied_at = Some(Instant::now());
        Some(self.copy_open_path())
    }

    /// Read string characters from `start` until the string ends or the text does; returns the
    /// next index.
    fn read_string(&mut self, text: &str, start: usize) -> usize {
        let bytes = text.as_bytes();
        let mut run_start = start;
        let mut index = start;
        while index < bytes.len() {
            if !self.open().escape.is_empty() {
                let c = text[index..]
                    .chars()
                    .next()
                    .expect("index is on a char boundary");
                if self.continue_escape(c) {
                    index += c.len_utf8();
                    run_start = index;
                    continue;
                }
                // Not part of the escape: read it as a normal string character below.
                run_start = index;
            }
            match bytes[index] {
                b'"' => {
                    if index > run_start {
                        self.append_string(&text[run_start..index]);
                    }
                    self.end_string();
                    return index + 1;
                }
                b'\\' => {
                    if index > run_start {
                        self.append_string(&text[run_start..index]);
                    }
                    self.open().escape.push('\\');
                    run_start = index + 1;
                }
                _ => {}
            }
            index += 1;
        }
        if text.len() > run_start {
            self.append_string(&text[run_start..]);
        }
        text.len()
    }

    /// Read number or literal characters from `start` up to the next delimiter; returns the
    /// delimiter's index.
    fn read_scalar(&mut self, text: &str, start: usize) -> usize {
        let end = text[start..]
            .bytes()
            .position(is_delimiter)
            .map_or(text.len(), |offset| start + offset);
        if end > start {
            self.scalar.push_str(&text[start..end]);
            if self.scalar_slot.is_some() {
                self.dirty = true;
            }
        }
        if end < text.len() {
            self.end_scalar();
        }
        end
    }

    /// Handle one character outside strings.
    fn structural(&mut self, c: char) {
        match c {
            ' ' | '\t' | '\n' | '\r' => {}
            '"' => self.start_string(),
            '{' => self.open_container(Container::Object(Map::new())),
            '[' => self.open_container(Container::Array(Vec::new())),
            '}' | ']' => self.close_container(),
            ':' => {
                if let Some(frame) = self.stack.last_mut()
                    && !frame.container.is_array()
                    && frame.phase == Phase::Colon
                {
                    frame.phase = Phase::Value;
                }
            }
            ',' => {
                if let Some(frame) = self.stack.last_mut() {
                    frame.phase = if frame.container.is_array() {
                        Phase::Value
                    } else {
                        Phase::Key
                    };
                }
            }
            other => {
                self.scalar.clear();
                self.scalar.push(other);
                self.scalar_slot = self.value_slot();
                if self.scalar_slot.is_some() {
                    self.dirty = true;
                }
            }
        }
    }

    /// Where a value starting now goes, or `None` when this position takes no value.
    fn value_slot(&self) -> Option<Slot> {
        let Some(frame) = self.stack.last() else {
            return self.root.is_none().then_some(Slot::Root);
        };
        match &frame.container {
            Container::Array(elements) => Some(Slot::Element(elements.len())),
            // A missing colon is tolerated: `{"a" 1}` reads as `{"a": 1}`.
            Container::Object(_) if matches!(frame.phase, Phase::Value | Phase::Colon) => {
                frame.key.clone().map(Slot::Member)
            }
            Container::Object(_) => None,
        }
    }

    fn start_string(&mut self) {
        // A missing comma is tolerated: `{"a": 1 "b": 2}` reads the second member.
        if let Some(frame) = self.stack.last()
            && !frame.container.is_array()
            && matches!(frame.phase, Phase::Key | Phase::Comma)
        {
            self.string = Some(OpenString {
                target: Target::Key,
                slot: None,
                text: String::new(),
                escape: String::new(),
                high_surrogate: None,
                path: None,
                change: None,
            });
            return;
        }
        let slot = self.value_slot();
        let path = self.reported_path(slot.as_ref());
        if let Some(slot) = &slot {
            self.place(slot, Value::String(String::new()));
        }
        let change = path.clone().map(|path| {
            self.report(JsonChange::Set {
                path,
                value: Value::String(String::new()),
            })
        });
        self.string = Some(OpenString {
            target: Target::Value,
            slot,
            text: String::new(),
            escape: String::new(),
            high_surrogate: None,
            path,
            change,
        });
    }

    /// The path of a value at `slot` in the innermost open container, when its changes are
    /// reported.
    fn reported_path(&self, slot: Option<&Slot>) -> Option<JsonPath> {
        let segment = match slot? {
            Slot::Root => return Some(Vec::new()),
            Slot::Member(key) => JsonPathSegment::Key(key.clone()),
            Slot::Element(index) => JsonPathSegment::Index(*index),
        };
        let frame = self.stack.last()?;
        if !frame.reports_inside {
            return None;
        }
        let mut path = frame.path.clone().unwrap_or_default();
        path.push(segment);
        Some(path)
    }

    fn report(&mut self, change: JsonChange) -> usize {
        self.changes.push(change);
        self.changes.len() - 1
    }

    fn open(&mut self) -> &mut OpenString {
        self.string.as_mut().expect("a string is open")
    }

    /// Continue an escape with `c`; returns false when `c` is not part of it and must be read
    /// normally.
    fn continue_escape(&mut self, c: char) -> bool {
        let open = self.open();
        if open.escape == "\\" {
            if let Some(decoded) = simple_escape(c) {
                open.escape.clear();
                self.append_string(decoded);
                return true;
            }
            if c == 'u' {
                open.escape.push('u');
                return true;
            }
            // Invalid escape: keep the backslash as text and read the character normally, as
            // repair_json() does.
            open.escape.clear();
            self.append_string("\\");
            return false;
        }
        if c.is_ascii_hexdigit() {
            open.escape.push(c);
            if open.escape.len() < 6 {
                return true;
            }
            let code = u32::from_str_radix(&open.escape[2..], 16).expect("four hex digits");
            open.escape.clear();
            self.append_code_unit(code);
            return true;
        }
        // `\u` without four hex digits: keep it as text and read the character normally, as
        // repair_json() does.
        let literal = std::mem::take(&mut open.escape);
        self.append_string(&literal);
        false
    }

    /// Append the UTF-16 code unit of a `\u` escape, pairing surrogates.
    fn append_code_unit(&mut self, code: u32) {
        let open = self.open();
        match code {
            0xD800..=0xDBFF => {
                let earlier = open.high_surrogate.replace(code);
                if earlier.is_some() {
                    self.append_text("\u{FFFD}");
                }
            }
            0xDC00..=0xDFFF => match open.high_surrogate.take() {
                Some(high) => {
                    let scalar = 0x10000 + ((high - 0xD800) << 10) + (code - 0xDC00);
                    let c = char::from_u32(scalar).expect("a surrogate pair is a scalar value");
                    self.append_text(c.encode_utf8(&mut [0; 4]));
                }
                None => self.append_string("\u{FFFD}"),
            },
            _ => {
                let c = char::from_u32(code).expect("not a surrogate");
                self.append_string(c.encode_utf8(&mut [0; 4]));
            }
        }
    }

    /// Append decoded text, first resolving a held-back high surrogate as unpaired.
    fn append_string(&mut self, text: &str) {
        if self.open().high_surrogate.take().is_some() {
            self.append_text("\u{FFFD}");
        }
        self.append_text(text);
    }

    fn append_text(&mut self, text: &str) {
        let last_change = self.changes.len().checked_sub(1);
        let open = self.string.as_mut().expect("a string is open");
        open.text.push_str(text);
        if open.target != Target::Value || open.slot.is_none() {
            return;
        }
        self.dirty = true;
        let Some(path) = &open.path else { return };
        // Extend this string's newest change while nothing came after it; otherwise report an
        // append.
        if let Some(index) = open.change
            && Some(index) == last_change
        {
            match &mut self.changes[index] {
                JsonChange::Set {
                    value: Value::String(value),
                    ..
                } => value.push_str(text),
                JsonChange::Append { text: appended, .. } => appended.push_str(text),
                JsonChange::Set { .. } => unreachable!("a string change sets a string"),
            }
            return;
        }
        let change = JsonChange::Append {
            path: path.clone(),
            text: text.to_string(),
        };
        self.changes.push(change);
        open.change = Some(self.changes.len() - 1);
    }

    fn end_string(&mut self) {
        if self.open().high_surrogate.take().is_some() {
            self.append_text("\u{FFFD}");
        }
        let open = self.string.take().expect("a string is open");
        match open.target {
            Target::Key => {
                if let Some(frame) = self.stack.last_mut() {
                    frame.key = Some(open.text);
                    frame.phase = Phase::Colon;
                }
            }
            Target::Value => {
                if let Some(slot) = &open.slot {
                    self.place(slot, Value::String(open.text));
                }
                self.value_ended();
            }
        }
    }

    fn end_scalar(&mut self) {
        let token = std::mem::take(&mut self.scalar);
        let slot = self.scalar_slot.take();
        if let Some(value) = scalar_value(&token) {
            if let Some(slot) = &slot {
                self.place(slot, value.clone());
            }
            if let Some(path) = self.reported_path(slot.as_ref()) {
                self.report(JsonChange::Set { path, value });
            }
        }
        self.value_ended();
    }

    /// The value of the unterminated string or scalar at the end of the text, if it shows.
    fn pending_value(&self) -> Option<(&Slot, Value)> {
        if let Some(open) = &self.string {
            if open.target != Target::Value {
                return None;
            }
            return open
                .slot
                .as_ref()
                .map(|slot| (slot, Value::String(open.text.clone())));
        }
        let slot = self.scalar_slot.as_ref()?;
        if self.scalar.is_empty() || self.scalar.len() > MAX_PENDING_SCALAR_LENGTH {
            return None;
        }
        pending_scalar_value(&self.scalar).map(|value| (slot, value))
    }

    fn open_container(&mut self, container: Container) {
        let slot = if self.stack.len() < MAX_NESTING {
            self.value_slot()
        } else {
            None
        };
        let path = self.reported_path(slot.as_ref());
        if let Some(slot) = &slot {
            // Holds the member or element position until the container closes.
            self.place(slot, Value::Null);
        }
        let reports_inside = path
            .as_ref()
            .is_some_and(|path| path.len() < MAX_REPORTED_DEPTH);
        if let Some(path) = &path {
            // A fresh container: the working one keeps changing, and a change never does.
            self.report(JsonChange::Set {
                path: path.clone(),
                value: container.empty_like(),
            });
        }
        let phase = if container.is_array() {
            Phase::Value
        } else {
            Phase::Key
        };
        self.stack.push(Frame {
            container,
            slot,
            path,
            reports_inside,
            key: None,
            phase,
        });
        self.dirty = true;
    }

    fn close_container(&mut self) {
        // A stray closer outside any container is ignored.
        let Some(frame) = self.stack.pop() else {
            return;
        };
        self.dirty = true;
        // Too deep to report inside: the whole value, final now that it is closed.
        if let Some(path) = &frame.path
            && !frame.reports_inside
        {
            let value = frame.container.to_value();
            self.report(JsonChange::Set {
                path: path.clone(),
                value,
            });
        }
        if let Some(slot) = &frame.slot {
            self.place(slot, frame.container.into_value());
        }
        self.value_ended();
    }

    /// A value in the innermost open container, or the root value when there is none, has ended.
    fn value_ended(&mut self) {
        match self.stack.last_mut() {
            Some(frame) => frame.phase = Phase::Comma,
            // Text after the root value is ignored, but only once a root value exists.
            None => self.done = self.root.is_some(),
        }
    }

    /// Set the value at `slot` in the open path.
    fn place(&mut self, slot: &Slot, value: Value) {
        self.dirty = true;
        match slot {
            Slot::Root => self.root = Some(value),
            Slot::Element(_) | Slot::Member(_) => {
                let frame = self.stack.last_mut().expect("a container is open");
                set_slot(&mut frame.container, slot, value);
            }
        }
    }

    /// A fresh snapshot: copies of the open containers, innermost first, each holding the copy of
    /// its open child.
    fn copy_open_path(&self) -> Option<Value> {
        let mut pending = self.pending_value();
        if self.stack.is_empty() {
            return match pending {
                Some((Slot::Root, value)) => Some(value),
                _ => self.root.clone(),
            };
        }
        let mut child: Option<Value> = None;
        let mut child_slot: Option<&Slot> = None;
        for frame in self.stack.iter().rev() {
            let mut copy = match &frame.container {
                Container::Object(members) => Container::Object(members.clone()),
                Container::Array(elements) => Container::Array(elements.clone()),
            };
            if let Some((slot, value)) = pending.take() {
                set_slot(&mut copy, slot, value);
            }
            if let (Some(value), Some(slot)) = (child.take(), child_slot) {
                set_slot(&mut copy, slot, value);
            }
            child = Some(copy.into_value());
            child_slot = frame.slot.as_ref();
        }
        // The outermost open container is the root unless it sits in an invalid position.
        match child_slot {
            Some(Slot::Root) => child,
            _ => self.root.clone(),
        }
    }
}

/// The arguments of one streaming tool call: the raw text, for the one full parse when the call
/// ends, and a [`StreamingJsonParser`] for the partial arguments in between. Providers keep one
/// per call in their own stream state, never on the content block, so the raw text never reaches
/// partial messages or transcripts.
#[derive(Debug, Default)]
pub struct ToolCallArgumentsStream {
    text: String,
    parser: StreamingJsonParser,
}

impl ToolCallArgumentsStream {
    /// A stream that has read no arguments.
    pub fn new() -> Self {
        Self::default()
    }

    /// The raw argument text received so far.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Append a piece of argument text; returns the new partial arguments when they changed, or
    /// `None` when the previous ones still stand.
    pub fn append(&mut self, delta: &str) -> Option<JsonObject> {
        self.text.push_str(delta);
        self.parser.append(delta);
        self.partial()
    }

    /// Take the provider's complete argument text. When it continues the text so far, returns the
    /// missing piece as the delta; otherwise starts over with `text` and returns no delta. Either
    /// way returns the partial arguments of `text` when they changed.
    pub fn replace(&mut self, text: &str) -> (Option<String>, Option<JsonObject>) {
        if let Some(delta) = text.strip_prefix(self.text.as_str()) {
            let delta = delta.to_string();
            let arguments = if delta.is_empty() {
                self.partial()
            } else {
                self.append(&delta)
            };
            return (Some(delta), arguments);
        }
        self.text.clear();
        self.parser = StreamingJsonParser::new();
        // Starting over always shows the new text's arguments, even when they look the same.
        let arguments = self.append(text).or_else(|| Some(self.current()));
        (None, arguments)
    }

    /// The final arguments: one full parse of the complete text, which reports malformed
    /// arguments.
    pub fn finish(&self) -> ParsedToolCallArguments {
        parse_tool_call_arguments(Some(&self.text))
    }

    fn partial(&mut self) -> Option<JsonObject> {
        let snapshot = self.parser.fresh_snapshot()?;
        // Tool arguments are an object; any other partial value shows as no arguments yet.
        Some(match snapshot {
            Some(Value::Object(members)) => members,
            _ => Map::new(),
        })
    }

    /// The arguments of the text read so far, paid for or not.
    fn current(&self) -> JsonObject {
        match self.parser.copy_open_path() {
            Some(Value::Object(members)) => members,
            _ => Map::new(),
        }
    }
}

fn simple_escape(c: char) -> Option<&'static str> {
    Some(match c {
        '"' => "\"",
        '\\' => "\\",
        '/' => "/",
        'b' => "\u{8}",
        'f' => "\u{c}",
        'n' => "\n",
        'r' => "\r",
        't' => "\t",
        _ => return None,
    })
}

fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\t' | b'\n' | b'\r' | b',' | b':' | b'{' | b'}' | b'[' | b']' | b'"'
    )
}

/// Whether `token` matches `-?(0|[1-9]\d*)(\.\d+)?([eE][+-]?\d+)?` (JSON's number grammar).
fn is_number(token: &str) -> bool {
    number_prefix_end(token)
        .is_some_and(|(end, exponent)| end == token.len() && exponent != Some(false))
}

/// Reads the longest JSON number at the start of `token`. Returns where it ends and whether an
/// exponent was read with digits (`Some(true)`), without (`Some(false)`), or not at all (`None`);
/// `None` when `token` does not start with a mantissa.
fn number_prefix_end(token: &str) -> Option<(usize, Option<bool>)> {
    let bytes = token.as_bytes();
    let digits = |from: usize| {
        bytes[from..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count()
    };
    let mut index = usize::from(bytes.first() == Some(&b'-'));
    match bytes.get(index) {
        Some(b'0') => index += 1,
        Some(b'1'..=b'9') => index += digits(index),
        _ => return None,
    }
    if bytes.get(index) == Some(&b'.') {
        let fraction = digits(index + 1);
        if fraction == 0 {
            return Some((index, None));
        }
        index += 1 + fraction;
    }
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        let mut exponent = index + 1;
        if matches!(bytes.get(exponent), Some(b'+' | b'-')) {
            exponent += 1;
        }
        let count = digits(exponent);
        if count == 0 {
            return Some((exponent, Some(false)));
        }
        return Some((exponent + count, Some(true)));
    }
    Some((index, None))
}

fn parse_number(token: &str) -> Option<Value> {
    serde_json::from_str::<Value>(token)
        .ok()
        .filter(Value::is_number)
}

/// The value of a complete number or literal token; `None` for anything else, which is dropped.
fn scalar_value(token: &str) -> Option<Value> {
    match token {
        "true" => Some(Value::Bool(true)),
        "false" => Some(Value::Bool(false)),
        "null" => Some(Value::Null),
        _ if is_number(token) => parse_number(token),
        _ => None,
    }
}

/// The value an unterminated number or literal token shows.
fn pending_scalar_value(token: &str) -> Option<Value> {
    for (literal, value) in [
        ("true", Value::Bool(true)),
        ("false", Value::Bool(false)),
        ("null", Value::Null),
    ] {
        if literal.starts_with(token) {
            return Some(value);
        }
    }
    if is_number(token) {
        return parse_number(token);
    }
    // A mantissa followed by an exponent marker without digits shows the mantissa.
    match number_prefix_end(token) {
        Some((end, Some(false))) if end == token.len() => {
            let mantissa = token
                .trim_end_matches(['+', '-'])
                .trim_end_matches(['e', 'E']);
            parse_number(mantissa)
        }
        _ => None,
    }
}

fn set_slot(container: &mut Container, slot: &Slot, value: Value) {
    match (container, slot) {
        (Container::Array(elements), Slot::Element(index)) => {
            if *index < elements.len() {
                elements[*index] = value;
            } else {
                elements.push(value);
            }
        }
        (Container::Object(members), Slot::Member(key)) => {
            // Like JSON.parse, a duplicate key keeps its first position and takes the last value.
            members.insert(key.clone(), value);
        }
        _ => {}
    }
}
