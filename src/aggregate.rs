//! Aggregates over the matching records: `count, p95(ms) by status every 5m`.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};

use crate::query::{tokenize, TokenKind};
use crate::time::{bucket_start, format_millis, parse_duration};
use crate::value::{lookup, parse_number, Fields, Value};

#[derive(Debug, Clone, PartialEq)]
pub enum Function {
    Count,
    CountField,
    Sum,
    Avg,
    Min,
    Max,
    Percentile(f64),
    Distinct,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Metric {
    pub function: Function,
    pub field: Option<String>,
    /// The column name in the output, such as `p95(ms)`.
    pub label: String,
}

/// A parsed `--agg` specification.
#[derive(Debug, Clone, PartialEq)]
pub struct AggSpec {
    pub metrics: Vec<Metric>,
    pub by: Vec<String>,
    /// Bucket width in milliseconds.
    pub every: Option<i64>,
}

fn field_name(kind: Option<&TokenKind>) -> Result<String, String> {
    match kind {
        Some(TokenKind::Word(name) | TokenKind::QuotedField(name)) => Ok(name.clone()),
        _ => Err("expected a field name".to_string()),
    }
}

impl AggSpec {
    pub fn parse(text: &str) -> Result<AggSpec, String> {
        let tokens = tokenize(text).map_err(|error| error.to_string())?;
        let mut kinds = tokens.iter().map(|token| &token.kind).peekable();
        let mut metrics = Vec::new();
        loop {
            let name = match kinds.next() {
                Some(TokenKind::Word(name)) => name.clone(),
                _ => return Err("expected an aggregate such as count or avg(field)".to_string()),
            };
            let field = if kinds.peek() == Some(&&TokenKind::LeftParen) {
                kinds.next();
                if kinds.peek() == Some(&&TokenKind::RightParen) {
                    kinds.next();
                    None
                } else {
                    let field = field_name(kinds.next())?;
                    if kinds.next() != Some(&TokenKind::RightParen) {
                        return Err(format!("missing ) after {name}({field}"));
                    }
                    Some(field)
                }
            } else {
                None
            };
            metrics.push(metric(&name, field)?);
            if kinds.peek() == Some(&&TokenKind::Comma) {
                kinds.next();
            } else {
                break;
            }
        }
        let is_word = |kind: Option<&&TokenKind>, word: &str| matches!(kind, Some(TokenKind::Word(found)) if found == word);
        let mut by = Vec::new();
        if is_word(kinds.peek(), "by") {
            kinds.next();
            loop {
                by.push(field_name(kinds.next().map(|kind| kind as &TokenKind))?);
                if kinds.peek() == Some(&&TokenKind::Comma) {
                    kinds.next();
                } else {
                    break;
                }
            }
        }
        let mut every = None;
        if is_word(kinds.peek(), "every") {
            kinds.next();
            let width = match kinds.next() {
                Some(TokenKind::Word(width)) => parse_duration(width)?,
                _ => return Err("expected a duration after every".to_string()),
            };
            if width == 0 {
                return Err("the bucket width can't be zero".to_string());
            }
            every = Some(width);
        }
        if kinds.next().is_some() {
            return Err("unexpected text after the aggregate".to_string());
        }
        Ok(AggSpec { metrics, by, every })
    }
}

fn metric(name: &str, field: Option<String>) -> Result<Metric, String> {
    let function = match name {
        "count" if field.is_some() => Function::CountField,
        "count" => Function::Count,
        "sum" => Function::Sum,
        "avg" => Function::Avg,
        "min" => Function::Min,
        "max" => Function::Max,
        "distinct" => Function::Distinct,
        other => match other.strip_prefix('p').and_then(parse_number) {
            Some(rank) if (0.0..=100.0).contains(&rank) => Function::Percentile(rank),
            _ => return Err(format!("unknown aggregate {other:?}")),
        },
    };
    if function != Function::Count && field.is_none() {
        return Err(format!("{name} needs a field, as in {name}(field)"));
    }
    let label = match &field {
        Some(field) => format!("{name}({field})"),
        None => name.to_string(),
    };
    Ok(Metric { function, field, label })
}

/// A running total for one metric in one group.
#[derive(Debug, Clone)]
enum Accumulator {
    Count(u64),
    Sum { total: f64, seen: bool },
    Avg { total: f64, count: u64 },
    Min(Option<f64>),
    Max(Option<f64>),
    /// Exact percentiles, so every value is held until the end.
    Values(Vec<f64>, f64),
    Distinct(HashSet<String>),
}

impl Accumulator {
    fn new(function: &Function) -> Accumulator {
        match function {
            Function::Count | Function::CountField => Accumulator::Count(0),
            Function::Sum => Accumulator::Sum { total: 0.0, seen: false },
            Function::Avg => Accumulator::Avg { total: 0.0, count: 0 },
            Function::Min => Accumulator::Min(None),
            Function::Max => Accumulator::Max(None),
            Function::Percentile(rank) => Accumulator::Values(Vec::new(), *rank),
            Function::Distinct => Accumulator::Distinct(HashSet::new()),
        }
    }

