//! ANSI-aware text measurement, wrapping, truncation and slicing.
//!
//! Port of `packages/tui/src/utils.ts`. All positions are byte offsets into UTF-8 strings;
//! widths are terminal columns.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::LazyLock;

use unicode_segmentation::UnicodeSegmentation;

use crate::unicode_tables as tables;

// ---------------------------------------------------------------------------
// Unicode property helpers
// ---------------------------------------------------------------------------

fn in_table(table: &[(u32, u32)], cp: u32) -> bool {
    table
        .binary_search_by(|&(lo, hi)| {
            if cp < lo {
                std::cmp::Ordering::Greater
            } else if cp > hi {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

/// East Asian width of a code point (2 for Wide/Fullwidth, 1 otherwise), matching
/// `get-east-asian-width` with `ambiguousAsWide: false`.
pub fn east_asian_width(cp: u32) -> usize {
    if in_table(tables::EAST_ASIAN_WIDE, cp) { 2 } else { 1 }
}

fn is_mark(c: char) -> bool {
    in_table(tables::MARK, c as u32)
}

fn is_non_printing(c: char) -> bool {
    in_table(tables::NON_PRINTING, c as u32)
}

fn is_terminal_spacing_mark(c: char) -> bool {
    in_table(tables::TERMINAL_SPACING_MARK, c as u32)
}

fn is_zero_width_char(c: char) -> bool {
    in_table(tables::ZERO_WIDTH, c as u32)
}

/// True if the character is a letter or a number (`[\p{L}\p{N}]`).
pub fn is_letter_or_number(c: char) -> bool {
    in_table(tables::LETTER_OR_NUMBER, c as u32)
}

/// Equivalent of `cjkBreakRegex.test(s)`: the string contains a Han, Hiragana, Katakana,
/// Hangul or Bopomofo character (by script extension).
pub fn contains_cjk_break(s: &str) -> bool {
    s.chars().any(is_cjk_break_char)
}

/// True if the character belongs to a CJK script (see [`contains_cjk_break`]).
pub fn is_cjk_break_char(c: char) -> bool {
    in_table(tables::CJK_BREAK, c as u32)
}

/// Equivalent of `cjkPunctuationRegex` for a single character.
pub fn is_cjk_punctuation_char(c: char) -> bool {
    in_table(tables::CJK_PUNCTUATION, c as u32)
}

/// Equivalent of `autocompleteSeparatorRegex` for a single character: whitespace or CJK punctuation.
pub fn is_autocomplete_separator_char(c: char) -> bool {
    is_whitespace_ch(c) || is_cjk_punctuation_char(c)
}

/// Equivalent of `autocompleteSeparatorRegex.test(s)`.
pub fn contains_autocomplete_separator(s: &str) -> bool {
    s.chars().any(is_autocomplete_separator_char)
}

/// JavaScript `\s` for a single character.
pub fn is_whitespace_ch(c: char) -> bool {
    in_table(tables::WHITESPACE, c as u32)
}

/// Check if a string contains a whitespace character (JavaScript `/\s/.test`).
pub fn is_whitespace_char(s: &str) -> bool {
    s.chars().any(is_whitespace_ch)
}

/// Characters matched by `PUNCTUATION_REGEX`.
pub const PUNCTUATION_CHARS: &str = "(){}[]<>.,;:'\"!?+-=*/\\|&%^$#@~`";

/// True for a single ASCII punctuation character from [`PUNCTUATION_CHARS`].
pub fn is_punctuation_ch(c: char) -> bool {
    PUNCTUATION_CHARS.contains(c)
}

/// Check if a string contains a punctuation character (`PUNCTUATION_REGEX.test`).
pub fn is_punctuation_char(s: &str) -> bool {
    s.chars().any(is_punctuation_ch)
}

/// JavaScript `String.prototype.trimEnd`.
pub fn js_trim_end(s: &str) -> &str {
    s.trim_end_matches(is_whitespace_ch)
}

/// JavaScript `String.prototype.trimStart`.
pub fn js_trim_start(s: &str) -> &str {
    s.trim_start_matches(is_whitespace_ch)
}

/// JavaScript `String.prototype.trim`.
pub fn js_trim(s: &str) -> &str {
    s.trim_matches(is_whitespace_ch)
}

/// Number of UTF-16 code units in a string (JavaScript `.length`).
pub fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

// ---------------------------------------------------------------------------
// Segmentation
// ---------------------------------------------------------------------------

/// Split text into extended grapheme clusters (equivalent to `Intl.Segmenter` with
/// `granularity: "grapheme"`).
pub fn graphemes(text: &str) -> impl Iterator<Item = &str> {
    text.graphemes(true)
}

/// Grapheme clusters with their byte offsets.
pub fn grapheme_indices(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.grapheme_indices(true)
}

/// A segment produced by the word segmenter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WordSegment<'a> {
    pub segment: &'a str,
    /// Byte offset of the segment in the input.
    pub index: usize,
    /// Whether the segment is a word (contains letters or numbers).
    pub is_word_like: bool,
}

/// Split text at word boundaries (equivalent to `Intl.Segmenter` with `granularity: "word"`).
///
/// Emulates ICU's word break iterator as used by V8:
/// - Han/Hiragana/Katakana runs are segmented with ICU's CJ dictionary (via ICU4X),
/// - Hangul runs form single words, separated from neighbouring scripts,
/// - Thai/Lao/Khmer/Myanmar letters behave like letters for the UAX #29 rules and
///   are split internally with ICU's dictionaries,
/// - everything else follows UAX #29.
///
/// Results can still differ from V8 in rare cases because ICU4X and ICU4C use
/// slightly different dictionary heuristics.
pub fn word_segments(text: &str) -> Vec<WordSegment<'_>> {
    let mut out: Vec<WordSegment<'_>> = Vec::new();
    let mut pos = 0;
    while pos < text.len() {
        let first = text[pos..].chars().next().unwrap();
        let class = dictionary_class(first);
        let mut end = pos + first.len_utf8();
        for c in text[end..].chars() {
            let same = match dictionary_class(c) {
                DictClass::None => class == DictClass::None || is_extend_char(c),
                other => other == class,
            };
            if !same {
                break;
            }
            end += c.len_utf8();
        }
        match class {
            DictClass::None => push_rule_segments(text, pos, end, &mut out),
            DictClass::Hangul => push_segment(text, pos, end, &mut out),
            DictClass::Cj => push_dictionary_segments(text, pos, end, &mut out),
        }
        pos = end;
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DictClass {
    None,
    Cj,
    Hangul,
}

fn dictionary_class(c: char) -> DictClass {
    let cp = c as u32;
    if matches!(cp,
        0x3005 | 0x3007 | 0x3021..=0x3029 | 0x3038..=0x303B
        | 0x3041..=0x3096 | 0x309D..=0x309F | 0x30A1..=0x30FA | 0x30FC..=0x30FF | 0x31F0..=0x31FF
        | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0xFF66..=0xFF9D | 0x20000..=0x3FFFF)
    {
        DictClass::Cj
    } else if matches!(cp, 0x1100..=0x11FF | 0x3131..=0x318E | 0xA960..=0xA97C | 0xAC00..=0xD7A3 | 0xD7B0..=0xD7FB | 0xFFA0..=0xFFDC) {
        DictClass::Hangul
    } else {
        DictClass::None
    }
}

/// Word_Break=Extend/Format/ZWJ approximation (combining marks, joiners, selectors).
fn is_extend_char(c: char) -> bool {
    let cp = c as u32;
    in_table(tables::MARK, cp) || matches!(cp, 0x200C | 0x200D | 0xFE00..=0xFE0F | 0xE0100..=0xE01EF | 0x3099 | 0x309A)
}

/// Letters of South-East Asian scripts handled by ICU's dictionary break engines.
fn is_complex_context_letter(c: char) -> bool {
    matches!(c as u32,
        0x0E01..=0x0E30 | 0x0E32..=0x0E33 | 0x0E40..=0x0E46 | 0x0E81..=0x0EB0 | 0x0EB2..=0x0EB3 | 0x0EBD..=0x0EC6
        | 0x0EDC..=0x0EDF | 0x1000..=0x102A | 0x103F..=0x1055 | 0x105A..=0x105D | 0x1061 | 0x1065..=0x1066
        | 0x106E..=0x1070 | 0x1075..=0x1081 | 0x108E | 0x1780..=0x17B3 | 0x17D7 | 0x17DC
        | 0x1A20..=0x1A54 | 0xA9E0..=0xA9E4 | 0xA9E6..=0xA9EF | 0xA9FA..=0xA9FE | 0xAA60..=0xAA7A | 0xAA7E..=0xAAAF
        | 0xAAB1 | 0xAAB5..=0xAAB6 | 0xAAB9..=0xAABD | 0xAAC0 | 0xAAC2 | 0xAADB..=0xAADD)
}

fn push_segment<'a>(text: &'a str, start: usize, end: usize, out: &mut Vec<WordSegment<'a>>) {
    let segment = &text[start..end];
    out.push(WordSegment { segment, index: start, is_word_like: segment.chars().any(is_letter_or_number) });
}

fn push_dictionary_segments<'a>(text: &'a str, start: usize, end: usize, out: &mut Vec<WordSegment<'a>>) {
    let run = &text[start..end];
    let mut prev = 0;
    for b in DICTIONARY_SEGMENTER.segment_str(run) {
        if b == 0 {
            continue;
        }
        push_segment(text, start + prev, start + b, out);
        prev = b;
    }
}

/// Interior dictionary break offsets of `run`, shifted by `base`.
fn dictionary_cuts(run: &str, base: usize, cuts: &mut Vec<usize>) {
    for b in DICTIONARY_SEGMENTER.segment_str(run) {
        if b > 0 && b < run.len() {
            cuts.push(base + b);
        }
    }
}

/// UAX #29 segmentation where complex-context letters count as letters, followed by
/// dictionary splitting inside runs of those letters.
fn push_rule_segments<'a>(text: &'a str, start: usize, end: usize, out: &mut Vec<WordSegment<'a>>) {
    let stretch = &text[start..end];
    if !stretch.chars().any(is_complex_context_letter) {
        for (i, segment) in stretch.split_word_bound_indices() {
            push_segment(text, start + i, start + i + segment.len(), out);
        }
        return;
    }
    // Replace complex-context letters with a same-length ALetter so the UAX #29 rules
    // treat them as letters; byte offsets stay aligned with the original text.
    let substituted: String =
        stretch.chars().map(|c| if is_complex_context_letter(c) && c.len_utf8() == 3 { '\u{2C00}' } else { c }).collect();
    for (i, segment) in substituted.split_word_bound_indices() {
        let seg_start = start + i;
        let original = &text[seg_start..seg_start + segment.len()];
        let mut cuts: Vec<usize> = Vec::new();
        let mut run_start: Option<usize> = None;
        let mut offset = 0;
        for c in original.chars() {
            if is_complex_context_letter(c) || (run_start.is_some() && is_extend_char(c)) {
                run_start.get_or_insert(offset);
            } else if let Some(rs) = run_start.take() {
                dictionary_cuts(&original[rs..offset], rs, &mut cuts);
            }
            offset += c.len_utf8();
        }
        if let Some(rs) = run_start {
            dictionary_cuts(&original[rs..], rs, &mut cuts);
        }
        let mut prev = 0;
        for cut in cuts.into_iter().chain(std::iter::once(original.len())) {
            push_segment(text, seg_start + prev, seg_start + cut, out);
            prev = cut;
        }
    }
}

static DICTIONARY_SEGMENTER: LazyLock<icu_segmenter::WordSegmenterBorrowed<'static>> = LazyLock::new(|| {
    icu_segmenter::WordSegmenter::new_dictionary(icu_segmenter::options::WordBreakInvariantOptions::default())
});

