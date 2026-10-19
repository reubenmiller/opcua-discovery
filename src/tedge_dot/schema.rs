//! Discover what the point library needs beyond the scan records: the shape of
//! every custom datatype a variable uses (structure fields, enumeration values,
//! the built-in type a derived type stands for) and the input arguments of
//! every method.
//!
//! The result is a plain [`Schema`] so the library writer stays pure. Types are
//! resolved in rounds: one Read of `DataTypeDefinition` for every pending type,
//! one inverse `HasSubtype` Browse for those without a definition, and the
//! field types and supertypes found go into the next round.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use opcua::client::Session;
use opcua::types::{
    Argument, AttributeId, BrowseDescription, BrowseDirection, BrowseResultMask, EnumDefinition,
    NodeId, NumericRange, QualifiedName, ReadValueId, ReferenceTypeId, StructureDefinition,
    StructureType, TimestampsToReturn, Variant,
};
use tracing::debug;

use crate::export::NodeRecord;
use crate::filters::predicate::builtin_datatype_name;

/// Resolution rounds before giving up (deeper type hierarchies are unheard of).
const MAX_ROUNDS: usize = 16;

/// What a datatype is, as far as a point library is concerned.
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    /// Maps to a tedge-dot datatype; `writable` when tedge-dot can write it.
    Scalar {
        datatype: &'static str,
        writable: bool,
    },
    /// An enumeration (`int32`), with its values and names when defined.
    Enum { cases: Vec<(i64, String)> },
    /// A structure whose fields can be selected with `field`.
    Structure { fields: Vec<Field> },
    /// Cannot become a point; the reason says why.
    Unsupported(String),
}

/// One field of a structure.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub name: String,
    /// The field's datatype key (see [`type_key`]).
    pub datatype: String,
    pub value_rank: i32,
}

/// A method's input datatypes, or why it cannot be a point.
pub type MethodInputs = Result<Vec<&'static str>, String>;

/// Everything discovered for one library write.
#[derive(Debug, Default, Clone)]
pub struct Schema {
    /// Shapes of the non-built-in datatypes, by [`type_key`].
    pub types: HashMap<String, Shape>,
    /// Method node id (string) → its inputs.
    pub methods: HashMap<String, MethodInputs>,
}

/// The key a datatype is known by: the built-in name for namespace-0 built-ins
/// (as in `NodeRecord::datatype`), else the node id string.
pub fn type_key(id: &NodeId) -> String {
    builtin_datatype_name(id)
        .map(str::to_string)
        .unwrap_or_else(|| id.to_string())
}

/// The tedge-dot datatype of a built-in or well-known datatype key, and whether
/// tedge-dot can write it (OPC UA connector spec §3.7).
pub fn builtin_shape(key: &str) -> Option<Shape> {
    let scalar = |datatype, writable| Shape::Scalar { datatype, writable };
    Some(match key {
        "Boolean" => scalar("bool", true),
        "SByte" => scalar("int8", true),
        "Byte" => scalar("uint8", true),
        "Int16" => scalar("int16", true),
        "UInt16" => scalar("uint16", true),
        "Int32" => scalar("int32", true),
        "UInt32" => scalar("uint32", true),
        "Int64" => scalar("int64", true),
        "UInt64" => scalar("uint64", true),
        "Float" => scalar("float32", true),
        // Duration (i=290) is a Double.
        "Double" | "i=290" => scalar("float64", true),
        "String" => scalar("string", true),
        // UtcTime (i=294) is a DateTime.
        "DateTime" | "i=294" | "Guid" | "LocalizedText" | "NodeId" | "ExpandedNodeId"
        | "QualifiedName" | "StatusCode" => scalar("string", false),
        "ByteString" => scalar("bytes", false),
        // Enumeration itself: the values are unknown.
        "i=29" => Shape::Enum { cases: Vec::new() },
        "Structure" | "BaseDataType" | "i=26" | "i=27" | "i=28" | "XmlElement" | "DataValue"
        | "Variant" | "DiagnosticInfo" => Shape::Unsupported(format!(
            "datatype {} is abstract or not supported",
            display(key)
        )),
        _ => return None,
    })
}

/// A readable name for the abstract numeric types.
fn display(key: &str) -> &str {
    match key {
        "i=26" => "Number",
        "i=27" => "Integer",
        "i=28" => "UInteger",
        other => other,
    }
}

