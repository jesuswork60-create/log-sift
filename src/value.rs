//! The value model shared by every log format.

use std::borrow::Cow;

/// Field names and values in the order they appeared. Duplicates are kept; lookups see the last.
pub type Fields = Vec<(String, Value)>;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    Text(String),
    Array(Vec<Value>),
    Object(Fields),
}

impl Value {
    /// A short name for the kind of value, as shown in schema listings.
    pub fn kind(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Number(_) => "number",
            Value::Text(_) => "text",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }

    /// The number this value stands for. Text such as `"12.5"` counts, because logfmt
    /// and access logs carry numbers as text.
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Number(number) => Some(*number),
            Value::Text(text) => parse_number(text),
            _ => None,
        }
    }

    /// The text form used for pattern matching, grouping and tables.
    pub fn display_text(&self) -> Cow<'_, str> {
        match self {
            Value::Null => Cow::Borrowed("null"),
            Value::Bool(true) => Cow::Borrowed("true"),
            Value::Bool(false) => Cow::Borrowed("false"),
            Value::Number(number) => Cow::Owned(format_number(*number)),
            Value::Text(text) => Cow::Borrowed(text),
            Value::Array(_) | Value::Object(_) => {
                let mut out = String::new();
                crate::json::write_value(self, &mut out);
                Cow::Owned(out)
            }
        }
    }

    /// Follows a dotted path below this value. Numeric parts index arrays.
    pub fn lookup(&self, path: &str) -> Option<&Value> {
        match self {
            Value::Object(fields) => lookup(fields, path),
            Value::Array(items) => {
                let (head, tail) = match path.split_once('.') {
                    Some((head, tail)) => (head, Some(tail)),
                    None => (path, None),
                };
                let item = items.get(head.parse::<usize>().ok()?)?;
                match tail {
                    Some(tail) => item.lookup(tail),
                    None => Some(item),
                }
            }
            _ => None,
        }
    }
}

/// Finds a field by dotted path. A key that literally contains the dots wins over nesting,
/// and each level is tried the same way, so `{"a.b": {"c": 1}}` answers `a.b.c`.
pub fn lookup<'a>(fields: &'a [(String, Value)], path: &str) -> Option<&'a Value> {
    let named = |name: &str| fields.iter().rev().find(|(key, _)| key == name).map(|(_, value)| value);
    if let Some(value) = named(path) {
        return Some(value);
    }
    path.match_indices('.').find_map(|(dot, _)| named(&path[..dot])?.lookup(&path[dot + 1..]))
}

/// Parses a plain decimal number. Stricter than `str::parse`, which also takes `inf` and `nan`
/// and would turn a word in a log line into a number.
pub fn parse_number(text: &str) -> Option<f64> {
    let digits = text.strip_prefix(['-', '+']).unwrap_or(text);
    if !digits.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
        return None;
    }
    if !digits.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-')) {
        return None;
    }
    text.parse::<f64>().ok().filter(|number| number.is_finite())
}

/// Writes whole numbers without a fraction and anything else in its shortest exact form.
pub fn format_number(number: f64) -> String {
    format!("{number}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(fields: &[(&str, Value)]) -> Fields {
        fields.iter().map(|(key, value)| (key.to_string(), value.clone())).collect()
    }

    #[test]
    fn numbers_in_text_count_but_words_do_not() {
        assert_eq!(Value::Text("12.5".into()).as_number(), Some(12.5));
        assert_eq!(Value::Text("-3e2".into()).as_number(), Some(-300.0));
        for word in ["nan", "NaN", "inf", "-infinity", "", "12a", "-", "e5", "1_000"] {
            assert_eq!(parse_number(word), None, "{word}");
        }
        assert_eq!(parse_number("1e999"), None);
    }

    #[test]
    fn whole_numbers_print_without_a_fraction() {
        assert_eq!(format_number(200.0), "200");
        assert_eq!(format_number(0.5), "0.5");
        assert_eq!(format_number(-1.25), "-1.25");
    }

    #[test]
    fn lookup_walks_objects_and_arrays() {
        let inner = Value::Object(object(&[("b", Value::Number(1.0))]));
        let list = Value::Array(vec![Value::Text("x".into()), inner.clone()]);
        let fields = object(&[("a", inner), ("list", list)]);
        assert_eq!(lookup(&fields, "a.b"), Some(&Value::Number(1.0)));
        assert_eq!(lookup(&fields, "list.0"), Some(&Value::Text("x".into())));
        assert_eq!(lookup(&fields, "list.1.b"), Some(&Value::Number(1.0)));
        assert_eq!(lookup(&fields, "list.9"), None);
        assert_eq!(lookup(&fields, "a.missing"), None);
        assert_eq!(lookup(&fields, "a.b.c"), None);
    }

    #[test]
    fn a_dotted_key_beats_nesting() {
        let nested = Value::Object(object(&[("b", Value::Number(1.0))]));
        let fields = object(&[("a", nested), ("a.b", Value::Number(2.0))]);
        assert_eq!(lookup(&fields, "a.b"), Some(&Value::Number(2.0)));
    }

    #[test]
    fn a_dotted_key_can_be_a_step_on_the_way() {
        let nested = Value::Object(object(&[("c", Value::Number(3.0))]));
        let fields = object(&[("a.b", nested)]);
        assert_eq!(lookup(&fields, "a.b.c"), Some(&Value::Number(3.0)));
    }

    #[test]
    fn the_last_duplicate_wins() {
        let fields = object(&[("a", Value::Number(1.0)), ("a", Value::Number(2.0))]);
        assert_eq!(lookup(&fields, "a"), Some(&Value::Number(2.0)));
    }

    #[test]
    fn compound_values_display_as_json() {
        let value = Value::Array(vec![Value::Number(1.0), Value::Text("x".into())]);
        assert_eq!(value.display_text(), "[1,\"x\"]");
    }
}