// ---------------------------------------------------------------------------
// Width calculation
// ---------------------------------------------------------------------------

/// Fast heuristic to avoid the RGI emoji test for most graphemes.
fn could_be_emoji(segment: &str) -> bool {
    let Some(first) = segment.chars().next() else {
        return false;
    };
    let cp = first as u32;
    (0x1f000..=0x1fbff).contains(&cp)
        || (0x2300..=0x23ff).contains(&cp)
        || (0x2600..=0x27bf).contains(&cp)
        || (0x2b50..=0x2b55).contains(&cp)
        || segment.contains('\u{FE0F}')
        || utf16_len(segment) > 2
}

fn is_emoji_modifier(c: char) -> bool {
    ('\u{1F3FB}'..='\u{1F3FF}').contains(&c)
}

fn is_regional_indicator(c: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
}

/// One element of an emoji sequence: a basic emoji, an emoji with VS16, or a modifier sequence.
fn is_rgi_element(chars: &[char]) -> bool {
    match chars {
        [c] => in_table(tables::RGI_SINGLE, *c as u32) || in_table(tables::RGI_WITH_FE0F, *c as u32),
        [c, '\u{FE0F}'] => in_table(tables::RGI_WITH_FE0F, *c as u32) || in_table(tables::RGI_SINGLE, *c as u32),
        [c, m] if is_emoji_modifier(*m) => in_table(tables::RGI_MODIFIER_BASE, *c as u32),
        [c, m, '\u{FE0F}'] if is_emoji_modifier(*m) => in_table(tables::RGI_MODIFIER_BASE, *c as u32),
        _ => false,
    }
}

