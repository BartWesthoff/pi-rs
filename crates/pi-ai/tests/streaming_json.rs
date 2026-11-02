//! Port of `test/streaming-json.test.ts` (upstream #9265): the incremental tool-argument parser
//! checked against an independent reference reader, `JSON.parse`, today's `parse_streaming_json`,
//! the partial-json and fixJson test vectors and JSONTestSuite.

use base64::Engine as _;
use pi_ai::utils::json_parse::{parse_streaming_json, parse_tool_call_arguments};
use pi_ai::{JsonChange, JsonPathSegment, StreamingJsonParser, ToolCallArgumentsStream};
use pi_json::js_json_parse;
use serde_json::{Map, Value, json};

// ─── Seeded random documents ───

/// Seeded random numbers, so every run checks the same documents.
struct Random(u64);

impl Random {
    fn next(&mut self) -> f64 {
        self.0 = (self.0.wrapping_mul(1_103_515_245).wrapping_add(12_345)) & 0x7fff_ffff;
        self.0 as f64 / 0x7fff_ffff as f64
    }
    fn below(&mut self, count: usize) -> usize {
        ((self.next() * count as f64) as usize).min(count - 1)
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

const STRING_PIECES: &[&str] = &[
    "a", "b", "x y", " ", "\n", "\t", "\\", "\"", "/", "é", "😀", "\u{1}", "\u{1f}", "\u{2028}",
    "{", "]", ",",
];

fn random_string(rand: &mut Random) -> String {
    let length = rand.below(6);
    (0..length).map(|_| *rand.pick(STRING_PIECES)).collect()
}

fn random_scalar(rand: &mut Random) -> Value {
    match rand.below(10) {
        0 => Value::String(random_string(rand)),
        1 => json!((rand.next() * 2000.0).floor() as i64 - 1000),
        2 => json!(rand.next() * 100.0),
        3 => json!(1e21),
        4 => json!(-5e-8),
        5 => json!(-0.0),
        6 => json!(0),
        7 => json!(true),
        8 => json!(false),
        _ => Value::Null,
    }
}

fn random_value(rand: &mut Random, depth: usize) -> Value {
    let roll = rand.next();
    if depth > 3 || roll < 0.3 {
        return random_scalar(rand);
    }
    if roll < 0.65 {
        let mut members = Map::new();
        for _ in 0..rand.below(4) {
            let key = random_string(rand);
            let value = random_value(rand, depth + 1);
            members.insert(key, value);
        }
        return Value::Object(members);
    }
    Value::Array(
        (0..rand.below(4))
            .map(|_| random_value(rand, depth + 1))
            .collect(),
    )
}

/// JSON text of `value`, with random insignificant whitespace when `spaced`.
fn render(value: &Value, rand: &mut Random, spaced: bool) -> String {
    let space = |rand: &mut Random| -> &'static str {
        if spaced {
            rand.pick(&["", "", " ", "\n  ", "\t", "\r\n"])
        } else {
            ""
        }
    };
    match value {
        Value::Array(items) => {
            let mut out = format!("[{}", space(rand));
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(space(rand));
                    out.push(',');
                    out.push_str(space(rand));
                }
                out.push_str(&render(item, rand, spaced));
            }
            out.push_str(space(rand));
            out.push(']');
            out
        }
        Value::Object(members) => {
            let mut out = format!("{{{}", space(rand));
            for (index, (key, item)) in members.iter().enumerate() {
                if index > 0 {
                    out.push_str(space(rand));
                    out.push(',');
                    out.push_str(space(rand));
                }
                out.push_str(&serde_json::to_string(key).unwrap());
                out.push_str(space(rand));
                out.push(':');
                out.push_str(space(rand));
                out.push_str(&render(item, rand, spaced));
            }
            out.push_str(space(rand));
            out.push('}');
            out
        }
        // Keep the sign of -0 so the text parses back to the same value.
        Value::Number(number)
            if number.as_f64() == Some(0.0) && number.as_f64().unwrap().is_sign_negative() =>
        {
            "-0".to_string()
        }
        other => serde_json::to_string(other).unwrap(),
    }
}

fn random_documents(seed: u64, count: usize, spaced: bool) -> Vec<String> {
    let mut rand = Random(seed);
    (0..count)
        .map(|_| {
            let value = if rand.next() < 0.8 {
                let path = random_string(&mut rand);
                let args = random_value(&mut rand, 0);
                json!({"path": path, "args": args})
            } else {
                random_value(&mut rand, 0)
            };
            render(&value, &mut rand, spaced)
        })
        .collect()
}

// ─── Reference reader ───

/// Reference for `StreamingJsonParser` on a prefix of valid JSON (strings may also hold raw control
/// characters and invalid escapes): a plain recursive-descent reading of the whole prefix that
/// follows the documented rules, written independently of the incremental parser.
struct Reference<'a> {
    text: &'a [char],
    pending_scalars: bool,
}

struct Read {
    value: Value,
    complete: bool,
    end: usize,
}

fn reference_snapshot(text: &str) -> Option<Value> {
    reference(text, true)
}

fn reference(text: &str, pending_scalars: bool) -> Option<Value> {
    let chars: Vec<char> = text.chars().collect();
    let reader = Reference {
        text: &chars,
        pending_scalars,
    };
    reader.value(reader.skip_space(0)).map(|read| read.value)
}