    fn add(&mut self, metric: &Metric, fields: &[(String, Value)]) {
        let value = metric.field.as_ref().and_then(|field| lookup(fields, field));
        match self {
            Accumulator::Count(count) => {
                let counts = metric.field.is_none() || value.is_some_and(|value| *value != Value::Null);
                *count += u64::from(counts);
            }
            Accumulator::Distinct(seen) => {
                if let Some(value) = value.filter(|value| **value != Value::Null) {
                    let text = value.display_text();
                    if !seen.contains(text.as_ref()) {
                        seen.insert(text.into_owned());
                    }
                }
            }
            numeric => {
                if let Some(number) = value.and_then(Value::as_number) {
                    numeric.add_number(number);
                }
            }
        }
    }

    fn add_number(&mut self, number: f64) {
        match self {
            Accumulator::Sum { total, seen } => {
                *total += number;
                *seen = true;
            }
            Accumulator::Avg { total, count } => {
                *total += number;
                *count += 1;
            }
            Accumulator::Min(best) => *best = Some(best.map_or(number, |best| best.min(number))),
            Accumulator::Max(best) => *best = Some(best.map_or(number, |best| best.max(number))),
            Accumulator::Values(values, _) => values.push(number),
            Accumulator::Count(_) | Accumulator::Distinct(_) => {}
        }
    }

    /// The result. Functions that saw no numbers give null rather than a made-up zero.
    fn finish(self) -> Value {
        let number = |value: Option<f64>| value.map_or(Value::Null, Value::Number);
        match self {
            Accumulator::Count(count) => Value::Number(count as f64),
            Accumulator::Sum { total, seen } => number(seen.then_some(total)),
            Accumulator::Avg { total, count } => number((count > 0).then(|| total / count as f64)),
            Accumulator::Min(best) | Accumulator::Max(best) => number(best),
            Accumulator::Values(mut values, rank) => number(percentile(&mut values, rank)),
            Accumulator::Distinct(seen) => Value::Number(seen.len() as f64),
        }
    }
}

/// Linear interpolation between closest ranks.
pub fn percentile(values: &mut [f64], rank: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let spot = rank / 100.0 * (values.len() - 1) as f64;
    let low = spot.floor() as usize;
    let high = spot.ceil() as usize;
    Some(values[low] + (values[high] - values[low]) * (spot - low as f64))
}

/// A group key: its original value, and the text that decides which group it belongs in, so
/// that `200` and `"200"` land together.
#[derive(Debug, Clone)]
struct GroupKey {
    text: Option<String>,
    number: Option<f64>,
    value: Value,
}

impl GroupKey {
    fn new(value: Option<&Value>) -> GroupKey {
        match value {
            None | Some(Value::Null) => GroupKey { text: None, number: None, value: Value::Null },
            Some(value) => GroupKey {
                text: Some(value.display_text().into_owned()),
                number: value.as_number(),
                value: value.clone(),
            },
        }
    }
}

impl Ord for GroupKey {
    /// Numbers first in numeric order, then text, then records with no value at all.
    fn cmp(&self, other: &GroupKey) -> Ordering {
        match (&self.text, &other.text) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Greater,
            (Some(_), None) => return Ordering::Less,
            (Some(_), Some(_)) => {}
        }
        let by_number = match (self.number, other.number) {
            (Some(a), Some(b)) => a.total_cmp(&b),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        };
        by_number.then_with(|| self.text.cmp(&other.text))
    }
}

