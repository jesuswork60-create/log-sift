//! Reads structured log lines (JSON, logfmt, nginx combined), filters them with a small
//! query language, and prints the matches or aggregates over them.
//!
//! ```
//! use logsift::query::Query;
//! use logsift::formats::Format;
//!
//! let fields = Format::Logfmt.parse_line("level=error status=503 path=/api/items").unwrap();
//! let query = Query::parse("level = error and status >= 500").unwrap();
//! assert!(query.matches(&fields));
//! ```

pub mod aggregate;
pub mod cli;
pub mod formats;
pub mod glob;
pub mod json;
pub mod output;
pub mod pipeline;
pub mod query;
pub mod regex;
pub mod schema;
pub mod time;
pub mod value;
