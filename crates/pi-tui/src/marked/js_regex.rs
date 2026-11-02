//! JavaScript regular expressions on top of `fancy-regex`.
//!
//! marked's grammar is written as JavaScript regex sources. [`JsRegex`] translates
//! such a source to the `fancy-regex` dialect, preserving JavaScript semantics for
//! `\s`, `\S`, `\d`, `\w`, `\b`, `.` and `\u` escapes, so the grammar can be ported
//! verbatim. All offsets are UTF-8 byte offsets.

use fancy_regex::{Captures, Regex};

/// JavaScript `WhiteSpace` + `LineTerminator` characters (the `\s` class), as class content.
const JS_SPACE: &str = r"\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}";
/// Complement of [`JS_SPACE`], as class content.
const JS_NON_SPACE: &str = r"\x{0}-\x{8}\x{E}-\x{1F}!-\x{9F}\x{A1}-\x{167F}\x{1681}-\x{1FFF}\x{200B}-\x{2027}\x{202A}-\x{202E}\x{2030}-\x{205E}\x{2060}-\x{2FFF}\x{3001}-\x{FEFE}\x{FF00}-\x{10FFFF}";
const JS_WORD: &str = "A-Za-z0-9_";
const JS_NON_WORD: &str = r"\x{0}-/:-@\[-\^`\{-\x{10FFFF}";

/// Translate a JavaScript regex source (with flags) to fancy-regex syntax.
pub fn translate(source: &str, flags: &str) -> String {
    let mut out = String::with_capacity(source.len() * 2);
    if flags.contains('i') {
        out.push_str("(?i)");
    }
    if flags.contains('m') {
        out.push_str("(?m)");
    }
    if flags.contains('s') {
        out.push_str("(?s)");
    }
    let chars: Vec<char> = source.chars().collect();
    let mut i = 0;
    let mut in_class = false;
    let mut class_start = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            let e = chars[i + 1];
            i += 2;
            match e {
                'd' => out.push_str(if in_class { "0-9" } else { "[0-9]" }),
                'D' => out.push_str(if in_class { r"\x{0}-/:-\x{10FFFF}" } else { "[^0-9]" }),
                'w' => {
                    if in_class {
                        out.push_str(JS_WORD);
                    } else {
                        out.push('[');
                        out.push_str(JS_WORD);
                        out.push(']');
                    }
                }
                'W' => {
                    if in_class {
                        out.push_str(JS_NON_WORD);
                    } else {
                        out.push_str("[^");
                        out.push_str(JS_WORD);
                        out.push(']');
                    }
                }
                's' => {
                    if in_class {
                        out.push_str(JS_SPACE);
                    } else {
                        out.push('[');
                        out.push_str(JS_SPACE);
                        out.push(']');
                    }
                }
                'S' => {
                    if in_class {
                        out.push_str(JS_NON_SPACE);
                    } else {
                        out.push_str("[^");
                        out.push_str(JS_SPACE);
                        out.push(']');
                    }
                }
                'b' if in_class => out.push_str(r"\x08"),
                'b' => out.push_str("(?:(?<=[A-Za-z0-9_])(?![A-Za-z0-9_])|(?<![A-Za-z0-9_])(?=[A-Za-z0-9_]))"),
                'B' => out.push_str("(?:(?<=[A-Za-z0-9_])(?=[A-Za-z0-9_])|(?<![A-Za-z0-9_])(?![A-Za-z0-9_]))"),
                'u' => {
                    if chars.get(i) == Some(&'{') {
                        let end = chars[i..].iter().position(|&c| c == '}').map_or(chars.len(), |p| i + p);
                        let hex: String = chars[i + 1..end].iter().collect();
                        out.push_str(&format!("\\x{{{hex}}}"));
                        i = end + 1;
                    } else {
                        let hex: String = chars[i..(i + 4).min(chars.len())].iter().collect();
                        out.push_str(&format!("\\x{{{hex}}}"));
                        i += 4;
                    }
                }
                'x' => {
                    let hex: String = chars[i..(i + 2).min(chars.len())].iter().collect();
                    out.push_str(&format!("\\x{{{hex}}}"));
                    i += 2;
                }
                '0' => out.push_str(r"\x{0}"),
                'n' | 't' | 'r' | 'f' | 'v' => {
                    out.push('\\');
                    out.push(e);
                }
                'k' | 'p' | 'P' => {
                    out.push('\\');
                    out.push(e);
                }
                '1'..='9' => {
                    out.push('\\');
                    out.push(e);
                }
                '<' | '>' => out.push(e),
                c if c.is_ascii_punctuation() => {
                    out.push('\\');
                    out.push(c);
                }
                c => out.push(c),
            }
            class_start = false;
            continue;
        }
        if in_class {
            match c {
                ']' if !class_start => {
                    in_class = false;
                    out.push(']');
                }
                '[' | '&' | '~' => {
                    out.push('\\');
                    out.push(c);
                }
                _ => out.push(c),
            }
            class_start = false;
            i += 1;
            continue;
        }
        match c {
            '[' => {
                in_class = true;
                out.push('[');
                if chars.get(i + 1) == Some(&'^') {
                    out.push('^');
                    i += 1;
                }
                class_start = false;
            }
            '.' => out.push_str(r"[^\n\r\x{2028}\x{2029}]"),
            _ => out.push(c),
        }
        i += 1;
    }
    out
}