/// Approximation of `/^\p{RGI_Emoji}$/v`. Basic emoji, VS16 sequences, keycaps, modifier
/// sequences and flags use tables generated from V8; ZWJ and tag sequences are matched
/// structurally.
fn is_rgi_emoji(segment: &str) -> bool {
    let chars: Vec<char> = segment.chars().collect();
    match chars.as_slice() {
        [] => false,
        [c] => in_table(tables::RGI_SINGLE, *c as u32),
        [c, '\u{FE0F}'] => in_table(tables::RGI_WITH_FE0F, *c as u32),
        [c, '\u{FE0F}', '\u{20E3}'] => in_table(tables::RGI_KEYCAP_BASE, *c as u32),
        [a, b] if is_regional_indicator(*a) && is_regional_indicator(*b) => {
            let code: String = [*a, *b]
                .iter()
                .map(|c| char::from(b'A' + (*c as u32 - 0x1F1E6) as u8))
                .collect();
            tables::RGI_FLAGS.contains(&code.as_str())
        }
        [c, m] if is_emoji_modifier(*m) => in_table(tables::RGI_MODIFIER_BASE, *c as u32),
        ['\u{1F3F4}', rest @ ..] if rest.last() == Some(&'\u{E007F}') => {
            let tag: String = rest[..rest.len() - 1]
                .iter()
                .filter_map(|c| {
                    let v = *c as u32;
                    if (0xE0020..=0xE007E).contains(&v) {
                        char::from_u32(v - 0xE0000)
                    } else {
                        None
                    }
                })
                .collect();
            tag.len() == rest.len() - 1 && matches!(tag.as_str(), "gbeng" | "gbsct" | "gbwls")
        }
        _ => {
            if !chars.contains(&'\u{200D}') {
                return false;
            }
            chars
                .split(|c| *c == '\u{200D}')
                .all(|part| !part.is_empty() && is_rgi_element(part))
        }
    }
}

/// Calculate the terminal width of a single grapheme cluster.
pub fn grapheme_width(segment: &str) -> usize {
    let bytes = segment.as_bytes();
    if bytes.len() == 1 {
        let code = bytes[0];
        if (0x20..=0x7e).contains(&code) {
            return 1;
        }
        if code == 0x09 {
            return 3;
        }
    }
    if segment.is_empty() {
        return 0;
    }

    // Some marks occupy cells even without a base character.
    if segment.chars().all(is_terminal_spacing_mark) {
        return segment.chars().count();
    }

    // Zero-width clusters
    if segment.chars().all(is_zero_width_char) {
        return 0;
    }

    // Emoji check with pre-filter
    if could_be_emoji(segment) && is_rgi_emoji(segment) {
        return 2;
    }

    // Get base visible codepoint
    let base = segment.trim_start_matches(is_non_printing);
    let mut chars = base.chars();
    let Some(first) = chars.next() else {
        return 0;
    };
    let cp = first as u32;

    // Regional indicator symbols are often rendered as full-width emoji in terminals.
    if (0x1f1e6..=0x1f1ff).contains(&cp) {
        return 2;
    }

    let mut width = east_asian_width(cp);

    // Count trailing visible code points that terminals may allocate cells for.
    let mut follows_mark = false;
    for ch in chars {
        if is_terminal_spacing_mark(ch) {
            width += 1;
            follows_mark = false;
        } else if is_mark(ch) {
            follows_mark = true;
        } else if !is_non_printing(ch) {
            let c = ch as u32;
            if follows_mark || (0xff00..=0xffef).contains(&c) {
                width += east_asian_width(c);
            } else if c == 0x0e33 || c == 0x0eb3 {
                width += 1;
            }
            follows_mark = false;
        }
    }

    width
}

const WIDTH_CACHE_SIZE: usize = 512;

thread_local! {
    static WIDTH_CACHE: RefCell<(HashMap<String, usize>, VecDeque<String>)> =
        RefCell::new((HashMap::new(), VecDeque::new()));
}

fn is_printable_ascii(s: &str) -> bool {
    s.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

/// Width of a string made of printable ASCII, tabs, and ANSI escape sequences, or `None` if
/// it contains anything else.
fn ascii_visible_width(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut width = 0;
    let mut i = 0;
    while i < bytes.len() {
        let code = bytes[i];
        if (0x20..=0x7e).contains(&code) {
            width += 1;
            i += 1;
        } else if code == 0x09 {
            width += 3;
            i += 1;
        } else if code == 0x1b {
            let length = ansi_code_length(s, i);
            if length == 0 {
                return None;
            }
            i += length;
        } else {
            return None;
        }
    }
    Some(width)
}

/// Calculate the visible width of a string in terminal columns.
pub fn visible_width(s: &str) -> usize {
    if s.is_empty() {
        return 0;
    }
    if let Some(w) = ascii_visible_width(s) {
        return w;
    }

    if let Some(w) = WIDTH_CACHE.with(|c| c.borrow().0.get(s).copied()) {
        return w;
    }

    let mut clean: std::borrow::Cow<str> = std::borrow::Cow::Borrowed(s);
    if s.contains('\t') {
        clean = std::borrow::Cow::Owned(s.replace('\t', "   "));
    }
    if clean.contains('\x1b') {
        let src = clean.as_ref();
        let bytes = src.as_bytes();
        let mut stripped = String::with_capacity(src.len());
        let mut copy_from = 0;
        let mut idx = 0;
        while let Some(off) = src[idx..].find('\x1b') {
            let esc = idx + off;
            let length = ansi_code_length(src, esc);
            if length > 0 {
                stripped.push_str(&src[copy_from..esc]);
                idx = esc + length;
                copy_from = idx;
            } else {
                idx = esc + 1;
            }
            if idx >= bytes.len() {
                break;
            }
        }
        stripped.push_str(&src[copy_from..]);
        clean = std::borrow::Cow::Owned(stripped);
    }

    let width: usize = clean.graphemes(true).map(grapheme_width).sum();

    WIDTH_CACHE.with(|c| {
        let mut c = c.borrow_mut();
        let (map, order) = &mut *c;
        if map.len() >= WIDTH_CACHE_SIZE
            && let Some(first) = order.pop_front()
        {
            map.remove(&first);
        }
        if map.insert(s.to_string(), width).is_none() {
            order.push_back(s.to_string());
        }
    });
    width
}

/// Remove ANSI, OSC, and APC control sequences while preserving visible text.
pub fn strip_terminal_sequences(s: &str) -> String {
    if !s.contains('\x1b') {
        return s.to_string();
    }
    let mut result = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let len = ansi_code_length(s, i);
        if len > 0 {
            i += len;
            continue;
        }
        let ch = s[i..].chars().next().unwrap();
        result.push(ch);
        i += ch.len_utf8();
    }
    result
}

