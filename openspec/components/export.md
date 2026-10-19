# Component: Export (`src/export/`)

> Renders/serializes discovered nodes. Defines the machine-readable record contract. Evidence: **[Confirmed]**.

## `record.rs` — `NodeRecord` (the export contract)

```jsonc
{
  // Always present:
  "browse_path":  "Factory/Line1/Motor/Speed",   // namespace-URI-qualified, "/"-separated
  "display_name": "Speed",
  "browse_name":  "Speed",                        // namespace-qualified
  "node_id":      "ns=2;s=Foo",
  "namespace_uri":"urn:example",
  "node_class":   "Variable",                     // Object | Variable | Method | ...
  "subscribable": true,                            // is_subscribable() heuristic

  // Optional (omitted when None via serde skip):
  "datatype":           "Double",
  "value_rank":         -1,
  "access_level":       1,                          // bitfield: bit0=read, bit1=write
  "user_access_level":  1,
  "historizing":        false,
  "validation_status":  "ok",                      // set by --validate-subscription
  "current_value":      "1500.0",                  // set by --preview-values
  "engineering_units":  "rpm",
  "description":        "Motor speed"
}
```

## `mod.rs` — writers
- `render_display(records, format)` — sort then print human format to stdout:
  - `Format::Tree` → `tree::render`, `Format::Table` → `table::render`, `Format::Json` → pretty JSON.
- `write_export(records, encoding, file)` — sort then write machine format to file/stdout:
  - `Encoding::Json` (array), `Ndjson` (one object/line), `Csv` (fixed columns).
  - `Encoding::TedgeDot` is refused here (it needs the namespace table and merge semantics);
    `browse` and the TUI write it through `src/tedge_dot/library.rs` after
    `src/tedge_dot/schema.rs::discover` has read the custom datatypes (DataTypeDefinition,
    HasSubtype) and method arguments: record → point(s) (scalar, enum + map, structure fields,
    `call_method`), skip reasons, shortest-unique snake_case ids, and a `toml_edit` merge keyed by
    `PointKey` (node + field, or object + method, by namespace URI).
- `NodeRecord::parent_node_id` is set for Methods only (the object they were browsed from).
- **CSV fixed columns:** `browse_path, display_name, browse_name, node_id, namespace_uri, node_class,
  datatype, value_rank, access_level, user_access_level, historizing, subscribable, validation_status,
  current_value, engineering_units, description` (avoids variable-columns-per-row).

## `table.rs`
`render(records) -> String` — aligned columns `PATH | CLASS | DATATYPE | ACCESS | SUBSCR | NODEID`.
`ACCESS` encodes `R`(read)/`W`(write)/`H`(historizing).

## `tree.rs`
`render(records) -> String` — Unicode tree (`├─`/`└─`); leaf annotations `[datatype]`, `[access]`,
`[Subscribable]`, `= value` when present.

## Invariants
- stdout carries only export/display output (logs → stderr).
- Optional fields are omitted in JSON when `None` but always present (fixed) in CSV.

## Extension points
- New export encoding → add an `Encoding` variant + branch in `write_export`.
- New record field → add to `NodeRecord` (+ CSV column list + table/tree rendering as desired).
