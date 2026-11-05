//! The nginx `combined` access log format:
//! `addr - user [time] "request" status bytes "referer" "agent"`.

use crate::value::{Fields, Value};

struct Cursor<'a> {
    rest: &'a str,
}

impl<'a> Cursor<'a> {
    fn word(&mut self, what: &str) -> Result<&'a str, String> {
        let text = self.rest.trim_start_matches(' ');
        if text.is_empty() {
            return Err(format!("line ends before {what}"));
        }
        let end = text.find(' ').unwrap_or(text.len());
        self.rest = &text[end..];
        Ok(&text[..end])
    }

    fn bracketed(&mut self) -> Result<&'a str, String> {
        let text = self.rest.trim_start_matches(' ');
        let inner = text.strip_prefix('[').ok_or("expected [time]")?;
        let end = inner.find(']').ok_or("unterminated [time]")?;
        self.rest = &inner[end + 1..];
        Ok(&inner[..end])
    }

    /// Reads a quoted string. A backslash protects the next character, and `\xHH` is what nginx
    /// writes for bytes it escapes.
    fn quoted(&mut self, what: &str) -> Result<String, String> {
        let text = self.rest.trim_start_matches(' ');
        let inner = text.strip_prefix('"').ok_or_else(|| format!("expected quoted {what}"))?;
        let mut out = Vec::new();
        let bytes = inner.as_bytes();
        let mut at = 0;
        while at < bytes.len() {
            match bytes[at] {
                b'"' => {
                    self.rest = &inner[at + 1..];
                    return Ok(String::from_utf8_lossy(&out).into_owned());
                }
                b'\\' if bytes.get(at + 1) == Some(&b'x') => {
                    let byte = inner.get(at + 2..at + 4).and_then(|hex| u8::from_str_radix(hex, 16).ok());
                    match byte {
                        Some(byte) => {
                            out.push(byte);
                            at += 4;
                        }
                        None => {
                            out.push(b'\\');
                            at += 1;
                        }
                    }
                }
                b'\\' if at + 1 < bytes.len() => {
                    out.push(bytes[at + 1]);
                    at += 2;
                }
                other => {
                    out.push(other);
                    at += 1;
                }
            }
        }
        Err(format!("unterminated quoted {what}"))
    }
}

/// `-` is how the format writes a value that isn't there.
fn dash_is_null(text: &str) -> Value {
    if text == "-" {
        Value::Null
    } else {
        Value::Text(text.to_string())
    }
}