/// Discover the shapes of the datatypes `records` use and the inputs of the
/// methods among them. Failures degrade to `Unsupported` per type or method;
/// discovery itself never fails the write.
pub async fn discover(session: &Session, records: &[NodeRecord]) -> Schema {
    let mut schema = Schema::default();
    let types: Vec<String> = records
        .iter()
        .filter(|r| r.node_class == "Variable")
        .filter_map(|r| r.datatype.clone())
        .filter(|key| builtin_shape(key).is_none())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    resolve_types(session, types, &mut schema).await;

    let methods: Vec<NodeId> = records
        .iter()
        .filter(|r| r.node_class == "Method")
        .filter_map(|r| NodeId::from_str(&r.node_id).ok())
        .collect();
    if !methods.is_empty() {
        let found = discover_methods(session, &methods, &mut schema).await;
        schema.methods.extend(found);
    }
    schema
}

/// Resolve `keys` (and every type they lead to) into `schema.types`.
async fn resolve_types(session: &Session, keys: Vec<String>, schema: &mut Schema) {
    // Derived types without a definition point at their supertype until it is resolved.
    let mut aliases: HashMap<String, String> = HashMap::new();
    let mut pending: Vec<NodeId> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut enqueue = |key: &str, pending: &mut Vec<NodeId>, schema: &mut Schema| {
        if builtin_shape(key).is_some() || !seen.insert(key.to_string()) {
            return;
        }
        match NodeId::from_str(key) {
            Ok(id) => pending.push(id),
            Err(_) => {
                schema.types.insert(
                    key.to_string(),
                    Shape::Unsupported(format!("datatype {key} is not supported")),
                );
            }
        }
    };
    for key in &keys {
        enqueue(key, &mut pending, schema);
    }

    for _ in 0..MAX_ROUNDS {
        if pending.is_empty() {
            break;
        }
        let batch = std::mem::take(&mut pending);
        let definitions = read_definitions(session, &batch).await;
        let mut undefined: Vec<NodeId> = Vec::new();
        for (id, definition) in batch.iter().zip(definitions) {
            let key = type_key(id);
            match definition {
                Some(Definition::Structure(def)) => {
                    let shape = structure_shape(&def);
                    if let Shape::Structure { fields } = &shape {
                        for f in fields {
                            enqueue(&f.datatype, &mut pending, schema);
                        }
                    }
                    schema.types.insert(key, shape);
                }
                Some(Definition::Enum(def)) => {
                    let cases = def
                        .fields
                        .unwrap_or_default()
                        .into_iter()
                        .map(|f| {
                            let name = f.name.as_ref().to_string();
                            let name = if name.is_empty() {
                                f.display_name.text.as_ref().to_string()
                            } else {
                                name
                            };
                            (f.value, name)
                        })
                        .collect();
                    schema.types.insert(key, Shape::Enum { cases });
                }
                None => undefined.push(id.clone()),
            }
        }
        let supertypes = browse_supertypes(session, &undefined).await;
        for (id, supertype) in undefined.iter().zip(supertypes) {
            let key = type_key(id);
            match supertype {
                Some(parent) => {
                    let parent_key = type_key(&parent);
                    enqueue(&parent_key, &mut pending, schema);
                    aliases.insert(key, parent_key);
                }
                None => {
                    schema.types.insert(
                        key.clone(),
                        Shape::Unsupported(format!(
                            "datatype {key} has no definition or supertype"
                        )),
                    );
                }
            }
        }
    }

    // Follow the aliases to a built-in or a resolved type.
    for (key, _) in aliases.clone() {
        let mut current = key.clone();
        let mut hops = 0;
        let shape = loop {
            if let Some(shape) =
                builtin_shape(&current).or_else(|| schema.types.get(&current).cloned())
            {
                break shape;
            }
            match aliases.get(&current) {
                Some(next) if hops < MAX_ROUNDS => {
                    current = next.clone();
                    hops += 1;
                }
                _ => break Shape::Unsupported(format!("datatype {key} could not be resolved")),
            }
        };
        schema.types.insert(key, shape);
    }
    // Anything still pending after the round limit.
    for id in pending {
        schema
            .types
            .entry(type_key(&id))
            .or_insert_with(|| Shape::Unsupported(format!("datatype {id} could not be resolved")));
    }
}

/// A `DataTypeDefinition` attribute value.
enum Definition {
    Structure(StructureDefinition),
    Enum(EnumDefinition),
}