/// Find the end of the next run of text that contains no escape sequence (starting at `i`).
fn text_run_end(line: &str, i: usize) -> usize {
    let bytes = line.as_bytes();
    let mut end = i;
    while end < bytes.len() {
        if bytes[end] == 0x1b && ansi_code_length(line, end) > 0 {
            break;
        }
        end += 1;
    }
    // `end` always lands on a char boundary because ESC is ASCII and we only stop there.
    end
}

/// Terminal-cell range occupied by a grapheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphemeCellRange {
    pub start: usize,
    pub end: usize,
}

/// Return the terminal-cell range occupied by the grapheme at a visible column.
pub fn get_grapheme_cell_range(line: &str, column: usize) -> Option<GraphemeCellRange> {
    let mut current_col = 0;
    let mut i = 0;
    while i < line.len() {
        let len = ansi_code_length(line, i);
        if len > 0 {
            i += len;
            continue;
        }
        let text_end = text_run_end(line, i);
        for segment in line[i..text_end].graphemes(true) {
            let width = grapheme_width(segment);
            if width > 0 && column >= current_col && column < current_col + width {
                return Some(GraphemeCellRange { start: current_col, end: current_col + width });
            }
            current_col += width;
        }
        i = text_end;
    }
    None
}

fn parse_osc8_url(code: &str) -> Option<Option<String>> {
    // /^\x1b\]8;[^;]*;([^\x07\x1b]*)(?:\x07|\x1b\\)$/
    let body = code.strip_prefix("\x1b]8;")?;
    let body = if let Some(b) = body.strip_suffix('\x07') {
        b
    } else {
        body.strip_suffix("\x1b\\")?
    };
    let semi = body.find(';')?;
    let params = &body[..semi];
    let url = &body[semi + 1..];
    if params.contains(';') || url.contains('\x07') || url.contains('\x1b') {
        return None;
    }
    Some(if url.is_empty() { None } else { Some(url.to_string()) })
}

/// Return the OSC 8 hyperlink covering a visible terminal column.
pub fn get_osc8_link_at_column(line: &str, column: usize) -> Option<String> {
    let mut active_url: Option<String> = None;
    let mut current_col = 0;
    let mut i = 0;
    while i < line.len() {
        let len = ansi_code_length(line, i);
        if len > 0 {
            if let Some(url) = parse_osc8_url(&line[i..i + len]) {
                active_url = url;
            }
            i += len;
            continue;
        }
        let text_end = text_run_end(line, i);
        for segment in line[i..text_end].graphemes(true) {
            let width = if segment == "\t" { 3 } else { grapheme_width(segment) };
            if column >= current_col && column < current_col + width {
                return active_url;
            }
            current_col += width;
        }
        i = text_end;
    }
    None
}

/// Normalize text for terminal output without changing logical editor content.
///
/// Thai/Lao AM vowels are decomposed and visible tabs are expanded to three spaces, while
/// tabs inside terminal string sequences stay untouched.
pub fn normalize_terminal_output(s: &str) -> String {
    let mut normalized: std::borrow::Cow<str> = std::borrow::Cow::Borrowed(s);
    if s.contains(['\u{0e33}', '\u{0eb3}']) {
        normalized = std::borrow::Cow::Owned(
            s.replace('\u{0e33}', "\u{0e4d}\u{0e32}").replace('\u{0eb3}', "\u{0ecd}\u{0eb2}"),
        );
    }
    if !normalized.contains('\t') {
        return normalized.into_owned();
    }
    let src = normalized.as_ref();
    let mut result = String::with_capacity(src.len() + 8);
    let mut i = 0;
    while i < src.len() {
        let len = ansi_code_length(src, i);
        if len > 0 {
            result.push_str(&src[i..i + len]);
            i += len;
            continue;
        }
        let ch = src[i..].chars().next().unwrap();
        if ch == '\t' {
            result.push_str("   ");
        } else {
            result.push(ch);
        }
        i += ch.len_utf8();
    }
    result
}

/// Extract the ANSI escape sequence at the given byte position, if any.
/// Returns the code and its byte length.
pub fn extract_ansi_code(s: &str, pos: usize) -> Option<(&str, usize)> {
    let len = ansi_code_length(s, pos);
    if len > 0 { Some((&s[pos..pos + len], len)) } else { None }
}

