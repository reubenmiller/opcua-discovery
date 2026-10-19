//! Attribute-based predicates: node class, data type, access level, namespace,
//! and the subscribable heuristic.

use std::collections::HashSet;
use std::str::FromStr;

use opcua::types::{NodeClass, NodeId};

use crate::cli::FilterArgs;
use crate::error::{Result, ScanError};
use crate::opcua::attributes::NodeAttributes;
use crate::opcua::namespaces::NamespaceTable;
use crate::opcua::node_class_of;

/// Resolves and matches OPC UA data types by builtin name, NodeId, or namespace.
#[derive(Debug, Default)]
pub struct DataTypeMatcher {
    /// Accepted builtin names, lower-cased (e.g. "double").
    names: HashSet<String>,
    /// Accepted explicit data type NodeIds.
    node_ids: HashSet<NodeId>,
    /// Accepted namespace URIs (matches any data type in that namespace).
    ns_uris: HashSet<String>,
    empty: bool,
}

impl DataTypeMatcher {
    /// Build from `--datatype` values and the namespace table.
    pub fn compile(values: &[String], namespaces: &NamespaceTable) -> Result<Self> {
        let mut m = DataTypeMatcher {
            empty: values.is_empty(),
            ..Default::default()
        };
        for v in values {
            if let Ok(node_id) = NodeId::from_str(v) {
                m.node_ids.insert(node_id);
            } else if namespaces.index(v).is_some() || v.contains(':') {
                // Looks like a namespace URI.
                m.ns_uris.insert(v.clone());
            } else {
                m.names.insert(v.to_ascii_lowercase());
            }
        }
        Ok(m)
    }

    /// Whether the matcher accepts everything (no datatype filter set).
    pub fn is_empty(&self) -> bool {
        self.empty
    }

    /// Whether a node's data type matches.
    pub fn matches(&self, data_type: Option<&NodeId>, namespaces: &NamespaceTable) -> bool {
        if self.empty {
            return true;
        }
        let Some(dt) = data_type else {
            return false;
        };
        if self.node_ids.contains(dt) {
            return true;
        }
        if let Some(uri) = namespaces.uri(dt.namespace)
            && self.ns_uris.contains(uri)
        {
            return true;
        }
        if let Some(name) = builtin_datatype_name(dt)
            && self.names.contains(&name.to_ascii_lowercase())
        {
            return true;
        }
        false
    }
}

/// Compiled, attribute-level predicate combining all metadata filters.
#[derive(Debug, Default)]
pub struct Predicate {
    node_classes: HashSet<i32>,
    has_node_class_filter: bool,
    datatype: DataTypeMatcher,
    readable: bool,
    writable: bool,
    historizing: bool,
    subscribable: bool,
    /// Namespace indexes that nodes must belong to, if set.
    ns_indexes: Option<HashSet<u16>>,
}

impl Predicate {
    /// Compile a predicate from CLI filter arguments and the namespace table.
    pub fn compile(filters: &FilterArgs, namespaces: &NamespaceTable) -> Result<Self> {
        let node_classes: HashSet<i32> = filters
            .node_class
            .iter()
            .map(|c| node_class_discriminant(node_class_of(*c)))
            .collect();

        let datatype = DataTypeMatcher::compile(&filters.datatype, namespaces)?;

        // Resolve namespace filters (URI / index) into a set of indexes.
        let mut ns_indexes: HashSet<u16> = HashSet::new();
        for uri in filters.include_ns.iter().chain(filters.ns_uri.iter()) {
            match namespaces.index(uri) {
                Some(idx) => {
                    ns_indexes.insert(idx);
                }
                None => {
                    return Err(ScanError::InvalidArgument(format!(
                        "namespace URI '{uri}' is not present on the server"
                    )));
                }
            }
        }
        if let Some(idx) = filters.ns_index {
            ns_indexes.insert(idx);
        }

        Ok(Self {
            has_node_class_filter: !node_classes.is_empty(),
            node_classes,
            datatype,
            readable: filters.readable,
            writable: filters.writable,
            historizing: filters.historizing,
            subscribable: filters.subscribable,
            ns_indexes: if ns_indexes.is_empty() {
                None
            } else {
                Some(ns_indexes)
            },
        })
    }