impl PartialOrd for GroupKey {
    fn partial_cmp(&self, other: &GroupKey) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for GroupKey {
    fn eq(&self, other: &GroupKey) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for GroupKey {}

/// Collects records and produces one row per group, ordered by bucket and then by key.
pub struct Aggregator {
    spec: AggSpec,
    groups: BTreeMap<(Option<i64>, Vec<GroupKey>), Vec<Accumulator>>,
    /// Records left out because `every` needs a time and they had none.
    pub no_time: u64,
}

impl Aggregator {
    pub fn new(spec: AggSpec) -> Aggregator {
        Aggregator { spec, groups: BTreeMap::new(), no_time: 0 }
    }

    pub fn needs_time(&self) -> bool {
        self.spec.every.is_some()
    }

    pub fn add(&mut self, fields: &[(String, Value)], time: Option<i64>) {
        let bucket = match self.spec.every {
            Some(width) => match time {
                Some(time) => Some(bucket_start(time, width)),
                None => {
                    self.no_time += 1;
                    return;
                }
            },
            None => None,
        };
        let key: Vec<GroupKey> = self.spec.by.iter().map(|field| GroupKey::new(lookup(fields, field))).collect();
        let metrics = &self.spec.metrics;
        let row = self
            .groups
            .entry((bucket, key))
            .or_insert_with(|| metrics.iter().map(|metric| Accumulator::new(&metric.function)).collect());
        for (accumulator, metric) in row.iter_mut().zip(metrics) {
            accumulator.add(metric, fields);
        }
    }

    /// The finished rows. With no groups and no records, a plain aggregate still gives one row,
    /// because "count = 0" is an answer. A grouped one gives none, having no groups to report.
    pub fn finish(mut self) -> Vec<Fields> {
        if self.groups.is_empty() && self.spec.by.is_empty() && self.spec.every.is_none() {
            let empty = self.spec.metrics.iter().map(|metric| Accumulator::new(&metric.function)).collect();
            self.groups.insert((None, Vec::new()), empty);
        }
        let spec = self.spec;
        self.groups
            .into_iter()
            .map(|((bucket, key), row)| {
                let mut fields = Vec::with_capacity(1 + key.len() + row.len());
                if let Some(bucket) = bucket {
                    fields.push(("bucket".to_string(), Value::Text(format_millis(bucket))));
                }
                for (name, part) in spec.by.iter().zip(key) {
                    fields.push((name.clone(), part.value));
                }
                for (metric, accumulator) in spec.metrics.iter().zip(row) {
                    fields.push((metric.label.clone(), accumulator.finish()));
                }
                fields
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::Format;

    fn run(spec: &str, lines: &[&str]) -> Vec<String> {
        let mut aggregator = Aggregator::new(AggSpec::parse(spec).unwrap());
        for line in lines {
            let fields = Format::Json.parse_line(line).unwrap();
            let time = lookup(&fields, "t").and_then(crate::time::from_value);
            aggregator.add(&fields, time);
        }
        aggregator.finish().iter().map(|row| crate::formats::write_logfmt(row)).collect()
    }

    #[test]
    fn parsing_specs() {
        let spec = AggSpec::parse("count, avg(ms), p95(ms) by status, host every 5m").unwrap();
        assert_eq!(spec.metrics.len(), 3);
        assert_eq!(spec.metrics[2].function, Function::Percentile(95.0));
        assert_eq!(spec.metrics[2].label, "p95(ms)");
        assert_eq!(spec.by, vec!["status", "host"]);
        assert_eq!(spec.every, Some(300_000));
        assert_eq!(AggSpec::parse("count()").unwrap().metrics[0].function, Function::Count);
        assert_eq!(AggSpec::parse("count(x)").unwrap().metrics[0].function, Function::CountField);
        assert_eq!(AggSpec::parse("p99.9(x)").unwrap().metrics[0].function, Function::Percentile(99.9));
    }

    #[test]
    fn bad_specs() {
        for text in [
            "", "avg", "nope(x)", "avg(", "avg(x", "count by", "count every", "count every soon", "count every 0s", "count x",
            "p101(x)", "px(x)", "count,", "avg(1)",
        ] {
            assert!(AggSpec::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn plain_totals() {
        let rows = run(
            "count, sum(n), avg(n), min(n), max(n)",
            &[r#"{"n":1}"#, r#"{"n":2}"#, r#"{"n":6}"#],
        );
        assert_eq!(rows, vec!["count=3 sum(n)=9 avg(n)=3 min(n)=1 max(n)=6"]);
    }

    #[test]
    fn numeric_text_counts_and_other_values_are_skipped() {
        let rows = run("sum(n), avg(n), count(n)", &[r#"{"n":"2"}"#, r#"{"n":"x"}"#, r#"{"n":4}"#, r#"{"n":null}"#, r#"{}"#]);
        assert_eq!(rows, vec!["sum(n)=6 avg(n)=3 count(n)=3"]);
    }

    #[test]
    fn nothing_seen_gives_null_and_zero_counts() {
        assert_eq!(run("count, sum(n), avg(n), p50(n), distinct(n)", &[]), vec!["count=0 sum(n)= avg(n)= p50(n)= distinct(n)=0"]);
        assert_eq!(run("sum(n)", &[r#"{"m":1}"#]), vec!["sum(n)="]);
        assert!(run("count by a", &[]).is_empty());
        assert!(run("count every 1m", &[]).is_empty());
    }

    #[test]
    fn percentiles_interpolate() {
        let mut values = vec![40.0, 10.0, 30.0, 20.0];
        assert_eq!(percentile(&mut values, 0.0), Some(10.0));
        assert_eq!(percentile(&mut values, 100.0), Some(40.0));
        assert_eq!(percentile(&mut values, 50.0), Some(25.0));
        assert_eq!(percentile(&mut [7.0], 90.0), Some(7.0));
        assert_eq!(percentile(&mut [], 50.0), None);
    }

    #[test]
    fn distinct_counts_values_by_their_text() {
        let rows = run("distinct(u)", &[r#"{"u":"a"}"#, r#"{"u":"b"}"#, r#"{"u":"a"}"#, r#"{"u":null}"#, r#"{}"#]);
        assert_eq!(rows, vec!["distinct(u)=2"]);
    }

    #[test]
    fn groups_come_out_in_numeric_order() {
        let rows = run("count by s", &[r#"{"s":500}"#, r#"{"s":90}"#, r#"{"s":"200"}"#, r#"{"s":200}"#]);
        assert_eq!(rows, vec!["s=90 count=1", "s=200 count=2", "s=500 count=1"]);
    }

    #[test]
    fn numbers_then_text_then_nothing() {
        let rows = run("count by s", &[r#"{"s":"b"}"#, r#"{}"#, r#"{"s":"a"}"#, r#"{"s":5}"#, r#"{"s":null}"#]);
        assert_eq!(rows, vec!["s=5 count=1", "s=a count=1", "s=b count=1", "s= count=2"]);
    }

    #[test]
    fn several_group_fields() {
        let rows = run("count by a, b", &[r#"{"a":1,"b":"x"}"#, r#"{"a":1,"b":"y"}"#, r#"{"a":1,"b":"x"}"#]);
        assert_eq!(rows, vec!["a=1 b=x count=2", "a=1 b=y count=1"]);
    }

    #[test]
    fn time_buckets_skip_records_without_a_time() {
        let lines = [
            r#"{"t":"2024-03-01T12:00:10Z"}"#,
            r#"{"t":"2024-03-01T12:04:59Z"}"#,
            r#"{"t":"2024-03-01T12:20:00Z"}"#,
            r#"{"other":1}"#,
        ];
        let rows = run("count every 5m", &lines);
        assert_eq!(rows, vec!["bucket=2024-03-01T12:00:00Z count=2", "bucket=2024-03-01T12:20:00Z count=1"]);
    }

    #[test]
    fn buckets_and_groups_together() {
        let lines = [r#"{"t":"2024-03-01T12:00:10Z","s":2}"#, r#"{"t":"2024-03-01T12:00:20Z","s":1}"#];
        let rows = run("count by s every 1m", &lines);
        assert_eq!(rows, vec!["bucket=2024-03-01T12:00:00Z s=1 count=1", "bucket=2024-03-01T12:00:00Z s=2 count=1"]);
    }
}
