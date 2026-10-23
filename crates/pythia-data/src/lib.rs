//! Export of query results: CSV, TSV, JSON, XLSX, INSERT statements,
//! Markdown and HTML.
//!
//! Cell values arrive as text (see `pythia_db::Cell`); column types decide how
//! they are written: numbers stay numbers in JSON/XLSX/INSERT, dates become
//! `TO_DATE`/`TO_TIMESTAMP` literals in INSERT scripts.

use std::io::Write;

pub mod import;

use pythia_db::{Cell, Column};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    Csv,
    Tsv,
    Json,
    Xlsx,
    Insert,
    Markdown,
    Html,
}

impl Format {
    pub const ALL: &[Format] = &[
        Format::Csv,
        Format::Tsv,
        Format::Json,
        Format::Xlsx,
        Format::Insert,
        Format::Markdown,
        Format::Html,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Format::Csv => "CSV",
            Format::Tsv => "Tab separated",
            Format::Json => "JSON",
            Format::Xlsx => "Excel (xlsx)",
            Format::Insert => "INSERT statements",
            Format::Markdown => "Markdown table",
            Format::Html => "HTML table",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::Tsv => "tsv",
            Format::Json => "json",
            Format::Xlsx => "xlsx",
            Format::Insert => "sql",
            Format::Markdown => "md",
            Format::Html => "html",
        }
    }

    /// Text formats can go to the clipboard.
    pub fn is_text(self) -> bool {
        self != Format::Xlsx
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub header: bool,
    /// Target table for INSERT statements.
    pub table: String,
    /// Sheet name for XLSX.
    pub sheet: String,
    /// CSV field separator (default `,`).
    pub delimiter: Option<u8>,
    /// How CSV/TSV write NULL (default: empty).
    pub null_text: String,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            header: true,
            table: "my_table".into(),
            sheet: "Result".into(),
            delimiter: None,
            null_text: String::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Csv(#[from] csv::Error),
    #[error(transparent)]
    Xlsx(#[from] rust_xlsxwriter::XlsxError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Writes `rows` in `format`.
pub fn export(
    format: Format,
    columns: &[Column],
    rows: &[Vec<Cell>],
    options: &Options,
    out: &mut dyn Write,
) -> Result<()> {
    match format {
        Format::Csv => delimited(
            options.delimiter.unwrap_or(b','),
            columns,
            rows,
            options,
            out,
        ),
        Format::Tsv => delimited(b'\t', columns, rows, options, out),
        Format::Json => json(columns, rows, out),
        Format::Xlsx => xlsx(columns, rows, options, out),
        Format::Insert => inserts(columns, rows, options, out),
        Format::Markdown => markdown(columns, rows, out),
        Format::Html => html(columns, rows, out),
    }
}

/// Convenience wrapper returning the exported bytes.
pub fn export_to_vec(
    format: Format,
    columns: &[Column],
    rows: &[Vec<Cell>],
    options: &Options,
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    export(format, columns, rows, options, &mut out)?;
    Ok(out)
}

/// Writes rows batch by batch, for results too large to hold in memory.
/// Formats that need all rows up front (Excel, HTML) can't stream.
pub struct StreamWriter<W: Write> {
    format: Format,
    columns: Vec<Column>,
    options: Options,
    out: W,
    rows: u64,
}

impl Format {
    /// Formats [`StreamWriter`] supports.
    pub const STREAMING: &[Format] = &[
        Format::Csv,
        Format::Tsv,
        Format::Json,
        Format::Insert,
        Format::Markdown,
    ];
}

impl<W: Write> StreamWriter<W> {
    pub fn new(format: Format, columns: Vec<Column>, options: Options, mut out: W) -> Result<Self> {
        assert!(
            Format::STREAMING.contains(&format),
            "{format:?} can't stream"
        );
        match format {
            Format::Csv | Format::Tsv if options.header => {
                let delimiter = if format == Format::Csv {
                    options.delimiter.unwrap_or(b',')
                } else {
                    b'\t'
                };
                let mut writer = csv::WriterBuilder::new()
                    .delimiter(delimiter)
                    .from_writer(&mut out);
                writer.write_record(columns.iter().map(|c| c.name.as_str()))?;
                writer.flush()?;
            }
            Format::Json => write!(out, "[")?,
            Format::Markdown => markdown_header(&columns, &mut out)?,
            _ => {}
        }
        Ok(Self {
            format,
            columns,
            options,
            out,
            rows: 0,
        })
    }

    pub fn write(&mut self, rows: &[Vec<Cell>]) -> Result<()> {
        let options = Options {
            header: false,
            ..self.options.clone()
        };
        match self.format {
            Format::Csv => delimited(
                options.delimiter.unwrap_or(b','),
                &self.columns,
                rows,
                &options,
                &mut self.out,
            )?,
            Format::Tsv => delimited(b'\t', &self.columns, rows, &options, &mut self.out)?,
            Format::Insert => inserts(&self.columns, rows, &options, &mut self.out)?,
            Format::Markdown => markdown_rows(rows, &mut self.out)?,
            Format::Json => {
                for (i, row) in rows.iter().enumerate() {
                    let sep = if self.rows == 0 && i == 0 {
                        "\n"
                    } else {
                        ",\n"
                    };
                    let object = serde_json::to_string_pretty(&json_object(&self.columns, row))?;
                    write!(self.out, "{sep}{}", indent(&object))?;
                }
            }
            Format::Xlsx | Format::Html => unreachable!(),
        }
        self.rows += rows.len() as u64;
        Ok(())
    }

    pub fn rows(&self) -> u64 {
        self.rows
    }

    pub fn finish(mut self) -> Result<W> {
        if self.format == Format::Json {
            let close = if self.rows == 0 { "]\n" } else { "\n]\n" };
            write!(self.out, "{close}")?;
        }
        self.out.flush()?;
        Ok(self.out)
    }
}

/// Indents a pretty-printed JSON object by two spaces (array element).
fn indent(text: &str) -> String {
    text.lines()
        .map(|l| format!("  {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn delimited(
    delimiter: u8,
    columns: &[Column],
    rows: &[Vec<Cell>],
    options: &Options,
    out: &mut dyn Write,
) -> Result<()> {
    let mut writer = csv::WriterBuilder::new()
        .delimiter(delimiter)
        .from_writer(out);
    if options.header {
        writer.write_record(columns.iter().map(|c| c.name.as_str()))?;
    }
    for row in rows {
        writer.write_record(
            row.iter()
                .map(|c| c.as_deref().unwrap_or(&options.null_text)),
        )?;
    }
    writer.flush()?;
    Ok(())
}

fn json(columns: &[Column], rows: &[Vec<Cell>], out: &mut dyn Write) -> Result<()> {
    let array: Vec<serde_json::Value> = rows.iter().map(|row| json_object(columns, row)).collect();
    serde_json::to_writer_pretty(&mut *out, &array)?;
    writeln!(out)?;
    Ok(())
}

fn json_object(columns: &[Column], row: &[Cell]) -> serde_json::Value {
    let object = columns
        .iter()
        .zip(row)
        .map(|(col, cell)| {
            let value = match cell {
                None => serde_json::Value::Null,
                Some(v) if col.numeric => number(v).map_or_else(|| v.clone().into(), Into::into),
                Some(v) => v.clone().into(),
            };
            (col.name.clone(), value)
        })
        .collect();
    serde_json::Value::Object(object)
}

/// Parses a number for typed output, keeping integers exact.
fn number(text: &str) -> Option<serde_json::Number> {
    if let Ok(i) = text.parse::<i64>() {
        return Some(i.into());
    }
    text.parse::<f64>()
        .ok()
        .and_then(serde_json::Number::from_f64)
}

fn xlsx(
    columns: &[Column],
    rows: &[Vec<Cell>],
    options: &Options,
    out: &mut dyn Write,
) -> Result<()> {
    use rust_xlsxwriter::{Format as XFormat, Workbook};
    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();
    sheet.set_name(sheet_name(&options.sheet))?;
    let bold = XFormat::new().set_bold();
    let date = XFormat::new().set_num_format("yyyy-mm-dd hh:mm:ss");
    let first_row = u32::from(options.header);
    if options.header {
        for (c, col) in columns.iter().enumerate() {
            sheet.write_string_with_format(0, c as u16, &col.name, &bold)?;
        }
    }
    for (r, row) in rows.iter().enumerate() {
        let r = r as u32 + first_row;
        for (c, (col, cell)) in columns.iter().zip(row).enumerate() {
            let Some(value) = cell else { continue };
            let c = c as u16;
            match (col.numeric, kind(col)) {
                (true, _) => match value.parse::<f64>() {
                    Ok(n) => sheet.write_number(r, c, n)?,
                    Err(_) => sheet.write_string(r, c, value)?,
                },
                (false, Kind::Date | Kind::Timestamp) => match excel_datetime(value) {
                    Some(dt) => sheet.write_datetime_with_format(r, c, &dt, &date)?,
                    None => sheet.write_string(r, c, value)?,
                },
                _ => sheet.write_string(r, c, value)?,
            };
        }
    }
    if options.header && !columns.is_empty() {
        sheet.set_freeze_panes(1, 0)?;
        sheet.autofilter(0, 0, rows.len() as u32, columns.len() as u16 - 1)?;
    }
    sheet.autofit();
    out.write_all(&workbook.save_to_buffer()?)?;
    Ok(())
}

/// Excel sheet names: max 31 chars, no `[]:*?/\`.
fn sheet_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !"[]:*?/\\".contains(*c))
        .take(31)
        .collect();
    if cleaned.is_empty() {
        "Result".into()
    } else {
        cleaned
    }
}

fn excel_datetime(text: &str) -> Option<rust_xlsxwriter::ExcelDateTime> {
    // `YYYY-MM-DD HH24:MI:SS[.ff…][ ±TZH:TZM| region]`; time zones are dropped.
    let text = text.split(' ').take(2).collect::<Vec<_>>().join("T");
    let text = text.split('.').next()?;
    rust_xlsxwriter::ExcelDateTime::parse_from_str(text).ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Date,
    Timestamp,
    TimestampTz,
    Binary,
}

fn kind(col: &Column) -> Kind {
    let t = col.type_name.to_ascii_uppercase();
    if t == "DATE" {
        Kind::Date
    } else if t.starts_with("TIMESTAMP") && t.contains("TIME ZONE") {
        Kind::TimestampTz
    } else if t.starts_with("TIMESTAMP") {
        Kind::Timestamp
    } else if t.starts_with("RAW") || t == "BLOB" || t == "LONG RAW" {
        Kind::Binary
    } else {
        Kind::Text
    }
}

/// Renders a value as a SQL literal for `col`.
pub fn sql_literal(col: &Column, value: Option<&str>) -> String {
    let Some(v) = value else {
        return "NULL".into();
    };
    if col.numeric && v.parse::<f64>().is_ok() {
        return v.to_owned();
    }
    let quoted = format!("'{}'", v.replace('\'', "''"));
    match kind(col) {
        Kind::Date => format!("to_date({quoted}, 'YYYY-MM-DD HH24:MI:SS')"),
        Kind::Timestamp if v.contains('.') => {
            format!("to_timestamp({quoted}, 'YYYY-MM-DD HH24:MI:SS.FF')")
        }
        Kind::Timestamp => format!("to_timestamp({quoted}, 'YYYY-MM-DD HH24:MI:SS')"),
        // TZR takes an offset (`+02:00`) or a region (`Europe/Zurich`).
        Kind::TimestampTz if v.contains('.') => {
            format!("to_timestamp_tz({quoted}, 'YYYY-MM-DD HH24:MI:SS.FF TZR')")
        }
        Kind::TimestampTz => format!("to_timestamp_tz({quoted}, 'YYYY-MM-DD HH24:MI:SS TZR')"),
        Kind::Binary => format!("hextoraw({quoted})"),
        Kind::Text => quoted,
    }
}

fn inserts(
    columns: &[Column],
    rows: &[Vec<Cell>],
    options: &Options,
    out: &mut dyn Write,
) -> Result<()> {
    let names: Vec<String> = columns.iter().map(|c| identifier(&c.name)).collect();
    let target = if options.table.trim().is_empty() {
        "my_table"
    } else {
        options.table.trim()
    };
    let head = format!("insert into {target} ({}) values (", names.join(", "));
    for row in rows {
        let values: Vec<String> = columns
            .iter()
            .zip(row)
            .map(|(c, v)| sql_literal(c, v.as_deref()))
            .collect();
        writeln!(out, "{head}{});", values.join(", "))?;
    }
    Ok(())
}

/// Lowercase plain identifiers; quote anything that needs it.
fn identifier(name: &str) -> String {
    let plain = name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || "_$#".contains(c));
    if plain {
        name.to_lowercase()
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

fn markdown(columns: &[Column], rows: &[Vec<Cell>], out: &mut dyn Write) -> Result<()> {
    markdown_header(columns, out)?;
    markdown_rows(rows, out)
}

fn markdown_escape(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

fn markdown_header(columns: &[Column], out: &mut dyn Write) -> Result<()> {
    let header: Vec<String> = columns.iter().map(|c| markdown_escape(&c.name)).collect();
    writeln!(out, "| {} |", header.join(" | "))?;
    let rule: Vec<&str> = columns
        .iter()
        .map(|c| if c.numeric { "---:" } else { "---" })
        .collect();
    writeln!(out, "| {} |", rule.join(" | "))?;
    Ok(())
}

fn markdown_rows(rows: &[Vec<Cell>], out: &mut dyn Write) -> Result<()> {
    for row in rows {
        let cells: Vec<String> = row
            .iter()
            .map(|c| markdown_escape(c.as_deref().unwrap_or("")))
            .collect();
        writeln!(out, "| {} |", cells.join(" | "))?;
    }
    Ok(())
}

fn html(columns: &[Column], rows: &[Vec<Cell>], out: &mut dyn Write) -> Result<()> {
    let escape = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    };
    writeln!(out, "<table>")?;
    writeln!(
        out,
        "  <thead><tr>{}</tr></thead>",
        columns
            .iter()
            .map(|c| format!("<th>{}</th>", escape(&c.name)))
            .collect::<String>()
    )?;
    writeln!(out, "  <tbody>")?;
    for row in rows {
        let cells: String = columns
            .iter()
            .zip(row)
            .map(|(c, v)| {
                let align = if c.numeric {
                    " style=\"text-align:right\""
                } else {
                    ""
                };
                format!("<td{align}>{}</td>", escape(v.as_deref().unwrap_or("")))
            })
            .collect();
        writeln!(out, "    <tr>{cells}</tr>")?;
    }
    writeln!(out, "  </tbody>\n</table>")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (Vec<Column>, Vec<Vec<Cell>>) {
        let col = |name: &str, ty: &str, numeric| Column {
            name: name.into(),
            type_name: ty.into(),
            numeric,
        };
        (
            vec![
                col("ID", "NUMBER(10)", true),
                col("NAME", "VARCHAR2(30)", false),
                col("HIRED", "DATE", false),
                col("Mixed Case", "VARCHAR2(5)", false),
            ],
            vec![
                vec![
                    Some("1".into()),
                    Some("O'Brien, \"Pat\"".into()),
                    Some("2020-01-02 03:04:05".into()),
                    None,
                ],
                vec![Some("2.5".into()), None, None, Some("a|b".into())],
            ],
        )
    }

    fn text(format: Format) -> String {
        let (c, r) = sample();
        String::from_utf8(
            export_to_vec(
                format,
                &c,
                &r,
                &Options {
                    table: "emp".into(),
                    ..Options::default()
                },
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn csv_quotes() {
        assert_eq!(
            text(Format::Csv),
            "ID,NAME,HIRED,Mixed Case\n1,\"O'Brien, \"\"Pat\"\"\",2020-01-02 03:04:05,\n2.5,,,a|b\n"
        );
    }

    #[test]
    fn insert_literals() {
        assert_eq!(
            text(Format::Insert),
            "insert into emp (id, name, hired, \"Mixed Case\") values (1, 'O''Brien, \"Pat\"', \
             to_date('2020-01-02 03:04:05', 'YYYY-MM-DD HH24:MI:SS'), NULL);\n\
             insert into emp (id, name, hired, \"Mixed Case\") values (2.5, NULL, NULL, 'a|b');\n"
        );
    }

    #[test]
    fn time_zone_literals() {
        let tz = Column {
            name: "AT".into(),
            type_name: "TIMESTAMP(6) WITH TIME ZONE".into(),
            numeric: false,
        };
        assert_eq!(
            sql_literal(&tz, Some("2026-07-01 12:00:00.000000 Europe/Zurich")),
            "to_timestamp_tz('2026-07-01 12:00:00.000000 Europe/Zurich', \
             'YYYY-MM-DD HH24:MI:SS.FF TZR')"
        );
        assert_eq!(
            sql_literal(&tz, Some("2026-07-01 12:00:00 +02:00")),
            "to_timestamp_tz('2026-07-01 12:00:00 +02:00', 'YYYY-MM-DD HH24:MI:SS TZR')"
        );
    }

    #[test]
    fn json_types() {
        let v: serde_json::Value = serde_json::from_str(&text(Format::Json)).unwrap();
        assert_eq!(v[0]["ID"], 1);
        assert_eq!(v[1]["ID"], 2.5);
        assert_eq!(v[1]["NAME"], serde_json::Value::Null);
    }

    #[test]
    fn markdown_and_html_escape() {
        assert!(text(Format::Markdown).contains("| 2.5 |  |  | a\\|b |"));
        assert!(
            text(Format::Markdown)
                .starts_with("| ID | NAME | HIRED | Mixed Case |\n| ---: | --- | --- | --- |\n")
        );
        assert!(text(Format::Html).contains("<td>O'Brien, &quot;Pat&quot;</td>"));
    }

    #[test]
    fn xlsx_is_zip() {
        let (c, r) = sample();
        let bytes = export_to_vec(Format::Xlsx, &c, &r, &Options::default()).unwrap();
        assert_eq!(&bytes[..2], b"PK");
    }

    #[test]
    fn streams_like_export() {
        let columns = vec![
            Column {
                name: "ID".into(),
                type_name: "NUMBER".into(),
                numeric: true,
            },
            Column {
                name: "NAME".into(),
                type_name: "VARCHAR2".into(),
                numeric: false,
            },
        ];
        let rows: Vec<Vec<Cell>> = (0..5)
            .map(|i| vec![Some(i.to_string()), Some(format!("n|{i},\"x\""))])
            .collect();
        for &format in Format::STREAMING {
            let options = Options::default();
            let whole = export_to_vec(format, &columns, &rows, &options).unwrap();
            let mut stream =
                StreamWriter::new(format, columns.clone(), options, Vec::new()).unwrap();
            stream.write(&rows[..2]).unwrap();
            stream.write(&rows[2..]).unwrap();
            assert_eq!(stream.rows(), 5);
            let streamed = stream.finish().unwrap();
            if format == Format::Json {
                let a: serde_json::Value = serde_json::from_slice(&whole).unwrap();
                let b: serde_json::Value = serde_json::from_slice(&streamed).unwrap();
                assert_eq!(a, b);
            } else {
                assert_eq!(
                    String::from_utf8(whole).unwrap(),
                    String::from_utf8(streamed).unwrap(),
                    "{format:?}"
                );
            }
        }
        let empty = StreamWriter::new(Format::Json, columns, Options::default(), Vec::new())
            .unwrap()
            .finish()
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&empty).unwrap(),
            serde_json::json!([])
        );
    }
}
