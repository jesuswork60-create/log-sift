//! The log formats and how to tell them apart.

mod logfmt;
mod nginx;

pub use logfmt::{parse as parse_logfmt, write as write_logfmt};
pub use nginx::parse as parse_nginx;

use crate::value::{Fields, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Json,
    Logfmt,
    Nginx,
}

impl Format {
    pub fn from_name(name: &str) -> Option<Format> {
        match name {
            "json" => Some(Format::Json),
            "logfmt" => Some(Format::Logfmt),
            "nginx" => Some(Format::Nginx),
            _ => None,
        }
    }

    /// Parses one line into fields, or explains why it could not.
    pub fn parse_line(self, line: &str) -> Result<Fields, String> {
        match self {
            Format::Json => match crate::json::parse(line) {
                Ok(Value::Object(fields)) => Ok(fields),
                Ok(other) => Err(format!("expected a JSON object, found {}", other.kind())),
                Err(error) => Err(error.to_string()),
            },
            Format::Logfmt => parse_logfmt(line),
            Format::Nginx => parse_nginx(line),
        }
    }
}

/// Picks the format most sample lines parse under. On a tie the stricter format wins, since
/// logfmt in particular will accept many lines that are really something else.
pub fn detect<S: AsRef<str>>(samples: &[S]) -> Option<Format> {
    let mut best = None;
    let mut best_count = 0;
    for format in [Format::Json, Format::Nginx, Format::Logfmt] {
        let count = samples.iter().filter(|line| format.parse_line(line.as_ref()).is_ok()).count();
        if count > best_count {
            best = Some(format);
            best_count = count;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    const NGINX: &str = r#"203.0.113.9 - - [10/Oct/2023:13:55:36 +0000] "GET /a?x=1 HTTP/1.1" 200 12 "-" "curl/8""#;

    #[test]
    fn names() {
        assert_eq!(Format::from_name("json"), Some(Format::Json));
        assert_eq!(Format::from_name("xml"), None);
    }

    #[test]
    fn json_lines_must_be_objects() {
        assert!(Format::Json.parse_line("{\"a\":1}").is_ok());
        assert!(Format::Json.parse_line("[1]").unwrap_err().contains("array"));
        assert!(Format::Json.parse_line("{").is_err());
    }

    #[test]
    fn detection() {
        assert_eq!(detect(&["{\"a\":1}", "{\"a\":\"x=y\"}"]), Some(Format::Json));
        assert_eq!(detect(&[NGINX]), Some(Format::Nginx));
        assert_eq!(detect(&["a=1 b=2", "a=3"]), Some(Format::Logfmt));
        assert_eq!(detect(&["no fields here", "at all"]), None);
        assert_eq!(detect::<&str>(&[]), None);
    }

    #[test]
    fn detection_goes_with_the_majority() {
        assert_eq!(detect(&["garbage", "a=1", "b=2"]), Some(Format::Logfmt));
        assert_eq!(detect(&["{\"a\":1}", "{\"b\":2}", "c=3"]), Some(Format::Json));
    }
}
