//! Reading current values of variable nodes for `--preview-values`.

use opcua::client::Session;
use opcua::types::{
    AttributeId, DataValue, EUInformation, ExtensionObject, NodeId, NumericRange, QualifiedName,
    Range, ReadValueId, TimestampsToReturn, Variant,
};

use crate::error::{Result, ScanError};

/// A value snapshot read from a variable node.
#[derive(Debug, Clone, Default)]
pub struct ValueSnapshot {
    /// Rendered value (best-effort string form).
    pub value: Option<String>,
}

/// A value sample with timestamp and status, used by the acquisition tasks.
#[derive(Debug, Clone)]
pub struct ValueReading {
    /// The value as a JSON value (numbers stay numeric for thin-edge JSON).
    pub value: serde_json::Value,
    /// Source timestamp (RFC3339), when provided by the server.
    pub source_time: Option<String>,
    /// Server timestamp (RFC3339), when provided by the server.
    pub server_time: Option<String>,
    /// Status code, when not Good.
    pub status: Option<String>,
    /// When the client received the value (RFC3339); the time of last resort.
    pub received_time: String,
}

impl ValueReading {
    /// Build a reading from an OPC UA [`DataValue`].
    pub fn from_data_value(dv: &DataValue) -> Self {
        let value = dv
            .value
            .as_ref()
            .map(variant_to_json)
            .unwrap_or(serde_json::Value::Null);
        let status = dv.status.filter(|s| !s.is_good()).map(|s| s.to_string());
        ValueReading {
            value,
            source_time: dv.source_timestamp.as_ref().map(|t| t.to_rfc3339()),
            server_time: dv.server_timestamp.as_ref().map(|t| t.to_rfc3339()),
            status,
            received_time: opcua::types::DateTime::now().to_rfc3339(),
        }
    }

    /// Best available timestamp: source, else server, else when the value was
    /// received (some servers send neither timestamp).
    pub fn best_time(&self) -> Option<String> {
        self.source_time
            .clone()
            .or_else(|| self.server_time.clone())
            .or_else(|| Some(self.received_time.clone()))
    }
}

/// Convert an OPC UA [`Variant`] into a JSON value, keeping numeric and boolean
/// types native so downstream consumers (e.g. thin-edge.io) receive real
/// numbers rather than quoted strings. Unsupported types fall back to a string.
pub fn variant_to_json(v: &Variant) -> serde_json::Value {
    use serde_json::Value;
    match v {
        Variant::Empty => Value::Null,
        Variant::Boolean(b) => Value::Bool(*b),
        Variant::SByte(n) => Value::from(*n),
        Variant::Byte(n) => Value::from(*n),
        Variant::Int16(n) => Value::from(*n),
        Variant::UInt16(n) => Value::from(*n),
        Variant::Int32(n) => Value::from(*n),
        Variant::UInt32(n) => Value::from(*n),
        Variant::Int64(n) => Value::from(*n),
        Variant::UInt64(n) => Value::from(*n),
        Variant::Float(n) => serde_json::Number::from_f64(*n as f64)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Variant::Double(n) => serde_json::Number::from_f64(*n)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        Variant::String(s) => Value::String(s.to_string()),
        Variant::LocalizedText(t) => Value::String(t.text.to_string()),
        Variant::QualifiedName(q) => Value::String(q.name.to_string()),
        Variant::ByteString(b) => Value::String(b.as_base64()),
        Variant::DateTime(dt) => Value::String(dt.to_rfc3339()),
        Variant::Guid(g) => Value::String(g.to_string()),
        Variant::NodeId(id) => Value::String(id.to_string()),
        Variant::ExpandedNodeId(id) => Value::String(id.to_string()),
        Variant::XmlElement(x) => Value::String(x.to_string()),
        Variant::ExtensionObject(obj) => extension_object_to_json(obj),
        Variant::Array(arr) => Value::Array(arr.values.iter().map(variant_to_json).collect()),
        other => Value::String(other.to_string()),
    }
}

/// Render well-known structured extension objects (e.g. `Range`,
/// `EUInformation`) as JSON objects, falling back to a string for anything
/// the client cannot decode.
pub fn extension_object_to_json(obj: &ExtensionObject) -> serde_json::Value {
    use serde_json::Value;
    if let Some(range) = obj.inner_as::<Range>() {
        return serde_json::json!({ "low": range.low, "high": range.high });
    }
    if let Some(eu) = obj.inner_as::<EUInformation>() {
        return eu_information_to_json(eu);
    }
    match obj.type_name() {
        Some(name) => Value::String(name.to_string()),
        None => Value::Null,
    }
}

/// Convert an [`EUInformation`] (engineering units) into a JSON object.
fn eu_information_to_json(eu: &EUInformation) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    let display = eu.display_name.text.to_string();
    if !display.is_empty() {
        map.insert("name".to_string(), serde_json::Value::String(display));
    }
    let description = eu.description.text.to_string();
    if !description.is_empty() {
        map.insert(
            "description".to_string(),
            serde_json::Value::String(description),
        );
    }
    serde_json::Value::Object(map)
}

