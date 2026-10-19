//! Output and export: human-readable formats (tree, table, JSON) and
//! machine-readable encodings (JSON array, NDJSON, CSV).

pub mod record;
pub mod table;
pub mod tree;

use std::io::Write;
use std::path::Path;

pub use record::NodeRecord;

use crate::cli::{Encoding, Format};
use crate::error::{Result, ScanError};

/// Render records for human display to stdout.
pub fn render_display(records: &mut [NodeRecord], format: Format) -> Result<()> {
    records.sort_by(|a, b| a.browse_path.cmp(&b.browse_path));
    let text = match format {
        Format::Tree => tree::render(records),
        Format::Table => table::render(records),
        Format::Json => {
            serde_json::to_string_pretty(&records).map_err(|e| ScanError::Export(e.to_string()))?
        }
    };
    print!("{text}");
    if !text.ends_with('\n') {
        println!();
    }
    Ok(())
}

/// Write records using a machine-readable encoding to a file or stdout.
pub fn write_export(
    records: &mut [NodeRecord],
    encoding: Encoding,
    file: Option<&Path>,
) -> Result<()> {
    records.sort_by(|a, b| a.browse_path.cmp(&b.browse_path));

    if encoding == Encoding::TedgeDot {
        // Needs the namespace table and merge semantics: see `tedge_dot::library`.
        return Err(ScanError::Export(
            "a tedge-dot point library can only be written by `browse`".into(),
        ));
    }

    let mut writer: Box<dyn Write> = match file {
        Some(path) => Box::new(std::io::BufWriter::new(std::fs::File::create(path)?)),
        None => Box::new(std::io::BufWriter::new(std::io::stdout())),
    };

    match encoding {
        Encoding::Json => {
            let json = serde_json::to_string_pretty(&records)
                .map_err(|e| ScanError::Export(e.to_string()))?;
            writer.write_all(json.as_bytes())?;
            writer.write_all(b"\n")?;
        }
        Encoding::Ndjson => {
            for record in records.iter() {
                let line =
                    serde_json::to_string(record).map_err(|e| ScanError::Export(e.to_string()))?;
                writer.write_all(line.as_bytes())?;
                writer.write_all(b"\n")?;
            }
        }
        Encoding::TedgeDot => unreachable!("handled above"),
        Encoding::Csv => {
            // Serde + skip_serializing_if would yield a variable number of
            // columns per row, which the CSV writer rejects. Emit a fixed set
            // of columns instead, mapping absent optionals to empty strings.
            let mut csv_writer = csv::Writer::from_writer(&mut writer);
            csv_writer
                .write_record(CSV_COLUMNS)
                .map_err(|e| ScanError::Export(e.to_string()))?;
            for record in records.iter() {
                csv_writer
                    .write_record(csv_row(record))
                    .map_err(|e| ScanError::Export(e.to_string()))?;
            }
            csv_writer
                .flush()
                .map_err(|e| ScanError::Export(e.to_string()))?;
        }
    }

    writer.flush()?;
    Ok(())
}

/// Fixed CSV column order.
const CSV_COLUMNS: &[&str] = &[
    "browse_path",
    "display_name",
    "browse_name",
    "node_id",
    "namespace_uri",
    "node_class",
    "datatype",
    "value_rank",
    "access_level",
    "user_access_level",
    "historizing",
    "subscribable",
    "validation_status",
    "current_value",
    "engineering_units",
    "description",
];

/// Render one record as a fixed-width CSV row (absent optionals become empty).
fn csv_row(r: &NodeRecord) -> Vec<String> {
    fn opt<T: ToString>(v: &Option<T>) -> String {
        v.as_ref().map(ToString::to_string).unwrap_or_default()
    }
    vec![
        r.browse_path.clone(),
        r.display_name.clone(),
        r.browse_name.clone(),
        r.node_id.clone(),
        r.namespace_uri.clone(),
        r.node_class.clone(),
        opt(&r.datatype),
        opt(&r.value_rank),
        opt(&r.access_level),
        opt(&r.user_access_level),
        opt(&r.historizing),
        r.subscribable.to_string(),
        opt(&r.validation_status),
        opt(&r.current_value),
        opt(&r.engineering_units),
        opt(&r.description),
    ]
}
