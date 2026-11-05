//! Reads lines from one input, parses and filters them, and hands the survivors to a sink.

use std::collections::VecDeque;
use std::fmt;
use std::io::{self, BufRead};

use crate::formats::{detect, Format};
use crate::query::Query;
use crate::value::{lookup, Fields, Value};

/// How many lines auto-detection looks at.
const DETECT_LINES: usize = 16;
/// Where to look for a time when no field is named. The first one that parses is used.
pub const TIME_FIELDS: [&str; 6] = ["timestamp", "@timestamp", "time", "ts", "date", "t"];

/// What to do with a line that doesn't parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnError {
    /// Count it and carry on.
    Skip,
    /// Say so on standard error and carry on.
    Warn,
    /// Stop reading.
    Fail,
    /// Pass it along as a record with `_raw` and `_error` fields.
    Keep,
}

impl OnError {
    pub fn from_name(name: &str) -> Option<OnError> {
        match name {
            "skip" => Some(OnError::Skip),
            "warn" => Some(OnError::Warn),
            "fail" => Some(OnError::Fail),
            "keep" => Some(OnError::Keep),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    pub lines: u64,
    pub blank: u64,
    pub bad: u64,
    /// Records dropped by a time window because no time could be read from them.
    pub no_time: u64,
    pub matched: u64,
}

impl fmt::Display for Counters {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "lines={} blank={} bad={} no_time={} matched={}",
            self.lines, self.blank, self.bad, self.no_time, self.matched
        )
    }
}

#[derive(Debug)]
pub enum ScanError {
    /// A bad line under `OnError::Fail`.
    BadLine { source: String, line: u64, message: String },
    Input { source: String, error: io::Error },
    /// Trouble from the sink, which means trouble writing the output.
    Output(io::Error),
}

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ScanError::BadLine { source, line, message } => write!(f, "{source}:{line}: {message}"),
            ScanError::Input { source, error } => write!(f, "{source}: {error}"),
            ScanError::Output(error) => write!(f, "writing output: {error}"),
        }
    }
}

/// A record that passed everything.
pub struct Entry<'a> {
    pub fields: &'a [(String, Value)],
    pub raw: &'a str,
    pub time: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Stop,
}

/// The settings for a scan, shared by all inputs of a run.
#[derive(Debug, Clone)]
pub struct Scanner {
    /// `None` works it out from the first lines of each input.
    pub format: Option<Format>,
    pub filter: Option<Query>,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub time_field: Option<String>,
    /// Read the time even without a window, because the sink wants it.
    pub want_time: bool,
    pub on_error: OnError,
    pub limit: Option<u64>,
}

impl Default for Scanner {
    fn default() -> Scanner {
        Scanner {
            format: None,
            filter: None,
            since: None,
            until: None,
            time_field: None,
            want_time: false,
            on_error: OnError::Warn,
            limit: None,
        }
    }
}

/// Reads whole lines as text. Invalid UTF-8 is replaced rather than rejected, because one bad
/// byte shouldn't lose a log line, and line endings are removed.
fn read_line(input: &mut dyn BufRead, buffer: &mut Vec<u8>) -> io::Result<Option<String>> {
    buffer.clear();
    if input.read_until(b'\n', buffer)? == 0 {
        return Ok(None);
    }
    if buffer.last() == Some(&b'\n') {
        buffer.pop();
    }
    if buffer.last() == Some(&b'\r') {
        buffer.pop();
    }
    Ok(Some(String::from_utf8_lossy(buffer).into_owned()))
}

impl Scanner {
    /// The time of a record: the named field, or else the first of the usual names that parses.
    pub fn record_time(&self, fields: &[(String, Value)]) -> Option<i64> {
        match &self.time_field {
            Some(name) => lookup(fields, name).and_then(crate::time::from_value),
            None => TIME_FIELDS
                .iter()
                .find_map(|name| lookup(fields, name).and_then(crate::time::from_value)),
        }
    }