    /// Whether a node passes all attribute-level predicates.
    pub fn matches(
        &self,
        node_id: &NodeId,
        attrs: &NodeAttributes,
        namespaces: &NamespaceTable,
    ) -> bool {
        if let Some(allowed) = &self.ns_indexes
            && !allowed.contains(&node_id.namespace)
        {
            return false;
        }

        if self.has_node_class_filter {
            match attrs.node_class {
                Some(class) if self.node_classes.contains(&node_class_discriminant(class)) => {}
                _ => return false,
            }
        }

        if !self.datatype.is_empty() {
            // Datatype filtering only applies to nodes that have a data type.
            if !self.datatype.matches(attrs.data_type.as_ref(), namespaces) {
                return false;
            }
        }

        if self.readable && !attrs.is_readable() {
            return false;
        }
        if self.writable && !attrs.is_writable() {
            return false;
        }
        if self.historizing && !attrs.is_historizing() {
            return false;
        }
        if self.subscribable && !is_subscribable(attrs) {
            return false;
        }

        true
    }
}

/// The subscribable heuristic: a Variable whose current value is readable.
pub fn is_subscribable(attrs: &NodeAttributes) -> bool {
    matches!(attrs.node_class, Some(NodeClass::Variable)) && attrs.is_readable()
}

/// Stable discriminant for a node class (matches the OPC UA NodeClass bit value).
fn node_class_discriminant(class: NodeClass) -> i32 {
    match class {
        NodeClass::Unspecified => 0,
        NodeClass::Object => 1,
        NodeClass::Variable => 2,
        NodeClass::Method => 4,
        NodeClass::ObjectType => 8,
        NodeClass::VariableType => 16,
        NodeClass::ReferenceType => 32,
        NodeClass::DataType => 64,
        NodeClass::View => 128,
    }
}

/// Map a known builtin (namespace 0) data type NodeId to its name.
pub fn builtin_datatype_name(node_id: &NodeId) -> Option<&'static str> {
    if node_id.namespace != 0 {
        return None;
    }
    let id = match &node_id.identifier {
        opcua::types::Identifier::Numeric(n) => *n,
        _ => return None,
    };
    Some(match id {
        1 => "Boolean",
        2 => "SByte",
        3 => "Byte",
        4 => "Int16",
        5 => "UInt16",
        6 => "Int32",
        7 => "UInt32",
        8 => "Int64",
        9 => "UInt64",
        10 => "Float",
        11 => "Double",
        12 => "String",
        13 => "DateTime",
        14 => "Guid",
        15 => "ByteString",
        16 => "XmlElement",
        17 => "NodeId",
        18 => "ExpandedNodeId",
        19 => "StatusCode",
        20 => "QualifiedName",
        21 => "LocalizedText",
        22 => "Structure",
        23 => "DataValue",
        24 => "BaseDataType",
        25 => "DiagnosticInfo",
        26 => "Number",
        27 => "Integer",
        28 => "UInteger",
        29 => "Enumeration",
        30 => "Image",
        // Well-known builtin subtypes mapped to their base primitive, so
        // their values are still typed (servers commonly use these).
        290 => "Double",   // Duration
        291 => "String",   // NumericRange
        294 => "DateTime", // UtcTime
        295 => "String",   // LocaleId
        _ => return None,
    })
}

