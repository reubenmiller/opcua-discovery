# Tasks

## 1. Discovery

- [x] 1.1 `src/tedge_dot/schema.rs`: `Schema`, `Shape`, `MethodInfo`; batched DataTypeDefinition reads, HasSubtype walk, cycle/depth limits
- [x] 1.2 Method discovery: Executable/UserExecutable, InputArguments → input datatypes
- [x] 1.3 `NodeRecord::parent_node_id` for Methods (engine + TUI)

## 2. Writer

- [x] 2.1 Shapes in the record mapping: derived scalars, enum `int32` + map, structure field expansion with skip reasons
- [x] 2.2 Method points (`call_method`, object_id/method_id/inputs)
- [x] 2.3 Point identity enum for merge and removal; ids from field path
- [x] 2.4 Unit tests with hand-built schemas

## 3. Wiring

- [x] 3.1 `browse --output tedge-dot`: run discovery before writing
- [x] 3.2 TUI: collect Methods, run discovery in the library job, removal by node covers fields/methods

## 4. Tests and checks

- [x] 4.1 Test server: structure (nested + array field), enumeration, derived type, methods (args, no args, unsupported arg, not executable)
- [x] 4.2 E2E tests; load a generated library with tedge-dot `describe` and read field/enum points with `tedge-dot read`

## 5. Docs

- [x] 5.1 README, `openspec/components/{export,tui}.md`; archive the change
