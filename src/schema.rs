//! Summarizes which fields a log has, for when you don't know what to query yet.

use std::collections::HashMap;

use crate::value::{Fields, Value};

#[derive(Debug, Default)]
struct FieldInfo {
    count: u64,
    kinds: Vec<&'static str>,
    example: Option<String>,
}

/// Collects field paths across records. Nested objects use dotted names, and array items
/// are listed under `name[]`.
#[derive(Debug, Default)]
pub struct SchemaCollector {
    fields: HashMap<String, FieldInfo>,
    records: u64,
}

impl SchemaCollector {
    pub fn add(&mut self, fields: &[(String, Value)]) {
        self.records += 1;
        // A field counts once per record, however many array items hold it.
        let mut seen = Vec::new();
        self.walk(fields, "", &mut seen);
    }

    fn walk(&mut self, fields: &[(String, Value)], prefix: &str, seen: &mut Vec<String>) {
        for (key, value) in fields {
            let path = if prefix.is_empty() { key.clone() } else { format!("{prefix}.{key}") };
            self.note(path, value, seen);
        }
    }

    fn note(&mut self, path: String, value: &Value, seen: &mut Vec<String>) {
        let info = self.fields.entry(path.clone()).or_default();
        if !seen.contains(&path) {
            info.count += 1;
            seen.push(path.clone());
        }
        if !info.kinds.contains(&value.kind()) {
            info.kinds.push(value.kind());
        }
        match value {
            Value::Object(inner) => self.walk(inner, &path, seen),
            Value::Array(items) => {
                let item_path = format!("{path}[]");
                for item in items {
                    self.note(item_path.clone(), item, seen);
                }
            }
            Value::Null => {}
            scalar => {
                if info.example.is_none() {
                    let example: String = scalar.display_text().chars().take(40).collect();
                    info.example = Some(example);
                }
            }
        }
    }

    /// One row per field, most common first and then by name.
    pub fn finish(self) -> Vec<Fields> {
        let records = self.records;
        let mut rows: Vec<(String, FieldInfo)> = self.fields.into_iter().collect();
        rows.sort_by(|a, b| b.1.count.cmp(&a.1.count).then_with(|| a.0.cmp(&b.0)));
        rows.into_iter()
            .map(|(path, mut info)| {
                info.kinds.sort_unstable();
                let share = if records == 0 { 0.0 } else { (info.count as f64 * 1000.0 / records as f64).round() / 10.0 };
                vec![
                    ("field".to_string(), Value::Text(path)),
                    ("count".to_string(), Value::Number(info.count as f64)),
                    ("percent".to_string(), Value::Number(share)),
                    ("types".to_string(), Value::Text(info.kinds.join(","))),
                    ("example".to_string(), info.example.map_or(Value::Null, Value::Text)),
                ]
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::{write_logfmt, Format};

    fn run(lines: &[&str]) -> Vec<String> {
        let mut schema = SchemaCollector::default();
        for line in lines {
            schema.add(&Format::Json.parse_line(line).unwrap());
        }
        schema.finish().iter().map(|row| write_logfmt(row)).collect()
    }

    #[test]
    fn counts_and_order() {
        let rows = run(&[r#"{"a":1,"b":"x"}"#, r#"{"a":2}"#]);
        assert_eq!(rows, vec!["field=a count=2 percent=100 types=number example=1", "field=b count=1 percent=50 types=text example=x"]);
    }

    #[test]
    fn mixed_types_are_all_listed() {
        let rows = run(&[r#"{"a":1}"#, r#"{"a":"x"}"#, r#"{"a":null}"#]);
        assert_eq!(rows, vec!["field=a count=3 percent=100 types=null,number,text example=1"]);
    }

    #[test]
    fn nesting_and_arrays() {
        let rows = run(&[r#"{"o":{"x":1},"l":[{"y":2},{"y":3}]}"#]);
        assert_eq!(
            rows,
            vec![
                "field=l count=1 percent=100 types=array example=",
                "field=l[] count=1 percent=100 types=object example=",
                "field=l[].y count=1 percent=100 types=number example=2",
                "field=o count=1 percent=100 types=object example=",
                "field=o.x count=1 percent=100 types=number example=1",
            ]
        );
    }

    #[test]
    fn nothing_in_nothing_out() {
        assert!(run(&[]).is_empty());
    }
}