impl Reference<'_> {
    fn skip_space(&self, mut index: usize) -> usize {
        while index < self.text.len() && " \t\n\r".contains(self.text[index]) {
            index += 1;
        }
        index
    }

    fn value(&self, start: usize) -> Option<Read> {
        match self.text.get(start)? {
            '"' => Some(self.string(start)),
            '{' => Some(self.object(start)),
            '[' => Some(self.array(start)),
            _ => self.scalar(start),
        }
    }

    fn string(&self, start: usize) -> Read {
        let text = self.text;
        let mut value = String::new();
        let mut high: Option<u32> = None;
        let flush = |value: &mut String, high: &mut Option<u32>| {
            if high.take().is_some() {
                value.push('\u{FFFD}');
            }
        };
        let partial = |value: String| Read {
            value: Value::String(value),
            complete: false,
            end: text.len(),
        };
        let mut index = start + 1;
        while index < text.len() {
            let c = text[index];
            if c == '"' {
                flush(&mut value, &mut high);
                return Read {
                    value: Value::String(value),
                    complete: true,
                    end: index + 1,
                };
            }
            if c != '\\' {
                flush(&mut value, &mut high);
                value.push(c);
                index += 1;
                continue;
            }
            let Some(&next) = text.get(index + 1) else {
                return partial(value);
            };
            let simple = match next {
                '"' => Some('"'),
                '\\' => Some('\\'),
                '/' => Some('/'),
                'b' => Some('\u{8}'),
                'f' => Some('\u{c}'),
                'n' => Some('\n'),
                'r' => Some('\r'),
                't' => Some('\t'),
                _ => None,
            };
            if let Some(decoded) = simple {
                flush(&mut value, &mut high);
                value.push(decoded);
                index += 2;
                continue;
            }
            if next == 'u' {
                let mut end = index + 2;
                while end < index + 6 && end < text.len() && text[end].is_ascii_hexdigit() {
                    end += 1;
                }
                if end == index + 6 {
                    let digits: String = text[index + 2..end].iter().collect();
                    let code = u32::from_str_radix(&digits, 16).unwrap();
                    match code {
                        0xD800..=0xDBFF => {
                            flush(&mut value, &mut high);
                            high = Some(code);
                        }
                        0xDC00..=0xDFFF => match high.take() {
                            Some(h) => value.push(
                                char::from_u32(0x10000 + ((h - 0xD800) << 10) + (code - 0xDC00))
                                    .unwrap(),
                            ),
                            None => value.push('\u{FFFD}'),
                        },
                        _ => {
                            flush(&mut value, &mut high);
                            value.push(char::from_u32(code).unwrap());
                        }
                    }
                    index = end;
                    continue;
                }
                if end == text.len() {
                    return partial(value);
                }
                // Not four hex digits: the escape stays literal and the next character is read normally.
                flush(&mut value, &mut high);
                value.extend(&text[index..end]);
                index = end;
                continue;
            }
            // Invalid escape: the backslash stays literal and the next character is read normally.
            flush(&mut value, &mut high);
            value.push('\\');
            index += 1;
        }
        // A high surrogate whose low half has not arrived is held back.
        partial(value)
    }

    fn object(&self, start: usize) -> Read {
        let mut members = Map::new();
        let len = self.text.len();
        let mut index = self.skip_space(start + 1);
        macro_rules! partial {
            () => {
                return Read {
                    value: Value::Object(members),
                    complete: false,
                    end: len,
                }
            };
        }
        if index >= len {
            partial!();
        }
        if self.text[index] == '}' {
            return Read {
                value: Value::Object(members),
                complete: true,
                end: index + 1,
            };
        }
        loop {
            assert_eq!(self.text[index], '"', "reference reads valid JSON only");
            let key = self.string(index);
            if !key.complete {
                partial!();
            }
            let Value::String(key_text) = key.value else {
                unreachable!()
            };
            index = self.skip_space(key.end);
            if index >= len {
                partial!();
            }
            index = self.skip_space(index + 1);
            let Some(member) = self.value(index) else {
                partial!()
            };
            members.insert(key_text, member.value);
            if !member.complete {
                partial!();
            }
            index = self.skip_space(member.end);
            if index >= len {
                partial!();
            }
            if self.text[index] == '}' {
                return Read {
                    value: Value::Object(members),
                    complete: true,
                    end: index + 1,
                };
            }
            index = self.skip_space(index + 1);
            if index >= len {
                partial!();
            }
        }
    }

    fn array(&self, start: usize) -> Read {
        let mut elements = Vec::new();
        let len = self.text.len();
        let mut index = self.skip_space(start + 1);
        macro_rules! partial {
            () => {
                return Read {
                    value: Value::Array(elements),
                    complete: false,
                    end: len,
                }
            };
        }
        if index >= len {
            partial!();
        }
        if self.text[index] == ']' {
            return Read {
                value: Value::Array(elements),
                complete: true,
                end: index + 1,
            };
        }
        loop {
            let Some(element) = self.value(index) else {
                partial!()
            };
            elements.push(element.value);
            if !element.complete {
                partial!();
            }
            index = self.skip_space(element.end);
            if index >= len {
                partial!();
            }
            if self.text[index] == ']' {
                return Read {
                    value: Value::Array(elements),
                    complete: true,
                    end: index + 1,
                };
            }
            index = self.skip_space(index + 1);
            if index >= len {
                partial!();
            }
        }
    }

    fn scalar(&self, start: usize) -> Option<Read> {
        let mut end = start;
        while end < self.text.len() && !" \t\n\r,:{}[]\"".contains(self.text[end]) {
            end += 1;
        }
        let token: String = self.text[start..end].iter().collect();
        if end < self.text.len() {
            return Some(Read {
                value: js_json_parse(&token).unwrap(),
                complete: true,
                end,
            });
        }
        if !self.pending_scalars {
            return None;
        }
        for (literal, value) in [
            ("true", json!(true)),
            ("false", json!(false)),
            ("null", Value::Null),
        ] {
            if literal.starts_with(&token) {
                return Some(Read {
                    value,
                    complete: false,
                    end,
                });
            }
        }
        if let Ok(value) = js_json_parse(&token) {
            return Some(Read {
                value,
                complete: false,
                end,
            });
        }
        let mantissa = token.trim_end_matches(['+', '-']);
        let mantissa = mantissa.strip_suffix(['e', 'E'])?;
        js_json_parse(mantissa).ok().map(|value| Read {
            value,
            complete: false,
            end,
        })
    }
}