/// A compiled JavaScript regular expression.
pub struct JsRegex {
    re: Regex,
    /// For a pattern that can only match at the start of the input: the same pattern followed by an
    /// empty alternative in a last capture group (`(?:re)|()`), and that group's index.
    ///
    /// fancy-regex searches a pattern it runs on its backtracking VM by retrying it at every start
    /// position, even when the pattern begins with `^`, so a failing `exec` costs a pass over the
    /// whole input. The lexer runs its block rules on the rest of the source for every block, which
    /// made lexing quadratic. With the empty alternative a failing match at the start ends the
    /// search there.
    anchored: Option<(Regex, usize)>,
    /// The original JavaScript source.
    pub source: String,
}

/// Whether a JavaScript regex can only match at the start of the input: it is not multiline and
/// every top-level alternative begins with `^`.
fn is_start_anchored(source: &str, flags: &str) -> bool {
    if flags.contains('m') || !source.starts_with('^') {
        return false;
    }
    let mut depth = 0usize;
    let mut in_class = false;
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '[' if !in_class => in_class = true,
            ']' if in_class => in_class = false,
            '(' if !in_class => depth += 1,
            ')' if !in_class => depth = depth.saturating_sub(1),
            '|' if !in_class && depth == 0 && chars.peek() != Some(&'^') => return false,
            _ => {}
        }
    }
    true
}

impl std::fmt::Debug for JsRegex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "/{}/", self.source)
    }
}

impl JsRegex {
    /// Compile a JavaScript regex source. Panics on invalid patterns (grammar is static).
    pub fn new(source: &str, flags: &str) -> Self {
        let translated = translate(source, flags);
        let build = |pattern: &str| {
            fancy_regex::RegexBuilder::new(pattern)
                .backtrack_limit(10_000_000)
                .build()
                .unwrap_or_else(|e| panic!("invalid regex /{source}/ -> {pattern}: {e}"))
        };
        let re = build(&translated);
        let anchored = is_start_anchored(source, flags)
            .then(|| (build(&format!("(?:{translated})|()")), re.captures_len()));
        Self {
            re,
            anchored,
            source: source.to_string(),
        }
    }

    /// `regex.exec(s)` (from the start).
    pub fn exec<'t>(&self, s: &'t str) -> Option<Captures<'t>> {
        match &self.anchored {
            Some((anchored, empty_group)) => anchored
                .captures(s)
                .ok()
                .flatten()
                .filter(|c| c.get(*empty_group).is_none()),
            None => self.re.captures(s).ok().flatten(),
        }
    }

    /// Search starting at byte `pos` (like `lastIndex` with the `g` flag).
    pub fn exec_at<'t>(&self, s: &'t str, pos: usize) -> Option<Captures<'t>> {
        if pos > s.len() {
            return None;
        }
        self.re.captures_from_pos(s, pos).ok().flatten()
    }

    /// `regex.test(s)`.
    pub fn test(&self, s: &str) -> bool {
        match &self.anchored {
            Some(_) => self.exec(s).is_some(),
            None => self.re.is_match(s).unwrap_or(false),
        }
    }

    /// `s.search(regex)`.
    pub fn search(&self, s: &str) -> Option<usize> {
        self.re.find(s).ok().flatten().map(|m| m.start())
    }

    /// All non-overlapping matches (like `matchAll`), with JavaScript's empty-match advance.
    pub fn match_all<'t>(&self, s: &'t str) -> Vec<Captures<'t>> {
        let mut out = Vec::new();
        let mut pos = 0;
        while pos <= s.len() {
            let Some(c) = self.exec_at(s, pos) else { break };
            let m = c.get(0).unwrap();
            pos = if m.end() == m.start() { next_char_boundary(s, m.end()) } else { m.end() };
            out.push(c);
        }
        out
    }

    /// `s.replace(/re/g, fn)`.
    pub fn replace_all(&self, s: &str, mut f: impl FnMut(&Captures<'_>) -> String) -> String {
        let mut out = String::with_capacity(s.len());
        let mut last = 0;
        for c in self.match_all(s) {
            let m = c.get(0).unwrap();
            out.push_str(&s[last..m.start()]);
            out.push_str(&f(&c));
            last = m.end();
        }
        out.push_str(&s[last..]);
        out
    }

    /// `s.replace(/re/, fn)` (first match only).
    pub fn replace_first(&self, s: &str, f: impl FnOnce(&Captures<'_>) -> String) -> String {
        match self.exec(s) {
            Some(c) => {
                let m = c.get(0).unwrap();
                format!("{}{}{}", &s[..m.start()], f(&c), &s[m.end()..])
            }
            None => s.to_string(),
        }
    }

    /// `s.replace(/re/g, literal)`.
    pub fn replace_all_str(&self, s: &str, with: &str) -> String {
        self.replace_all(s, |_| with.to_string())
    }

    /// `s.split(/re/)` (no captures in the separator).
    pub fn split(&self, s: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut last = 0;
        for c in self.match_all(s) {
            let m = c.get(0).unwrap();
            if m.end() == m.start() && (m.start() == 0 || m.start() == s.len()) {
                continue;
            }
            out.push(s[last..m.start()].to_string());
            last = m.end();
        }
        out.push(s[last..].to_string());
        out
    }
}

fn next_char_boundary(s: &str, pos: usize) -> usize {
    let mut p = pos + 1;
    while p < s.len() && !s.is_char_boundary(p) {
        p += 1;
    }
    p
}

/// Capture group `i` as a string, `""` when it did not participate (JS `cap[i] || ''`).
pub fn group<'t>(c: &Captures<'t>, i: usize) -> &'t str {
    c.get(i).map_or("", |m| m.as_str())
}