/// Byte length of the ANSI/OSC/APC escape sequence starting at `pos`, or 0 if there is none.
pub fn ansi_code_length(s: &str, pos: usize) -> usize {
    let bytes = s.as_bytes();
    if pos >= bytes.len() || bytes[pos] != 0x1b {
        return 0;
    }
    let next = bytes.get(pos + 1).copied();
    match next {
        Some(b'[') => {
            for (j, &c) in bytes.iter().enumerate().skip(pos + 2) {
                if matches!(c, b'm' | b'G' | b'K' | b'H' | b'J') {
                    return j + 1 - pos;
                }
            }
            0
        }
        Some(b']') | Some(b'_') => {
            let mut j = pos + 2;
            while j < bytes.len() {
                let c = bytes[j];
                if c == 0x07 {
                    return j + 1 - pos;
                }
                if c == 0x1b && bytes.get(j + 1) == Some(&b'\\') {
                    return j + 2 - pos;
                }
                j += 1;
            }
            0
        }
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// OSC 8 hyperlinks and SGR tracking
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActiveHyperlink {
    params: String,
    url: String,
    terminator: &'static str,
}

/// `undefined` → None, `null` → Some(None), link → Some(Some(link))
fn parse_osc8_hyperlink(code: &str) -> Option<Option<ActiveHyperlink>> {
    if !code.starts_with("\x1b]8;") {
        return None;
    }
    let terminator: &'static str = if code.ends_with('\x07') { "\x07" } else { "\x1b\\" };
    let end = code.len().saturating_sub(terminator.len()).max(4);
    let body = &code[4..end];
    let sep = body.find(';')?;
    let params = &body[..sep];
    let url = &body[sep + 1..];
    if url.is_empty() {
        return Some(None);
    }
    Some(Some(ActiveHyperlink { params: params.to_string(), url: url.to_string(), terminator }))
}

fn format_osc8_hyperlink(h: &ActiveHyperlink) -> String {
    format!("\x1b]8;{};{}{}", h.params, h.url, h.terminator)
}

fn format_osc8_close(terminator: &str) -> String {
    format!("\x1b]8;;{terminator}")
}

fn get_active_osc8_close(prefix: &str) -> String {
    if !prefix.contains("\x1b]8;") {
        return String::new();
    }
    let mut active: Option<ActiveHyperlink> = None;
    let mut i = 0;
    while i < prefix.len() {
        let len = ansi_code_length(prefix, i);
        if len > 0 {
            if let Some(h) = parse_osc8_hyperlink(&prefix[i..i + len]) {
                active = h;
            }
            i += len;
        } else {
            i += 1;
        }
    }
    active.map(|h| format_osc8_close(h.terminator)).unwrap_or_default()
}

/// Tracks active ANSI SGR codes to preserve styling across line breaks.
#[derive(Debug, Clone, Default)]
pub struct AnsiCodeTracker {
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    blink: bool,
    inverse: bool,
    hidden: bool,
    strikethrough: bool,
    fg_color: Option<String>,
    bg_color: Option<String>,
    active_hyperlink: Option<ActiveHyperlink>,
}

impl AnsiCodeTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one escape sequence.
    pub fn process(&mut self, ansi_code: &str) {
        if let Some(h) = parse_osc8_hyperlink(ansi_code) {
            self.active_hyperlink = h;
            return;
        }
        if !ansi_code.ends_with('m') {
            return;
        }
        // /\x1b\[([\d;]*)m/
        let Some(start) = ansi_code.find("\x1b[") else {
            return;
        };
        let rest = &ansi_code[start + 2..];
        let params_len = rest.bytes().take_while(|b| b.is_ascii_digit() || *b == b';').count();
        if rest.as_bytes().get(params_len) != Some(&b'm') {
            return;
        }
        let params = &rest[..params_len];
        if params.is_empty() || params == "0" {
            self.reset();
            return;
        }
        let parts: Vec<&str> = params.split(';').collect();
        let mut i = 0;
        while i < parts.len() {
            let code: Option<i64> = parts[i].parse().ok();
            if code == Some(38) || code == Some(48) {
                if parts.get(i + 1) == Some(&"5") && parts.get(i + 2).is_some() {
                    let color_code = format!("{};{};{}", parts[i], parts[i + 1], parts[i + 2]);
                    if code == Some(38) {
                        self.fg_color = Some(color_code);
                    } else {
                        self.bg_color = Some(color_code);
                    }
                    i += 3;
                    continue;
                } else if parts.get(i + 1) == Some(&"2") && parts.get(i + 4).is_some() {
                    let color_code = format!(
                        "{};{};{};{};{}",
                        parts[i],
                        parts[i + 1],
                        parts[i + 2],
                        parts[i + 3],
                        parts[i + 4]
                    );
                    if code == Some(38) {
                        self.fg_color = Some(color_code);
                    } else {
                        self.bg_color = Some(color_code);
                    }
                    i += 5;
                    continue;
                }
            }
            match code {
                Some(0) => self.reset(),
                Some(1) => self.bold = true,
                Some(2) => self.dim = true,
                Some(3) => self.italic = true,
                Some(4) => self.underline = true,
                Some(5) => self.blink = true,
                Some(7) => self.inverse = true,
                Some(8) => self.hidden = true,
                Some(9) => self.strikethrough = true,
                Some(21) => self.bold = false,
                Some(22) => {
                    self.bold = false;
                    self.dim = false;
                }
                Some(23) => self.italic = false,
                Some(24) => self.underline = false,
                Some(25) => self.blink = false,
                Some(27) => self.inverse = false,
                Some(28) => self.hidden = false,
                Some(29) => self.strikethrough = false,
                Some(39) => self.fg_color = None,
                Some(49) => self.bg_color = None,
                Some(c) if (30..=37).contains(&c) || (90..=97).contains(&c) => {
                    self.fg_color = Some(c.to_string());
                }
                Some(c) if (40..=47).contains(&c) || (100..=107).contains(&c) => {
                    self.bg_color = Some(c.to_string());
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn reset(&mut self) {
        self.bold = false;
        self.dim = false;
        self.italic = false;
        self.underline = false;
        self.blink = false;
        self.inverse = false;
        self.hidden = false;
        self.strikethrough = false;
        self.fg_color = None;
        self.bg_color = None;
    }

    /// Clear all state for reuse.
    pub fn clear(&mut self) {
        self.reset();
        self.active_hyperlink = None;
    }

    pub fn get_active_codes(&self) -> String {
        let mut codes: Vec<&str> = Vec::new();
        if self.bold {
            codes.push("1");
        }
        if self.dim {
            codes.push("2");
        }
        if self.italic {
            codes.push("3");
        }
        if self.underline {
            codes.push("4");
        }
        if self.blink {
            codes.push("5");
        }
        if self.inverse {
            codes.push("7");
        }
        if self.hidden {
            codes.push("8");
        }
        if self.strikethrough {
            codes.push("9");
        }
        if let Some(fg) = &self.fg_color {
            codes.push(fg);
        }
        if let Some(bg) = &self.bg_color {
            codes.push(bg);
        }
        let mut result = if codes.is_empty() { String::new() } else { format!("\x1b[{}m", codes.join(";")) };
        if let Some(h) = &self.active_hyperlink {
            result.push_str(&format_osc8_hyperlink(h));
        }
        result
    }

    pub fn get_active_background_code(&self) -> String {
        self.bg_color.as_ref().map(|bg| format!("\x1b[{bg}m")).unwrap_or_default()
    }

    pub fn has_active_codes(&self) -> bool {
        self.bold
            || self.dim
            || self.italic
            || self.underline
            || self.blink
            || self.inverse
            || self.hidden
            || self.strikethrough
            || self.fg_color.is_some()
            || self.bg_color.is_some()
            || self.active_hyperlink.is_some()
    }

    /// Reset codes for attributes that need to be turned off at line end (underline and
    /// OSC 8 hyperlinks).
    pub fn get_line_end_reset(&self) -> String {
        let mut result = String::new();
        if self.underline {
            result.push_str("\x1b[24m");
        }
        if let Some(h) = &self.active_hyperlink {
            result.push_str(&format_osc8_close(h.terminator));
        }
        result
    }
}

fn update_tracker_from_text(text: &str, tracker: &mut AnsiCodeTracker) {
    let mut i = 0;
    while let Some(off) = text[i..].find('\x1b') {
        let pos = i + off;
        let len = ansi_code_length(text, pos);
        if len > 0 {
            tracker.process(&text[pos..pos + len]);
            i = pos + len;
        } else {
            i = pos + 1;
        }
        if i >= text.len() {
            break;
        }
    }
}

/// Return only the background color active at the end of an ANSI-styled string.
pub fn get_active_background_ansi(text: &str) -> String {
    let mut tracker = AnsiCodeTracker::new();
    update_tracker_from_text(text, &mut tracker);
    tracker.get_active_background_code()
}

// ---------------------------------------------------------------------------
// Wrapping
// ---------------------------------------------------------------------------

#[derive(PartialEq, Eq, Clone, Copy)]
enum TokenKind {
    Space,
    Word,
}

/// Split text into words while keeping ANSI codes attached.
fn split_into_tokens_with_ansi(text: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut pending_ansi = String::new();
    let mut current_kind: Option<TokenKind> = None;
    let mut i = 0;

    while i < text.len() {
        let len = ansi_code_length(text, i);
        if len > 0 {
            pending_ansi.push_str(&text[i..i + len]);
            i += len;
            continue;
        }

        // Visible text runs up to the next escape sequence.
        let first_len = text[i..].chars().next().map_or(1, char::len_utf8);
        let mut end = text[i + first_len..].find('\x1b').map(|o| i + first_len + o);
        while let Some(e) = end {
            if ansi_code_length(text, e) != 0 {
                break;
            }
            end = text[e + 1..].find('\x1b').map(|o| e + 1 + o);
        }
        let end = end.unwrap_or(text.len());
        let chunk = &text[i..end];
        let ascii = is_printable_ascii(chunk);
        let mut handle = |segment: &str| {
            let is_space = segment == " ";
            if !ascii && !is_space && contains_cjk_break(segment) {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                    current_kind = None;
                }
                let mut token = std::mem::take(&mut pending_ansi);
                token.push_str(segment);
                tokens.push(token);
                return;
            }
            let kind = if is_space { TokenKind::Space } else { TokenKind::Word };
            if !current.is_empty() && current_kind != Some(kind) {
                tokens.push(std::mem::take(&mut current));
                current_kind = None;
            }
            if !pending_ansi.is_empty() {
                current.push_str(&pending_ansi);
                pending_ansi.clear();
            }
            current_kind = Some(kind);
            current.push_str(segment);
        };
        if ascii {
            for k in 0..chunk.len() {
                handle(&chunk[k..k + 1]);
            }
        } else {
            for g in chunk.graphemes(true) {
                handle(g);
            }
        }
        i = end;
    }

    if !pending_ansi.is_empty() {
        if !current.is_empty() {
            current.push_str(&pending_ansi);
        } else if let Some(last) = tokens.last_mut() {
            last.push_str(&pending_ansi);
        } else {
            current = pending_ansi;
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Split on `\r\n`, `\r` or `\n`.
pub fn split_lines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\r' => {
                out.push(&text[start..i]);
                if bytes.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
                start = i + 1;
            }
            b'\n' => {
                out.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&text[start..]);
    out
}

/// Wrap text with ANSI codes preserved.
///
/// Only does word wrapping: no padding, no background colors. Returns lines where each line
/// is at most `width` visible columns. Active ANSI codes are preserved across line breaks.
pub fn wrap_text_with_ansi(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut result: Vec<String> = Vec::new();
    let mut tracker = AnsiCodeTracker::new();
    for input_line in split_lines(text) {
        let prefix = if result.is_empty() { String::new() } else { tracker.get_active_codes() };
        let wrapped = wrap_single_line(&format!("{prefix}{input_line}"), width);
        result.extend(wrapped);
        update_tracker_from_text(input_line, &mut tracker);
    }
    if result.is_empty() { vec![String::new()] } else { result }
}

fn wrap_single_line(line: &str, width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }
    if visible_width(line) <= width {
        return vec![line.to_string()];
    }

    let mut wrapped: Vec<String> = Vec::new();
    let mut tracker = AnsiCodeTracker::new();
    let tokens = split_into_tokens_with_ansi(line);

    let mut current_line = String::new();
    let mut current_visible = 0usize;

    for token in &tokens {
        let token_visible = visible_width(token);
        let is_ws = js_trim(token).is_empty();

        if token_visible > width && !is_ws {
            if !current_line.is_empty() {
                let reset = tracker.get_line_end_reset();
                if !reset.is_empty() {
                    current_line.push_str(&reset);
                }
                wrapped.push(std::mem::take(&mut current_line));
            }
            let broken = break_long_word(token, width, &mut tracker);
            let n = broken.len();
            let mut it = broken.into_iter();
            for _ in 0..n - 1 {
                wrapped.push(it.next().unwrap());
            }
            current_line = it.next().unwrap();
            current_visible = visible_width(&current_line);
            continue;
        }

        let total_needed = current_visible + token_visible;
        if total_needed > width && current_visible > 0 {
            let mut line_to_wrap = js_trim_end(&current_line).to_string();
            let reset = tracker.get_line_end_reset();
            if !reset.is_empty() {
                line_to_wrap.push_str(&reset);
            }
            wrapped.push(line_to_wrap);
            if is_ws {
                current_line = tracker.get_active_codes();
                current_visible = 0;
            } else {
                current_line = tracker.get_active_codes() + token;
                current_visible = token_visible;
            }
        } else {
            current_line.push_str(token);
            current_visible += token_visible;
        }
        update_tracker_from_text(token, &mut tracker);
    }

    if !current_line.is_empty() {
        wrapped.push(current_line);
    }
    if wrapped.is_empty() {
        vec![String::new()]
    } else {
        wrapped.into_iter().map(|l| js_trim_end(&l).to_string()).collect()
    }
}

fn break_long_word(word: &str, width: usize, tracker: &mut AnsiCodeTracker) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current_line = tracker.get_active_codes();
    let mut current_width = 0usize;

    let mut i = 0;
    while i < word.len() {
        let len = ansi_code_length(word, i);
        if len > 0 {
            let code = &word[i..i + len];
            current_line.push_str(code);
            tracker.process(code);
            i += len;
            continue;
        }
        let end = text_run_end(word, i);
        for grapheme in word[i..end].graphemes(true) {
            if grapheme.is_empty() {
                continue;
            }
            let gw = visible_width(grapheme);
            if current_width + gw > width {
                let reset = tracker.get_line_end_reset();
                if !reset.is_empty() {
                    current_line.push_str(&reset);
                }
                lines.push(std::mem::take(&mut current_line));
                current_line = tracker.get_active_codes();
                current_width = 0;
            }
            current_line.push_str(grapheme);
            current_width += gw;
        }
        i = end;
    }
    if !current_line.is_empty() {
        lines.push(current_line);
    }
    if lines.is_empty() { vec![String::new()] } else { lines }
}

/// Apply background color to a line, padding to full width.
pub fn apply_background_to_line(line: &str, width: usize, bg_fn: &dyn Fn(&str) -> String) -> String {
    let padding = width.saturating_sub(visible_width(line));
    let with_padding = format!("{line}{}", " ".repeat(padding));
    bg_fn(&with_padding)
}

// ---------------------------------------------------------------------------
// Truncation
// ---------------------------------------------------------------------------

fn truncate_fragment_to_width(text: &str, max_width: usize) -> (String, usize) {
    if max_width == 0 || text.is_empty() {
        return (String::new(), 0);
    }
    if is_printable_ascii(text) {
        let clipped = &text[..text.len().min(max_width)];
        return (clipped.to_string(), clipped.len());
    }
    let has_ansi = text.contains('\x1b');
    let has_tabs = text.contains('\t');
    if !has_ansi && !has_tabs {
        let mut result = String::new();
        let mut width = 0;
        for g in text.graphemes(true) {
            let w = grapheme_width(g);
            if width + w > max_width {
                break;
            }
            result.push_str(g);
            width += w;
        }
        return (result, width);
    }

    let mut result = String::new();
    let mut width = 0;
    let mut i = 0;
    let mut pending_ansi = String::new();
    let bytes = text.as_bytes();
    while i < text.len() {
        let len = ansi_code_length(text, i);
        if len > 0 {
            pending_ansi.push_str(&text[i..i + len]);
            i += len;
            continue;
        }
        if bytes[i] == b'\t' {
            if width + 3 > max_width {
                break;
            }
            if !pending_ansi.is_empty() {
                result.push_str(&pending_ansi);
                pending_ansi.clear();
            }
            result.push('\t');
            width += 3;
            i += 1;
            continue;
        }
        let mut end = i;
        while end < bytes.len() && bytes[end] != b'\t' {
            if bytes[end] == 0x1b && ansi_code_length(text, end) > 0 {
                break;
            }
            end += 1;
        }
        for g in text[i..end].graphemes(true) {
            let w = grapheme_width(g);
            if width + w > max_width {
                return (result, width);
            }
            if !pending_ansi.is_empty() {
                result.push_str(&pending_ansi);
                pending_ansi.clear();
            }
            result.push_str(g);
            width += w;
        }
        i = end;
    }
    (result, width)
}

fn finalize_truncated_result(
    prefix: &str,
    prefix_width: usize,
    ellipsis: &str,
    ellipsis_width: usize,
    max_width: usize,
    pad: bool,
) -> String {
    let reset = "\x1b[0m";
    let hyperlink_close = get_active_osc8_close(prefix);
    let visible = prefix_width + ellipsis_width;
    let result = if !ellipsis.is_empty() {
        format!("{prefix}{hyperlink_close}{reset}{ellipsis}{reset}")
    } else {
        format!("{prefix}{hyperlink_close}{reset}")
    };
    if pad { result + &" ".repeat(max_width.saturating_sub(visible)) } else { result }
}

/// Truncate text to fit within a maximum visible width, adding `ellipsis` if needed.
/// Optionally pad with spaces to reach exactly `max_width`.
pub fn truncate_to_width(text: &str, max_width: usize, ellipsis: &str, pad: bool) -> String {
    if max_width == 0 {
        return String::new();
    }
    if text.is_empty() {
        return if pad { " ".repeat(max_width) } else { String::new() };
    }

    let ellipsis_width = visible_width(ellipsis);
    if ellipsis_width >= max_width {
        let text_width = visible_width(text);
        if text_width <= max_width {
            return if pad { format!("{text}{}", " ".repeat(max_width - text_width)) } else { text.to_string() };
        }
        let (clipped, clipped_width) = truncate_fragment_to_width(ellipsis, max_width);
        if clipped_width == 0 {
            return if pad { " ".repeat(max_width) } else { String::new() };
        }
        return finalize_truncated_result("", 0, &clipped, clipped_width, max_width, pad);
    }

    if is_printable_ascii(text) {
        if text.len() <= max_width {
            return if pad { format!("{text}{}", " ".repeat(max_width - text.len())) } else { text.to_string() };
        }
        let target = max_width - ellipsis_width;
        return finalize_truncated_result(&text[..target], target, ellipsis, ellipsis_width, max_width, pad);
    }

    let target_width = max_width - ellipsis_width;
    let mut result = String::new();
    let mut pending_ansi = String::new();
    let mut visible_so_far = 0usize;
    let mut kept_width = 0usize;
    let mut keep_contiguous_prefix = true;
    let mut overflowed = false;
    let has_ansi = text.contains('\x1b');
    let has_tabs = text.contains('\t');

    let exhausted_input = if !has_ansi && !has_tabs {
        for g in text.graphemes(true) {
            let w = grapheme_width(g);
            if keep_contiguous_prefix && kept_width + w <= target_width {
                result.push_str(g);
                kept_width += w;
            } else {
                keep_contiguous_prefix = false;
            }
            visible_so_far += w;
            if visible_so_far > max_width {
                overflowed = true;
                break;
            }
        }
        !overflowed
    } else {
        let bytes = text.as_bytes();
        let mut i = 0;
        'outer: while i < text.len() {
            let len = ansi_code_length(text, i);
            if len > 0 {
                pending_ansi.push_str(&text[i..i + len]);
                i += len;
                continue;
            }
            if bytes[i] == b'\t' {
                if keep_contiguous_prefix && kept_width + 3 <= target_width {
                    if !pending_ansi.is_empty() {
                        result.push_str(&pending_ansi);
                        pending_ansi.clear();
                    }
                    result.push('\t');
                    kept_width += 3;
                } else {
                    keep_contiguous_prefix = false;
                    pending_ansi.clear();
                }
                visible_so_far += 3;
                if visible_so_far > max_width {
                    overflowed = true;
                    break;
                }
                i += 1;
                continue;
            }
            let mut end = i;
            while end < bytes.len() && bytes[end] != b'\t' {
                if bytes[end] == 0x1b && ansi_code_length(text, end) > 0 {
                    break;
                }
                end += 1;
            }
            for g in text[i..end].graphemes(true) {
                let w = grapheme_width(g);
                if keep_contiguous_prefix && kept_width + w <= target_width {
                    if !pending_ansi.is_empty() {
                        result.push_str(&pending_ansi);
                        pending_ansi.clear();
                    }
                    result.push_str(g);
                    kept_width += w;
                } else {
                    keep_contiguous_prefix = false;
                    pending_ansi.clear();
                }
                visible_so_far += w;
                if visible_so_far > max_width {
                    overflowed = true;
                    break 'outer;
                }
            }
            i = end;
        }
        i >= text.len()
    };

    if !overflowed && exhausted_input {
        return if pad {
            format!("{text}{}", " ".repeat(max_width.saturating_sub(visible_so_far)))
        } else {
            text.to_string()
        };
    }
    finalize_truncated_result(&result, kept_width, ellipsis, ellipsis_width, max_width, pad)
}

/// [`truncate_to_width`] with the default ellipsis `"..."` and no padding.
pub fn truncate(text: &str, max_width: usize) -> String {
    truncate_to_width(text, max_width, "...", false)
}

// ---------------------------------------------------------------------------
// Slicing
// ---------------------------------------------------------------------------

/// Extract a range of visible columns from a line. Handles ANSI codes and wide chars.
/// If `strict`, wide chars at the boundary that would extend past the range are excluded.
pub fn slice_by_column(line: &str, start_col: usize, length: usize, strict: bool) -> String {
    slice_with_width(line, start_col, length, strict).0
}

/// Like [`slice_by_column`] but also returns the actual visible width of the result.
pub fn slice_with_width(line: &str, start_col: usize, length: usize, strict: bool) -> (String, usize) {
    if length == 0 {
        return (String::new(), 0);
    }
    let end_col = start_col + length;
    let mut result = String::new();
    let mut result_width = 0;
    let mut current_col = 0;
    let mut i = 0;
    let mut pending_ansi = String::new();

    while i < line.len() {
        let len = ansi_code_length(line, i);
        if len > 0 {
            let code = &line[i..i + len];
            if current_col >= start_col && current_col < end_col {
                result.push_str(&pending_ansi);
                result.push_str(code);
                pending_ansi.clear();
            } else if current_col < start_col {
                pending_ansi.push_str(code);
            }
            i += len;
            continue;
        }
        let text_end = text_run_end(line, i);
        for g in line[i..text_end].graphemes(true) {
            let w = grapheme_width(g);
            let in_range = current_col >= start_col && current_col < end_col;
            let fits = !strict || current_col + w <= end_col;
            if in_range && fits {
                if !pending_ansi.is_empty() {
                    result.push_str(&pending_ansi);
                    pending_ansi.clear();
                }
                result.push_str(g);
                result_width += w;
            }
            current_col += w;
            if current_col >= end_col {
                break;
            }
        }
        i = text_end;
        if current_col >= end_col {
            break;
        }
    }
    (result, result_width)
}

/// Result of [`extract_segments`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Segments {
    pub before: String,
    pub before_width: usize,
    pub after: String,
    pub after_width: usize,
}

/// Extract "before" and "after" segments from a line in a single pass. Used for overlay
/// compositing; styling active before the overlay is carried into the "after" segment.
pub fn extract_segments(
    line: &str,
    before_end: usize,
    after_start: usize,
    after_len: usize,
    strict_after: bool,
) -> Segments {
    let mut out = Segments::default();
    let mut current_col = 0;
    let mut i = 0;
    let mut pending_ansi_before = String::new();
    let mut after_started = false;
    let after_end = after_start + after_len;
    let mut tracker = AnsiCodeTracker::new();

    let done = |col: usize| if after_len == 0 { col >= before_end } else { col >= after_end };

    while i < line.len() {
        let len = ansi_code_length(line, i);
        if len > 0 {
            let code = &line[i..i + len];
            tracker.process(code);
            if current_col < before_end {
                pending_ansi_before.push_str(code);
            } else if current_col >= after_start
                && current_col < after_end
                && (after_started || code.starts_with("\x1b_"))
            {
                out.after.push_str(code);
            }
            i += len;
            continue;
        }
        let text_end = text_run_end(line, i);
        for g in line[i..text_end].graphemes(true) {
            let w = grapheme_width(g);
            if current_col < before_end && current_col + w <= before_end {
                if !pending_ansi_before.is_empty() {
                    out.before.push_str(&pending_ansi_before);
                    pending_ansi_before.clear();
                }
                out.before.push_str(g);
                out.before_width += w;
            } else if current_col >= after_start && current_col < after_end {
                let fits = !strict_after || current_col + w <= after_end;
                if fits {
                    if !after_started {
                        out.after.push_str(&tracker.get_active_codes());
                        after_started = true;
                    }
                    out.after.push_str(g);
                    out.after_width += w;
                }
            }
            current_col += w;
            if done(current_col) {
                break;
            }
        }
        i = text_end;
        if done(current_col) {
            break;
        }
    }
    out
}