/// Map an OPC UA builtin data-type name (from [`builtin_datatype_name`]) to a
/// normalized, cross-platform type name (e.g. `Double` -> `float64`).
///
/// Returns `None` for anything that is not a concrete scalar primitive
/// (abstract base types, structures, and custom types), so callers can render
/// an explicit `null` rather than a misleading type.
pub fn canonical_data_type(builtin_name: &str) -> Option<&'static str> {
    Some(match builtin_name {
        "Boolean" => "bool",
        "SByte" => "int8",
        "Byte" => "uint8",
        "Int16" => "int16",
        "UInt16" => "uint16",
        "Int32" => "int32",
        "UInt32" => "uint32",
        "Int64" => "int64",
        "UInt64" => "uint64",
        "Float" => "float32",
        "Double" => "float64",
        "String" => "string",
        "DateTime" => "datetime",
        "Guid" => "guid",
        "ByteString" => "bytes",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use opcua::types::Identifier;

    #[test]
    fn builtin_names() {
        let double = NodeId::new(0, 11u32);
        assert_eq!(builtin_datatype_name(&double), Some("Double"));
        let custom = NodeId::new(2, 5000u32);
        assert_eq!(builtin_datatype_name(&custom), None);
    }

    #[test]
    fn canonical_data_type_names() {
        assert_eq!(canonical_data_type("Double"), Some("float64"));
        assert_eq!(canonical_data_type("Float"), Some("float32"));
        assert_eq!(canonical_data_type("Int32"), Some("int32"));
        assert_eq!(canonical_data_type("Boolean"), Some("bool"));
        assert_eq!(canonical_data_type("ByteString"), Some("bytes"));
        assert_eq!(canonical_data_type("Structure"), None);
        assert_eq!(canonical_data_type("Number"), None);
    }

    #[test]
    fn datatype_matcher_by_name_and_nodeid() {
        let ns = NamespaceTable::from_uris(vec!["http://opcfoundation.org/UA/".into()]);
        let m = DataTypeMatcher::compile(&["Double".into(), "i=10".into()], &ns).unwrap();
        let double = NodeId::new(0, 11u32);
        let float = NodeId::new(0, 10u32);
        let boolean = NodeId::new(0, 1u32);
        assert!(m.matches(Some(&double), &ns));
        assert!(m.matches(Some(&float), &ns));
        assert!(!m.matches(Some(&boolean), &ns));
    }

    #[test]
    fn subscribable_requires_readable_variable() {
        let mut attrs = NodeAttributes {
            node_class: Some(NodeClass::Variable),
            access_level: Some(0),
            ..Default::default()
        };
        assert!(!is_subscribable(&attrs));
        attrs.access_level = Some(1);
        assert!(is_subscribable(&attrs));
        attrs.node_class = Some(NodeClass::Object);
        assert!(!is_subscribable(&attrs));
        let _ = Identifier::Numeric(1);
    }

    #[test]
    fn builtin_datatype_table_covers_all_scalar_primitives() {
        // Every concrete scalar primitive must map to a canonical name so the
        // emitted `dataType` field is stable across servers.
        for (id, name, canonical) in [
            (1u32, "Boolean", Some("bool")),
            (2, "SByte", Some("int8")),
            (3, "Byte", Some("uint8")),
            (4, "Int16", Some("int16")),
            (5, "UInt16", Some("uint16")),
            (6, "Int32", Some("int32")),
            (7, "UInt32", Some("uint32")),
            (8, "Int64", Some("int64")),
            (9, "UInt64", Some("uint64")),
            (10, "Float", Some("float32")),
            (11, "Double", Some("float64")),
            (12, "String", Some("string")),
            (13, "DateTime", Some("datetime")),
            (14, "Guid", Some("guid")),
            (15, "ByteString", Some("bytes")),
        ] {
            let node = NodeId::new(0, id);
            assert_eq!(builtin_datatype_name(&node), Some(name), "id {id}");
            assert_eq!(canonical_data_type(name), canonical, "name {name}");
        }
    }

    #[test]
    fn builtin_subtypes_fold_to_their_primitive() {
        // Duration (290) and UtcTime (294) are common server data types that
        // must still produce typed values.
        assert_eq!(
            builtin_datatype_name(&NodeId::new(0, 290u32)),
            Some("Double")
        );
        assert_eq!(
            builtin_datatype_name(&NodeId::new(0, 294u32)),
            Some("DateTime")
        );
        assert_eq!(
            builtin_datatype_name(&NodeId::new(0, 295u32)),
            Some("String")
        );
    }

    #[test]
    fn builtin_name_is_namespace_zero_numeric_only() {
        // Non-zero namespace -> not a builtin.
        assert_eq!(builtin_datatype_name(&NodeId::new(2, 11u32)), None);
        // String identifiers in ns 0 are not builtin numeric ids.
        assert_eq!(builtin_datatype_name(&NodeId::new(0, "Double")), None);
    }

    #[test]
    fn datatype_matcher_by_namespace_uri() {
        let ns = NamespaceTable::from_uris(vec![
            "http://opcfoundation.org/UA/".into(),
            "urn:vendor:types".into(),
        ]);
        let m = DataTypeMatcher::compile(&["urn:vendor:types".into()], &ns).unwrap();
        let vendor_type = NodeId::new(1, 5000u32);
        let base_type = NodeId::new(0, 11u32);
        assert!(m.matches(Some(&vendor_type), &ns));
        assert!(!m.matches(Some(&base_type), &ns));
        // A node with no data type never matches a non-empty matcher.
        assert!(!m.matches(None, &ns));
    }

    #[test]
    fn empty_datatype_matcher_accepts_everything() {
        let ns = NamespaceTable::from_uris(vec!["http://opcfoundation.org/UA/".into()]);
        let m = DataTypeMatcher::compile(&[], &ns).unwrap();
        assert!(m.is_empty());
        assert!(m.matches(None, &ns));
        assert!(m.matches(Some(&NodeId::new(0, 1u32)), &ns));
    }

    fn attrs(class: NodeClass, access: u8) -> NodeAttributes {
        NodeAttributes {
            node_class: Some(class),
            access_level: Some(access),
            ..Default::default()
        }
    }

    #[test]
    fn predicate_node_class_filter_gates_emission() {
        let ns = NamespaceTable::from_uris(vec!["http://opcfoundation.org/UA/".into()]);
        let filters = FilterArgs {
            node_class: vec![crate::cli::NodeClassArg::Variable],
            ..Default::default()
        };
        let p = Predicate::compile(&filters, &ns).unwrap();
        let id = NodeId::new(0, 1u32);
        assert!(p.matches(&id, &attrs(NodeClass::Variable, 1), &ns));
        assert!(!p.matches(&id, &attrs(NodeClass::Object, 1), &ns));
    }

    #[test]
    fn predicate_access_and_historizing_flags() {
        let ns = NamespaceTable::from_uris(vec!["http://opcfoundation.org/UA/".into()]);
        let id = NodeId::new(0, 1u32);

        let readable_only = FilterArgs {
            readable: true,
            ..Default::default()
        };
        let p = Predicate::compile(&readable_only, &ns).unwrap();
        assert!(p.matches(&id, &attrs(NodeClass::Variable, 1), &ns));
        assert!(!p.matches(&id, &attrs(NodeClass::Variable, 0), &ns));

        let historizing = FilterArgs {
            historizing: true,
            ..Default::default()
        };
        let p = Predicate::compile(&historizing, &ns).unwrap();
        let mut hist = attrs(NodeClass::Variable, 1);
        assert!(!p.matches(&id, &hist, &ns));
        hist.historizing = Some(true);
        assert!(p.matches(&id, &hist, &ns));
    }

    #[test]
    fn predicate_namespace_filter_rejects_unknown_uri() {
        let ns = NamespaceTable::from_uris(vec!["http://opcfoundation.org/UA/".into()]);
        let filters = FilterArgs {
            ns_uri: Some("urn:not:present".into()),
            ..Default::default()
        };
        let err = Predicate::compile(&filters, &ns).unwrap_err();
        assert!(
            err.to_string().contains("not present"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn predicate_namespace_index_filter_restricts_by_node_namespace() {
        let ns = NamespaceTable::from_uris(vec![
            "http://opcfoundation.org/UA/".into(),
            "urn:vendor".into(),
        ]);
        let filters = FilterArgs {
            ns_index: Some(1),
            ..Default::default()
        };
        let p = Predicate::compile(&filters, &ns).unwrap();
        assert!(p.matches(&NodeId::new(1, 10u32), &attrs(NodeClass::Variable, 1), &ns));
        assert!(!p.matches(&NodeId::new(0, 10u32), &attrs(NodeClass::Variable, 1), &ns));
    }
}
