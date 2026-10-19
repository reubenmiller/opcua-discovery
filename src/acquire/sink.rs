//! Output sink for acquired data.
//!
//! A single sink is shared across all tasks in an acquisition run. It supports
//! both time-series *samples* (poll / subscribe / history) and structural
//! *records* (scan), rendered in one of several formats. Streaming formats
//! (`tedge`, `ndjson`) write each item immediately; batch formats (`json`,
//! `csv`) buffer items and emit them on [`Sink::finish`].

use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

use serde_json::{Map, Value};

use crate::acquire::config::{MetadataMode, OutputConfig, OutputFormat};
use crate::error::{Result, ScanError};
use crate::export::record::NodeRecord;

/// Descriptive context about a sampled data point, attached to every sample so
/// downstream consumers can interpret it without a separate lookup.
#[derive(Debug, Clone, Default)]
pub struct SampleMeta {
    /// Normalized, cross-platform data type name (e.g. `float64`), when the
    /// node's declared type maps to a known scalar primitive.
    pub data_type: Option<String>,
    /// Engineering units label folded in from the node's `EngineeringUnits`
    /// property, when present. Rendered at the top level as `units`.
    pub engineering_units: Option<String>,
    /// Browse name (namespace-qualified text).
    pub browse_name: Option<String>,
    /// Display name.
    pub display_name: Option<String>,
    /// Full browse path (URI-qualified), when known.
    pub browse_path: Option<String>,
    /// Namespace URI of the node.
    pub namespace: Option<String>,
    /// Engineering range (low, high) folded in from the node's `EURange`
    /// property, when present.
    pub eu_range: Option<(f64, f64)>,
    /// Connection endpoint URL the data originated from.
    pub source_endpoint: Option<String>,
    /// Human-readable connection / source name (from `[connection] name`).
    pub source_name: Option<String>,
    /// Explicit output key (a tedge-dot point id); wins over the browse path.
    pub key: Option<String>,
}

impl SampleMeta {
    /// Render the *descriptive* metadata (browse/display name, path, namespace,
    /// engineering range) as a JSON object with only the populated sub-keys.
    /// Type and units are surfaced at the top level instead.
    fn descriptive_json(&self) -> Value {
        let mut map = Map::new();
        if let Some(v) = &self.browse_name {
            map.insert("browseName".to_string(), Value::String(v.clone()));
        }
        if let Some(v) = &self.display_name {
            map.insert("displayName".to_string(), Value::String(v.clone()));
        }
        if let Some(v) = &self.browse_path {
            map.insert("browsePath".to_string(), Value::String(v.clone()));
        }
        if let Some(v) = &self.namespace {
            map.insert("namespace".to_string(), Value::String(v.clone()));
        }
        if let Some((low, high)) = self.eu_range {
            let mut range = Map::new();
            range.insert("low".to_string(), json_number(low));
            range.insert("high".to_string(), json_number(high));
            map.insert("euRange".to_string(), Value::Object(range));
        }
        if self.source_endpoint.is_some() || self.source_name.is_some() {
            let mut source = Map::new();
            if let Some(v) = &self.source_endpoint {
                source.insert("endpoint".to_string(), Value::String(v.clone()));
            }
            if let Some(v) = &self.source_name {
                source.insert("name".to_string(), Value::String(v.clone()));
            }
            map.insert("source".to_string(), Value::Object(source));
        }
        Value::Object(map)
    }

    /// The output key for this data point: the full browse path when known,
    /// otherwise the supplied fallback measurement name.
    fn output_key<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.key
            .as_deref()
            .or(self.browse_path.as_deref())
            .unwrap_or(fallback)
    }
}