/// Whether capture group `i` participated with a non-empty match (JS truthiness).
pub fn has(c: &Captures<'_>, i: usize) -> bool {
    c.get(i).is_some_and(|m| !m.as_str().is_empty())
}

/// Whether capture group `i` participated at all (JS `cap[i] !== undefined`).
pub fn defined(c: &Captures<'_>, i: usize) -> bool {
    c.get(i).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_js_classes() {
        let re = JsRegex::new(r"^\s+\S", "");
        assert!(re.test("\u{FEFF} x"));
        let re = JsRegex::new(r"[\s\S]+", "");
        assert_eq!(re.exec("a\nb").unwrap().get(0).unwrap().as_str(), "a\nb");
        let re = JsRegex::new(r"\b_", "");
        assert!(re.test("a _"));
        assert!(!re.test("a_"));
        let re = JsRegex::new(r"^.$", "");
        assert!(!re.test("\r"));
        let re = JsRegex::new(r"[^\[\]`]", "");
        assert!(re.test("a"));
        let re = JsRegex::new(r"[a-zA-Z0-9.!#$%&'*+/=?^_`{|}~-]+", "");
        assert!(re.test("~&"));
        let re = JsRegex::new(r"[\u{1100}-\u{11ff}　]", "u");
        assert!(re.test("\u{3000}"));
    }

    #[test]
    fn start_anchored_patterns_match_like_the_plain_pattern() {
        assert!(is_start_anchored(r"^ {0,3}(`{3,}|~{3,})", ""));
        assert!(is_start_anchored(r"^(?:a|b)c", ""));
        assert!(is_start_anchored(r"^[|]x", ""));
        assert!(!is_start_anchored(r"^a|b", ""));
        assert!(is_start_anchored(r"^a|^b", ""));
        assert!(!is_start_anchored(r"^a|^b|c", ""));
        assert!(!is_start_anchored(r"^a", "m"));
        assert!(!is_start_anchored(r"a^", ""));
        let cases = [
            (r"^(a+)(?=b)", ["aab", "xaab", "b", ""]),
            (r"^(?:(x)|(y))\1?z", ["xxz", "yz", "zxz", "q"]),
            (
                r"^(?!\s)([^\n]+)(?:\n|$)",
                ["line\nnext", " lead", "\n", "end"],
            ),
            (
                r"^(?:\*+(?:((?!\*)[.,])|([^\s*]))?)|^_+(?:((?!_)[.,])|([^\s_]))?",
                ["**a", "__,", "x**", "_"],
            ),
        ];
        for (source, inputs) in cases {
            let re = JsRegex::new(source, "");
            assert!(re.anchored.is_some(), "{source}");
            for input in inputs {
                let plain = re.re.captures(input).ok().flatten();
                let anchored = re.exec(input);
                assert_eq!(
                    anchored.is_some(),
                    plain.is_some(),
                    "/{source}/ on {input:?}"
                );
                assert_eq!(re.test(input), plain.is_some(), "/{source}/ on {input:?}");
                if let (Some(plain), Some(anchored)) = (plain, anchored) {
                    for i in 0..plain.len() {
                        assert_eq!(
                            plain.get(i).map(|m| m.range()),
                            anchored.get(i).map(|m| m.range()),
                            "/{source}/ group {i}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn replace_and_split() {
        let re = JsRegex::new(r"\s+", "g");
        assert_eq!(re.split("a  b c"), vec!["a", "b", "c"]);
        assert_eq!(re.replace_all_str("a  b", " "), "a b");
    }
}
