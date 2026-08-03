//! The four output formats.
//!
//! Each is a rendering of the one envelope `nostdb-core` produces. JSON is the canonical
//! form; JSONL and CSV carry strictly less, and the table carries least of all.
//!
//! # The table is the only unstable one
//!
//! Column widths, padding, and truncation may change between versions. A script parsing
//! it is parsing something this crate does not promise. The other three are stable, which
//! is why one of them is always available.

use nostdb_core::result::{ResultEnvelope, value_csv};
use std::fmt;
use std::io::Write;

/// How a result is written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Format {
    /// Aligned columns, for a person. Not stable.
    #[default]
    Table,
    /// The whole envelope as one JSON document.
    Json,
    /// A header line, one line per row, then a trailer line.
    Jsonl,
    /// RFC 4180 with a header row. Carries no summary and no warnings.
    Csv,
}

impl Format {
    /// Reads a format from the name a caller supplies.
    #[must_use]
    pub fn from_text(text: &str) -> Option<Self> {
        Some(match text {
            "table" => Self::Table,
            "json" => Self::Json,
            "jsonl" => Self::Jsonl,
            "csv" => Self::Csv,
            _ => return None,
        })
    }

    /// The accepted spellings, for a usage message.
    pub const NAMES: [&'static str; 4] = ["table", "json", "jsonl", "csv"];

    /// The spelling this format is written as.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Table => "table",
            Self::Json => "json",
            Self::Jsonl => "jsonl",
            Self::Csv => "csv",
        }
    }

    /// Reports whether this format carries the warnings itself.
    ///
    /// When it does not, a caller has to write them somewhere else, which for a command
    /// is standard error. A warning nobody sees is the same as no warning.
    #[must_use]
    pub const fn carries_warnings(self) -> bool {
        matches!(self, Self::Json | Self::Jsonl)
    }
}