    /// Runs one input through to the sink. `Flow::Stop` means the limit was reached or the
    /// sink wanted no more, so later inputs need not be read.
    pub fn scan(
        &self,
        input: &mut dyn BufRead,
        source: &str,
        counters: &mut Counters,
        warn: &mut dyn FnMut(&str),
        sink: &mut dyn FnMut(&Entry) -> io::Result<Flow>,
    ) -> Result<Flow, ScanError> {
        if self.limit.is_some_and(|limit| counters.matched >= limit) {
            return Ok(Flow::Stop);
        }
        let input_error = |error| ScanError::Input { source: source.to_string(), error };
        let mut buffer = Vec::new();
        let mut waiting = VecDeque::new();
        let format = match self.format {
            Some(format) => format,
            None => {
                let mut samples: Vec<String> = Vec::new();
                while samples.len() < DETECT_LINES {
                    match read_line(input, &mut buffer).map_err(input_error)? {
                        Some(line) => {
                            if !line.trim().is_empty() {
                                samples.push(line.clone());
                            }
                            waiting.push_back(line);
                        }
                        None => break,
                    }
                }
                // With nothing recognized, reading as JSON at least gives each line its own error.
                detect(&samples).unwrap_or(Format::Json)
            }
        };
        let mut number = 0;
        loop {
            let line = match waiting.pop_front() {
                Some(line) => line,
                None => match read_line(input, &mut buffer).map_err(input_error)? {
                    Some(line) => line,
                    None => return Ok(Flow::Continue),
                },
            };
            number += 1;
            counters.lines += 1;
            if line.trim().is_empty() {
                counters.blank += 1;
                continue;
            }
            let fields = match format.parse_line(&line) {
                Ok(fields) => fields,
                Err(message) => {
                    counters.bad += 1;
                    match self.on_error {
                        OnError::Skip => continue,
                        OnError::Warn => {
                            warn(&format!("{source}:{number}: {message}"));
                            continue;
                        }
                        OnError::Fail => {
                            return Err(ScanError::BadLine { source: source.to_string(), line: number, message });
                        }
                        OnError::Keep => keep_fields(&line, message),
                    }
                }
            };
            if self.handle(&fields, &line, counters, sink).map_err(ScanError::Output)? == Flow::Stop {
                return Ok(Flow::Stop);
            }
        }
    }

    fn handle(
        &self,
        fields: &[(String, Value)],
        line: &str,
        counters: &mut Counters,
        sink: &mut dyn FnMut(&Entry) -> io::Result<Flow>,
    ) -> io::Result<Flow> {
        let windowed = self.since.is_some() || self.until.is_some();
        let time = if windowed || self.want_time { self.record_time(fields) } else { None };
        if windowed {
            let Some(at) = time else {
                counters.no_time += 1;
                return Ok(Flow::Continue);
            };
            if self.since.is_some_and(|since| at < since) || self.until.is_some_and(|until| at >= until) {
                return Ok(Flow::Continue);
            }
        }
        if self.filter.as_ref().is_some_and(|filter| !filter.matches(fields)) {
            return Ok(Flow::Continue);
        }
        counters.matched += 1;
        let flow = sink(&Entry { fields, raw: line, time })?;
        if self.limit.is_some_and(|limit| counters.matched >= limit) {
            return Ok(Flow::Stop);
        }
        Ok(flow)
    }
}

