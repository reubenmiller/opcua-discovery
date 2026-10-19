# tedge-dot-point-library Specification

## Purpose
Produces a tedge-dot OPC UA point library (contract §3.4, OPC UA connector spec §3.4/§3.7/§3.9)
from nodes discovered by a scan or marked in the TUI, so a device type's point list is generated
from the server instead of written by hand, and can be refined over several sessions.

## Requirements

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

### Requirement: Skip rules
A Variable SHALL be skipped when it is an array (ValueRank 0 or greater), its datatype cannot be resolved to a supported shape, or it is neither readable nor writable. A Method SHALL be skipped by the method rules.

#### Scenario: Array variable
- **WHEN** a scan matches a Double variable with ValueRank 1
- **THEN** no point is written for it and the header lists it with reason `array`

#### Scenario: Folders
- **WHEN** a scan without `--type` matches folders and variables
- **THEN** only the variables become points and the folders are not listed

#### Scenario: Abstract declared type
- **WHEN** a variable is declared with DataType `Structure` (i=22)
- **THEN** no point is written and the reason says the declared datatype is abstract

### Requirement: Skipped nodes are reported
Skipped Variables, structure fields and Methods SHALL be listed in a comment at the top of a new library with node id, browse path (plus the field path) and reason, at most 50 and then the count of the rest. Other node classes, and the `InputArguments`/`OutputArguments` properties of methods (datatype `Argument`), SHALL be skipped silently.

#### Scenario: Skipped variable listed
- **WHEN** a scan matches an array variable and a scalar Double
- **THEN** the new library's header lists the array variable with reason `array`

#### Scenario: Method argument properties
- **WHEN** a scan matches a method and its `InputArguments` property
- **THEN** the property is neither a point nor listed as skipped

### Requirement: Derived datatypes
A datatype that is neither built-in nor has a `DataTypeDefinition` SHALL be resolved through its `HasSubtype` supertypes to the first built-in type, and mapped like that type. A type deriving from `Enumeration` (i=29) SHALL map to `int32`.

#### Scenario: Subtype of Double
- **WHEN** a variable has the custom DataType `Percent`, a subtype of Double without a definition
- **THEN** its point has `datatype = "float64"`

### Requirement: Enumerations
A variable or structure field whose datatype has an `EnumDefinition` SHALL become an `int32` point with `map = { cases = [{ eq = <value>, to = "<name>" }, ...], as = "string" }`, one case per enumeration field in definition order.

#### Scenario: Pump state
- **WHEN** a variable has an enumeration DataType with fields `Stopped = 0`, `Running = 1`, `Fault = 2`
- **THEN** its point has `datatype = "int32"` and map cases `0 → "Stopped"`, `1 → "Running"`, `2 → "Fault"` with `as = "string"`

#### Scenario: Writable state
- **WHEN** that variable is readable and writable
- **THEN** its point also has `access = "read_write"`

### Requirement: Structure fields
A scalar variable whose datatype has a `StructureDefinition` of kind Structure, StructureWithOptionalFields or Union SHALL yield one point per leaf field whose type maps to a tedge-dot datatype (built-in, derived or enumeration), with `address = { node_id = "...", field = "<dotted path>" }`. Nested structure fields SHALL be followed. Field points SHALL have no `access` key.

#### Scenario: Nested structure
- **WHEN** variable `Pump1/Status` has a structure type with fields `Speed` (Double) and `Motor` (a structure with `Current` Float)
- **THEN** two points are written with `field = "Speed"` (`float64`) and `field = "Motor.Current"` (`float32`), both addressing the `Status` node

#### Scenario: Unsupported fields
- **WHEN** the structure also has an array field `History` and a field of type `XmlElement`
- **THEN** no point is written for either, and both are listed as skipped with the variable's path plus the field path

#### Scenario: Subtyped values
- **WHEN** the structure is of kind StructureWithSubtypedValues or UnionWithSubtypedValues
- **THEN** no point is written for the variable and the reason names the structure kind

#### Scenario: Nothing mappable
- **WHEN** no field of the structure maps
- **THEN** the variable is skipped with a reason saying no field could be mapped

### Requirement: Method points
A Method SHALL become a point with `access = "call_method"`, `name`, `description` when present, and `address = { object_id = "<parent>", method_id = "<method>", inputs = [...] }`, where `object_id` is the node the method was browsed from and `inputs` lists the mapped datatype of each `InputArguments` entry in order (omitted when there are none). Input types SHALL be limited to `bool`, the integers, `float32`, `float64` and `string` (an enumeration maps to `int32`).

#### Scenario: Multiply
- **WHEN** object `Machine` has method `Multiply` with input arguments `a: Int32`, `b: Int32`
- **THEN** its point has `access = "call_method"` and `address = { object_id = "ns=2;s=Machine", method_id = "ns=2;s=Machine.Multiply", inputs = ["int32", "int32"] }`

#### Scenario: No arguments
- **WHEN** method `Reset` has no `InputArguments` property
- **THEN** its point has no `inputs` key

#### Scenario: Unsupported argument
- **WHEN** a method has an input argument of a structure type or an array argument
- **THEN** no point is written and the reason names the argument

#### Scenario: Not executable
- **WHEN** a method's Executable or UserExecutable attribute is false
- **THEN** no point is written and the reason is `not executable`

### Requirement: Field and method point ids
The id of a field point SHALL be built like a variable's id from its browse path extended by the field path segments (`Pump1/Status` + `Motor.Current` → `motor_current`, extended up the path on collisions). Method points SHALL be named from their browse path like variables.

#### Scenario: Same field in two pumps
- **WHEN** `Pump1/Status` and `Pump2/Status` both yield field `Speed`
- **THEN** the ids are `pump1_status_speed` and `pump2_status_speed`

### Requirement: Points are matched by what they address
For merging, a variable point SHALL be identified by its node and its `field` (none for a whole-node point), and a method point by its `object_id` and `method_id`, each resolved through the library's namespace table. A node SHALL count as covered when a point with the same identity exists. In the TUI, unmarking a loaded node SHALL remove every point addressing it: its whole-node point, its field points, and method points whose `method_id` is that node.

#### Scenario: New field of a known node
- **WHEN** the library has a point for `Status` field `Speed` and the scan also yields field `Motor.Current`
- **THEN** only the `Motor.Current` point is appended

#### Scenario: Unmark a structure variable
- **WHEN** the library has two field points of `Status`, the user unmarks the loaded `Status` node in the TUI and presses `t`
- **THEN** both field points are removed

#### Scenario: Method already in the library
- **WHEN** the library has a `call_method` point for `Machine`/`Multiply` and the scan finds the same method
- **THEN** no second point is appended

### Requirement: Methods in scans and the TUI
`NodeRecord`s of Methods SHALL carry `parent_node_id`, the node they were browsed from. The TUI's `t` SHALL include Methods under marked nodes and marked Method nodes themselves.

#### Scenario: JSON export
- **WHEN** `browse --output json` matches method `Multiply` under `Machine`
- **THEN** its record has `"parent_node_id": "ns=2;s=Machine"`

#### Scenario: TUI
- **WHEN** the user marks `Machine` and presses `t`
- **THEN** the library gets a `call_method` point for each mappable method of `Machine`