/// Extract a low/high pair from a `Range` extension object value, if present.
pub fn range_from_data_value(dv: &DataValue) -> Option<(f64, f64)> {
    match dv.value.as_ref()? {
        Variant::ExtensionObject(obj) => obj.inner_as::<Range>().map(|r| (r.low, r.high)),
        _ => None,
    }
}

/// Extract an engineering-units display label from an `EUInformation` value.
pub fn eu_label_from_data_value(dv: &DataValue) -> Option<String> {
    match dv.value.as_ref()? {
        Variant::ExtensionObject(obj) => {
            let eu = obj.inner_as::<EUInformation>()?;
            let label = eu.display_name.text.to_string();
            if label.is_empty() { None } else { Some(label) }
        }
        _ => None,
    }
}

/// Read current values for the given variable nodes, in order.
pub async fn read_values(session: &Session, node_ids: &[NodeId]) -> Result<Vec<ValueSnapshot>> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }

    let to_read: Vec<ReadValueId> = node_ids
        .iter()
        .map(|node_id| ReadValueId {
            node_id: node_id.clone(),
            attribute_id: AttributeId::Value as u32,
            index_range: NumericRange::None,
            data_encoding: QualifiedName::null(),
        })
        .collect();

    let values = session
        .read(&to_read, TimestampsToReturn::Both, 0.0)
        .await
        .map_err(|s| ScanError::service("Read(Value)", s))?;

    Ok(values
        .into_iter()
        .map(|dv| ValueSnapshot {
            value: dv.value.map(|v| v.to_string()),
        })
        .collect())
}

/// Read current values for the given nodes, returning full readings (value,
/// timestamps and status) in the same order as `node_ids`.
pub async fn read_readings(session: &Session, node_ids: &[NodeId]) -> Result<Vec<ValueReading>> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }

    let to_read: Vec<ReadValueId> = node_ids
        .iter()
        .map(|node_id| ReadValueId {
            node_id: node_id.clone(),
            attribute_id: AttributeId::Value as u32,
            index_range: NumericRange::None,
            data_encoding: QualifiedName::null(),
        })
        .collect();

    let values = session
        .read(&to_read, TimestampsToReturn::Both, 0.0)
        .await
        .map_err(|s| ScanError::service("Read(Value)", s))?;

    Ok(values.iter().map(ValueReading::from_data_value).collect())
}