fn keep_fields(line: &str, message: String) -> Fields {
    vec![("_raw".to_string(), Value::Text(line.to_string())), ("_error".to_string(), Value::Text(message))]
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Run {
        rows: Vec<String>,
        counters: Counters,
        warnings: Vec<String>,
        result: Result<Flow, ScanError>,
    }

    fn run(scanner: &Scanner, text: &str) -> Run {
        let mut rows = Vec::new();
        let mut counters = Counters::default();
        let mut warnings = Vec::new();
        let result = scanner.scan(
            &mut text.as_bytes(),
            "input",
            &mut counters,
            &mut |message| warnings.push(message.to_string()),
            &mut |entry| {
                rows.push(entry.raw.to_string());
                Ok(Flow::Continue)
            },
        );
        Run { rows, counters, warnings, result }
    }

    #[test]
    fn plain_reading() {
        let outcome = run(&Scanner::default(), "{\"a\":1}\n\n{\"a\":2}");
        assert_eq!(outcome.rows, vec!["{\"a\":1}", "{\"a\":2}"]);
        assert_eq!(outcome.counters, Counters { lines: 3, blank: 1, matched: 2, ..Counters::default() });
    }

    #[test]
    fn line_endings_are_removed() {
        assert_eq!(run(&Scanner::default(), "a=1\r\nb=2\r\n").rows, vec!["a=1", "b=2"]);
    }

    #[test]
    fn invalid_utf8_does_not_lose_the_line() {
        let scanner = Scanner { format: Some(Format::Logfmt), ..Scanner::default() };
        let mut rows = 0;
        let mut counters = Counters::default();
        scanner
            .scan(&mut &b"a=\xff1\n"[..], "input", &mut counters, &mut |_| {}, &mut |_| {
                rows += 1;
                Ok(Flow::Continue)
            })
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn detection_handles_more_lines_than_it_samples() {
        let text: String = (0..40).map(|n| format!("n={n}\n")).collect();
        let outcome = run(&Scanner::default(), &text);
        assert_eq!(outcome.rows.len(), 40);
        assert_eq!(outcome.rows[39], "n=39");
    }

    #[test]
    fn bad_lines_warn_with_their_place_by_default() {
        let outcome = run(&Scanner { format: Some(Format::Json), ..Scanner::default() }, "{\"a\":1}\n\nbroken\n");
        assert_eq!(outcome.rows.len(), 1);
        assert_eq!(outcome.counters.bad, 1);
        assert_eq!(outcome.warnings.len(), 1);
        assert!(outcome.warnings[0].starts_with("input:3: "), "{}", outcome.warnings[0]);
    }

    #[test]
    fn bad_lines_can_be_skipped_quietly() {
        let scanner = Scanner { format: Some(Format::Json), on_error: OnError::Skip, ..Scanner::default() };
        let outcome = run(&scanner, "broken\n");
        assert!(outcome.warnings.is_empty());
        assert_eq!(outcome.counters.bad, 1);
    }

    #[test]
    fn bad_lines_can_stop_the_scan() {
        let scanner = Scanner { format: Some(Format::Json), on_error: OnError::Fail, ..Scanner::default() };
        let outcome = run(&scanner, "{\"a\":1}\nbroken\n{\"a\":2}\n");
        assert_eq!(outcome.rows.len(), 1);
        assert!(matches!(outcome.result, Err(ScanError::BadLine { line: 2, .. })));
    }

    #[test]
    fn bad_lines_can_be_kept() {
        let scanner = Scanner {
            format: Some(Format::Json),
            on_error: OnError::Keep,
            filter: Some(Query::parse("exists(_raw)").unwrap()),
            ..Scanner::default()
        };
        let outcome = run(&scanner, "{\"a\":1}\nbroken\n");
        assert_eq!(outcome.rows, vec!["broken"]);
    }

    #[test]
    fn filters_apply() {
        let scanner = Scanner { filter: Some(Query::parse("a = 2").unwrap()), ..Scanner::default() };
        assert_eq!(run(&scanner, "a=1\na=2\n").rows, vec!["a=2"]);
    }

    #[test]
    fn limit_stops_reading() {
        let scanner = Scanner { limit: Some(2), ..Scanner::default() };
        let outcome = run(&scanner, "a=1\na=2\na=3\n");
        assert_eq!(outcome.rows.len(), 2);
        assert_eq!(outcome.counters.lines, 2);
        assert_eq!(outcome.result.unwrap(), Flow::Stop);
    }

    #[test]
    fn limit_carries_across_inputs() {
        let scanner = Scanner { limit: Some(1), ..Scanner::default() };
        let mut counters = Counters::default();
        let mut sink = |_: &Entry| Ok(Flow::Continue);
        let first = scanner.scan(&mut &b"a=1\n"[..], "one", &mut counters, &mut |_| {}, &mut sink).unwrap();
        let second = scanner.scan(&mut &b"a=2\n"[..], "two", &mut counters, &mut |_| {}, &mut sink).unwrap();
        assert_eq!((first, second), (Flow::Stop, Flow::Stop));
        assert_eq!(counters.lines, 1);
    }

    #[test]
    fn time_window_includes_since_and_excludes_until() {
        let at = |text: &str| crate::time::parse_text(text);
        let scanner = Scanner {
            since: at("2024-01-01T00:00:10Z"),
            until: at("2024-01-01T00:00:20Z"),
            ..Scanner::default()
        };
        let text = "\
ts=2024-01-01T00:00:09Z n=before
ts=2024-01-01T00:00:10Z n=first
ts=2024-01-01T00:00:19Z n=last
ts=2024-01-01T00:00:20Z n=after
n=undated
";
        let outcome = run(&scanner, text);
        assert_eq!(outcome.rows, vec!["ts=2024-01-01T00:00:10Z n=first", "ts=2024-01-01T00:00:19Z n=last"]);
        assert_eq!(outcome.counters.no_time, 1);
    }

    #[test]
    fn time_comes_from_the_named_field_when_given() {
        let scanner = Scanner { time_field: Some("seen".to_string()), since: Some(1000), ..Scanner::default() };
        assert_eq!(run(&scanner, "ts=2024-01-01 n=1\nseen=2024-01-01 n=2\n").rows, vec!["seen=2024-01-01 n=2"]);
    }

    #[test]
    fn an_unusable_time_field_falls_through_to_the_next_name() {
        let scanner = Scanner::default();
        let fields = Format::Logfmt.parse_line("time=soon ts=1970-01-01T00:00:01Z").unwrap();
        assert_eq!(scanner.record_time(&fields), Some(1000));
    }

    #[test]
    fn counters_read_well() {
        let counters = Counters { lines: 3, blank: 1, bad: 0, no_time: 0, matched: 2 };
        assert_eq!(counters.to_string(), "lines=3 blank=1 bad=0 no_time=0 matched=2");
    }
}
