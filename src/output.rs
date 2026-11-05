//! Writes rows as the original line, JSON lines, logfmt or an aligned table.

use std::io::{self, Write};

use crate::formats::write_logfmt;
use crate::json::write_object;
use crate::value::{lookup, Fields, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    Raw,
    Json,
    Logfmt,
    Table,
}

impl OutputKind {
    pub fn from_name(name: &str) -> Option<OutputKind> {
        match name {
            "raw" => Some(OutputKind::Raw),
            "json" => Some(OutputKind::Json),
            "logfmt" => Some(OutputKind::Logfmt),
            "table" => Some(OutputKind::Table),
            _ => None,
        }
    }
}

/// Picks out the named fields, flat under their dotted names. Missing ones are left out.
pub fn project(fields: &[(String, Value)], names: &[String]) -> Fields {
    names
        .iter()
        .filter_map(|name| lookup(fields, name).map(|value| (name.clone(), value.clone())))
        .collect()
}

/// Writes rows to a stream. Tables wait until `finish`, since column widths depend on every row.
pub struct RowWriter<'a> {
    out: &'a mut dyn Write,
    kind: OutputKind,
    columns: Option<Vec<String>>,
    max_width: Option<usize>,
    table: Vec<Fields>,
}

impl<'a> RowWriter<'a> {
    pub fn new(out: &'a mut dyn Write, kind: OutputKind, columns: Option<Vec<String>>, max_width: Option<usize>) -> RowWriter<'a> {
        RowWriter { out, kind, columns, max_width, table: Vec::new() }
    }

    /// Writes one row. `raw` is the line it came from, used by the raw output only.
    pub fn write(&mut self, fields: &[(String, Value)], raw: &str) -> io::Result<()> {
        match self.kind {
            OutputKind::Raw => writeln!(self.out, "{raw}"),
            OutputKind::Json => {
                let mut line = String::new();
                match &self.columns {
                    Some(names) => write_object(&project(fields, names), &mut line),
                    None => write_object(fields, &mut line),
                }
                writeln!(self.out, "{line}")
            }
            OutputKind::Logfmt => {
                let line = match &self.columns {
                    Some(names) => write_logfmt(&project(fields, names)),
                    None => write_logfmt(fields),
                };
                writeln!(self.out, "{line}")
            }
            OutputKind::Table => {
                self.table.push(match &self.columns {
                    Some(names) => project(fields, names),
                    None => fields.to_vec(),
                });
                Ok(())
            }
        }
    }

    pub fn finish(mut self) -> io::Result<()> {
        if self.kind == OutputKind::Table {
            let columns = self.columns.take();
            let text = render_table(&self.table, columns.as_deref(), self.max_width);
            self.out.write_all(text.as_bytes())?;
        }
        Ok(())
    }
}

fn cell_text(value: Option<&Value>, max_width: Option<usize>) -> String {
    let text = match value {
        None | Some(Value::Null) => "-".to_string(),
        Some(value) => value.display_text().replace('\n', "\\n").replace('\t', "\\t"),
    };
    match max_width {
        Some(width) if text.chars().count() > width => {
            let kept: String = text.chars().take(width.saturating_sub(1)).collect();
            format!("{kept}…")
        }
        _ => text,
    }
}

/// Lays out rows in columns, numbers on the right. Columns come from `columns`, or else in the
/// order they first appear. Widths count characters, so wide scripts won't line up perfectly.
pub fn render_table(rows: &[Fields], columns: Option<&[String]>, max_width: Option<usize>) -> String {
    let names: Vec<String> = match columns {
        Some(columns) => columns.to_vec(),
        None => {
            let mut names: Vec<String> = Vec::new();
            for (name, _) in rows.iter().flatten() {
                if !names.contains(name) {
                    names.push(name.clone());
                }
            }
            names
        }
    };
    if names.is_empty() {
        return String::new();
    }
    // Rows hold flat names here, so a plain search is right even for names with dots in them.
    let find = |row: &'_ Fields, name: &str| -> Option<Value> {
        row.iter().rev().find(|(key, _)| key == name).map(|(_, value)| value.clone())
    };
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|row| names.iter().map(|name| cell_text(find(row, name).as_ref(), max_width)).collect())
        .collect();
    let numeric: Vec<bool> = (0..names.len())
        .map(|column| {
            let mut values = rows.iter().filter_map(|row| find(row, &names[column])).filter(|value| *value != Value::Null).peekable();
            values.peek().is_some() && values.all(|value| value.as_number().is_some())
        })
        .collect();
    let widths: Vec<usize> = (0..names.len())
        .map(|column| {
            let longest = cells.iter().map(|row| row[column].chars().count()).max().unwrap_or(0);
            longest.max(names[column].chars().count())
        })
        .collect();
    let mut out = String::new();
    let mut line = |parts: &[String], right: &dyn Fn(usize) -> bool| {
        let mut text = String::new();
        for (column, part) in parts.iter().enumerate() {
            if column > 0 {
                text.push_str("  ");
            }
            let width = widths[column];
            if right(column) {
                text.push_str(&format!("{part:>width$}"));
            } else {
                text.push_str(&format!("{part:<width$}"));
            }
        }
        out.push_str(text.trim_end());
        out.push('\n');
    };
    line(&names, &|column| numeric[column]);
    for row in &cells {
        line(row, &|column| numeric[column]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::Format;

    fn row(json: &str) -> Fields {
        Format::Json.parse_line(json).unwrap()
    }

    fn written(kind: OutputKind, columns: Option<Vec<String>>, rows: &[&str]) -> String {
        let mut out = Vec::new();
        let mut writer = RowWriter::new(&mut out, kind, columns, None);
        for line in rows {
            writer.write(&row(line), line).unwrap();
        }
        writer.finish().unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn raw_keeps_the_line_as_written() {
        assert_eq!(written(OutputKind::Raw, None, &[r#"{ "a" : 1 }"#]), "{ \"a\" : 1 }\n");
    }

    #[test]
    fn json_is_normalized() {
        assert_eq!(written(OutputKind::Json, None, &[r#"{ "a" : 1.50 }"#]), "{\"a\":1.5}\n");
    }

    #[test]
    fn logfmt_output() {
        assert_eq!(written(OutputKind::Logfmt, None, &[r#"{"a":"x y","b":2}"#]), "a=\"x y\" b=2\n");
    }

    #[test]
    fn projection_flattens_and_skips_what_is_missing() {
        let names = vec!["a.b".to_string(), "c".to_string(), "nope".to_string()];
        assert_eq!(written(OutputKind::Json, Some(names), &[r#"{"a":{"b":1},"c":2}"#]), "{\"a.b\":1,\"c\":2}\n");
    }

    #[test]
    fn tables_align_and_right_justify_numbers() {
        let text = written(OutputKind::Table, None, &[r#"{"name":"alpha","n":5}"#, r#"{"name":"b","n":123}"#]);
        assert_eq!(text, "name     n\nalpha    5\nb      123\n");
    }

    #[test]
    fn table_columns_are_the_union_and_gaps_show_a_dash() {
        let text = written(OutputKind::Table, None, &[r#"{"a":"x"}"#, r#"{"b":"y"}"#, r#"{"a":null}"#]);
        assert_eq!(text, "a  b\nx  -\n-  y\n-  -\n");
    }

    #[test]
    fn table_with_columns_keeps_dotted_names() {
        let names = vec!["a.b".to_string(), "gone".to_string()];
        let text = written(OutputKind::Table, Some(names), &[r#"{"a":{"b":"v"}}"#]);
        assert_eq!(text, "a.b  gone\nv    -\n");
    }

    #[test]
    fn line_breaks_in_cells_are_escaped() {
        let text = written(OutputKind::Table, None, &[r#"{"a":"x\ny"}"#]);
        assert_eq!(text, "a\nx\\ny\n");
    }

    #[test]
    fn long_cells_are_cut_when_asked() {
        let rows = vec![row(r#"{"a":"abcdefgh"}"#)];
        assert_eq!(render_table(&rows, None, Some(4)), "a\nabc…\n");
    }

    #[test]
    fn an_empty_table_is_empty() {
        assert_eq!(written(OutputKind::Table, None, &[]), "");
    }

    #[test]
    fn a_table_with_named_columns_prints_its_header_even_when_empty() {
        assert_eq!(written(OutputKind::Table, Some(vec!["a".to_string()]), &[]), "a\n");
    }
}