pub fn parse(line: &str) -> Result<Fields, String> {
    let mut cursor = Cursor { rest: line };
    let mut fields: Fields = Vec::with_capacity(12);
    let mut put = |name: &str, value: Value| fields.push((name.to_string(), value));

    put("remote_addr", Value::Text(cursor.word("the address")?.to_string()));
    let ident = cursor.word("the ident")?;
    if ident != "-" {
        return Err(format!("expected - after the address, found {ident:?}"));
    }
    put("remote_user", dash_is_null(cursor.word("the user")?));
    let time = cursor.bracketed()?;
    crate::time::parse_text(time).ok_or_else(|| format!("bad time {time:?}"))?;
    put("time", Value::Text(time.to_string()));

    let request = cursor.quoted("request")?;
    let pieces: Vec<&str> = request.split(' ').collect();
    if let [method, path, protocol] = pieces[..] {
        put("method", Value::Text(method.to_string()));
        put("path", Value::Text(path.to_string()));
        put("protocol", Value::Text(protocol.to_string()));
    }
    put("request", Value::Text(request.clone()));

    let status = cursor.word("the status")?;
    let status: u16 = status.parse().map_err(|_| format!("bad status {status:?}"))?;
    put("status", Value::Number(f64::from(status)));
    let bytes = cursor.word("the size")?;
    let bytes: u64 = bytes.parse().map_err(|_| format!("bad size {bytes:?}"))?;
    put("bytes", Value::Number(bytes as f64));

    put("referer", dash_is_null(&cursor.quoted("referer")?));
    put("user_agent", dash_is_null(&cursor.quoted("user agent")?));

    let extra = cursor.rest.trim();
    if !extra.is_empty() {
        put("extra", Value::Text(extra.to_string()));
    }
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::lookup;

    const LINE: &str = r#"203.0.113.9 - alpha [10/Oct/2023:13:55:36 -0700] "GET /items?page=2 HTTP/1.1" 200 2326 "https://example.com/" "Mozilla/5.0 (X11)""#;

    #[test]
    fn a_full_line() {
        let fields = parse(LINE).unwrap();
        let text = |name: &str| lookup(&fields, name).unwrap().display_text().into_owned();
        assert_eq!(text("remote_addr"), "203.0.113.9");
        assert_eq!(text("remote_user"), "alpha");
        assert_eq!(text("time"), "10/Oct/2023:13:55:36 -0700");
        assert_eq!(text("method"), "GET");
        assert_eq!(text("path"), "/items?page=2");
        assert_eq!(text("protocol"), "HTTP/1.1");
        assert_eq!(lookup(&fields, "status"), Some(&Value::Number(200.0)));
        assert_eq!(lookup(&fields, "bytes"), Some(&Value::Number(2326.0)));
        assert_eq!(text("referer"), "https://example.com/");
        assert_eq!(text("user_agent"), "Mozilla/5.0 (X11)");
    }

    #[test]
    fn dashes_become_null() {
        let line = r#"203.0.113.9 - - [10/Oct/2023:13:55:36 +0000] "GET / HTTP/1.1" 304 0 "-" "-""#;
        let fields = parse(line).unwrap();
        assert_eq!(lookup(&fields, "remote_user"), Some(&Value::Null));
        assert_eq!(lookup(&fields, "referer"), Some(&Value::Null));
        assert_eq!(lookup(&fields, "user_agent"), Some(&Value::Null));
    }

    #[test]
    fn a_junk_request_keeps_the_raw_field_only() {
        let line = "203.0.113.9 - - [10/Oct/2023:13:55:36 +0000] \"\\x16\\x03\\x01\" 400 150 \"-\" \"-\"";
        let fields = parse(line).unwrap();
        assert_eq!(lookup(&fields, "request").unwrap().display_text(), "\u{16}\u{3}\u{1}");
        assert_eq!(lookup(&fields, "method"), None);
    }

    #[test]
    fn escaped_quotes_in_strings() {
        let line = r#"203.0.113.9 - - [10/Oct/2023:13:55:36 +0000] "GET / HTTP/1.1" 200 1 "-" "a \"quoted\" agent""#;
        assert_eq!(lookup(&parse(line).unwrap(), "user_agent").unwrap().display_text(), "a \"quoted\" agent");
        let line = r#"203.0.113.9 - - [10/Oct/2023:13:55:36 +0000] "GET / HTTP/1.1" 200 1 "-" "a\x22b""#;
        assert_eq!(lookup(&parse(line).unwrap(), "user_agent").unwrap().display_text(), "a\"b");
    }

    #[test]
    fn extra_columns_are_kept_together() {
        let line = format!("{LINE} 0.123 \"upstream\"");
        assert_eq!(lookup(&parse(&line).unwrap(), "extra").unwrap().display_text(), "0.123 \"upstream\"");
    }

    #[test]
    fn broken_lines_are_refused() {
        for line in [
            "", "just text", "203.0.113.9 x - [10/Oct/2023:13:55:36 +0000] \"GET\" 200 1 \"-\" \"-\"",
            "203.0.113.9 - - no-time \"GET\" 200 1 \"-\" \"-\"",
            "203.0.113.9 - - [garbage] \"GET\" 200 1 \"-\" \"-\"",
            "203.0.113.9 - - [10/Oct/2023:13:55:36 +0000] \"GET\" abc 1 \"-\" \"-\"",
            "203.0.113.9 - - [10/Oct/2023:13:55:36 +0000] \"GET\" 200 -1 \"-\" \"-\"",
            "203.0.113.9 - - [10/Oct/2023:13:55:36 +0000] \"GET\" 200 1",
            "203.0.113.9 - - [10/Oct/2023:13:55:36 +0000] \"GET 200 1 \"-\" \"-",
        ] {
            assert!(parse(line).is_err(), "{line}");
        }
    }
}
