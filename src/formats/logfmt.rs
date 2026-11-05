//! `key=value key2="quoted value" flag` lines. Values stay text; comparisons coerce them later.

use crate::value::{Fields, Value};

/// Parses a line. It needs at least one `key=value` pair, otherwise every sentence of prose
/// would count as a list of flags.
pub fn parse(line: &str) -> Result<Fields, String> {
    let mut fields = Vec::new();
    let mut saw_pair = false;
    let mut rest = line.trim_start();
    while !rest.is_empty() {
        let end = rest.find([' ', '\t', '=', '"']).unwrap_or(rest.len());
        let key = &rest[..end];
        if key.is_empty() {
            return Err(format!("expected a key at {:?}", shorten(rest)));
        }
        rest = &rest[end..];
        match rest.chars().next() {
            Some('=') => {
                saw_pair = true;
                rest = &rest[1..];
                let (value, after) = if rest.starts_with('"') { quoted(rest)? } else { bare(rest)? };
                fields.push((key.to_string(), Value::Text(value)));
                rest = after;
            }
            Some('"') => return Err(format!("unexpected quote after {key:?}")),
            _ => fields.push((key.to_string(), Value::Bool(true))),
        }
        rest = rest.trim_start();
    }
    if !saw_pair {
        return Err("no key=value pairs".to_string());
    }
    Ok(fields)
}

fn shorten(text: &str) -> String {
    text.chars().take(20).collect()
}

fn bare(text: &str) -> Result<(String, &str), String> {
    let end = text.find([' ', '\t']).unwrap_or(text.len());
    if text[..end].contains('"') {
        return Err(format!("stray quote in {:?}", shorten(&text[..end])));
    }
    Ok((text[..end].to_string(), &text[end..]))
}

fn quoted(text: &str) -> Result<(String, &str), String> {
    let mut value = String::new();
    let mut chars = text.char_indices().skip(1);
    while let Some((index, c)) = chars.next() {
        match c {
            '"' => {
                let after = &text[index + 1..];
                if !after.is_empty() && !after.starts_with([' ', '\t']) {
                    return Err(format!("text straight after a closing quote: {:?}", shorten(after)));
                }
                return Ok((value, after));
            }
            '\\' => match chars.next() {
                Some((_, 'n')) => value.push('\n'),
                Some((_, 't')) => value.push('\t'),
                Some((_, 'r')) => value.push('\r'),
                Some((_, other)) => value.push(other),
                None => break,
            },
            other => value.push(other),
        }
    }
    Err("unterminated quote".to_string())
}

fn needs_quotes(text: &str) -> bool {
    text.is_empty() || text.chars().any(|c| c == '=' || c == '"' || c.is_whitespace() || c.is_control())
}

fn write_quoted(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out.push('"');
}

/// Writes fields as one line. `true` flags stay as `key=true`; null is left empty, and nested
/// values are written as quoted JSON.
pub fn write(fields: &[(String, Value)]) -> String {
    let mut out = String::new();
    for (index, (key, value)) in fields.iter().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        out.push_str(key);
        out.push('=');
        if !matches!(value, Value::Null) {
            let text = value.display_text();
            if needs_quotes(&text) {
                write_quoted(&text, &mut out);
            } else {
                out.push_str(&text);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(fields: &Fields, key: &str) -> String {
        crate::value::lookup(fields, key).unwrap().display_text().into_owned()
    }

    #[test]
    fn pairs_and_flags() {
        let fields = parse("level=info msg=\"all good\" retry count=3").unwrap();
        assert_eq!(text(&fields, "level"), "info");
        assert_eq!(text(&fields, "msg"), "all good");
        assert_eq!(crate::value::lookup(&fields, "retry"), Some(&Value::Bool(true)));
        assert_eq!(fields.len(), 4);
    }

    #[test]
    fn empty_values() {
        let fields = parse("a= b=\"\" c=1").unwrap();
        assert_eq!(text(&fields, "a"), "");
        assert_eq!(text(&fields, "b"), "");
        assert_eq!(text(&fields, "c"), "1");
    }

    #[test]
    fn values_may_hold_equals_signs() {
        assert_eq!(text(&parse("q=a=b").unwrap(), "q"), "a=b");
    }

    #[test]
    fn escapes() {
        let fields = parse(r#"m="say \"hi\"\nC:\\x""#).unwrap();
        assert_eq!(text(&fields, "m"), "say \"hi\"\nC:\\x");
    }

    #[test]
    fn extra_white_space_is_fine() {
        assert_eq!(parse("  a=1 \t b=2  ").unwrap().len(), 2);
    }

    #[test]
    fn multibyte_text_is_kept() {
        assert_eq!(text(&parse("名前=値 b=\"é ü\"").unwrap(), "名前"), "値");
    }

    #[test]
    fn broken_lines_are_refused() {
        for line in ["just some words", "", "=x", "a=\"open", "a=\"x\"y", "a=b\"c", "\"a\"=1", "a=\"x\\"] {
            assert!(parse(line).is_err(), "{line}");
        }
    }

    #[test]
    fn write_quotes_only_when_needed() {
        let fields = vec![
            ("a".to_string(), Value::Text("plain".into())),
            ("b".to_string(), Value::Text("two words".into())),
            ("c".to_string(), Value::Number(1.5)),
            ("d".to_string(), Value::Null),
            ("e".to_string(), Value::Text(String::new())),
            ("f".to_string(), Value::Array(vec![Value::Number(1.0)])),
        ];
        assert_eq!(write(&fields), r#"a=plain b="two words" c=1.5 d= e="" f=[1]"#);
    }

    #[test]
    fn written_lines_read_back() {
        let fields = vec![
            ("q".to_string(), Value::Text("say \"hi\"\n\tback\\slash=1".into())),
            ("n".to_string(), Value::Text("7".into())),
        ];
        assert_eq!(parse(&write(&fields)).unwrap(), fields);
    }
}
