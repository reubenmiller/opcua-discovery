# Spec Delta

## REMOVED Requirements

### Requirement: Nodes that cannot be points are skipped
**Reason**: it skipped custom structures and enumerations and dropped methods silently; replaced by "Skip rules", which keeps the array, folder and access rules and reports only what still cannot be mapped.
**Migration**: none; nodes that were skipped before now become points when they can be mapped.

## ADDED Requirements

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