fn json_number(n: f64) -> Value {
    serde_json::Number::from_f64(n)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

/// A single time-series sample produced by a poll/subscribe/history task.
#[derive(Debug, Clone)]
pub struct Sample {
    /// Measurement name (alias or browse name).
    pub name: String,
    /// Source node id (string form).
    pub node_id: String,
    /// The value (numeric values stay numeric).
    pub value: Value,
    /// Timestamp (RFC3339), when available.
    pub time: Option<String>,
    /// Status code string, when not Good.
    pub status: Option<String>,
    /// Descriptive context about the data point.
    pub meta: std::sync::Arc<SampleMeta>,
}

/// Shared, thread-safe output sink.
pub struct Sink {
    format: OutputFormat,
    topic: Option<String>,
    metadata: MetadataMode,
    state: Mutex<State>,
}

struct State {
    writer: Box<dyn Write + Send>,
    /// Buffered JSON objects for batch formats (json/csv).
    buffer: Vec<Value>,
}

impl Sink {
    /// Build a sink from the resolved output config.
    pub fn new(output: &OutputConfig) -> Result<Self> {
        let writer: Box<dyn Write + Send> = match &output.file {
            Some(path) => open_file(path)?,
            None => Box::new(std::io::stdout()),
        };
        Ok(Sink {
            format: output.format,
            topic: output.topic.clone(),
            metadata: output.metadata,
            state: Mutex::new(State {
                writer,
                buffer: Vec::new(),
            }),
        })
    }

    /// Emit a time-series sample.
    pub fn emit_sample(&self, sample: &Sample) -> Result<()> {
        let mut state = self.lock();
        match self.format {
            OutputFormat::Tedge => {
                let obj = tedge_object(sample, self.metadata);
                self.write_line(&mut state, &obj)
            }
            OutputFormat::Ndjson => {
                let obj = sample_object(sample, self.metadata, false);
                self.write_line(&mut state, &obj)
            }
            OutputFormat::Json => {
                state
                    .buffer
                    .push(sample_object(sample, self.metadata, false));
                Ok(())
            }
            OutputFormat::Csv => {
                state
                    .buffer
                    .push(sample_object(sample, self.metadata, true));
                Ok(())
            }
        }
    }

    /// Emit a structural node record (scan task).
    pub fn emit_record(&self, record: &NodeRecord) -> Result<()> {
        let value = serde_json::to_value(record).map_err(|e| ScanError::Export(e.to_string()))?;
        let mut state = self.lock();
        match self.format {
            OutputFormat::Tedge | OutputFormat::Ndjson => self.write_line(&mut state, &value),
            OutputFormat::Json | OutputFormat::Csv => {
                state.buffer.push(value);
                Ok(())
            }
        }
    }

    /// Flush any buffered batch output. Must be called once at the end of a run.
    pub fn finish(&self) -> Result<()> {
        let mut state = self.lock();
        match self.format {
            OutputFormat::Json => {
                let text = serde_json::to_string_pretty(&state.buffer)
                    .map_err(|e| ScanError::Export(e.to_string()))?;
                state.writer.write_all(text.as_bytes())?;
                state.writer.write_all(b"\n")?;
            }
            OutputFormat::Csv => {
                let rows = std::mem::take(&mut state.buffer);
                write_csv(&mut state.writer, &rows)?;
            }
            OutputFormat::Tedge | OutputFormat::Ndjson => {}
        }
        state.writer.flush()?;
        Ok(())
    }

    fn write_line(&self, state: &mut State, value: &Value) -> Result<()> {
        let json = serde_json::to_string(value).map_err(|e| ScanError::Export(e.to_string()))?;
        if let Some(topic) = &self.topic {
            writeln!(state.writer, "[{topic}] {json}")?;
        } else {
            writeln!(state.writer, "{json}")?;
        }
        state.writer.flush()?;
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// thin-edge.io measurement shape: `{ "time": ..., "<name>": <value> }`.
///
/// With [`MetadataMode::Nested`] the descriptive context is attached under a
/// sibling `meta` object, and the data type and units are nested there too
/// (placing extra string fields at the top level would break a strict
/// thin-edge measurement). Use [`MetadataMode::Off`] to keep the output
/// strictly thin-edge compatible.
fn tedge_object(sample: &Sample, metadata: MetadataMode) -> Value {
    let mut map = Map::new();
    if let Some(time) = &sample.time {
        map.insert("time".to_string(), Value::String(time.clone()));
    }
    let key = sample.meta.output_key(&sample.name).to_string();
    map.insert(key, sample.value.clone());
    if metadata == MetadataMode::Nested {
        let mut meta = match sample.meta.descriptive_json() {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        if let Some(dt) = &sample.meta.data_type {
            meta.insert("dataType".to_string(), Value::String(dt.clone()));
        }
        if let Some(units) = &sample.meta.engineering_units {
            meta.insert("units".to_string(), Value::String(units.clone()));
        }
        if !meta.is_empty() {
            map.insert("meta".to_string(), Value::Object(meta));
        }
    }
    Value::Object(map)
}

/// Fixed-schema record used for json/ndjson/csv.
///
/// In [`MetadataMode::Nested`] every top-level key is always present (with
/// `null` where a value is absent) so downstream parsers see a stable schema:
/// `time, key, value, dataType, nodeId, units, status, meta`. When
/// `flatten_meta` is set (csv), the descriptive `meta` sub-keys are spread as
/// top-level columns instead of being nested. [`MetadataMode::Off`] emits a
/// reduced set without type/units/meta.
fn sample_object(sample: &Sample, metadata: MetadataMode, flatten_meta: bool) -> Value {
    let mut map = Map::new();
    let time = sample
        .time
        .clone()
        .map(Value::String)
        .unwrap_or(Value::Null);
    map.insert("time".to_string(), time);
    map.insert(
        "key".to_string(),
        Value::String(sample.meta.output_key(&sample.name).to_string()),
    );
    map.insert("value".to_string(), sample.value.clone());

    if metadata == MetadataMode::Off {
        map.insert("nodeId".to_string(), Value::String(sample.node_id.clone()));
        map.insert("status".to_string(), opt_string(&sample.status));
        return Value::Object(map);
    }

    map.insert("dataType".to_string(), opt_string(&sample.meta.data_type));
    map.insert("nodeId".to_string(), Value::String(sample.node_id.clone()));
    map.insert(
        "units".to_string(),
        opt_string(&sample.meta.engineering_units),
    );
    map.insert("status".to_string(), opt_string(&sample.status));

    let descriptive = sample.meta.descriptive_json();
    if flatten_meta {
        if let Value::Object(meta) = descriptive {
            for (k, v) in meta {
                map.entry(k).or_insert(v);
            }
        }
    } else {
        map.insert("meta".to_string(), descriptive);
    }
    Value::Object(map)
}

/// Render an optional string as a JSON string or explicit `null`.
fn opt_string(value: &Option<String>) -> Value {
    value.clone().map(Value::String).unwrap_or(Value::Null)
}

/// Write buffered JSON objects as CSV, using the union of keys (in first-seen
/// order) as the header so heterogeneous rows still align.
fn write_csv(writer: &mut dyn Write, rows: &[Value]) -> Result<()> {
    let mut columns: Vec<String> = Vec::new();
    for row in rows {
        if let Value::Object(map) = row {
            for key in map.keys() {
                if !columns.iter().any(|c| c == key) {
                    columns.push(key.clone());
                }
            }
        }
    }

    let mut csv_writer = csv::Writer::from_writer(writer);
    csv_writer
        .write_record(&columns)
        .map_err(|e| ScanError::Export(e.to_string()))?;
    for row in rows {
        let record: Vec<String> = columns
            .iter()
            .map(|col| match row.get(col) {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Null) | None => String::new(),
                Some(other) => other.to_string(),
            })
            .collect();
        csv_writer
            .write_record(&record)
            .map_err(|e| ScanError::Export(e.to_string()))?;
    }
    csv_writer
        .flush()
        .map_err(|e| ScanError::Export(e.to_string()))?;
    Ok(())
}

fn open_file(path: &Path) -> Result<Box<dyn Write + Send>> {
    let file = std::fs::File::create(path)?;
    Ok(Box::new(std::io::BufWriter::new(file)))
}

#[cfg(test)]
mod key_tests {
    use super::*;

    #[test]
    fn explicit_key_wins_over_browse_path_and_name() {
        let mut meta = SampleMeta {
            browse_path: Some("Factory/Motor/Temperature".into()),
            ..Default::default()
        };
        assert_eq!(meta.output_key("Temperature"), "Factory/Motor/Temperature");
        meta.key = Some("temperature".into());
        assert_eq!(meta.output_key("Temperature"), "temperature");
        assert_eq!(SampleMeta::default().output_key("name"), "name");
    }
}
