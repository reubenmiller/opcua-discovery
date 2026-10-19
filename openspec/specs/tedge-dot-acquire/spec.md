# tedge-dot-acquire Specification

## Purpose
Lets `opcua-scan acquire` collect data for the points a tedge-dot connector config defines for a
device, so the point list is maintained once, in tedge-dot's format, and used by both tools.

## Requirements

### Requirement: Acquire from a tedge-dot config
`opcua-scan acquire` SHALL accept `--tedge-dot-config <FILE>` and `--device <NAME>` (device selection as for every connecting command). The connection SHALL be taken from the config as for every connecting command. `--tedge-dot-config` SHALL conflict with `--config-dir`; `--device` SHALL require `--tedge-dot-config`.

#### Scenario: Acquire a device's points
- **WHEN** `opcua-scan acquire --tedge-dot-config opcua.toml --device plc-7 --duration 5s` runs and `plc-7` has points `temperature` and `speed`
- **THEN** samples for `temperature` and `speed` are written to stdout for 5 seconds

#### Scenario: No acquire file needed
- **WHEN** no `--config` is given
- **THEN** the run needs no other file than the tedge-dot config (and the libraries it names)

### Requirement: Points come from libraries and inline points
The device's points SHALL be its `points_from` libraries in order followed by its inline `[[device.point]]` entries, a later point with the same `id` replacing an earlier one.

#### Scenario: Library and override
- **WHEN** the device has `points_from = ["pump"]`, `pump.toml` defines `flow` and `level`, and an inline point redefines `level`
- **THEN** `flow` from the library and the inline `level` are acquired, each once

### Requirement: Libraries are found like tedge-dot finds them
A library name SHALL be looked up as `<dir>/opcua/<name>.toml` in the directories of `[connector] point_library_path`, else `TEDGE_DOT_POINT_LIBRARY_PATH` (colon-separated), else `/etc/tedge/plugins/ot/points.d` then `/usr/share/tedge-dot/points.d`. A reference ending in `.toml` or containing `/` SHALL be a path, relative to the config file. A missing library SHALL fail the run naming it.

#### Scenario: Path reference
- **WHEN** `points_from = ["libs/pump.toml"]`
- **THEN** `pump.toml` is read from the `libs` directory next to the config file

#### Scenario: Unknown library
- **WHEN** `points_from = ["nonexistent"]`
- **THEN** the run fails with an error naming `nonexistent` and the paths that were tried

### Requirement: Namespace translation
A point's configured namespace index SHALL name a URI in its table (a library point's library `namespaces`, else the device's `protocol_address.namespaces`; an inline point's device table), and the node SHALL be read at the server's current index for that URI. Namespace 0, and points without a table, SHALL use the index as written.

#### Scenario: Renumbered server
- **WHEN** a library point has `node_id = "ns=2;s=Flow"`, the library's table lists `urn:acme` at index 2, and the server lists it at index 3
- **THEN** the node `ns=3;s=Flow` is acquired

### Requirement: Namespace failures stop the run
A URI the server does not list SHALL fail the run naming the point's node and the URI, without reading any node at the configured index. An index outside its table SHALL fail naming the point.

#### Scenario: Namespace missing on the server
- **WHEN** the table's URI for a point is not in the server's namespace array
- **THEN** the run fails with an error naming the node and the URI, and no node is read at the configured index

#### Scenario: Index outside the table
- **WHEN** a point has `ns=5;i=1` and its table has three entries
- **THEN** the run fails with an error naming the point and the index

### Requirement: Mode per point
A point with `subscribe = false` SHALL be polled; every other point SHALL use mode `auto` (subscribed when the server can monitor it, else polled). `--poll-only` and an extras file's `[acquire]` settings SHALL apply as for any acquire run.

#### Scenario: Polled point
- **WHEN** point `run_hours` has `subscribe = false`
- **THEN** it is routed to the poll task

### Requirement: Points acquire skips
Points with `enabled = false` SHALL be left out silently. Points with `access = "call_method"` or `"write"`, and points whose address has `field` or `index`, SHALL be skipped with a warning naming the point and the reason. When no point remains, the run SHALL fail.

#### Scenario: Method and field points
- **WHEN** the device has a `call_method` point and a point with `field = "Motor.Current"`
- **THEN** both are skipped with warnings, and the other points are acquired

#### Scenario: Nothing left
- **WHEN** every point of the device is skipped
- **THEN** the run fails with an error saying the device has no point acquire can read

### Requirement: Samples keyed by point id
Samples SHALL be keyed by the point `id` in every output format.

#### Scenario: tedge format
- **WHEN** point `flow` is sampled with output format `tedge`
- **THEN** the sample is `{"time": ..., "flow": <value>, ...}`

### Requirement: Optional acquire extras file
With `--tedge-dot-config`, `--config <FILE>` SHALL be read for `[acquire]`, `[output]` and `[[task]]` only; any other table SHALL be an error naming it. Without `[[task]]` entries (or without the file), the run SHALL use a poll task with the device's `poll_interval`, else `[connector] poll_interval`, else 2s, and a subscribe task whose sampling interval is the device's `sampling_interval`, else the connector's, else the poll interval.

#### Scenario: History from an extras file
- **WHEN** `--config extras.toml` has one `[[task]] mode = "history"` with a `start`
- **THEN** the device's points' history is read and no poll or subscribe task runs

#### Scenario: Extras file with a selection
- **WHEN** the extras file has `[selection]`
- **THEN** the run fails with an error saying only `[acquire]`, `[output]` and `[[task]]` are allowed with `--tedge-dot-config`

#### Scenario: Defaults from tedge-dot
- **WHEN** the device has `poll_interval = "5s"` and no extras file is given
- **THEN** polled points are read every 5 seconds

### Requirement: The tedge-dot config is never written
An acquire run from a tedge-dot config SHALL NOT modify the connector config or any library (reconstructed browse paths are not cached).

#### Scenario: Read-only
- **WHEN** an acquire run from a tedge-dot config finishes
- **THEN** the config and library files are byte-for-byte unchanged