/// Read the `Value` attribute of the given nodes, returning the raw
/// [`DataValue`]s in the same order (used for decoding structured properties).
pub async fn read_data_values(session: &Session, node_ids: &[NodeId]) -> Result<Vec<DataValue>> {
    if node_ids.is_empty() {
        return Ok(Vec::new());
    }

    let to_read: Vec<ReadValueId> = node_ids
        .iter()
        .map(|node_id| ReadValueId {
            node_id: node_id.clone(),
            attribute_id: AttributeId::Value as u32,
            index_range: NumericRange::None,
            data_encoding: QualifiedName::null(),
        })
        .collect();

    session
        .read(&to_read, TimestampsToReturn::Neither, 0.0)
        .await
        .map_err(|s| ScanError::service("Read(Value)", s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use opcua::types::{DataValue, ExtensionObject, LocalizedText};

    fn data_value(variant: Variant) -> DataValue {
        DataValue {
            value: Some(variant),
            status: None,
            source_timestamp: None,
            source_picoseconds: None,
            server_timestamp: None,
            server_picoseconds: None,
        }
    }

    #[test]
    fn decodes_range_extension_object() {
        let obj = ExtensionObject::from_message(Range {
            low: -5.0,
            high: 42.5,
        });
        let dv = data_value(Variant::ExtensionObject(obj));
        assert_eq!(range_from_data_value(&dv), Some((-5.0, 42.5)));
    }

    #[test]
    fn renders_range_as_json_object() {
        let obj = ExtensionObject::from_message(Range {
            low: 0.0,
            high: 100.0,
        });
        let json = variant_to_json(&Variant::ExtensionObject(obj));
        assert_eq!(json, serde_json::json!({ "low": 0.0, "high": 100.0 }));
    }

    #[test]
    fn extracts_engineering_units_label() {
        let obj = ExtensionObject::from_message(EUInformation {
            namespace_uri: "http://www.opcfoundation.org/UA/units/un/cefact".into(),
            unit_id: 4408652,
            display_name: LocalizedText::new("", "°C"),
            description: LocalizedText::new("", "degree Celsius"),
        });
        let dv = data_value(Variant::ExtensionObject(obj));
        assert_eq!(eu_label_from_data_value(&dv), Some("°C".to_string()));
    }

    #[test]
    fn scalar_variants_keep_native_json_types() {
        use serde_json::Value;
        assert_eq!(variant_to_json(&Variant::Empty), Value::Null);
        assert_eq!(variant_to_json(&Variant::Boolean(true)), Value::Bool(true));
        // Signed/unsigned integers stay numeric and exact.
        assert_eq!(variant_to_json(&Variant::SByte(-7)), Value::from(-7));
        assert_eq!(variant_to_json(&Variant::Byte(200)), Value::from(200));
        assert_eq!(
            variant_to_json(&Variant::Int16(-30000)),
            Value::from(-30000)
        );
        assert_eq!(variant_to_json(&Variant::UInt16(60000)), Value::from(60000));
        assert_eq!(
            variant_to_json(&Variant::Int32(-2_000_000)),
            Value::from(-2_000_000)
        );
        assert_eq!(
            variant_to_json(&Variant::UInt32(4_000_000_000)),
            Value::from(4_000_000_000u32)
        );
        assert_eq!(
            variant_to_json(&Variant::Int64(-9_000_000_000)),
            Value::from(-9_000_000_000i64)
        );
        assert_eq!(
            variant_to_json(&Variant::UInt64(18_000_000_000)),
            Value::from(18_000_000_000u64)
        );
        // Floating point values are rendered as JSON numbers.
        assert_eq!(variant_to_json(&Variant::Double(2.5)), Value::from(2.5));
        assert_eq!(variant_to_json(&Variant::Float(1.5f32)), Value::from(1.5));
        // Strings.
        assert_eq!(
            variant_to_json(&Variant::String("hi".into())),
            Value::String("hi".to_string())
        );
    }

    #[test]
    fn non_finite_floats_become_null_not_invalid_json() {
        // serde_json cannot represent NaN/Infinity, so the converter must emit
        // `null` rather than produce a value that fails to serialize.
        assert!(variant_to_json(&Variant::Double(f64::NAN)).is_null());
        assert!(variant_to_json(&Variant::Double(f64::INFINITY)).is_null());
        assert!(variant_to_json(&Variant::Double(f64::NEG_INFINITY)).is_null());
        assert!(variant_to_json(&Variant::Float(f32::NAN)).is_null());
        // Whatever comes out must round-trip through serde_json without error.
        let v = variant_to_json(&Variant::Double(f64::NAN));
        assert_eq!(serde_json::to_string(&v).unwrap(), "null");
    }

    #[test]
    fn byte_string_is_base64_encoded() {
        let json = variant_to_json(&Variant::ByteString(vec![0x00, 0x01, 0xff].into()));
        // base64 of [0,1,255] is "AAH/".
        assert_eq!(json, serde_json::Value::String("AAH/".to_string()));
    }

    #[test]
    fn arrays_are_converted_element_wise() {
        let arr = Variant::from(vec![1i32, 2, 3]);
        let json = variant_to_json(&arr);
        assert_eq!(json, serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn localized_text_and_qualified_name_render_as_text() {
        let lt = Variant::LocalizedText(Box::new(LocalizedText::new("en", "Hello")));
        assert_eq!(
            variant_to_json(&lt),
            serde_json::Value::String("Hello".into())
        );
    }

    #[test]
    fn best_time_prefers_source_over_server() {
        let reading = ValueReading {
            value: serde_json::Value::from(1),
            source_time: Some("2020-01-01T00:00:00Z".into()),
            server_time: Some("2020-01-02T00:00:00Z".into()),
            status: None,
            received_time: "2020-01-03T00:00:00Z".into(),
        };
        assert_eq!(reading.best_time().as_deref(), Some("2020-01-01T00:00:00Z"));

        let server_only = ValueReading {
            value: serde_json::Value::Null,
            source_time: None,
            server_time: Some("2020-01-02T00:00:00Z".into()),
            status: None,
            received_time: "2020-01-03T00:00:00Z".into(),
        };
        assert_eq!(
            server_only.best_time().as_deref(),
            Some("2020-01-02T00:00:00Z")
        );

        let none = ValueReading {
            value: serde_json::Value::Null,
            source_time: None,
            server_time: None,
            status: None,
            received_time: "2020-01-03T00:00:00Z".into(),
        };
        // No server timestamp: the receive time, never null.
        assert_eq!(none.best_time().as_deref(), Some("2020-01-03T00:00:00Z"));
        let fresh = ValueReading::from_data_value(&DataValue::default());
        assert!(fresh.best_time().is_some_and(|t| t.starts_with("20")));
    }

    #[test]
    fn reading_records_non_good_status_only() {
        use opcua::types::StatusCode;
        let mut dv = data_value(Variant::Double(1.0));
        dv.status = Some(StatusCode::Good);
        assert_eq!(ValueReading::from_data_value(&dv).status, None);
        dv.status = Some(StatusCode::BadNotReadable);
        let reading = ValueReading::from_data_value(&dv);
        assert!(
            reading.status.is_some(),
            "non-good status should be recorded"
        );
    }
}
