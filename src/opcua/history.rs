//! Historical (raw) data retrieval via the OPC UA HistoryRead service.

use opcua::client::HistoryReadAction;
use opcua::client::Session;
use opcua::types::{
    DateTime, HistoryData, HistoryReadResult, HistoryReadValueId, NodeId, NumericRange,
    QualifiedName, ReadRawModifiedDetails, TimestampsToReturn,
};

use crate::error::{Result, ScanError};
use crate::opcua::value::ValueReading;

/// Parameters for a raw history read.
#[derive(Debug, Clone)]
pub struct HistoryQuery {
    /// Inclusive start time.
    pub start: DateTime,
    /// Exclusive end time.
    pub end: DateTime,
    /// Maximum values returned per node per request (0 = server default).
    pub max_values: u32,
}

/// Read raw historical values for a single node, following continuation points
/// until the server has returned all data in the requested time range.
pub async fn read_raw(
    session: &Session,
    node_id: &NodeId,
    query: &HistoryQuery,
) -> Result<Vec<ValueReading>> {
    let mut readings = Vec::new();
    let mut continuation_point = opcua::types::ByteString::null();

    loop {
        let details = ReadRawModifiedDetails {
            is_read_modified: false,
            start_time: query.start,
            end_time: query.end,
            num_values_per_node: query.max_values,
            return_bounds: false,
        };

        let to_read = vec![HistoryReadValueId {
            node_id: node_id.clone(),
            index_range: NumericRange::None,
            data_encoding: QualifiedName::null(),
            continuation_point: continuation_point.clone(),
        }];

        let results = session
            .history_read(
                HistoryReadAction::ReadRawModifiedDetails(details),
                TimestampsToReturn::Both,
                false,
                &to_read,
            )
            .await
            .map_err(|s| ScanError::service("HistoryRead", s))?;

        let Some(result) = results.into_iter().next() else {
            break;
        };
        let HistoryReadResult {
            status_code,
            continuation_point: next_cp,
            history_data,
        } = result;

        if !status_code.is_good() {
            return Err(ScanError::service("HistoryRead", status_code));
        }

        if let Some(data) = history_data.into_inner_as::<HistoryData>()
            && let Some(values) = data.data_values
        {
            readings.extend(values.iter().map(ValueReading::from_data_value));
        }

        if next_cp.is_null_or_empty() {
            break;
        }
        continuation_point = next_cp;
    }

    Ok(readings)
}

/// Parse an RFC3339 timestamp into an OPC UA [`DateTime`].
pub fn parse_time(s: &str) -> Result<DateTime> {
    DateTime::parse_from_rfc3339(s)
        .map_err(|e| ScanError::InvalidArgument(format!("invalid time '{s}': {e}")))
}
