//! The exportable node record and its field set.

use serde::Serialize;

/// A fully described, discovered node. This is the unit of export and display.
#[derive(Debug, Clone, Serialize)]
pub struct NodeRecord {
    /// Full browse path using namespace URIs (not numeric indexes).
    pub browse_path: String,
    /// Display name.
    pub display_name: String,
    /// Browse name (namespace-qualified text).
    pub browse_name: String,
    /// NodeId in string form (e.g. `ns=2;s=Foo`).
    pub node_id: String,
    /// Namespace URI of the node.
    pub namespace_uri: String,
    /// Node class (Object, Variable, ...).
    pub node_class: String,
    /// Data type name or NodeId (Variables only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub datatype: Option<String>,
    /// Value rank (Variables only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_rank: Option<i32>,
    /// AccessLevel bitfield (Variables only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_level: Option<u8>,
    /// UserAccessLevel bitfield (Variables only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_access_level: Option<u8>,
    /// Historizing flag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub historizing: Option<bool>,
    /// Whether the node looks subscribable.
    pub subscribable: bool,
    /// Subscription validation status (only when `--validate-subscription`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation_status: Option<String>,
    /// Current value (only when `--preview-values`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_value: Option<String>,
    /// Engineering units, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engineering_units: Option<String>,
    /// Description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The node a Method was browsed from (the object it is called on).
    /// Only set for Methods.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_node_id: Option<String>,
}
