//! Write discovered nodes as a tedge-dot OPC UA point library (contract §3.4):
//!
//! ```toml
//! [library]
//! protocol   = "opcua"
//! type       = "acme-line"
//! namespaces = ["http://opcfoundation.org/UA/", "urn:server", "urn:acme"]
//!
//! [[point]]
//! id       = "temperature"
//! name     = "Temperature"
//! datatype = "float64"
//! address  = { node_id = "ns=2;s=Temperature" }
//! ```
//!
//! A new library is rendered from scratch; an existing one is merged into with
//! `toml_edit`, so its comments, layout and hand-edited points survive. Nodes
//! are matched by `(namespace URI, identifier)`, which makes the match
//! independent of how the server or the library numbers its namespaces.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use opcua::types::{Identifier, NodeId};
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value, value};

use super::schema::{Schema, Shape, builtin_shape};
use crate::export::NodeRecord;
use crate::opcua::namespaces::NamespaceTable;

/// Namespace 0, which a namespace table must start with.
const OPCUA_NS: &str = "http://opcfoundation.org/UA/";
/// How many skipped nodes the header comment lists by name.
const MAX_LISTED_SKIPS: usize = 50;
/// The `Argument` datatype of method argument properties.
const ARGUMENT_TYPE: &str = "i=296";
/// How deep nested structure fields are followed.
const MAX_FIELD_DEPTH: usize = 8;

/// AccessLevel bits (OPC UA Part 3 §8.57).
const CURRENT_READ: u8 = 0x01;
const CURRENT_WRITE: u8 = 0x02;

/// Settings for a library write.
#[derive(Debug, Clone, Default)]
pub struct LibraryOptions {
    /// `[library] type` for a new library.
    pub library_type: Option<String>,
    /// The endpoint the nodes were read from (header comment only).
    pub endpoint: String,
}

/// A node (or structure field) that was not turned into a point, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub node_id: String,
    pub browse_path: String,
    pub reason: String,
}

/// The result of a library write.
#[derive(Debug)]
pub struct Outcome {
    /// The library text to write.
    pub text: String,
    /// Points appended.
    pub added: usize,
    /// Points removed (TUI only).
    pub removed: usize,
    /// Points in the library after the write.
    pub total: usize,
    /// Variables, fields and methods that could not become points.
    pub skipped: Vec<Skipped>,
}

/// What a point addresses.
#[derive(Debug, Clone)]
enum Target {
    /// A variable, or one field of a structured variable.
    Variable {
        node_id: NodeId,
        field: Option<String>,
        datatype: &'static str,
        access: Option<&'static str>,
        subscribe: bool,
        /// Enumeration values and names, written as a `map`.
        cases: Option<Vec<(i64, String)>>,
    },
    /// A method called on an object.
    Method {
        object_id: NodeId,
        method_id: NodeId,
        inputs: Vec<&'static str>,
    },
}

/// A point to be written, before it has an id.
#[derive(Debug, Clone)]
struct Candidate {
    target: Target,
    /// Browse path segments (then field path segments), the most specific last.
    segments: Vec<String>,
    browse_path: String,
    name: String,
    description: Option<String>,
    unit: Option<String>,
}

/// A node, independent of namespace numbering.
type NodeKey = (String, String);

/// What a library point addresses, independent of namespace numbering.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum PointKey {
    Variable {
        node: NodeKey,
        field: Option<String>,
    },
    Method {
        object: NodeKey,
        method: NodeKey,
    },
}

impl PointKey {
    /// Whether the point addresses `node` (as variable or as method).
    fn addresses(&self, node: &NodeKey) -> bool {
        match self {
            PointKey::Variable { node: n, .. } => n == node,
            PointKey::Method { method, .. } => method == node,
        }
    }
}

/// Build the library text: a new library when `existing` is `None`, else
/// `existing` merged with the mappable `records`. Points addressing a node in
/// `remove` (server node id strings) are dropped from `existing`. `schema`
/// holds the discovered custom datatypes and method arguments.
pub fn write(
    existing: Option<&str>,
    records: &[NodeRecord],
    remove: &HashSet<String>,
    server: &NamespaceTable,
    options: &LibraryOptions,
    schema: &Schema,
) -> Result<Outcome, String> {
    let (mut candidates, skipped) = map_records(records, schema);
    candidates.sort_by(|a, b| a.browse_path.cmp(&b.browse_path));

    let is_new = existing.is_none();
    let mut doc = match existing {
        Some(text) => parse_library(text)?,
        None => new_library(server, options, &skipped),
    };

    // The table the file's node ids are written in. A library without one
    // gets the server's: its ids were taken from (some instance of) it.
    let mut table: Vec<String> = match library_namespaces(&doc)? {
        Some(table) => table,
        None => {
            let uris = server.uris().to_vec();
            set_namespaces(&mut doc, &uris);
            uris
        }
    };

    // Remove, then index what is left.
    let remove: Vec<NodeKey> = remove
        .iter()
        .filter_map(|id| NodeId::from_str(id).ok())
        .filter_map(|id| key_in(&id, server.uris()))
        .collect();
    let points = points_mut(&mut doc)?;
    let before = points.len();
    points.retain(|p| {
        !point_key(p, &table).is_some_and(|k| remove.iter().any(|node| k.addresses(node)))
    });
    let removed = before - points.len();

    let mut covered: HashSet<PointKey> = HashSet::new();
    let mut taken: HashSet<String> = HashSet::new();
    for p in points.iter() {
        covered.extend(point_key(p, &table));
        if let Some(id) = p.get("id").and_then(Item::as_str) {
            taken.insert(id.to_string());
        }
    }

    let mut fresh: Vec<Candidate> = Vec::new();
    for c in candidates {
        let Some(key) = candidate_key(&c, server.uris()) else {
            continue;
        };
        if covered.insert(key) {
            fresh.push(c);
        }
    }

    let ids = assign_ids(&fresh, &taken);
    let mut table_grew = false;
    for (c, id) in fresh.iter().zip(ids) {
        let mut to_lib = |n: &NodeId| to_library_index(n, server, &mut table, &mut table_grew);
        points.push(render_point(c, &id, &mut to_lib));
    }
    let added = fresh.len();
    let total = points.len();
    if table_grew {
        set_namespaces(&mut doc, &table);
    }

    if total == 0 {
        return Err(if is_new {
            format!(
                "no node could be written as a tedge-dot point ({} node(s) skipped)",
                skipped.len()
            )
        } else {
            "the library would have no points left; not writing it".to_string()
        });
    }

    Ok(Outcome {
        text: doc.to_string(),
        added,
        removed,
        total,
        skipped,
    })
}

