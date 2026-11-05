//! The filter language. See `parser` for the grammar and `Query::matches` for what a match means.

mod lexer;
mod parser;

pub use lexer::{tokenize, Token, TokenKind};

use std::fmt;

use crate::regex::Regex;
use crate::value::{lookup, parse_number, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryError {
    /// Character position in the query text.
    pub position: usize,
    pub message: String,
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at character {}", self.message, self.position)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextOp {
    Contains,
    StartsWith,
    EndsWith,
    Glob,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Exists(String),
    Compare { field: String, op: CompareOp, value: Value },
    Regex { field: String, regex: Regex, negated: bool },
    In { field: String, values: Vec<Value>, negated: bool },
    Text { field: String, op: TextOp, needle: String },
}

/// A parsed filter, ready to run against any number of records.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    expr: Expr,
}

impl Query {
    pub fn parse(text: &str) -> Result<Query, QueryError> {
        parser::parse(text).map(|expr| Query { expr })
    }

    pub fn expr(&self) -> &Expr {
        &self.expr
    }

    /// Joins two queries so a record must pass both.
    pub fn and(self, other: Query) -> Query {
        Query { expr: Expr::And(Box::new(self.expr), Box::new(other.expr)) }
    }

    pub fn matches(&self, fields: &[(String, Value)]) -> bool {
        run(&self.expr, fields)
    }
}

fn run(expr: &Expr, fields: &[(String, Value)]) -> bool {
    match expr {
        Expr::And(left, right) => run(left, fields) && run(right, fields),
        Expr::Or(left, right) => run(left, fields) || run(right, fields),
        Expr::Not(inner) => !run(inner, fields),
        Expr::Exists(field) => lookup(fields, field).is_some(),
        Expr::Compare { field, op, value } => lookup(fields, field).is_some_and(|actual| {
            if *op == CompareOp::Ne {
                !any_item(actual, &|item| compare(item, CompareOp::Eq, value))
            } else {
                any_item(actual, &|item| compare(item, *op, value))
            }
        }),
        Expr::Regex { field, regex, negated } => lookup(fields, field).is_some_and(|actual| {
            any_item(actual, &|item| is_scalar(item) && regex.is_match(&item.display_text())) != *negated
        }),
        Expr::In { field, values, negated } => lookup(fields, field).is_some_and(|actual| {
            let hit = any_item(actual, &|item| values.iter().any(|value| compare(item, CompareOp::Eq, value)));
            hit != *negated
        }),
        Expr::Text { field, op, needle } => lookup(fields, field)
            .is_some_and(|actual| any_item(actual, &|item| is_scalar(item) && text_matches(*op, &item.display_text(), needle))),
    }
}

/// An array matches when any of its items does, and anything else stands for itself.
fn any_item(actual: &Value, test: &dyn Fn(&Value) -> bool) -> bool {
    match actual {
        Value::Array(items) => items.iter().any(test),
        other => test(other),
    }
}

fn is_scalar(value: &Value) -> bool {
    !matches!(value, Value::Array(_) | Value::Object(_) | Value::Null)
}

fn text_matches(op: TextOp, text: &str, needle: &str) -> bool {
    match op {
        TextOp::Contains => text.contains(needle),
        TextOp::StartsWith => text.starts_with(needle),
        TextOp::EndsWith => text.ends_with(needle),
        TextOp::Glob => crate::glob::matches(needle, text),
    }
}

