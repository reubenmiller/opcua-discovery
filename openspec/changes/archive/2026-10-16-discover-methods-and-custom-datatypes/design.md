# Design

## Context

The library writer (`src/tedge_dot/library.rs`) is pure: `NodeRecord`s + namespace table in,
TOML out. A record has the declared DataType as a built-in name or a NodeId string, and no
information about structure layouts, enumerations, supertypes or method arguments. tedge-dot
reads structure layouts from `DataTypeDefinition` itself (connector spec §3.7) and needs only
`node_id` + `field` + `datatype`; method points declare `inputs` (§3.10).

## Decisions

**A discovery pass before writing.** New `src/tedge_dot/schema.rs` takes the session and the
records and returns a `Schema`: for every non-built-in DataType used by a record, a resolved
`Shape` (`Scalar(datatype, writable)`, `Enum(cases)`, `Structure(fields)`, `Unsupported(reason)`),
and for every Method record its `MethodInfo` (executable, input datatypes or reason). The
writer takes `&Schema` and stays pure and unit-testable with hand-built schemas. The pass runs in
`browse` before writing and in the TUI's library job.

**Resolution, batched and cached.** Unknown type ids are resolved breadth-first: one Read of
`DataTypeDefinition` (attribute 23) for all pending types; types without a definition get one
Browse of inverse `HasSubtype` (i=45) for their supertype; field types found in definitions join
the next round. A depth limit (16) and a visited set stop cycles. A server without attribute 23
(OPC UA 1.03) returns `BadAttributeIdInvalid`, which degrades to supertype resolution.

**Shapes.** `StructureDefinition` with `StructureType` Structure / StructureWithOptionalFields /
Union → fields (name, type, ValueRank); SubtypedValues kinds → Unsupported. `EnumDefinition` →
cases (value, name). Built-in ids 1–15 and the string-rendered ones (LocalizedText, NodeId,
QualifiedName, StatusCode, ExpandedNodeId) map as today; `Enumeration` (i=29) → int32 without a
map; abstract `Structure` (22), `BaseDataType` (24), `Number` (26), `Integer` (27), `UInteger` (28)
→ Unsupported.

**Field expansion** happens in the writer: a structure shape expands recursively into
`(field path, leaf shape)` pairs with a depth limit (8); array fields and unsupported leaves are
reported as `Skipped` with `browse_path + "." + field path`. Field points get no `access`
(tedge-dot makes them read-only and rejects `access` with `field`).

**Methods.** The parent comes from the browse: the engine and the TUI know the node a reference
was followed from, and store it in `NodeRecord::parent_node_id` for Method records only (keeps
JSON exports of variables unchanged). Discovery reads `Executable`/`UserExecutable` and browses
the method's `HasProperty` children (`browse_properties`) for `InputArguments`, whose value is an
array of `Argument` ExtensionObjects (`inner_as::<Argument>()`).

**Point identity** becomes an enum used for merge and removal:
`Variable { node: (uri, ident), field: Option<String> }` and
`Method { object: (uri, ident), method: (uri, ident) }`. Removal by node removes `Variable`s with
that node and `Method`s with that method.

**Enum maps** use `as = "string"` so a code missing from the definition is still published (as its
number in text) instead of turning the sample bad; all outputs stay strings, as §4.3 requires.

## Risks / Trade-offs

- One Read + Browse round per type-nesting level; typical servers need 2–3 rounds per scan.
- Field points of large structures can be many; they are listed by path and easy to delete.
- A variable declared abstract (`Structure`) is skipped; resolving it needs a value read and the
  encoding → `HasEncoding` → DataType walk tedge-dot does at runtime (future work).
