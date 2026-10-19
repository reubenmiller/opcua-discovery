# Proposal

## Why

The generated point library only covers variables of built-in scalar types. Real devices expose
much of their data as custom structures (a pump's `Status` with `Speed`, `Motor.Current`, ...),
their states as enumerations, and their commands as methods. tedge-dot reads all three
(OPC UA connector spec §3.7 structure fields, §3.10 `call_method` points, contract §4.3 value
maps), but today the engineer has to write those points by hand from UaExpert. The server
already describes them: `DataTypeDefinition` for structures and enumerations, `HasSubtype` for
derived types, `InputArguments` for methods.

## What Changes

- **Custom structures:** a variable whose datatype has a `StructureDefinition` (structure,
  optional fields, union) yields one typed point per supported leaf field, with
  `address = { node_id, field = "Motor.Current" }`. Nested structures are followed; array fields
  and unsupported field types are skipped and reported.
- **Enumerations:** a variable (or field) of an enumeration type becomes an `int32` point with a
  `map` from each value to its name (`as = "string"` for codes the definition does not list).
- **Derived types:** a custom datatype without a definition is resolved through its supertypes
  (`HasSubtype`) to the built-in type it derives from (a `Percent` subtype of Double →
  `float64`; a subtype of Enumeration → `int32`).
- **Methods:** a Method node yields an `access = "call_method"` point with
  `address = { object_id, method_id, inputs }`, the inputs read from its `InputArguments`.
  Methods that are not executable or take unsupported arguments are skipped and reported.
- Merging and TUI removal understand field points and method points; ids extend with the field
  path; the TUI's `t` includes marked methods.
- `NodeRecord` gains `parent_node_id` for Method records (JSON/NDJSON exports), so the object a
  method is called on is known.

Not in this change: variables declared with the abstract `Structure`/`BaseDataType` (their
concrete type is only visible in the value), array elements (`index`), `mode = "raw"` points,
method output arguments (tedge-dot reads them per call).

## Capabilities

### New Capabilities
<!-- none: everything extends the point library -->

### Modified Capabilities
- `tedge-dot-point-library`: structure field points, enumeration maps, derived types, method
  points, and the skip, id, merge and TUI rules that change with them.

## Impact

- Code: new `src/tedge_dot/schema.rs` (type and method discovery over the session),
  `src/tedge_dot/library.rs` (field/method/enum points, point keys), `src/export/record.rs` +
  `src/scanner/engine.rs` + TUI (`parent_node_id`), `src/cli/browse.rs` and TUI (run discovery
  before writing; collect Methods).
- Tests: test server gains a structure type, an enumeration, a derived type and methods;
  unit and e2e tests; generated libraries checked with tedge-dot.
- Users: additive. Libraries generated before still merge; new runs add the newly mappable
  nodes.
