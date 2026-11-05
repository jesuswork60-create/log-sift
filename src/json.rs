//! A small JSON reader and writer. Object order and duplicate keys are kept as written.

use std::fmt;

use crate::value::Value;

const MAX_DEPTH: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonError {
    /// Byte offset into the input.
    pub offset: usize,
    pub message: String,
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.message, self.offset)
    }
}

pub fn parse(text: &str) -> Result<Value, JsonError> {
    let mut reader = Reader { bytes: text.as_bytes(), text, at: 0 };
    reader.spaces();
    let value = reader.value(0)?;
    reader.spaces();
    if reader.at != reader.bytes.len() {
        return Err(reader.error("unexpected text after the value"));
    }
    Ok(value)
}

struct Reader<'a> {
    bytes: &'a [u8],
    text: &'a str,
    at: usize,
}

impl Reader<'_> {
    fn error(&self, message: &str) -> JsonError {
        JsonError { offset: self.at, message: message.to_string() }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn spaces(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, JsonError> {
        if depth > MAX_DEPTH {
            return Err(self.error("nested too deeply"));
        }
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string().map(Value::Text),
            Some(b't') => self.word("true", Value::Bool(true)),
            Some(b'f') => self.word("false", Value::Bool(false)),
            Some(b'n') => self.word("null", Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.error("unexpected character")),
            None => Err(self.error("unexpected end of input")),
        }
    }

    fn word(&mut self, word: &str, value: Value) -> Result<Value, JsonError> {
        if self.text[self.at..].starts_with(word) {
            self.at += word.len();
            Ok(value)
        } else {
            Err(self.error("unexpected word"))
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.at;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.at += 1;
        }
        self.at - start
    }

    fn number(&mut self) -> Result<Value, JsonError> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        let first = self.at;
        if self.digits() == 0 {
            return Err(self.error("expected a digit"));
        }
        if self.bytes[first] == b'0' && self.at - first > 1 {
            self.at = first;
            return Err(self.error("leading zeros are not allowed"));
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            if self.digits() == 0 {
                return Err(self.error("expected a digit after the decimal point"));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if self.digits() == 0 {
                return Err(self.error("expected a digit in the exponent"));
            }
        }
        match self.text[start..self.at].parse::<f64>() {
            Ok(number) if number.is_finite() => Ok(Value::Number(number)),
            _ => {
                self.at = start;
                Err(self.error("number out of range"))
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let digits = self.text.get(self.at..self.at + 4).filter(|d| d.bytes().all(|b| b.is_ascii_hexdigit()));
        match digits {
            Some(digits) => {
                self.at += 4;
                Ok(u32::from_str_radix(digits, 16).expect("checked hex digits"))
            }
            None => Err(self.error("expected four hex digits")),
        }
    }

    fn string(&mut self) -> Result<String, JsonError> {
        self.at += 1;
        let mut out = String::new();
        loop {
            let start = self.at;
            while !matches!(self.peek(), None | Some(b'"' | b'\\' | 0..=0x1f)) {
                self.at += 1;
            }
            out.push_str(&self.text[start..self.at]);
            match self.peek() {
                Some(b'"') => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.at += 1;
                    let escape = self.peek().ok_or_else(|| self.error("unfinished escape"))?;
                    self.at += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.unicode_escape()?),
                        _ => {
                            self.at -= 1;
                            return Err(self.error("unknown escape"));
                        }
                    }
                }
                Some(_) => return Err(self.error("control character in a string")),
                None => return Err(self.error("unterminated string")),
            }
        }
    }

    fn unicode_escape(&mut self) -> Result<char, JsonError> {
        let first = self.hex4()?;
        let code = match first {
            0xD800..=0xDBFF => {
                if !self.text[self.at..].starts_with("\\u") {
                    return Err(self.error("high surrogate without its pair"));
                }
                self.at += 2;
                let second = self.hex4()?;
                if !(0xDC00..=0xDFFF).contains(&second) {
                    return Err(self.error("high surrogate without its pair"));
                }
                0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00)
            }
            0xDC00..=0xDFFF => return Err(self.error("low surrogate without its pair")),
            other => other,
        };
        char::from_u32(code).ok_or_else(|| self.error("bad unicode escape"))
    }

    fn array(&mut self, depth: usize) -> Result<Value, JsonError> {
        self.at += 1;
        let mut items = Vec::new();
        self.spaces();
        if self.peek() == Some(b']') {
            self.at += 1;
            return Ok(Value::Array(items));
        }
        loop {
            self.spaces();
            items.push(self.value(depth + 1)?);
            self.spaces();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b']') => {
                    self.at += 1;
                    return Ok(Value::Array(items));
                }
                _ => return Err(self.error("expected , or ]")),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, JsonError> {
        self.at += 1;
        let mut fields = Vec::new();
        self.spaces();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Ok(Value::Object(fields));
        }
        loop {
            self.spaces();
            if self.peek() != Some(b'"') {
                return Err(self.error("expected a field name"));
            }
            let key = self.string()?;
            self.spaces();
            if self.peek() != Some(b':') {
                return Err(self.error("expected :"));
            }
            self.at += 1;
            self.spaces();
            let value = self.value(depth + 1)?;
            fields.push((key, value));
            self.spaces();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b'}') => {
                    self.at += 1;
                    return Ok(Value::Object(fields));
                }
                _ => return Err(self.error("expected , or }")),
            }
        }
    }
}