// ─── Streaming helpers ───

#[derive(Clone, Copy)]
enum Chunks {
    Fixed(usize),
    /// Random sizes from 1 to 12, seeded by the text.
    Random,
}

const CHUNK_SIZES: [Chunks; 5] = [
    Chunks::Fixed(1),
    Chunks::Fixed(2),
    Chunks::Fixed(3),
    Chunks::Fixed(7),
    Chunks::Random,
];

/// A stable seed from the text, so random chunk sizes differ per document.
fn seed(text: &str) -> u64 {
    text.chars().fold(7u64, |hash, c| {
        (hash.wrapping_mul(31).wrapping_add(c as u64)) & 0x7fff_ffff
    }) + 1
}

/// Splits `text` into pieces of whole characters.
fn pieces(text: &str, chunks: Chunks) -> Vec<&str> {
    let mut rand = match chunks {
        Chunks::Random => Some(Random(seed(text))),
        Chunks::Fixed(_) => None,
    };
    let boundaries: Vec<usize> = text
        .char_indices()
        .map(|(index, _)| index)
        .chain([text.len()])
        .collect();
    let mut out = Vec::new();
    let mut at = 0;
    while at + 1 < boundaries.len() {
        let size = match (chunks, rand.as_mut()) {
            (Chunks::Fixed(size), _) => size,
            (Chunks::Random, Some(rand)) => 1 + rand.below(12),
            (Chunks::Random, None) => unreachable!(),
        };
        let next = (at + size).min(boundaries.len() - 1);
        out.push(&text[boundaries[at]..boundaries[next]]);
        at = next;
    }
    out
}

/// Feed `text` in pieces; returns the text read and the snapshot after each piece.
fn stream(text: &str, chunks: Chunks) -> Vec<(String, Option<Value>)> {
    let mut parser = StreamingJsonParser::new();
    let mut read = String::new();
    pieces(text, chunks)
        .into_iter()
        .map(|piece| {
            parser.append(piece);
            read.push_str(piece);
            (read.clone(), parser.snapshot().cloned())
        })
        .collect()
}

fn final_snapshot(text: &str, chunks: Chunks) -> Option<Value> {
    if text.is_empty() {
        let mut parser = StreamingJsonParser::new();
        parser.append("");
        return parser.snapshot().cloned();
    }
    stream(text, chunks)
        .pop()
        .and_then(|(_, snapshot)| snapshot)
}

fn whole(text: &str) -> Option<Value> {
    final_snapshot(text, Chunks::Fixed(text.chars().count().max(1)))
}

/// A copy of a value kept current by applying the parser's changes.
#[derive(Default)]
struct Mirror {
    value: Option<Value>,
}

impl Mirror {
    fn apply(&mut self, changes: &[JsonChange]) {
        for change in changes {
            let path = match change {
                JsonChange::Set { path, .. } | JsonChange::Append { path, .. } => path,
            };
            let target = match path.split_last() {
                None => self.value.get_or_insert(Value::Null),
                Some((last, parents)) => {
                    let mut parent = self.value.as_mut().expect("a root before its members");
                    for segment in parents {
                        parent = step(parent, segment);
                    }
                    match (parent, last) {
                        (Value::Object(members), JsonPathSegment::Key(key)) => {
                            members.entry(key.clone()).or_insert(Value::Null)
                        }
                        (Value::Array(items), JsonPathSegment::Index(index)) => {
                            if *index == items.len() {
                                items.push(Value::Null);
                            }
                            &mut items[*index]
                        }
                        (parent, last) => panic!("{last:?} does not fit {parent}"),
                    }
                }
            };
            match change {
                JsonChange::Set { value, .. } => *target = value.clone(),
                JsonChange::Append { text, .. } => match target {
                    Value::String(value) => value.push_str(text),
                    other => panic!("append to {other}"),
                },
            }
        }
    }
}

fn step<'a>(value: &'a mut Value, segment: &JsonPathSegment) -> &'a mut Value {
    match (value, segment) {
        (Value::Object(members), JsonPathSegment::Key(key)) => {
            members.get_mut(key).expect("member exists")
        }
        (Value::Array(items), JsonPathSegment::Index(index)) => &mut items[*index],
        (value, segment) => panic!("{segment:?} does not fit {value}"),
    }
}

/// Feed `text` in pieces into a parser and a mirror of its changes; checks the mirror after each piece.
fn mirror_stream(text: &str, chunks: Chunks) -> Mirror {
    let mut parser = StreamingJsonParser::new();
    let mut mirror = Mirror::default();
    let mut read = String::new();
    for piece in pieces(text, chunks) {
        mirror.apply(parser.append(piece));
        read.push_str(piece);
        // Changes leave out unterminated numbers and literals, which snapshots show.
        assert_eq!(mirror.value, reference(&read, false), "{read:?}");
    }
    // A space ends a number or literal at the end, which changes report only once it ends.
    mirror.apply(parser.append(" "));
    mirror
}