/// Read `DataTypeDefinition` for each type; `None` where the server has none
/// (or does not know the attribute, as OPC UA 1.03 servers).
async fn read_definitions(session: &Session, ids: &[NodeId]) -> Vec<Option<Definition>> {
    let to_read: Vec<ReadValueId> = ids
        .iter()
        .map(|id| ReadValueId {
            node_id: id.clone(),
            attribute_id: AttributeId::DataTypeDefinition as u32,
            index_range: NumericRange::None,
            data_encoding: QualifiedName::null(),
        })
        .collect();
    let values = match session
        .read(&to_read, TimestampsToReturn::Neither, 0.0)
        .await
    {
        Ok(values) => values,
        Err(status) => {
            debug!(%status, "reading DataTypeDefinition failed");
            return ids.iter().map(|_| None).collect();
        }
    };
    let mut out: Vec<Option<Definition>> = values
        .into_iter()
        .map(|dv| match dv.value {
            Some(Variant::ExtensionObject(obj)) => {
                if let Some(def) = obj.inner_as::<StructureDefinition>() {
                    Some(Definition::Structure(def.clone()))
                } else {
                    obj.inner_as::<EnumDefinition>()
                        .map(|def| Definition::Enum(def.clone()))
                }
            }
            _ => None,
        })
        .collect();
    out.resize_with(ids.len(), || None);
    out
}

fn structure_shape(def: &StructureDefinition) -> Shape {
    match def.structure_type {
        StructureType::Structure
        | StructureType::StructureWithOptionalFields
        | StructureType::Union => Shape::Structure {
            fields: def
                .fields
                .clone()
                .unwrap_or_default()
                .into_iter()
                .map(|f| Field {
                    name: f.name.as_ref().to_string(),
                    datatype: type_key(&f.data_type),
                    value_rank: f.value_rank,
                })
                .collect(),
        },
        other => Shape::Unsupported(format!("structure kind {other:?} is not supported")),
    }
}

/// The supertype of each type (inverse `HasSubtype`), aligned to `ids`.
async fn browse_supertypes(session: &Session, ids: &[NodeId]) -> Vec<Option<NodeId>> {
    if ids.is_empty() {
        return Vec::new();
    }
    let descriptions: Vec<BrowseDescription> = ids
        .iter()
        .map(|id| BrowseDescription {
            node_id: id.clone(),
            browse_direction: BrowseDirection::Inverse,
            reference_type_id: ReferenceTypeId::HasSubtype.into(),
            include_subtypes: false,
            node_class_mask: 0,
            result_mask: BrowseResultMask::All as u32,
        })
        .collect();
    let mut out: Vec<Option<NodeId>> = match session.browse(&descriptions, 0, None).await {
        Ok(results) => results
            .into_iter()
            .map(|r| {
                r.references
                    .unwrap_or_default()
                    .into_iter()
                    .next()
                    .map(|reference| reference.node_id.node_id)
            })
            .collect(),
        Err(status) => {
            debug!(%status, "browsing supertypes failed");
            Vec::new()
        }
    };
    out.resize(ids.len(), None);
    out
}