pub fn write_string(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

pub fn write_value(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) => out.push_str(&crate::value::format_number(*number)),
        Value::Text(text) => write_string(text, out),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_value(item, out);
            }
            out.push(']');
        }
        Value::Object(fields) => write_object(fields, out),
    }
}

pub fn write_object(fields: &[(String, Value)], out: &mut String) {
    out.push('{');
    for (index, (key, value)) in fields.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        write_string(key, out);
        out.push(':');
        write_value(value, out);
    }
    out.push('}');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(text: &str) -> String {
        let mut out = String::new();
        write_value(&parse(text).unwrap(), &mut out);
        out
    }

    #[test]
    fn scalars() {
        assert_eq!(parse("null").unwrap(), Value::Null);
        assert_eq!(parse(" true ").unwrap(), Value::Bool(true));
        assert_eq!(parse("-1.5e2").unwrap(), Value::Number(-150.0));
        assert_eq!(parse("\"hi\"").unwrap(), Value::Text("hi".into()));
    }

    #[test]
    fn order_and_duplicates_survive() {
        assert_eq!(round_trip(r#"{"b":1,"a":2,"b":3}"#), r#"{"b":1,"a":2,"b":3}"#);
    }

    #[test]
    fn nested_values_round_trip() {
        let text = r#"{"a":[1,{"b":null},[]],"c":{}}"#;
        assert_eq!(round_trip(text), text);
    }

    #[test]
    fn escapes_are_decoded() {
        let value = parse(r#""a\"b\\c\/d\n\t\u0041\u00e9""#).unwrap();
        assert_eq!(value, Value::Text("a\"b\\c/d\n\tAé".into()));
    }

    #[test]
    fn surrogate_pairs_join() {
        assert_eq!(parse(r#""\ud83d\ude00""#).unwrap(), Value::Text("😀".into()));
    }

    #[test]
    fn lone_surrogates_are_errors() {
        assert!(parse(r#""\ud83d""#).is_err());
        assert!(parse(r#""\ud83dx""#).is_err());
        assert!(parse(r#""\ude00""#).is_err());
        assert!(parse(r#""\ud83d\u0041""#).is_err());
    }

    #[test]
    fn writer_escapes_what_it_must() {
        let mut out = String::new();
        write_string("a\"b\\c\n\u{1}é", &mut out);
        assert_eq!(out, "\"a\\\"b\\\\c\\n\\u0001é\"");
        assert_eq!(parse(&out).unwrap(), Value::Text("a\"b\\c\n\u{1}é".into()));
    }

    #[test]
    fn broken_input_is_reported_with_an_offset() {
        let error = parse(r#"{"a":}"#).unwrap_err();
        assert_eq!(error.offset, 5);
        for text in [
            "", "{", "[", "[1,]", "{\"a\"}", "{\"a\":1,}", "{a:1}", "tru", "01", "-", "1.", "1e", "\"abc", "\"\\x\"", "1 2",
            "\"a\nb\"", "\"\\u12\"", "1e999",
        ] {
            assert!(parse(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn multibyte_text_near_a_unicode_escape_is_not_a_panic() {
        assert!(parse("\"\\u12é\"").is_err());
    }

    #[test]
    fn depth_is_limited() {
        let deep = "[".repeat(MAX_DEPTH + 5);
        assert_eq!(parse(&deep).unwrap_err().message, "nested too deeply");
        let fine = format!("{}{}", "[".repeat(50), "]".repeat(50));
        assert!(parse(&fine).is_ok());
    }
}