fn key(name: &str) -> JsonPathSegment {
    JsonPathSegment::Key(name.into())
}

fn set(path: Vec<JsonPathSegment>, value: Value) -> JsonChange {
    JsonChange::Set { path, value }
}

// ─── Differential fuzzing ───

#[test]
fn matches_the_reference_after_every_piece_at_every_chunk_size_with_insignificant_whitespace() {
    for text in random_documents(1, 250, true) {
        for chunks in CHUNK_SIZES {
            for (read, snapshot) in stream(&text, chunks) {
                assert_eq!(snapshot, reference_snapshot(&read), "{read:?}");
            }
        }
    }
}

#[test]
fn matches_parse_streaming_json_at_every_prefix_of_compact_documents() {
    for text in random_documents(2, 400, false) {
        for (read, snapshot) in stream(&text, Chunks::Fixed(1)) {
            // Documented differences: parse_streaming_json trims trailing whitespace, and shows an
            // unfinished root `null` as {}.
            if read.ends_with(char::is_whitespace) {
                continue;
            }
            if snapshot == Some(Value::Null) && "null".starts_with(read.as_str()) {
                continue;
            }
            assert_eq!(
                snapshot.unwrap_or_else(|| json!({})),
                parse_streaming_json(Some(&read)),
                "{read:?}"
            );
        }
    }
}

#[test]
fn reads_complete_documents_exactly_like_json_parse() {
    let mut documents = random_documents(3, 300, true);
    documents.extend(random_documents(4, 300, false));
    for text in documents {
        let expected = js_json_parse(&text).unwrap();
        for chunks in [
            Chunks::Fixed(1),
            Chunks::Fixed(2),
            Chunks::Fixed(3),
            Chunks::Fixed(7),
            Chunks::Fixed(5),
        ] {
            assert_eq!(
                final_snapshot(&text, chunks),
                Some(expected.clone()),
                "{text}"
            );
        }
    }
}

#[test]
fn repairs_raw_control_characters_and_invalid_escapes_and_shows_them_while_streaming() {
    let mut rand = Random(6);
    let pieces_list = [
        "\n", "\t", "\u{0}", "\\d", "\\x41", "\\u12", "\\uZZZZ", "\\u00e9", "\\\\", "\\q", "a",
        " ", "é",
    ];
    for round in 0..300 {
        let content: String = (0..6).map(|_| *rand.pick(&pieces_list)).collect();
        let text = format!("{{\"path\":\"p\",\"content\":\"{content}\",\"n\":{round}}}");
        // Complete text: the same repaired value as today's parser.
        assert_eq!(
            final_snapshot(&text, Chunks::Fixed(1)),
            Some(parse_streaming_json(Some(&text))),
            "{text:?}"
        );
        for (read, snapshot) in stream(&text, Chunks::Fixed(1)) {
            assert_eq!(snapshot, reference_snapshot(&read), "{read:?}");
        }
    }
}

// ─── Strings ───