/// Compares a field with a literal, coercing between text, numbers and booleans when one side
/// can honestly be read as the other. Nulls only ever equal nulls, and mismatched kinds never
/// order, so `x < 5` is false when `x` is `"abc"`.
fn compare(actual: &Value, op: CompareOp, literal: &Value) -> bool {
    use std::cmp::Ordering::{Equal, Greater, Less};
    let ordering = match (actual, literal) {
        (Value::Null, Value::Null) => Some(Equal),
        (Value::Null, _) | (_, Value::Null) => None,
        (Value::Number(a), Value::Number(b)) => a.partial_cmp(b),
        (Value::Text(a), Value::Number(b)) => parse_number(a).and_then(|a| a.partial_cmp(b)),
        (Value::Number(a), Value::Text(b)) => parse_number(b).and_then(|b| a.partial_cmp(&b)),
        (Value::Text(a), Value::Text(b)) => match (parse_number(a), parse_number(b)) {
            // Both numeric, so "9" < "10" and "1.0" = "1", as a reader of the log would expect.
            (Some(a), Some(b)) => a.partial_cmp(&b),
            _ => Some(a.cmp(b)),
        },
        (Value::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
        (Value::Bool(a), Value::Text(b)) | (Value::Text(b), Value::Bool(a)) => match b.as_str() {
            "true" if *a => Some(Equal),
            "false" if !*a => Some(Equal),
            _ => None,
        },
        _ => None,
    };
    match (op, ordering) {
        (_, None) => false,
        (CompareOp::Eq, Some(found)) => found == Equal,
        (CompareOp::Ne, Some(found)) => found != Equal,
        (CompareOp::Lt, Some(found)) => found == Less,
        (CompareOp::Le, Some(found)) => found != Greater,
        (CompareOp::Gt, Some(found)) => found == Greater,
        (CompareOp::Ge, Some(found)) => found != Less,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::Format;

    fn fields(json: &str) -> Vec<(String, Value)> {
        Format::Json.parse_line(json).unwrap()
    }

    fn check(query: &str, json: &str) -> bool {
        Query::parse(query).unwrap_or_else(|error| panic!("{query}: {error}")).matches(&fields(json))
    }

    #[test]
    fn comparison_of_numbers() {
        let log = r#"{"status":503}"#;
        assert!(check("status = 503", log));
        assert!(check("status == 503", log));
        assert!(check("status != 200", log));
        assert!(check("status >= 500", log));
        assert!(check("status > 500", log));
        assert!(check("status < 600", log));
        assert!(check("status <= 503", log));
        assert!(!check("status < 503", log));
        assert!(!check("status = 200", log));
    }

    #[test]
    fn text_and_numbers_coerce_when_they_can() {
        assert!(check("status >= 500", r#"{"status":"503"}"#));
        assert!(check("status = \"503\"", r#"{"status":503}"#));
        assert!(check("status = 503", r#"{"status":"503.0"}"#));
        assert!(!check("status = 503", r#"{"status":"abc"}"#));
        assert!(!check("status < 5", r#"{"status":"abc"}"#));
        assert!(!check("status > 5", r#"{"status":"abc"}"#));
        assert!(!check("status = hello", r#"{"status":5}"#));
    }

    #[test]
    fn numeric_text_compares_as_numbers() {
        assert!(check("n < \"10\"", r#"{"n":"9"}"#));
        assert!(check("name < bob", r#"{"name":"alice"}"#));
        assert!(!check("name > bob", r#"{"name":"alice"}"#));
    }

    #[test]
    fn unquoted_words_are_text() {
        assert!(check("level = error", r#"{"level":"error"}"#));
        assert!(check("LEVEL = error OR level = error", r#"{"level":"error"}"#));
        assert!(check("path = /api/items", r#"{"path":"/api/items"}"#));
    }

    #[test]
    fn booleans_and_text() {
        assert!(check("ok = true", r#"{"ok":true}"#));
        assert!(check("ok = false", r#"{"ok":false}"#));
        assert!(!check("ok = true", r#"{"ok":false}"#));
        assert!(check("ok = true", r#"{"ok":"true"}"#));
        assert!(check("ok != true", r#"{"ok":false}"#));
        assert!(check("ok = \"true\"", r#"{"ok":true}"#));
        assert!(!check("ok = \"maybe\"", r#"{"ok":true}"#));
        assert!(!check("ok = 1", r#"{"ok":true}"#));
    }

    #[test]
    fn missing_fields_fail_every_test_including_not_equal() {
        let log = r#"{"other":1}"#;
        for query in [
            "x = 1", "x != 1", "x < 1", "x > 1", "x ~ a", "x !~ a", "x contains a", "x in (1)", "x not in (1)", "x = null",
            "x != null", "exists(x)",
        ] {
            assert!(!check(query, log), "{query}");
        }
        assert!(check("not x = 1", log));
        assert!(check("not exists(x)", log));
    }

    #[test]
    fn null_counts_as_present_and_equals_only_null() {
        let log = r#"{"x":null}"#;
        assert!(check("exists(x)", log));
        assert!(check("x = null", log));
        assert!(!check("x != null", log));
        assert!(!check("x = 0", log));
        assert!(check("x != 0", log));
        assert!(!check("x < 5", log));
        assert!(!check("x contains null", log));
        assert!(check("y != null", r#"{"y":0}"#));
        assert!(!check("y = null", r#"{"y":0}"#));
    }

    #[test]
    fn nested_fields() {
        let log = r#"{"http":{"status":200,"hosts":["a","b"]}}"#;
        assert!(check("http.status = 200", log));
        assert!(check("http.hosts.1 = b", log));
        assert!(!check("http.missing = 1", log));
    }

    #[test]
    fn arrays_match_when_any_item_does() {
        let log = r#"{"tags":["a","b"],"n":[1,5]}"#;
        assert!(check("tags = a", log));
        assert!(!check("tags = c", log));
        assert!(check("n > 3", log));
        assert!(!check("n > 5", log));
        assert!(check("tags contains b", log));
        assert!(check("tags ~ '^b$'", log));
        assert!(check("tags in (b, c)", log));
    }

    #[test]
    fn negative_tests_on_arrays_mean_no_item_matches() {
        let log = r#"{"tags":["a","b"]}"#;
        assert!(check("tags != c", log));
        assert!(!check("tags != a", log));
        assert!(check("tags !~ z", log));
        assert!(!check("tags !~ a", log));
        assert!(check("tags not in (y, z)", log));
        assert!(!check("tags not in (a, z)", log));
    }

    #[test]
    fn objects_match_only_exists() {
        let log = r#"{"o":{"a":1}}"#;
        assert!(check("exists(o)", log));
        assert!(!check("o = 1", log));
        assert!(!check("o contains a", log));
    }

    #[test]
    fn text_operators() {
        let log = r#"{"path":"/api/items/7"}"#;
        assert!(check("path contains items", log));
        assert!(check("path startswith /api", log));
        assert!(check("path endswith '/7'", log));
        assert!(check("path glob '/api/*/?'", log));
        assert!(!check("path glob '/api'", log));
        assert!(!check("path contains ITEMS", log));
        assert!(check("n contains 2", r#"{"n":123}"#));
    }

    #[test]
    fn regular_expressions() {
        let log = r#"{"path":"/api/items/7","n":404}"#;
        assert!(check(r"path ~ '^/api/\w+/\d+$'", log));
        assert!(check("path !~ '^/admin'", log));
        assert!(!check("path ~ '^/admin'", log));
        assert!(check("n ~ '^4'", log));
    }

    #[test]
    fn in_lists() {
        let log = r#"{"method":"GET","status":200}"#;
        assert!(check("method in (GET, POST)", log));
        assert!(check("status in (200, 201)", log));
        assert!(check("method not in (PUT, DELETE)", log));
        assert!(!check("method in (PUT)", log));
        assert!(!check("method not in (GET)", log));
    }

    #[test]
    fn precedence_and_grouping() {
        let log = r#"{"a":1,"b":2,"c":3}"#;
        assert!(check("a = 9 and b = 9 or c = 3", log));
        assert!(!check("a = 9 and (b = 9 or c = 3)", log));
        assert!(check("c = 3 or a = 9 and b = 9", log));
        assert!(!check("not a = 1 or b = 9", log));
        assert!(check("!(a = 9)", log));
        assert!(check("a = 1 && b = 2 || a = 9", log));
        assert!(check("not not a = 1", log));
    }

    #[test]
    fn backticks_name_awkward_fields() {
        assert!(check("`odd name` = 1", r#"{"odd name":1}"#));
        assert!(check("`404` = 1", r#"{"404":1}"#));
        assert!(check("`and` = 1", r#"{"and":1}"#));
    }

    #[test]
    fn a_field_named_like_a_function_still_works() {
        assert!(check("exists = 1", r#"{"exists":1}"#));
    }

    #[test]
    fn syntax_errors_say_where() {
        for (query, position) in [
            ("", 0),
            ("a", 1),
            ("a =", 3),
            ("= 1", 0),
            ("a = 1 b", 6),
            ("(a = 1", 6),
            ("a = 1)", 5),
            ("a in 1", 5),
            ("a in ()", 6),
            ("a in (1,)", 8),
            ("a not 1", 6),
            ("exists(", 7),
            ("exists(a", 8),
            ("a ~ 5", 4),
            ("a ~ '('", 4),
            ("404 = 1", 0),
            ("a and", 2),
            ("a = 1 and", 9),
            ("a = 1 or", 8),
        ] {
            let error = Query::parse(query).expect_err(query);
            assert_eq!(error.position, position, "{query}: {error}");
        }
    }

    #[test]
    fn nesting_is_limited() {
        let deep = format!("{}a = 1{}", "(".repeat(200), ")".repeat(200));
        assert!(Query::parse(&deep).unwrap_err().message.contains("too deeply"));
        let nots = format!("{}a = 1", "not ".repeat(200));
        assert!(Query::parse(&nots).is_err());
    }

    #[test]
    fn queries_join_with_and() {
        let both = Query::parse("a = 1").unwrap().and(Query::parse("b = 2").unwrap());
        assert!(both.matches(&fields(r#"{"a":1,"b":2}"#)));
        assert!(!both.matches(&fields(r#"{"a":1,"b":3}"#)));
    }
}
