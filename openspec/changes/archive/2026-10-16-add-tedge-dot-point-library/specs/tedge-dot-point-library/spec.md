# Spec Delta

## Purpose

Produces a tedge-dot OPC UA point library (contract §3.4, OPC UA connector spec §3.4/§3.7/§3.9)
from nodes discovered by a scan or marked in the TUI, so a device type's point list is generated
from the server instead of written by hand, and can be refined over several sessions.

## ADDED Requirements

### Requirement: CLI export as a point library
`browse --output tedge-dot` SHALL write the matched nodes as a point library to `--file`, or to stdout when `--file` is absent. The library SHALL contain only a `[library]` table and `[[point]]` entries, and SHALL load in tedge-dot without changes.

#### Scenario: Export to a file
- **WHEN** `opcua-scan browse <endpoint> --depth unlimited --type Variable --output tedge-dot --file acme.toml` finds the variables `Temperature` (Double) and `Running` (Boolean)
- **THEN** `acme.toml` has `[library] protocol = "opcua"` and two `[[point]]` entries with `datatype = "float64"` and `datatype = "bool"`

#### Scenario: Export to stdout
- **WHEN** `--output tedge-dot` is given without `--file`
- **THEN** the library is printed to stdout and the scan summary to stderr

### Requirement: Library table
`[library]` SHALL carry `protocol = "opcua"`, `namespaces` (the server's `NamespaceArray` in index order), and `type` when one is known: `--library-type <TYPE>`, else the `type` of the device chosen with `--tedge-dot-config`. Without either, `type` SHALL be omitted.

#### Scenario: Type from the device
- **WHEN** `--tedge-dot-config` selects a device with `type = "acme-line"` and no `--library-type` is given
- **THEN** the library has `type = "acme-line"`

#### Scenario: Namespace table
- **WHEN** the server's `NamespaceArray` is `["http://opcfoundation.org/UA/", "urn:server", "urn:acme"]`
- **THEN** the library has `namespaces = ["http://opcfoundation.org/UA/", "urn:server", "urn:acme"]` and every `node_id` uses those indexes

### Requirement: Point fields
Each point SHALL have `id`, `name` (the display name), `datatype` and `address = { node_id = "<ns=..;..>" }`, plus `description` when the node has one and `unit` when the scan knows the node's engineering units.

#### Scenario: Point from a Double variable
- **WHEN** the node `ns=2;s=Temperature` with display name `Temperature` and description `Motor temperature` is exported
- **THEN** its point has `name = "Temperature"`, `description = "Motor temperature"`, `datatype = "float64"`, `address = { node_id = "ns=2;s=Temperature" }`

### Requirement: Datatype mapping
The datatype SHALL map Boolean→bool, SByte→int8, Byte→uint8, Int16/UInt16/Int32/UInt32/Int64/UInt64→int16/uint16/int32/uint32/int64/uint64, Float→float32, Double and Duration→float64, String, DateTime, UtcTime, Guid, LocalizedText, NodeId, ExpandedNodeId, QualifiedName and StatusCode→string, and ByteString→bytes.

#### Scenario: DateTime variable
- **WHEN** a scalar DateTime variable is exported
- **THEN** its point has `datatype = "string"`

#### Scenario: ByteString variable
- **WHEN** a scalar ByteString variable is exported
- **THEN** its point has `datatype = "bytes"`

### Requirement: Nodes that cannot be points are skipped
A node SHALL be skipped when it is not a Variable, is an array (ValueRank 0 or greater), has a datatype outside the mapping (structures, enumerations, abstract types), or is neither readable nor writable. Skipped Variables SHALL be listed in a comment at the top of the library with node id, browse path and reason; non-Variables SHALL be skipped silently. The comment SHALL list at most 50 nodes and then the count of the rest.

#### Scenario: Structure variable
- **WHEN** a scan matches a variable whose datatype is a custom structure
- **THEN** no point is written for it and the header comment lists it with reason `datatype ns=2;i=3001 has no tedge-dot datatype`

#### Scenario: Array variable
- **WHEN** a scan matches a Double variable with ValueRank 1
- **THEN** no point is written for it and the header lists it with reason `array`

#### Scenario: Folders
- **WHEN** a scan without `--type` matches folders and variables
- **THEN** only the variables become points and the folders are not listed

### Requirement: Access and subscribe
Access SHALL be derived from AccessLevel, combined with UserAccessLevel when the server reports it: readable and writable → `access = "read_write"`, writable only → `access = "write"`, readable only → no `access` key. A writable node whose datatype tedge-dot cannot write (every string-mapped type except String, and ByteString) SHALL be exported as read-only. A readable node that is not subscribable SHALL get `subscribe = false`.

#### Scenario: Writable setpoint
- **WHEN** a Double variable has AccessLevel CurrentRead|CurrentWrite
- **THEN** its point has `access = "read_write"`

#### Scenario: User may only read
- **WHEN** AccessLevel is CurrentRead|CurrentWrite and UserAccessLevel is CurrentRead
- **THEN** its point has no `access` key

#### Scenario: Writable DateTime
- **WHEN** a DateTime variable has AccessLevel CurrentRead|CurrentWrite
- **THEN** its point has no `access` key

### Requirement: Point ids
A point id SHALL consist of lowercase ASCII letters, digits and `_`, derived from the node's browse name in snake_case (`MotorSpeed` → `motor_speed`). When ids collide, each colliding id SHALL be prefixed with its parent's browse name, repeated up the path until unique; ids still equal after the whole path SHALL get a numeric suffix `_2`, `_3`, .... Ids SHALL be unique within the library, including points already in it.

#### Scenario: Unique names
- **WHEN** the scan finds `Line1/Motor/Temperature` and `Line1/Motor/Speed`
- **THEN** the ids are `temperature` and `speed`

#### Scenario: Same name in two branches
- **WHEN** the scan finds `Line1/Motor/Temperature` and `Line2/Motor/Temperature`
- **THEN** the ids are `line1_motor_temperature` and `line2_motor_temperature`

#### Scenario: Non-ASCII and punctuation
- **WHEN** a node's browse name is `Drehzahl (U/min)`
- **THEN** its id is `drehzahl_u_min`

### Requirement: Empty result is an error
When no node yields a point, the command SHALL fail with an error that says no point was found and how many nodes were skipped, and SHALL NOT create or change the target file.

#### Scenario: Only folders matched
- **WHEN** `--output tedge-dot --file acme.toml` matches only Objects
- **THEN** the command exits with an error and `acme.toml` is not created

### Requirement: Merge into an existing library
When the target file exists, it SHALL be read as a point library and merged into, preserving its comments, formatting, `[library]` keys and every existing point verbatim. A node already covered by an existing point (its address resolves to the same node) SHALL NOT be added again. New points SHALL be appended. A target that exists but is not an OPC UA point library SHALL be an error and SHALL NOT be changed.

#### Scenario: Hand edits survive
- **WHEN** `acme.toml` has a point `boiler_temp` for `ns=2;s=Temperature` with a hand-written `transform`, and the scan finds `Temperature` and `Speed`
- **THEN** after the export `boiler_temp` is unchanged with its `transform`, and one new point for `Speed` is appended

#### Scenario: Not a library
- **WHEN** `--file` names an existing JSON export
- **THEN** the command fails with an error saying the file is not an OPC UA point library, and the file is unchanged

### Requirement: Merge uses the library's namespace table
Existing node ids SHALL be resolved through the library's `namespaces` table when present (literally when absent). New points SHALL be written in the library's table's indexes; a namespace missing from the table SHALL be appended to it. A library without a table SHALL get the server's table.

#### Scenario: Server renumbered
- **WHEN** the library's table has `urn:acme` at index 2 and its point `ns=2;s=Temperature`, and the server now lists `urn:acme` at index 3
- **THEN** `Temperature` (server `ns=3;s=Temperature`) counts as covered, and a new node `ns=3;s=Speed` is written as `ns=2;s=Speed`

#### Scenario: New namespace
- **WHEN** the library's table has three entries and a new node is in namespace `urn:vendor` that the table lacks
- **THEN** `urn:vendor` is appended as entry 3 and the node is written with `ns=3`

### Requirement: TUI writes a point library
In the TUI, `t` SHALL write the marked nodes (each mark covering its whole subtree; with nothing marked, the loaded Variables) as a point library to `--library <FILE>` (default `point-library.toml`), applying the same mapping, skip rules and merge as the CLI export. The status line SHALL report the number of points added, removed and skipped, and the path.

#### Scenario: Save marked subtree
- **WHEN** the user marks `Line1` and presses `t`
- **THEN** every mappable Variable below `Line1` is written to the library and the status line shows the counts and the path

### Requirement: TUI edits an existing library
When `--library` names an existing library, the TUI SHALL mark each node that one of its points resolves to as the node appears in the tree. On `t`, a point SHALL be removed when its node is loaded in the tree and neither marked nor under a marked node; points whose node was never loaded SHALL be kept.

#### Scenario: Unmark removes
- **WHEN** the library has points for `Temperature` and `Speed`, the user expands `Motor`, unmarks `Speed` and presses `t`
- **THEN** the `Speed` point is removed and `Temperature` is kept unchanged

#### Scenario: Unvisited points stay
- **WHEN** the library has a point for a node in a branch the user never expands, and the user presses `t`
- **THEN** that point is kept