#[test]
fn holds_an_escape_back_until_it_is_complete_at_every_split_position() {
    for (text, value) in [
        (r#"{"a":"x\u00e9y"}"#, "xéy"),
        (r#"{"a":"x\ny"}"#, "x\ny"),
        (r#"{"a":"x\\y"}"#, "x\\y"),
        (r#"{"a":"x\"y"}"#, "x\"y"),
        (r#"{"a":"x\/y"}"#, "x/y"),
    ] {
        for split in 0..=text.len() {
            let mut parser = StreamingJsonParser::new();
            parser.append(&text[..split]);
            assert_eq!(
                parser.snapshot().cloned(),
                reference_snapshot(&text[..split]),
                "{}",
                &text[..split]
            );
            parser.append(&text[split..]);
            assert_eq!(parser.snapshot(), Some(&json!({"a": value})));
        }
    }
}

#[test]
fn joins_surrogate_pairs_split_across_pieces() {
    let escaped = r#"{"a":"\ud83d\ude00!"}"#;
    for split in 0..=escaped.len() {
        let mut parser = StreamingJsonParser::new();
        parser.append(&escaped[..split]);
        parser.snapshot();
        parser.append(&escaped[split..]);
        assert_eq!(parser.snapshot(), Some(&json!({"a": "😀!"})));
    }
    let raw = "{\"a\":\"😀!\"}";
    for (split, _) in raw.char_indices() {
        let mut parser = StreamingJsonParser::new();
        parser.append(&raw[..split]);
        parser.append(&raw[split..]);
        assert_eq!(parser.snapshot(), Some(&json!({"a": "😀!"})));
    }
    // Rust difference: a high surrogate whose low half has not arrived is held back, and an unpaired
    // one becomes U+FFFD, as in js_json_parse.
    let mut parser = StreamingJsonParser::new();
    parser.append(r#"{"a":"\ud83d"#);
    assert_eq!(parser.snapshot(), Some(&json!({"a": ""})));
    parser.append(r#"x"}"#);
    assert_eq!(parser.snapshot(), Some(&json!({"a": "\u{FFFD}x"})));
    assert_eq!(
        parser.snapshot(),
        js_json_parse(r#"{"a":"\ud83dx"}"#).ok().as_ref()
    );
}

#[test]
fn keeps_trailing_whitespace_of_an_unterminated_string() {
    let mut parser = StreamingJsonParser::new();
    parser.append("{\"code\":\"x\t\t  ");
    assert_eq!(parser.snapshot(), Some(&json!({"code": "x\t\t  "})));
}

#[test]
fn keeps_raw_control_characters_and_invalid_escapes_visible_while_streaming() {
    let mut parser = StreamingJsonParser::new();
    parser.append("{\"a\":\"x\ny \\d \\u12\"");
    assert_eq!(parser.snapshot(), Some(&json!({"a": "x\ny \\d \\u12"})));
}

#[test]
fn keeps_invalid_u_escapes_literal_without_dropping_fields() {
    assert_eq!(
        final_snapshot(
            r#"{"path":"a.tex","content":"\usepackage{x}","mode":"w"}"#,
            Chunks::Fixed(1)
        ),
        Some(json!({"path": "a.tex", "content": "\\usepackage{x}", "mode": "w"}))
    );
    assert_eq!(
        final_snapshot(r#"{"text":"caf\u00e9 \url"}"#, Chunks::Fixed(3)),
        Some(json!({"text": "café \\url"}))
    );
    let mut parser = StreamingJsonParser::new();
    parser.append(r#"{"text":"ab\u00"#);
    assert_eq!(parser.snapshot(), Some(&json!({"text": "ab"})));
}

#[test]
fn keeps_the_last_value_of_a_duplicate_key_in_the_first_keys_position() {
    let text = r#"{"a":1,"b":2,"a":"three"}"#;
    let snapshot = final_snapshot(text, Chunks::Fixed(1)).unwrap();
    let expected = js_json_parse(text).unwrap();
    assert_eq!(snapshot, expected);
    let keys = |value: &Value| {
        value
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(keys(&snapshot), keys(&expected));
}

// ─── Ported test vectors ───

/// partial-json (MIT), with every partial type allowed, as parse_streaming_json uses it.
fn partial_json_vectors() -> Vec<(&'static str, Option<Value>)> {
    vec![
        ("\"", Some(json!(""))),
        ("[\"", Some(json!([""]))),
        ("{\"\": \"", Some(json!({"": ""}))),
        ("n", Some(Value::Null)),
        ("t", Some(json!(true))),
        ("f", Some(json!(false))),
        ("0", Some(json!(0))),
        ("-1.25e+4", Some(json!(-1.25e4))),
        ("-1.25e+", Some(json!(-1.25))),
        ("-1.25e", Some(json!(-1.25))),
        // Differences: an invalid escape stays visible, and Infinity/NaN words are dropped.
        ("\" \\x12", Some(json!(" \\x12"))),
        ("I", None),
        ("-I", None),
        ("N", None),
    ]
}

/// Vercel AI SDK fixJson (Apache-2.0): the expected values are JSON.parse() of its repaired texts.
fn fix_json_vectors() -> Vec<(&'static str, Option<Value>)> {
    vec![
        ("", None),
        ("nul", Some(Value::Null)),
        ("t", Some(json!(true))),
        ("fals", Some(json!(false))),
        ("12.2", Some(json!(12.2))),
        ("-12", Some(json!(-12))),
        ("-", None),
        ("2.5e", Some(json!(2.5))),
        ("2.5e-", Some(json!(2.5))),
        ("2.5e3", Some(json!(2.5e3))),
        ("-2.5e3", Some(json!(-2.5e3))),
        ("2.5E", Some(json!(2.5))),
        ("2.5E-", Some(json!(2.5))),
        ("2.5E3", Some(json!(2.5e3))),
        ("-2.5E3", Some(json!(-2.5e3))),
        ("12.34e", Some(json!(12.34))),
        ("5e", Some(json!(5))),
        ("\"abc", Some(json!("abc"))),
        (
            "\"value with \\\"quoted\\\" text and \\\\ escape",
            Some(json!("value with \"quoted\" text and \\ escape")),
        ),
        ("\"value with \\", Some(json!("value with "))),
        ("\"\\u", Some(json!(""))),
        ("\"\\u12", Some(json!(""))),
        ("\"text \\u00", Some(json!("text "))),
        ("{\"a\":\"\\u12", Some(json!({"a": ""}))),
        (
            "\"value with unicode \u{3c}\"",
            Some(json!("value with unicode <")),
        ),
        ("[", Some(json!([]))),
        ("[[1], [2", Some(json!([[1], [2]]))),
        ("[[\"1\"], [\"2", Some(json!([["1"], ["2"]]))),
        ("[[false], [nu", Some(json!([[false], [null]]))),
        ("[[[]], [[]", Some(json!([[[]], [[]]]))),
        ("[[{}], [{", Some(json!([[{}], [{}]]))),
        ("[1, ", Some(json!([1]))),
        ("[[], 123", Some(json!([[], 123]))),
        ("{\"key\":", Some(json!({}))),
        (
            "{\"a\": {\"b\": 1}, \"c\": {\"d\": 2",
            Some(json!({"a": {"b": 1}, "c": {"d": 2}})),
        ),
        (
            "{\"a\": {\"b\": \"1\"}, \"c\": {\"d\": 2",
            Some(json!({"a": {"b": "1"}, "c": {"d": 2}})),
        ),
        (
            "{\"a\": {\"b\": false}, \"c\": {\"d\": 2",
            Some(json!({"a": {"b": false}, "c": {"d": 2}})),
        ),
        (
            "{\"a\": {\"b\": []}, \"c\": {\"d\": 2",
            Some(json!({"a": {"b": []}, "c": {"d": 2}})),
        ),
        (
            "{\"a\": {\"b\": {}}, \"c\": {\"d\": 2",
            Some(json!({"a": {"b": {}}, "c": {"d": 2}})),
        ),
        ("{\"ke", Some(json!({}))),
        ("{\"k1\": 1, \"k2", Some(json!({"k1": 1}))),
        ("{\"k1\": 1, \"k2\":", Some(json!({"k1": 1}))),
        ("{\"key\": \"value\"  ", Some(json!({"key": "value"}))),
        ("{\"a\": {\"b\": {}", Some(json!({"a": {"b": {}}}))),
        ("[1, [2, 3, [", Some(json!([1, [2, 3, []]]))),
        ("[false, [true, [", Some(json!([false, [true, []]]))),
        ("{\"key\": {\"subKey\":", Some(json!({"key": {}}))),
        (
            "{\"key\": 123, \"key2\": {\"subKey\":",
            Some(json!({"key": 123, "key2": {}})),
        ),
        (
            "{\"key\": null, \"key2\": {\"subKey\":",
            Some(json!({"key": null, "key2": {}})),
        ),
        ("{\"key\": [1, 2, {", Some(json!({"key": [1, 2, {}]}))),
        (
            "[1, 2, {\"key\": \"value\",",
            Some(json!([1, 2, {"key": "value"}])),
        ),
        (
            "{\"a\": {\"b\": [\"c\", {\"d\": \"e\",",
            Some(json!({"a": {"b": ["c", {"d": "e"}]}})),
        ),
        (
            "{\"a\": {\"b\": {\"c\": {\"d\":",
            Some(json!({"a": {"b": {"c": {}}}})),
        ),
        ("{\"a\": 1, \"b\": [", Some(json!({"a": 1, "b": []}))),
        ("{\"a\": 1, \"b\": {", Some(json!({"a": 1, "b": {}}))),
        ("{\"a\": 1, \"b\": \"", Some(json!({"a": 1, "b": ""}))),
        (
            "{\n  \"a\": [\n    {\n      \"a1\": \"v1\",\n      \"a2\": \"v2\",\n      \"a3\": \"v3\"\n    }\n  ],\n  \"b\": [\n    {\n      \"b1\": \"n",
            Some(json!({"a": [{"a1": "v1", "a2": "v2", "a3": "v3"}], "b": [{"b1": "n"}]})),
        ),
        (
            "{\"type\":\"div\",\"children\":[{\"type\":\"Card\",\"props\":{}",
            Some(json!({"type": "div", "children": [{"type": "Card", "props": {}}]})),
        ),
        // Difference: fixJson completes `12.` to 12; like parse_streaming_json, a number cut after
        // its dot is omitted.
        ("12.", None),
        ("12.e", None),
    ]
}

#[test]
fn reads_the_partial_json_and_fix_json_vectors() {
    for (input, expected) in partial_json_vectors().into_iter().chain(fix_json_vectors()) {
        for size in [input.chars().count().max(1), 1, 2] {
            assert_eq!(
                final_snapshot(input, Chunks::Fixed(size)),
                expected,
                "{input:?} in pieces of {size}"
            );
        }
    }
}

/// Asserts `value` is plain, finite JSON data.
fn expect_json(value: &Option<Value>, label: &str) {
    fn walk(value: &Value, label: &str) {
        match value {
            Value::Number(number) => {
                assert!(number.as_f64().is_some_and(f64::is_finite), "{label}")
            }
            Value::Array(items) => items.iter().for_each(|item| walk(item, label)),
            Value::Object(members) => members.values().for_each(|item| walk(item, label)),
            _ => {}
        }
    }
    if let Some(value) = value {
        walk(value, label);
    }
}

#[test]
fn reads_every_json_test_suite_document_json_parse_accepts_exactly_like_it() {
    let suite: Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/json-test-suite.json"
        ))
        .unwrap(),
    )
    .unwrap();
    let files = suite["files"].as_object().unwrap();
    assert!(files.len() > 300);
    for (name, base64) in files {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(base64.as_str().unwrap())
            .unwrap();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let expected = js_json_parse(&text).ok();
        for size in [text.chars().count().max(1), 1, 3] {
            let snapshot = final_snapshot(&text, Chunks::Fixed(size));
            match &expected {
                Some(expected) => assert_eq!(snapshot.as_ref(), Some(expected), "{name}"),
                None => expect_json(&snapshot, name),
            }
        }
    }
}

// ─── Malformed text ───

const MALFORMED: &[&str] = &[
    "{\"a\":1 \"b\":2}",
    "{\"a\" 1, \"b\": 2}",
    "{a:1, 'b': 2}",
    "{\"a\":1,,\"b\":2,}",
    "[1 2, , 3,]",
    "{\"a\":[1,2}, \"b\":3]",
    "]]]}}}{",
    "{\"a\":NaN,\"b\":Infinity,\"c\":-Infinity,\"d\":undefined,\"e\":1}",
    "{\"a\":01,\"b\":1.,\"c\":.5,\"d\":+1,\"e\":0x10,\"f\":1e}",
    "{\"a\":\"x\"} trailing garbage {\"b\":2}",
    "{\"a\": tru, \"b\": nulll, \"c\": falsey}",
    "{\"a\":{\"b\":{\"c\":",
    "{\"a\\\": 1}",
    "\"unterminated \\",
    "// comment\n{\"a\":1}",
    "\u{0}{\u{1}\"a\"\u{2}:\u{3}1}",
    "{\"a\":\"x\"}}}]]]",
];

#[test]
fn never_fails_and_keeps_snapshots_plain_json_at_every_chunk_size() {
    for text in MALFORMED {
        for chunks in CHUNK_SIZES {
            let steps = stream(text, chunks);
            for (_, snapshot) in &steps {
                expect_json(snapshot, text);
            }
            // Every chunk size reads malformed text the same way.
            assert_eq!(
                steps.last().and_then(|(_, s)| s.clone()),
                whole(text),
                "{text}"
            );
        }
    }
}

#[test]
fn tolerates_missing_commas_and_colons_and_drops_values_in_invalid_positions() {
    assert_eq!(whole("{\"a\":1 \"b\":2}"), Some(json!({"a": 1, "b": 2})));
    assert_eq!(whole("{\"a\" 1, \"b\": 2}"), Some(json!({"a": 1, "b": 2})));
    assert_eq!(whole("[1 2, , 3,]"), Some(json!([1, 2, 3])));
    assert_eq!(
        whole("{\"a\":NaN,\"b\":Infinity,\"e\":1}"),
        Some(json!({"e": 1}))
    );
    assert_eq!(
        whole("{\"a\":\"x\"} trailing garbage {\"b\":2}"),
        Some(json!({"a": "x"}))
    );
}

#[test]
fn survives_random_corruption_of_valid_documents() {
    let mut rand = Random(7);
    let noise = [
        "\"", "\\", "{", "}", "[", "]", ",", ":", "x", "1", "-", "\n", "t",
    ];
    for text in random_documents(8, 300, true) {
        let mut characters: Vec<String> = text.chars().map(String::from).collect();
        for _ in 0..3 {
            let at = rand.below(characters.len() + 1);
            if rand.next() < 0.5 {
                if at < characters.len() {
                    characters.remove(at);
                }
            } else {
                characters.insert(at, rand.pick(&noise).to_string());
            }
        }
        let corrupted = characters.concat();
        for (_, snapshot) in stream(&corrupted, Chunks::Random) {
            expect_json(&snapshot, &corrupted);
        }
        // The changes keep a copy equal to the parser's own value, malformed or not.
        let mut parser = StreamingJsonParser::new();
        let mut mirror = Mirror::default();
        for piece in pieces(&corrupted, Chunks::Fixed(4)) {
            mirror.apply(parser.append(piece));
        }
        // A space ends a number or literal at the end, which snapshots show and changes do not report yet.
        mirror.apply(parser.append(" "));
        assert_eq!(mirror.value.as_ref(), parser.snapshot(), "{corrupted:?}");
    }
}

// ─── Changes ───

#[test]
fn changes_keep_a_copy_equal_to_the_value_read_so_far_and_to_json_parse_at_the_end() {
    for text in random_documents(9, 200, true) {
        for chunks in CHUNK_SIZES {
            let mirror = mirror_stream(&text, chunks);
            assert_eq!(mirror.value, js_json_parse(&text).ok(), "{text}");
        }
    }
}

#[test]
fn changes_report_a_number_that_ends_and_a_string_that_starts_in_one_piece_in_order() {
    let mut parser = StreamingJsonParser::new();
    assert_eq!(parser.append("{\"n\":1"), [set(vec![], json!({}))]);
    assert_eq!(
        parser.append("2,\"s\":\"ab"),
        [
            set(vec![key("n")], json!(12)),
            set(vec![key("s")], json!("ab"))
        ]
    );
    assert_eq!(
        parser.append("c\",\"t\":tru"),
        [JsonChange::Append {
            path: vec![key("s")],
            text: "c".into()
        }]
    );
    assert_eq!(parser.append("e}"), [set(vec![key("t")], json!(true))]);
    assert_eq!(parser.append(" {\"ignored\":1}"), []);
}

#[test]
fn changes_report_nested_containers_empty_then_their_members_and_items() {
    use JsonPathSegment::Index;
    let mut parser = StreamingJsonParser::new();
    assert_eq!(
        parser.append("{\"a\":[1,{\"b\":[\"x\""),
        [
            set(vec![], json!({})),
            set(vec![key("a")], json!([])),
            set(vec![key("a"), Index(0)], json!(1)),
            set(vec![key("a"), Index(1)], json!({})),
            set(vec![key("a"), Index(1), key("b")], json!([])),
            set(vec![key("a"), Index(1), key("b"), Index(0)], json!("x")),
        ]
    );
    assert_eq!(
        parser.append(",\"y"),
        [set(
            vec![key("a"), Index(1), key("b"), Index(1)],
            json!("y")
        )]
    );
    assert_eq!(
        parser.append("z\"]}],\"a\":\"again\"}"),
        [
            JsonChange::Append {
                path: vec![key("a"), Index(1), key("b"), Index(1)],
                text: "z".into()
            },
            set(vec![key("a")], json!("again")),
        ]
    );
}

#[test]
fn changes_hold_split_escapes_back_and_report_their_decoded_text_once_complete() {
    let text = r#"{"a":"x\u00e9\n\\\"\ud83d\ude00\q"}"#;
    let expected = js_json_parse(r#"{"a":"x\u00e9\n\\\"\ud83d\ude00\\q"}"#).unwrap();
    for first in 1..text.len() {
        for second in first..text.len() {
            let mut parser = StreamingJsonParser::new();
            let mut mirror = Mirror::default();
            for piece in [&text[..first], &text[first..second], &text[second..]] {
                mirror.apply(parser.append(piece));
            }
            assert_eq!(mirror.value.as_ref(), Some(&expected));
        }
    }
    let mut parser = StreamingJsonParser::new();
    parser.append("{\"a\":\"x");
    assert_eq!(parser.append("\\u00"), []);
    assert_eq!(
        parser.append("e9"),
        [JsonChange::Append {
            path: vec![key("a")],
            text: "é".into()
        }]
    );
}

#[test]
fn changes_report_a_container_nested_deeper_than_32_levels_once_whole_when_it_closes() {
    let depth = 40;
    let text = format!("{}\"deep\"{}", "{\"a\":".repeat(depth), "}".repeat(depth));
    let mut parser = StreamingJsonParser::new();
    let mut mirror = Mirror::default();
    for piece in pieces(&text, Chunks::Fixed(3)) {
        mirror.apply(parser.append(piece));
    }
    assert_eq!(mirror.value, js_json_parse(&text).ok());
    let changes = StreamingJsonParser::new().append(&text).to_vec();
    // The containers down to path length 32, then the one at length 32 again, whole, as it closes.
    assert_eq!(changes.len(), 34);
    let JsonChange::Set { path, value } = changes.last().unwrap() else {
        panic!("a set")
    };
    assert_eq!(path.len(), 32);
    assert_eq!(
        Some(value),
        js_json_parse(&text[32 * 5..text.len() - 32]).ok().as_ref()
    );
}

#[test]
fn changes_report_nothing_for_values_in_invalid_positions_and_text_after_the_root() {
    let mut parser = StreamingJsonParser::new();
    assert_eq!(
        parser.append("{\"a\":1 2, \"b\" \"x\"}"),
        [
            set(vec![], json!({})),
            set(vec![key("a")], json!(1)),
            set(vec![key("b")], json!("x"))
        ]
    );
    assert_eq!(parser.append("{\"c\":3}"), []);
}

// ─── Nesting and large documents ───

#[test]
fn drops_containers_nested_deeper_than_the_limit_without_overflowing_the_stack() {
    let depth = 200_000;
    let text = format!("{}1{}", "[".repeat(depth), "]".repeat(depth));
    let mut parser = StreamingJsonParser::new();
    for piece in pieces(&text, Chunks::Fixed(4096)) {
        parser.append(piece);
        parser.snapshot();
    }
    let mut value = parser.snapshot().cloned().expect("the shallow part shows");
    let mut levels = 0;
    while let Value::Array(mut items) = value {
        levels += 1;
        value = if items.is_empty() {
            Value::Null
        } else {
            items.remove(0)
        };
    }
    assert_eq!(levels, pi_ai::utils::streaming_json::MAX_NESTING);
}

#[test]
fn keeps_very_large_documents_moving_forward_while_their_preview_lags() {
    let items: Vec<usize> = (0..100_000).collect();
    let large = json!({"items": items}).to_string();
    let mut parser = StreamingJsonParser::new();
    let mut shown = 0;
    let mut fresh = 0;
    for piece in pieces(&large, Chunks::Fixed(16)) {
        parser.append(piece);
        let before = parser.version();
        let count = parser
            .snapshot()
            .and_then(|v| v["items"].as_array().map(Vec::len))
            .unwrap_or(0);
        fresh += usize::from(parser.version() != before);
        // The preview only moves forward.
        assert!(count >= shown);
        shown = count;
    }
    assert!(
        fresh < large.len() / 16,
        "a large document does not copy after every piece ({fresh})"
    );
    assert_eq!(parser.snapshot().cloned(), js_json_parse(&large).ok());
}

// ─── ToolCallArgumentsStream ───

#[test]
fn shows_partial_arguments_as_an_object_and_finishes_with_one_full_parse() {
    let mut args = ToolCallArgumentsStream::new();
    assert_eq!(args.append("  "), None, "nothing to show yet");
    assert_eq!(
        args.append(r#"{"path":"a.ts","content":"x"#),
        Some(
            json!({"path": "a.ts", "content": "x"})
                .as_object()
                .unwrap()
                .clone()
        )
    );
    assert!(args.append("y").is_some());
    assert_eq!(args.append(""), None, "an empty piece keeps the arguments");
    assert_eq!(args.text(), r#"  {"path":"a.ts","content":"xy"#);
    assert_eq!(
        args.finish(),
        parse_tool_call_arguments(Some(r#"  {"path":"a.ts","content":"xy"#))
    );
    args.append("\"}");
    let finished = args.finish();
    assert_eq!(
        Value::Object(finished.arguments),
        json!({"path": "a.ts", "content": "xy"})
    );
    assert_eq!(finished.arguments_error, None);
}

#[test]
fn shows_non_object_roots_as_no_arguments_yet() {
    let mut args = ToolCallArgumentsStream::new();
    assert_eq!(args.append("[1,2"), Some(Map::new()));
    args.append("]");
    assert!(
        args.finish()
            .arguments_error
            .unwrap()
            .starts_with("Tool arguments must be a JSON object")
    );
}

#[test]
fn replaces_the_text_with_the_providers_complete_text_returning_the_missing_piece() {
    let mut args = ToolCallArgumentsStream::new();
    args.append(r#"{"a":"x"#);
    let (delta, arguments) = args.replace(r#"{"a":"xyz"}"#);
    assert_eq!(delta.as_deref(), Some("yz\"}"));
    assert_eq!(arguments.map(Value::Object), Some(json!({"a": "xyz"})));
    let (delta, arguments) = args.replace(r#"{"b":2}"#);
    assert_eq!(delta, None);
    assert_eq!(arguments.map(Value::Object), Some(json!({"b": 2})));
    assert_eq!(args.text(), r#"{"b":2}"#);
}