impl fmt::Display for Format {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Writes `envelope` to `out` in `format`.
///
/// Only the data goes here. A format that cannot carry warnings leaves them for the
/// caller, which [`Format::carries_warnings`] reports.
///
/// # Errors
///
/// Returns whatever the writer reports.
pub fn write(
    envelope: &ResultEnvelope,
    format: Format,
    out: &mut dyn Write,
) -> std::io::Result<()> {
    match format {
        Format::Json => writeln!(
            out,
            "{}",
            serde_json::to_string_pretty(&envelope.to_json())
                .unwrap_or_else(|_| envelope.to_json().to_string())
        ),
        Format::Jsonl => {
            writeln!(out, "{}", envelope.jsonl_header())?;
            for row in envelope.rows_json() {
                writeln!(out, "{row}")?;
            }
            writeln!(out, "{}", envelope.jsonl_trailer())
        }
        Format::Csv => write_csv(envelope, out),
        Format::Table => write_table(envelope, out),
    }
}

/// Quotes a CSV field when RFC 4180 requires it.
fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn write_csv(envelope: &ResultEnvelope, out: &mut dyn Write) -> std::io::Result<()> {
    let header: Vec<String> = envelope
        .columns
        .iter()
        .map(|name| csv_field(name))
        .collect();
    writeln!(out, "{}", header.join(","))?;
    for row in &envelope.rows {
        let fields: Vec<String> = row
            .iter()
            .map(|value| csv_field(&value_csv(value)))
            .collect();
        writeln!(out, "{}", fields.join(","))?;
    }
    Ok(())
}

/// Renders a value the way the table shows it: a bare string, an empty cell for null.
fn cell(value: &nostdb_core::execute::QueryValue) -> String {
    value_csv(value)
}

fn write_table(envelope: &ResultEnvelope, out: &mut dyn Write) -> std::io::Result<()> {
    if envelope.columns.is_empty() {
        writeln!(out, "(no columns)")?;
        return Ok(());
    }

    let rows: Vec<Vec<String>> = envelope
        .rows
        .iter()
        .map(|row| row.iter().map(cell).collect())
        .collect();

    // Width is counted in Unicode scalar values rather than bytes, so a non-ASCII name
    // does not throw the alignment out. It is still not display width, which needs the
    // East Asian Width tables; the table is explicitly unstable, and adding a dependency
    // for a format nothing may parse is not worth it.
    let mut widths: Vec<usize> = envelope
        .columns
        .iter()
        .map(|name| name.chars().count())
        .collect();
    for row in &rows {
        for (index, field) in row.iter().enumerate() {
            if let Some(width) = widths.get_mut(index) {
                *width = (*width).max(field.chars().count());
            }
        }
    }

    let line = |fields: &[String], out: &mut dyn Write| -> std::io::Result<()> {
        let padded: Vec<String> = fields
            .iter()
            .enumerate()
            .map(|(index, field)| {
                let width = widths.get(index).copied().unwrap_or(0);
                let padding = width.saturating_sub(field.chars().count());
                format!("{field}{}", " ".repeat(padding))
            })
            .collect();
        writeln!(out, "{}", padded.join("  ").trim_end())
    };

    line(&envelope.columns, out)?;
    let rule: Vec<String> = widths.iter().map(|width| "-".repeat(*width)).collect();
    writeln!(out, "{}", rule.join("  "))?;
    for row in &rows {
        line(row, out)?;
    }
    writeln!(
        out,
        "\n{} row{}",
        envelope.rows.len(),
        if envelope.rows.len() == 1 { "" } else { "s" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostdb_core::execute::QueryValue;

    fn envelope(columns: &[&str], rows: Vec<Vec<QueryValue>>) -> ResultEnvelope {
        ResultEnvelope {
            columns: columns.iter().map(|name| (*name).to_owned()).collect(),
            rows,
            database_generation: 1,
            linked_databases_opened: 0,
            writes: None,
            warnings: Vec::new(),
        }
    }

    fn rendered(envelope: &ResultEnvelope, format: Format) -> String {
        let mut out = Vec::new();
        write(envelope, format, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn a_format_is_read_from_its_name_and_nothing_else() {
        for name in Format::NAMES {
            assert_eq!(Format::from_text(name).map(|f| f.as_str()), Some(name));
        }
        assert_eq!(Format::from_text("yaml"), None);
        assert_eq!(Format::from_text("JSON"), None, "matching is exact");
    }

    #[test]
    fn json_is_one_document_carrying_the_whole_envelope() {
        let text = rendered(
            &envelope(&["n"], vec![vec![QueryValue::Integer(1)]]),
            Format::Json,
        );
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("one JSON document");
        assert_eq!(
            parsed["result_version"],
            nostdb_core::result::RESULT_VERSION
        );
        assert_eq!(parsed["summary"]["rows"], 1);
    }

    #[test]
    fn jsonl_is_a_header_the_rows_and_a_trailer() {
        let text = rendered(
            &envelope(
                &["n"],
                vec![vec![QueryValue::Integer(1)], vec![QueryValue::Integer(2)]],
            ),
            Format::Jsonl,
        );
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "{text}");
        for line in &lines {
            serde_json::from_str::<serde_json::Value>(line).expect("every line is JSON");
        }
        let header: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(header["columns"], serde_json::json!(["n"]));
        let trailer: serde_json::Value = serde_json::from_str(lines[3]).unwrap();
        assert_eq!(trailer["summary"]["rows"], 2);
    }

    #[test]
    fn a_jsonl_consumer_that_stops_early_has_fewer_rows_never_wrong_ones() {
        let text = rendered(
            &envelope(
                &["n"],
                vec![vec![QueryValue::Integer(1)], vec![QueryValue::Integer(2)]],
            ),
            Format::Jsonl,
        );
        let lines: Vec<&str> = text.lines().collect();
        // Read the header and one row, then stop.
        let first: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(first, serde_json::json!([1]));
    }

    #[test]
    fn csv_quotes_only_what_rfc_4180_requires() {
        let text = rendered(
            &envelope(
                &["plain", "comma", "quote"],
                vec![vec![
                    QueryValue::Text("simple".to_owned()),
                    QueryValue::Text("a,b".to_owned()),
                    QueryValue::Text("say \"hi\"".to_owned()),
                ]],
            ),
            Format::Csv,
        );
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "plain,comma,quote");
        assert_eq!(lines[1], "simple,\"a,b\",\"say \"\"hi\"\"\"");
    }

    #[test]
    fn csv_writes_null_as_an_empty_field() {
        let text = rendered(
            &envelope(
                &["a", "b"],
                vec![vec![QueryValue::Null, QueryValue::Integer(1)]],
            ),
            Format::Csv,
        );
        assert_eq!(text.lines().nth(1), Some(",1"));
    }

    #[test]
    fn only_json_and_jsonl_carry_the_warnings() {
        assert!(Format::Json.carries_warnings());
        assert!(Format::Jsonl.carries_warnings());
        assert!(!Format::Csv.carries_warnings());
        assert!(!Format::Table.carries_warnings());
    }

    #[test]
    fn the_table_aligns_by_scalar_count_rather_than_byte_length() {
        let text = rendered(
            &envelope(
                &["name"],
                vec![
                    vec![QueryValue::Text("한글".to_owned())],
                    vec![QueryValue::Text("ab".to_owned())],
                ],
            ),
            Format::Table,
        );
        let lines: Vec<&str> = text.lines().collect();
        // Two scalars either way, so the rule matches the widest cell rather than the
        // byte length of the multi-byte one.
        assert_eq!(lines[1], "----");
        assert!(text.contains("2 rows"), "{text}");
    }

    #[test]
    fn the_table_says_so_when_there_are_no_columns() {
        let text = rendered(&envelope(&[], Vec::new()), Format::Table);
        assert!(text.contains("no columns"), "{text}");
    }

    #[test]
    fn the_table_counts_one_row_in_the_singular() {
        let text = rendered(
            &envelope(&["n"], vec![vec![QueryValue::Integer(1)]]),
            Format::Table,
        );
        assert!(text.contains("1 row\n"), "{text}");
    }
}