/// Executable flags and `InputArguments` of each method.
async fn discover_methods(
    session: &Session,
    methods: &[NodeId],
    schema: &mut Schema,
) -> HashMap<String, MethodInputs> {
    let mut found: HashMap<String, MethodInputs> = HashMap::new();

    // Executable and UserExecutable, two per method.
    let to_read: Vec<ReadValueId> = methods
        .iter()
        .flat_map(|id| {
            [AttributeId::Executable, AttributeId::UserExecutable].map(|attr| ReadValueId {
                node_id: id.clone(),
                attribute_id: attr as u32,
                index_range: NumericRange::None,
                data_encoding: QualifiedName::null(),
            })
        })
        .collect();
    let flags = session
        .read(&to_read, TimestampsToReturn::Neither, 0.0)
        .await
        .unwrap_or_default();
    let executable = |i: usize| -> bool {
        // An unreadable flag is no evidence against the method.
        [2 * i, 2 * i + 1].iter().all(|&j| {
            !matches!(
                flags.get(j).and_then(|dv| dv.value.as_ref()),
                Some(Variant::Boolean(false))
            )
        })
    };

    // InputArguments: a HasProperty child of that browse name.
    let properties = crate::opcua::browse::browse_properties(session, methods)
        .await
        .unwrap_or_default();
    let mut argument_nodes: Vec<(usize, NodeId)> = Vec::new();
    for (i, refs) in properties.iter().enumerate() {
        if let Some(r) = refs
            .iter()
            .find(|r| r.browse_name.name.as_ref() == "InputArguments")
        {
            argument_nodes.push((i, r.node_id.node_id.clone()));
        }
    }
    let ids: Vec<NodeId> = argument_nodes.iter().map(|(_, id)| id.clone()).collect();
    let values = crate::opcua::value::read_data_values(session, &ids)
        .await
        .unwrap_or_default();
    let mut arguments: HashMap<usize, Vec<Argument>> = HashMap::new();
    for ((i, _), dv) in argument_nodes.iter().zip(values) {
        let args = match dv.value {
            Some(Variant::Array(array)) => array
                .values
                .iter()
                .filter_map(|v| match v {
                    Variant::ExtensionObject(obj) => obj.inner_as::<Argument>().cloned(),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        arguments.insert(*i, args);
    }

    // Argument types may be custom: resolve them like variable types.
    let arg_types: Vec<String> = arguments
        .values()
        .flatten()
        .map(|a| type_key(&a.data_type))
        .filter(|k| builtin_shape(k).is_none() && !schema.types.contains_key(k))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    resolve_types(session, arg_types, schema).await;

    for (i, id) in methods.iter().enumerate() {
        let inputs = if !executable(i) {
            Err("not executable".to_string())
        } else {
            arguments
                .get(&i)
                .map(|args| args.iter().map(|a| input_type(a, schema)).collect())
                .unwrap_or_else(|| Ok(Vec::new()))
        };
        found.insert(id.to_string(), inputs);
    }
    found
}

/// The tedge-dot datatype of one input argument (contract §4: no bytes, no
/// string-rendered types).
fn input_type(arg: &Argument, schema: &Schema) -> Result<&'static str, String> {
    let name = arg.name.as_ref();
    if arg.value_rank >= 0 {
        return Err(format!("argument '{name}' is an array"));
    }
    let key = type_key(&arg.data_type);
    let shape = builtin_shape(&key).or_else(|| schema.types.get(&key).cloned());
    match shape {
        Some(Shape::Scalar {
            datatype,
            writable: true,
        }) if datatype != "bytes" => Ok(datatype),
        Some(Shape::Enum { .. }) => Ok("int32"),
        _ => Err(format!(
            "argument '{name}' has datatype {key}, which a method point cannot pass"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_keys_match_record_datatypes() {
        assert_eq!(type_key(&NodeId::new(0, 11u32)), "Double");
        assert_eq!(type_key(&NodeId::new(0, 852u32)), "i=852");
        assert_eq!(type_key(&NodeId::new(2, 3001u32)), "ns=2;i=3001");
    }

    #[test]
    fn abstract_types_are_unsupported() {
        for key in ["Structure", "BaseDataType", "i=26"] {
            assert!(
                matches!(builtin_shape(key), Some(Shape::Unsupported(_))),
                "{key}"
            );
        }
        assert_eq!(builtin_shape("i=29"), Some(Shape::Enum { cases: vec![] }));
        assert_eq!(builtin_shape("ns=2;i=1"), None);
    }

    fn arg(name: &str, data_type: NodeId, value_rank: i32) -> Argument {
        Argument {
            name: name.into(),
            data_type,
            value_rank,
            array_dimensions: None,
            description: Default::default(),
        }
    }

    #[test]
    fn input_types() {
        let mut schema = Schema::default();
        schema
            .types
            .insert("ns=2;i=5".into(), Shape::Enum { cases: vec![] });
        assert_eq!(
            input_type(&arg("a", NodeId::new(0, 6u32), -1), &schema),
            Ok("int32")
        );
        assert_eq!(
            input_type(&arg("e", NodeId::new(2, 5u32), -1), &schema),
            Ok("int32")
        );
        assert!(
            input_type(&arg("arr", NodeId::new(0, 6u32), 1), &schema)
                .unwrap_err()
                .contains("array")
        );
        assert!(
            input_type(&arg("t", NodeId::new(0, 13u32), -1), &schema)
                .unwrap_err()
                .contains("DateTime")
        );
        assert!(input_type(&arg("b", NodeId::new(0, 15u32), -1), &schema).is_err());
    }
}