/// The server node ids (as strings) the points of a library address
/// (variables and methods), for pre-marking them in the TUI. Points that do
/// not resolve are left out.
pub fn node_ids(text: &str, server: &NamespaceTable) -> Result<HashSet<String>, String> {
    let mut doc = parse_library(text)?;
    let table = library_namespaces(&doc)?.unwrap_or_else(|| server.uris().to_vec());
    let points = points_mut(&mut doc)?;
    Ok(points
        .iter()
        .filter_map(|p| point_key(p, &table))
        .filter_map(|k| {
            let (uri, ident) = match k {
                PointKey::Variable { node, .. } => node,
                PointKey::Method { method, .. } => method,
            };
            let ns = if uri == OPCUA_NS {
                0
            } else {
                server.index(&uri)?
            };
            let id = NodeId::from_str(&format!("ns={ns};{ident}")).ok()?;
            Some(id.to_string())
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Record → points

/// Split records into point candidates and skipped Variables, fields and
/// Methods. Other node classes are dropped silently.
fn map_records(records: &[NodeRecord], schema: &Schema) -> (Vec<Candidate>, Vec<Skipped>) {
    let mut candidates = Vec::new();
    let mut skipped = Vec::new();
    for r in records {
        let mapped = match r.node_class.as_str() {
            // A method's InputArguments/OutputArguments: metadata, not data.
            "Variable" if r.datatype.as_deref() == Some(ARGUMENT_TYPE) => continue,
            "Variable" => map_variable(r, schema, &mut skipped),
            "Method" => map_method(r, schema).map(|c| vec![c]),
            _ => continue,
        };
        match mapped {
            Ok(cs) => candidates.extend(cs),
            Err(reason) => skipped.push(Skipped {
                node_id: r.node_id.clone(),
                browse_path: r.browse_path.clone(),
                reason,
            }),
        }
    }
    (candidates, skipped)
}

/// The shape of a datatype key: built-in, else discovered.
fn shape_of(key: &str, schema: &Schema) -> Shape {
    builtin_shape(key)
        .or_else(|| schema.types.get(key).cloned())
        .unwrap_or_else(|| Shape::Unsupported(format!("datatype {key} has no tedge-dot datatype")))
}

/// One Variable as its point(s): one for a scalar or enumeration, one per
/// mappable leaf field for a structure. Unmappable fields go to `skipped`.
fn map_variable(
    r: &NodeRecord,
    schema: &Schema,
    skipped: &mut Vec<Skipped>,
) -> Result<Vec<Candidate>, String> {
    let node_id = NodeId::from_str(&r.node_id)
        .map_err(|_| format!("node id '{}' cannot be parsed", r.node_id))?;
    if r.value_rank.is_some_and(|rank| rank >= 0) {
        return Err("array".into());
    }
    let dt = r.datatype.as_deref().ok_or("no datatype")?;

    // No AccessLevel read: assume the default (readable), as a server must
    // report it for Variables and the TUI may not have read it yet.
    let mut level = r.access_level.unwrap_or(CURRENT_READ);
    if let Some(user) = r.user_access_level {
        level &= user;
    }
    let read = level & CURRENT_READ != 0;
    let can_write = level & CURRENT_WRITE != 0;
    if !read && !can_write {
        return Err("neither readable nor writable".into());
    }
    let access = |writable_type: bool| match (read, can_write && writable_type) {
        (true, false) => None,
        (true, true) => Some("read_write"),
        (false, _) => Some("write"),
    };
    let subscribe = !read || r.subscribable;
    let base = |target: Target, segments: Vec<String>| Candidate {
        target,
        segments,
        browse_path: r.browse_path.clone(),
        name: Some(r.display_name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| r.browse_name.clone()),
        description: r.description.clone().filter(|d| !d.is_empty()),
        unit: r.engineering_units.clone().filter(|u| !u.is_empty()),
    };

    match shape_of(dt, schema) {
        Shape::Scalar { datatype, writable } => {
            if !read && !writable {
                return Err(format!("write-only, and tedge-dot cannot write {dt}"));
            }
            Ok(vec![base(
                Target::Variable {
                    node_id,
                    field: None,
                    datatype,
                    access: access(writable),
                    subscribe,
                    cases: None,
                },
                segments(r),
            )])
        }
        Shape::Enum { cases } => Ok(vec![base(
            Target::Variable {
                node_id,
                field: None,
                datatype: "int32",
                access: access(true),
                subscribe,
                cases: Some(cases).filter(|c| !c.is_empty()),
            },
            segments(r),
        )]),
        Shape::Structure { fields } => {
            if !read {
                return Err("write-only structure (field points are read-only)".into());
            }
            let mut leaves = Vec::new();
            expand_fields(&fields, "", schema, 0, &mut leaves, &mut |path, reason| {
                skipped.push(Skipped {
                    node_id: r.node_id.clone(),
                    browse_path: format!("{}.{path}", r.browse_path),
                    reason,
                });
            });
            if leaves.is_empty() {
                return Err("no field of the structure could be mapped".into());
            }
            Ok(leaves
                .into_iter()
                .map(|(path, datatype, cases)| {
                    let mut segs = segments(r);
                    segs.extend(path.split('.').map(str::to_string));
                    let mut c = base(
                        Target::Variable {
                            node_id: node_id.clone(),
                            field: Some(path.clone()),
                            datatype,
                            // tedge-dot makes field points read-only.
                            access: None,
                            subscribe,
                            cases,
                        },
                        segs,
                    );
                    c.name = format!("{}.{path}", c.name);
                    c
                })
                .collect())
        }
        Shape::Unsupported(reason) => Err(reason),
    }
}

/// A mappable leaf field: dotted path, datatype, enumeration cases.
type Leaf = (String, &'static str, Option<Vec<(i64, String)>>);

/// The mappable leaves of a structure, depth first in definition order.
fn expand_fields(
    fields: &[super::schema::Field],
    prefix: &str,
    schema: &Schema,
    depth: usize,
    leaves: &mut Vec<Leaf>,
    skip: &mut dyn FnMut(String, String),
) {
    for f in fields {
        let path = if prefix.is_empty() {
            f.name.clone()
        } else {
            format!("{prefix}.{}", f.name)
        };
        if f.value_rank >= 0 {
            skip(path, "array field".into());
            continue;
        }
        match shape_of(&f.datatype, schema) {
            Shape::Scalar { datatype, .. } => leaves.push((path, datatype, None)),
            Shape::Enum { cases } => {
                leaves.push((path, "int32", Some(cases).filter(|c| !c.is_empty())))
            }
            Shape::Structure { fields } if depth + 1 < MAX_FIELD_DEPTH => {
                expand_fields(&fields, &path, schema, depth + 1, leaves, skip)
            }
            Shape::Structure { .. } => skip(path, "structure nested too deeply".into()),
            Shape::Unsupported(reason) => skip(path, reason),
        }
    }
}

/// A Method as a `call_method` point.
fn map_method(r: &NodeRecord, schema: &Schema) -> Result<Candidate, String> {
    let method_id = NodeId::from_str(&r.node_id)
        .map_err(|_| format!("node id '{}' cannot be parsed", r.node_id))?;
    let object_id = r
        .parent_node_id
        .as_deref()
        .and_then(|p| NodeId::from_str(p).ok())
        .ok_or("the object the method belongs to is unknown")?;
    let inputs = schema
        .methods
        .get(&r.node_id)
        .cloned()
        .unwrap_or_else(|| Err("method arguments could not be read".into()))?;
    Ok(Candidate {
        target: Target::Method {
            object_id,
            method_id,
            inputs,
        },
        segments: segments(r),
        browse_path: r.browse_path.clone(),
        name: Some(r.display_name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| r.browse_name.clone()),
        description: r.description.clone().filter(|d| !d.is_empty()),
        unit: None,
    })
}

/// Browse path segments with the browse name last. The browse name may itself
/// contain `/`, so it is cut off the path rather than split out of it.
fn segments(r: &NodeRecord) -> Vec<String> {
    let parent = r
        .browse_path
        .strip_suffix(r.browse_name.as_str())
        .map(|p| p.trim_end_matches('/'))
        .unwrap_or_else(|| r.browse_path.rsplit_once('/').map_or("", |(p, _)| p));
    let mut segments: Vec<String> = parent
        .split('/')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    segments.push(if r.browse_name.is_empty() {
        r.display_name.clone()
    } else {
        r.browse_name.clone()
    });
    segments
}

// ---------------------------------------------------------------------------
// Point ids

/// `MotorSpeed` → `motor_speed`, `Drehzahl (U/min)` → `drehzahl_u_min`.
fn snake_case(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_alphanumeric() {
            if c.is_ascii_uppercase() && i > 0 {
                let prev = chars[i - 1];
                let next_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
                if prev.is_ascii_lowercase()
                    || prev.is_ascii_digit()
                    || (prev.is_ascii_uppercase() && next_lower)
                {
                    out.push('_');
                }
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push('_');
        }
    }
    out.split('_')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

/// The id made of the last `levels` segments.
fn id_of(segments: &[String], levels: usize) -> String {
    let id = segments[segments.len().saturating_sub(levels)..]
        .iter()
        .map(|s| snake_case(s))
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    if id.is_empty() { "point".into() } else { id }
}

/// Unique ids for `candidates`: the shortest path suffix that is unique among
/// them and not in `taken`, then `_2`, `_3`, ... for what is still equal.
fn assign_ids(candidates: &[Candidate], taken: &HashSet<String>) -> Vec<String> {
    // A field id starts from its whole field path (`motor_current`, not `current`).
    let mut levels: Vec<usize> = candidates
        .iter()
        .map(|c| match &c.target {
            Target::Variable {
                field: Some(field), ..
            } => field.split('.').count(),
            _ => 1,
        })
        .collect();
    loop {
        let ids: Vec<String> = candidates
            .iter()
            .zip(&levels)
            .map(|(c, &l)| id_of(&c.segments, l))
            .collect();
        let mut count: HashMap<&str, usize> = HashMap::new();
        for id in &ids {
            *count.entry(id.as_str()).or_default() += 1;
        }
        let mut grew = false;
        for (i, id) in ids.iter().enumerate() {
            let clash = count[id.as_str()] > 1 || taken.contains(id);
            if clash && levels[i] < candidates[i].segments.len() {
                levels[i] += 1;
                grew = true;
            }
        }
        if !grew {
            let mut used = taken.clone();
            return ids
                .into_iter()
                .map(|id| {
                    let mut unique = id.clone();
                    let mut n = 2;
                    while used.contains(&unique) {
                        unique = format!("{id}_{n}");
                        n += 1;
                    }
                    used.insert(unique.clone());
                    unique
                })
                .collect();
        }
    }
}

// ---------------------------------------------------------------------------
// Namespaces and node keys

/// `(namespace URI, identifier)` of a node id under the table `uris`.
fn key_in(node_id: &NodeId, uris: &[String]) -> Option<NodeKey> {
    let uri = if node_id.namespace == 0 {
        OPCUA_NS
    } else {
        uris.get(node_id.namespace as usize)?.as_str()
    };
    Some((uri.to_string(), node_id.identifier.to_string()))
}

/// A textual node id, or `namespace` + `identifier`, from an address table.
fn address_node(address: &Item) -> Option<NodeId> {
    match address.get("node_id").and_then(Item::as_str) {
        Some(text) => NodeId::from_str(text).ok(),
        None => {
            let ns = u16::try_from(address.get("namespace")?.as_integer()?).ok()?;
            let identifier = match address.get("identifier")?.as_value()? {
                Value::String(s) => Identifier::String(s.value().as_str().into()),
                Value::Integer(n) => Identifier::Numeric(u32::try_from(*n.value()).ok()?),
                _ => return None,
            };
            Some(NodeId::new(ns, identifier))
        }
    }
}

/// What a library point addresses, resolved through the library's table: a
/// method (`object_id` + `method_id`), or a node and optional `field`.
fn point_key(point: &Table, table: &[String]) -> Option<PointKey> {
    let address = point.get("address")?;
    if let Some(method) = address.get("method_id").and_then(Item::as_str) {
        let object = address.get("object_id").and_then(Item::as_str)?;
        return Some(PointKey::Method {
            object: key_in(&NodeId::from_str(object).ok()?, table)?,
            method: key_in(&NodeId::from_str(method).ok()?, table)?,
        });
    }
    Some(PointKey::Variable {
        node: key_in(&address_node(address)?, table)?,
        field: address
            .get("field")
            .and_then(Item::as_str)
            .map(str::to_string),
    })
}

/// The key of a candidate, in server numbering.
fn candidate_key(c: &Candidate, uris: &[String]) -> Option<PointKey> {
    Some(match &c.target {
        Target::Variable { node_id, field, .. } => PointKey::Variable {
            node: key_in(node_id, uris)?,
            field: field.clone(),
        },
        Target::Method {
            object_id,
            method_id,
            ..
        } => PointKey::Method {
            object: key_in(object_id, uris)?,
            method: key_in(method_id, uris)?,
        },
    })
}

/// `node_id` (server numbering) rewritten in the library's table, appending
/// the namespace to the table when it lacks it.
fn to_library_index(
    node_id: &NodeId,
    server: &NamespaceTable,
    table: &mut Vec<String>,
    grew: &mut bool,
) -> String {
    if node_id.namespace == 0 {
        return node_id.to_string();
    }
    let Some(uri) = server.uri(node_id.namespace) else {
        return node_id.to_string();
    };
    let index = match table.iter().position(|u| u == uri) {
        Some(i) => i,
        None => {
            table.push(uri.to_string());
            *grew = true;
            table.len() - 1
        }
    };
    NodeId::new(index as u16, node_id.identifier.clone()).to_string()
}

// ---------------------------------------------------------------------------
// TOML

/// Parse an existing library, refusing anything that is not an OPC UA one.
fn parse_library(text: &str) -> Result<DocumentMut, String> {
    let not_a_library = |why: &str| format!("not an OPC UA point library ({why})");
    let doc: DocumentMut = text.parse().map_err(|_| not_a_library("not TOML"))?;
    match doc
        .get("library")
        .and_then(|l| l.get("protocol"))
        .and_then(Item::as_str)
    {
        Some("opcua") => {}
        Some(other) => return Err(not_a_library(&format!("[library] protocol is '{other}'"))),
        None => return Err(not_a_library("no [library] protocol")),
    }
    for key in ["connector", "connection", "device", "mqtt"] {
        if doc.contains_key(key) {
            return Err(not_a_library(&format!(
                "has [{key}]; is it a connector config?"
            )));
        }
    }
    Ok(doc)
}

/// `[library] namespaces` of a library, if it has one.
fn library_namespaces(doc: &DocumentMut) -> Result<Option<Vec<String>>, String> {
    let Some(item) = doc.get("library").and_then(|l| l.get("namespaces")) else {
        return Ok(None);
    };
    let array = item
        .as_array()
        .ok_or("[library] namespaces must be an array of strings")?;
    let table: Vec<String> = array
        .iter()
        .map(|v| v.as_str().map(str::to_string))
        .collect::<Option<_>>()
        .ok_or("[library] namespaces must be an array of strings")?;
    if table.first().map(String::as_str) != Some(OPCUA_NS) {
        return Err(format!("[library] namespaces entry 0 must be {OPCUA_NS}"));
    }
    Ok(Some(table))
}

fn set_namespaces(doc: &mut DocumentMut, uris: &[String]) {
    let mut array: Array = uris.iter().map(String::as_str).collect();
    // One URI per line: tables are long and diffs stay readable.
    for v in array.iter_mut() {
        v.decor_mut().set_prefix("\n  ");
    }
    array.set_trailing("\n");
    array.set_trailing_comma(true);
    doc["library"]["namespaces"] = value(array);
}

/// The `[[point]]` array, created when absent.
fn points_mut(doc: &mut DocumentMut) -> Result<&mut ArrayOfTables, String> {
    if !doc.contains_key("point") {
        doc.insert("point", Item::ArrayOfTables(ArrayOfTables::new()));
    }
    doc["point"]
        .as_array_of_tables_mut()
        .ok_or_else(|| "[[point]] must be an array of tables".to_string())
}

/// A fresh library: header comment and `[library]`.
fn new_library(
    server: &NamespaceTable,
    options: &LibraryOptions,
    skipped: &[Skipped],
) -> DocumentMut {
    let mut header = format!(
        "# tedge-dot OPC UA point library, generated by opcua-scan {} from {}.\n\
         # Node ids use the namespace indexes of [library] namespaces.\n",
        env!("CARGO_PKG_VERSION"),
        options.endpoint
    );
    if !skipped.is_empty() {
        header.push_str(&format!("#\n# Skipped {} node(s):\n", skipped.len()));
        for s in skipped.iter().take(MAX_LISTED_SKIPS) {
            header.push_str(&format!(
                "#   {}  {}  ({})\n",
                s.node_id, s.browse_path, s.reason
            ));
        }
        if skipped.len() > MAX_LISTED_SKIPS {
            header.push_str(&format!(
                "#   ... and {} more\n",
                skipped.len() - MAX_LISTED_SKIPS
            ));
        }
    }
    header.push('\n');

    let mut library = Table::new();
    library.decor_mut().set_prefix(header);
    library.insert("protocol", value("opcua"));
    if let Some(t) = &options.library_type {
        library.insert("type", value(t.as_str()));
    }
    let mut doc = DocumentMut::new();
    doc.insert("library", Item::Table(library));
    set_namespaces(&mut doc, server.uris());
    doc
}

/// One `[[point]]` table; `to_lib` writes a node id in the library's numbering.
fn render_point(c: &Candidate, id: &str, to_lib: &mut dyn FnMut(&NodeId) -> String) -> Table {
    let mut t = Table::new();
    t.decor_mut().set_prefix("\n");
    t.insert("id", value(id));
    t.insert("name", value(c.name.as_str()));
    if let Some(d) = &c.description {
        t.insert("description", value(d.as_str()));
    }
    let mut address = InlineTable::new();
    match &c.target {
        Target::Variable {
            node_id,
            field,
            datatype,
            access,
            subscribe,
            cases,
        } => {
            t.insert("datatype", value(*datatype));
            if let Some(a) = access {
                t.insert("access", value(*a));
            }
            if !subscribe {
                t.insert("subscribe", value(false));
            }
            if let Some(u) = &c.unit {
                t.insert("unit", value(u.as_str()));
            }
            address.insert("node_id", to_lib(node_id).into());
            if let Some(f) = field {
                address.insert("field", f.as_str().into());
            }
            t.insert("address", value(address));
            if let Some(cases) = cases {
                t.insert("map", render_map(cases));
            }
        }
        Target::Method {
            object_id,
            method_id,
            inputs,
        } => {
            t.insert("access", value("call_method"));
            address.insert("object_id", to_lib(object_id).into());
            address.insert("method_id", to_lib(method_id).into());
            if !inputs.is_empty() {
                let inputs: Array = inputs.iter().copied().collect();
                address.insert("inputs", Value::Array(inputs));
            }
            t.insert("address", value(address));
        }
    }
    t
}

/// An enumeration's `map`: one case per value, and unknown codes as text.
fn render_map(cases: &[(i64, String)]) -> Item {
    let mut array = Array::new();
    for (eq, to) in cases {
        let mut case = InlineTable::new();
        case.insert("eq", (*eq).into());
        case.insert("to", to.as_str().into());
        let mut v = Value::InlineTable(case);
        v.decor_mut().set_prefix("\n  ");
        array.push_formatted(v);
    }
    array.set_trailing("\n");
    array.set_trailing_comma(true);
    let mut map = Table::new();
    map.insert("cases", value(array));
    map.insert("as", value("string"));
    Item::Table(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    const SERVER: [&str; 3] = [OPCUA_NS, "urn:server", "urn:acme"];

    fn server() -> NamespaceTable {
        NamespaceTable::from_uris(SERVER.iter().map(|s| s.to_string()).collect())
    }

    fn var(path: &str, node_id: &str, datatype: &str) -> NodeRecord {
        let name = path.rsplit('/').next().unwrap().to_string();
        NodeRecord {
            browse_path: path.into(),
            display_name: name.clone(),
            browse_name: name,
            node_id: node_id.into(),
            namespace_uri: String::new(),
            node_class: "Variable".into(),
            datatype: Some(datatype.into()),
            value_rank: Some(-1),
            access_level: Some(CURRENT_READ),
            user_access_level: None,
            historizing: None,
            subscribable: true,
            validation_status: None,
            current_value: None,
            engineering_units: None,
            description: None,
            parent_node_id: None,
        }
    }

    fn options() -> LibraryOptions {
        LibraryOptions {
            library_type: Some("acme-line".into()),
            endpoint: "opc.tcp://plc:4840".into(),
        }
    }

    fn new(records: &[NodeRecord]) -> Outcome {
        write(
            None,
            records,
            &HashSet::new(),
            &server(),
            &options(),
            &Schema::default(),
        )
        .unwrap()
    }

    fn parsed(text: &str) -> toml::Table {
        text.parse().expect("output is valid TOML")
    }

    fn points(text: &str) -> Vec<toml::Table> {
        parsed(text)["point"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_table().unwrap().clone())
            .collect()
    }

    #[test]
    fn new_library_has_library_table_and_points() {
        let out = new(&[
            var("Line1/Motor/Temperature", "ns=2;s=Temperature", "Double"),
            var("Line1/Motor/Running", "ns=2;s=Running", "Boolean"),
        ]);
        let doc = parsed(&out.text);
        let lib = doc["library"].as_table().unwrap();
        assert_eq!(lib["protocol"].as_str(), Some("opcua"));
        assert_eq!(lib["type"].as_str(), Some("acme-line"));
        let ns: Vec<&str> = lib["namespaces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(ns, SERVER);
        let pts = points(&out.text);
        // Sorted by browse path.
        assert_eq!(pts[0]["id"].as_str(), Some("running"));
        assert_eq!(pts[0]["datatype"].as_str(), Some("bool"));
        assert_eq!(pts[1]["id"].as_str(), Some("temperature"));
        assert_eq!(pts[1]["datatype"].as_str(), Some("float64"));
        assert_eq!(
            pts[1]["address"]["node_id"].as_str(),
            Some("ns=2;s=Temperature")
        );
        assert_eq!((out.added, out.total), (2, 2));
        assert!(out.text.starts_with("# tedge-dot OPC UA point library"));
    }

    #[test]
    fn library_has_only_library_and_points() {
        let out = new(&[var("T", "ns=2;s=T", "Double")]);
        let keys: Vec<String> = parsed(&out.text).keys().cloned().collect();
        assert_eq!(keys, ["library", "point"]);
    }

    #[test]
    fn point_fields() {
        let mut r = var("Motor/Temperature", "ns=2;s=Temperature", "Double");
        r.description = Some("Motor temperature".into());
        r.engineering_units = Some("°C".into());
        let p = &points(&new(&[r]).text)[0];
        assert_eq!(p["name"].as_str(), Some("Temperature"));
        assert_eq!(p["description"].as_str(), Some("Motor temperature"));
        assert_eq!(p["unit"].as_str(), Some("°C"));
        assert!(p.get("access").is_none());
        assert!(p.get("subscribe").is_none());
    }

    #[rstest]
    #[case("Boolean", "bool")]
    #[case("SByte", "int8")]
    #[case("Byte", "uint8")]
    #[case("UInt64", "uint64")]
    #[case("Float", "float32")]
    #[case("i=290", "float64")]
    #[case("DateTime", "string")]
    #[case("i=294", "string")]
    #[case("LocalizedText", "string")]
    #[case("StatusCode", "string")]
    #[case("ByteString", "bytes")]
    fn datatypes(#[case] opcua: &str, #[case] tedge: &str) {
        let p = &points(&new(&[var("V", "ns=2;i=1", opcua)]).text)[0];
        assert_eq!(p["datatype"].as_str(), Some(tedge));
    }

    #[rstest]
    #[case::structure("ns=2;i=3001", None, "datatype ns=2;i=3001 has no tedge-dot datatype")]
    #[case::abstract_type("BaseDataType", None, "is abstract or not supported")]
    #[case::array("Double", Some(1), "array")]
    #[case::any_dimensions("Double", Some(0), "array")]
    fn skipped(#[case] dt: &str, #[case] rank: Option<i32>, #[case] reason: &str) {
        let mut r = var("Line1/X", "ns=2;s=X", dt);
        if rank.is_some() {
            r.value_rank = rank;
        }
        let out = new(&[r, var("Line1/Ok", "ns=2;s=Ok", "Double")]);
        assert_eq!(out.skipped.len(), 1);
        assert!(out.skipped[0].reason.contains(reason), "{:?}", out.skipped);
        assert!(
            out.text.contains("#   ns=2;s=X  Line1/X  ("),
            "{}",
            out.text
        );
        assert_eq!(out.total, 1);
    }

    #[test]
    fn non_variables_are_dropped_silently() {
        let mut folder = var("Line1", "ns=2;s=Line1", "Double");
        folder.node_class = "Object".into();
        let out = new(&[folder, var("Line1/T", "ns=2;s=T", "Double")]);
        assert!(out.skipped.is_empty());
        assert!(!out.text.contains("Skipped"));
    }

    #[test]
    fn header_lists_at_most_fifty_skips() {
        let mut records: Vec<NodeRecord> = (0..60)
            .map(|i| var(&format!("S{i}"), &format!("ns=2;i={i}"), "Structure"))
            .collect();
        records.push(var("Ok", "ns=2;s=Ok", "Double"));
        let out = new(&records);
        assert!(out.text.contains("Skipped 60 node(s)"));
        assert!(out.text.contains("... and 10 more"));
    }

    #[rstest]
    #[case::read_write(CURRENT_READ | CURRENT_WRITE, None, "Double", Some("read_write"))]
    #[case::write_only(CURRENT_WRITE, None, "Double", Some("write"))]
    #[case::user_may_only_read(CURRENT_READ | CURRENT_WRITE, Some(CURRENT_READ), "Double", None)]
    #[case::datetime_is_not_writable(CURRENT_READ | CURRENT_WRITE, None, "DateTime", None)]
    fn access(
        #[case] level: u8,
        #[case] user: Option<u8>,
        #[case] dt: &str,
        #[case] expected: Option<&str>,
    ) {
        let mut r = var("V", "ns=2;s=V", dt);
        r.access_level = Some(level);
        r.user_access_level = user;
        let p = &points(&new(&[r]).text)[0];
        assert_eq!(p.get("access").and_then(|a| a.as_str()), expected);
    }

    #[test]
    fn no_access_is_skipped() {
        let mut r = var("V", "ns=2;s=V", "Double");
        r.access_level = Some(0);
        let err = write(
            None,
            &[r],
            &HashSet::new(),
            &server(),
            &options(),
            &Schema::default(),
        )
        .unwrap_err();
        assert!(err.contains("1 node(s) skipped"), "{err}");
    }

    #[test]
    fn not_subscribable_gets_subscribe_false() {
        let mut r = var("V", "ns=2;s=V", "Double");
        r.subscribable = false;
        let p = &points(&new(&[r]).text)[0];
        assert_eq!(p["subscribe"].as_bool(), Some(false));
    }

    #[rstest]
    #[case("MotorSpeed", "motor_speed")]
    #[case("Drehzahl (U/min)", "drehzahl_u_min")]
    #[case("HTTPServer", "http_server")]
    #[case("Temp2Max", "temp2_max")]
    #[case("already_snake", "already_snake")]
    #[case("Größe", "gr_e")]
    fn snake(#[case] input: &str, #[case] expected: &str) {
        assert_eq!(snake_case(input), expected);
    }

    #[test]
    fn ids_grow_only_where_they_collide() {
        let out = new(&[
            var("Line1/Motor/Temperature", "ns=2;s=L1.T", "Double"),
            var("Line2/Motor/Temperature", "ns=2;s=L2.T", "Double"),
            var("Line1/Motor/Speed", "ns=2;s=L1.S", "Double"),
        ]);
        let ids: Vec<String> = points(&out.text)
            .iter()
            .map(|p| p["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            ids,
            [
                "speed",
                "line1_motor_temperature",
                "line2_motor_temperature"
            ]
        );
    }

    #[test]
    fn identical_paths_get_numeric_suffixes() {
        let out = new(&[
            var("A/T", "ns=2;i=1", "Double"),
            var("A/T", "ns=2;i=2", "Double"),
        ]);
        let ids: Vec<String> = points(&out.text)
            .iter()
            .map(|p| p["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids, ["a_t", "a_t_2"]);
    }

    #[test]
    fn browse_name_with_slash() {
        let mut r = var("Motor/Drehzahl (U/min)", "ns=2;s=N", "Double");
        r.browse_name = "Drehzahl (U/min)".into();
        assert_eq!(segments(&r), ["Motor", "Drehzahl (U/min)"]);
        let p = &points(&new(&[r]).text)[0];
        assert_eq!(p["id"].as_str(), Some("drehzahl_u_min"));
    }

    #[test]
    fn empty_result_is_an_error() {
        let err = write(
            None,
            &[],
            &HashSet::new(),
            &server(),
            &options(),
            &Schema::default(),
        )
        .unwrap_err();
        assert!(err.contains("no node could be written"), "{err}");
    }

    const EXISTING: &str = r#"# Hand-written header
[library]
protocol = "opcua"
namespaces = ["http://opcfoundation.org/UA/", "urn:server", "urn:acme"]

[[point]]
id = "boiler_temp"   # keep me
datatype = "float64"
address = { node_id = "ns=2;s=Temperature" }
transform = { multiplier = 0.1 }
"#;

    fn merge(existing: &str, records: &[NodeRecord], remove: &[&str]) -> Outcome {
        merge_with(existing, records, remove, &server()).unwrap()
    }

    fn merge_with(
        existing: &str,
        records: &[NodeRecord],
        remove: &[&str],
        server: &NamespaceTable,
    ) -> Result<Outcome, String> {
        let remove: HashSet<String> = remove.iter().map(|s| s.to_string()).collect();
        write(
            Some(existing),
            records,
            &remove,
            server,
            &options(),
            &Schema::default(),
        )
    }

    #[test]
    fn merge_keeps_hand_edits_and_appends() {
        let out = merge(
            EXISTING,
            &[
                var("Motor/Temperature", "ns=2;s=Temperature", "Double"),
                var("Motor/Speed", "ns=2;s=Speed", "Float"),
            ],
            &[],
        );
        assert!(out.text.starts_with(EXISTING), "{}", out.text);
        assert_eq!((out.added, out.removed, out.total), (1, 0, 2));
        let pts = points(&out.text);
        assert_eq!(pts[1]["id"].as_str(), Some("speed"));
    }

    #[test]
    fn merge_matches_structured_addresses() {
        let existing = EXISTING.replace(
            "address = { node_id = \"ns=2;s=Temperature\" }",
            "address = { namespace = 2, identifier = \"Temperature\" }",
        );
        let out = merge(
            &existing,
            &[var("Motor/Temperature", "ns=2;s=Temperature", "Double")],
            &[],
        );
        assert_eq!(out.added, 0);
    }

    #[test]
    fn merge_avoids_existing_ids() {
        let out = merge(
            EXISTING,
            &[var("Boiler/Temp", "ns=2;s=Other", "Double")],
            &[],
        );
        // `temp` is free; a node whose id would be `boiler_temp` is extended.
        assert_eq!(points(&out.text)[1]["id"].as_str(), Some("temp"));
        let out = merge(
            EXISTING,
            &[var("X/BoilerTemp", "ns=2;s=Other", "Double")],
            &[],
        );
        assert_eq!(points(&out.text)[1]["id"].as_str(), Some("x_boiler_temp"));
    }

    #[test]
    fn merge_through_a_renumbered_server() {
        // The server now lists urn:acme at index 3.
        let renumbered = NamespaceTable::from_uris(
            [OPCUA_NS, "urn:server", "urn:new", "urn:acme"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        );
        let out = merge_with(
            EXISTING,
            &[
                var("Motor/Temperature", "ns=3;s=Temperature", "Double"),
                var("Motor/Speed", "ns=3;s=Speed", "Double"),
            ],
            &[],
            &renumbered,
        )
        .unwrap();
        assert_eq!(out.added, 1);
        let pts = points(&out.text);
        assert_eq!(pts[1]["address"]["node_id"].as_str(), Some("ns=2;s=Speed"));
    }

    #[test]
    fn merge_appends_a_missing_namespace() {
        let server = NamespaceTable::from_uris(
            [OPCUA_NS, "urn:server", "urn:acme", "urn:vendor"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        );
        let out = merge_with(
            EXISTING,
            &[var("Vendor/X", "ns=3;s=X", "Double")],
            &[],
            &server,
        )
        .unwrap();
        let doc = parsed(&out.text);
        let ns = doc["library"]["namespaces"].as_array().unwrap();
        assert_eq!(ns.len(), 4);
        assert_eq!(ns[3].as_str(), Some("urn:vendor"));
        assert_eq!(
            points(&out.text)[1]["address"]["node_id"].as_str(),
            Some("ns=3;s=X")
        );
    }

    #[test]
    fn library_without_table_gets_the_servers() {
        let existing = EXISTING.replace(
            "namespaces = [\"http://opcfoundation.org/UA/\", \"urn:server\", \"urn:acme\"]\n",
            "",
        );
        let out = merge(&existing, &[var("S", "ns=2;s=Speed", "Double")], &[]);
        let doc = parsed(&out.text);
        assert_eq!(doc["library"]["namespaces"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn remove_drops_points() {
        let out = merge(
            EXISTING,
            &[var("Motor/Speed", "ns=2;s=Speed", "Double")],
            &["ns=2;s=Temperature"],
        );
        assert_eq!((out.added, out.removed, out.total), (1, 1, 1));
        assert!(!out.text.contains("boiler_temp"));
    }

    #[test]
    fn removing_everything_is_refused() {
        let err = merge_with(EXISTING, &[], &["ns=2;s=Temperature"], &server()).unwrap_err();
        assert!(err.contains("no points left"), "{err}");
    }

    #[rstest]
    #[case::json("[{\"node_id\": \"x\"}]")]
    #[case::modbus("[library]\nprotocol = \"modbus\"\n")]
    #[case::connector_config("[library]\nprotocol = \"opcua\"\n[connection]\n")]
    fn not_a_library(#[case] text: &str) {
        let err = merge_with(text, &[var("S", "ns=2;s=S", "Double")], &[], &server()).unwrap_err();
        assert!(err.contains("not an OPC UA point library"), "{err}");
    }

    #[test]
    fn bad_namespace_table_is_refused() {
        let existing = EXISTING.replace("\"http://opcfoundation.org/UA/\", ", "");
        let err = merge_with(&existing, &[], &[], &server()).unwrap_err();
        assert!(err.contains("entry 0"), "{err}");
    }

    #[test]
    fn node_ids_translate_to_server_numbering() {
        let renumbered = NamespaceTable::from_uris(
            [OPCUA_NS, "urn:server", "urn:new", "urn:acme"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        );
        let ids = node_ids(EXISTING, &renumbered).unwrap();
        assert_eq!(ids, HashSet::from(["ns=3;s=Temperature".to_string()]));
    }

    // --- custom datatypes and methods -----------------------------------

    fn field(name: &str, datatype: &str, value_rank: i32) -> super::super::schema::Field {
        super::super::schema::Field {
            name: name.into(),
            datatype: datatype.into(),
            value_rank,
        }
    }

    /// A pump: `Status` structure (Speed, nested Motor.Current, State enum,
    /// History array, Blob XmlElement), a `Percent` subtype of Double, and
    /// methods Multiply/Reset/Broken.
    fn pump_schema() -> Schema {
        let mut schema = Schema::default();
        schema.types.insert(
            "ns=2;i=3001".into(),
            Shape::Structure {
                fields: vec![
                    field("Speed", "Double", -1),
                    field("Motor", "ns=2;i=3002", -1),
                    field("State", "ns=2;i=3003", -1),
                    field("History", "Double", 1),
                    field("Blob", "XmlElement", -1),
                ],
            },
        );
        schema.types.insert(
            "ns=2;i=3002".into(),
            Shape::Structure {
                fields: vec![field("Current", "Float", -1)],
            },
        );
        schema.types.insert(
            "ns=2;i=3003".into(),
            Shape::Enum {
                cases: vec![(0, "Stopped".into()), (1, "Running".into())],
            },
        );
        schema.types.insert(
            "ns=2;i=3004".into(),
            Shape::Scalar {
                datatype: "float64",
                writable: true,
            },
        );
        schema
            .methods
            .insert("ns=2;s=Multiply".into(), Ok(vec!["int32", "int32"]));
        schema.methods.insert("ns=2;s=Reset".into(), Ok(vec![]));
        schema
            .methods
            .insert("ns=2;s=Broken".into(), Err("not executable".into()));
        schema
    }

    fn method(path: &str, node_id: &str, parent: &str) -> NodeRecord {
        let mut r = var(path, node_id, "Double");
        r.node_class = "Method".into();
        r.datatype = None;
        r.value_rank = None;
        r.parent_node_id = Some(parent.into());
        r
    }

    fn with_schema(records: &[NodeRecord]) -> Outcome {
        write(
            None,
            records,
            &HashSet::new(),
            &server(),
            &options(),
            &pump_schema(),
        )
        .unwrap()
    }

    #[test]
    fn structure_yields_field_points() {
        let out = with_schema(&[var("Pump1/Status", "ns=2;s=Status", "ns=2;i=3001")]);
        let pts = points(&out.text);
        let fields: Vec<(&str, &str, &str)> = pts
            .iter()
            .map(|p| {
                (
                    p["id"].as_str().unwrap(),
                    p["address"]["field"].as_str().unwrap(),
                    p["datatype"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            fields,
            [
                ("speed", "Speed", "float64"),
                ("motor_current", "Motor.Current", "float32"),
                ("state", "State", "int32"),
            ]
        );
        assert!(pts.iter().all(|p| p.get("access").is_none()));
        assert_eq!(pts[0]["address"]["node_id"].as_str(), Some("ns=2;s=Status"));
        assert_eq!(pts[1]["name"].as_str(), Some("Status.Motor.Current"));
        // The enum field carries its labels.
        assert_eq!(pts[2]["map"]["cases"].as_array().unwrap().len(), 2);
        // Array and unsupported fields are reported with their path.
        let reasons: Vec<(&str, &str)> = out
            .skipped
            .iter()
            .map(|s| (s.browse_path.as_str(), s.reason.as_str()))
            .collect();
        assert_eq!(reasons[0], ("Pump1/Status.History", "array field"));
        assert_eq!(reasons[1].0, "Pump1/Status.Blob");
    }

    #[test]
    fn same_field_in_two_structures_gets_extended_ids() {
        let out = with_schema(&[
            var("Pump1/Status", "ns=2;s=P1", "ns=2;i=3002"),
            var("Pump2/Status", "ns=2;s=P2", "ns=2;i=3002"),
        ]);
        let ids: Vec<String> = points(&out.text)
            .iter()
            .map(|p| p["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids, ["pump1_status_current", "pump2_status_current"]);
    }

    #[test]
    fn structure_without_mappable_fields_is_skipped() {
        let mut schema = pump_schema();
        schema.types.insert(
            "ns=2;i=9".into(),
            Shape::Structure {
                fields: vec![field("Blob", "XmlElement", -1)],
            },
        );
        let records = [
            var("X", "ns=2;s=X", "ns=2;i=9"),
            var("Ok", "ns=2;s=Ok", "Double"),
        ];
        let out = write(
            None,
            &records,
            &HashSet::new(),
            &server(),
            &options(),
            &schema,
        )
        .unwrap();
        assert!(
            out.skipped
                .iter()
                .any(|s| s.browse_path == "X" && s.reason.contains("no field"))
        );
    }

    #[test]
    fn enum_variable_gets_int32_and_map() {
        let mut r = var("Pump1/State", "ns=2;s=State", "ns=2;i=3003");
        r.access_level = Some(CURRENT_READ | CURRENT_WRITE);
        let out = with_schema(&[r]);
        let p = &points(&out.text)[0];
        assert_eq!(p["datatype"].as_str(), Some("int32"));
        assert_eq!(p["access"].as_str(), Some("read_write"));
        let map = p["map"].as_table().unwrap();
        assert_eq!(map["as"].as_str(), Some("string"));
        let cases = map["cases"].as_array().unwrap();
        assert_eq!(cases[1]["eq"].as_integer(), Some(1));
        assert_eq!(cases[1]["to"].as_str(), Some("Running"));
    }

    #[test]
    fn derived_type_maps_to_its_base() {
        let out = with_schema(&[var("Level", "ns=2;s=Level", "ns=2;i=3004")]);
        assert_eq!(points(&out.text)[0]["datatype"].as_str(), Some("float64"));
    }

    #[test]
    fn abstract_declared_type_is_skipped() {
        let out = with_schema(&[
            var("A", "ns=2;s=A", "Structure"),
            var("Ok", "ns=2;s=Ok", "Double"),
        ]);
        assert!(
            out.skipped[0].reason.contains("abstract"),
            "{:?}",
            out.skipped
        );
    }

    #[test]
    fn methods_become_call_method_points() {
        let out = with_schema(&[
            method("Machine/Multiply", "ns=2;s=Multiply", "ns=2;s=Machine"),
            method("Machine/Reset", "ns=2;s=Reset", "ns=2;s=Machine"),
            method("Machine/Broken", "ns=2;s=Broken", "ns=2;s=Machine"),
        ]);
        let pts = points(&out.text);
        assert_eq!(pts.len(), 2);
        let multiply = &pts[0];
        assert_eq!(multiply["id"].as_str(), Some("multiply"));
        assert_eq!(multiply["access"].as_str(), Some("call_method"));
        assert!(multiply.get("datatype").is_none());
        let address = multiply["address"].as_table().unwrap();
        assert_eq!(address["object_id"].as_str(), Some("ns=2;s=Machine"));
        assert_eq!(address["method_id"].as_str(), Some("ns=2;s=Multiply"));
        let inputs: Vec<&str> = address["inputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(inputs, ["int32", "int32"]);
        assert!(pts[1]["address"].get("inputs").is_none());
        assert_eq!(out.skipped[0].reason, "not executable");
    }

    #[test]
    fn merge_matches_fields_and_methods() {
        let first = with_schema(&[
            var("Pump1/Status", "ns=2;s=Status", "ns=2;i=3002"),
            method("Machine/Reset", "ns=2;s=Reset", "ns=2;s=Machine"),
        ]);
        // Same nodes again plus a new structure field: only the new field is added.
        let mut schema = pump_schema();
        schema.types.insert(
            "ns=2;i=3002".into(),
            Shape::Structure {
                fields: vec![field("Current", "Float", -1), field("Voltage", "Float", -1)],
            },
        );
        let records = [
            var("Pump1/Status", "ns=2;s=Status", "ns=2;i=3002"),
            method("Machine/Reset", "ns=2;s=Reset", "ns=2;s=Machine"),
        ];
        let out = write(
            Some(&first.text),
            &records,
            &HashSet::new(),
            &server(),
            &options(),
            &schema,
        )
        .unwrap();
        assert_eq!((out.added, out.total), (1, 3));
        assert_eq!(
            points(&out.text)[2]["address"]["field"].as_str(),
            Some("Voltage")
        );
    }

    #[test]
    fn removing_a_node_removes_its_fields_and_methods() {
        let first = with_schema(&[
            var("Pump1/Status", "ns=2;s=Status", "ns=2;i=3001"),
            method("Machine/Reset", "ns=2;s=Reset", "ns=2;s=Machine"),
            var("Ok", "ns=2;s=Ok", "Double"),
        ]);
        let remove: HashSet<String> = ["ns=2;s=Status", "ns=2;s=Reset"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let out = write(
            Some(&first.text),
            &[],
            &remove,
            &server(),
            &options(),
            &pump_schema(),
        )
        .unwrap();
        assert_eq!((out.removed, out.total), (4, 1));
        let ids = node_ids(&first.text, &server()).unwrap();
        assert!(
            ids.contains("ns=2;s=Status") && ids.contains("ns=2;s=Reset"),
            "{ids:?}"
        );
    }
}
