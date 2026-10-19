# Proposal

## Why

tedge-dot's OPC UA connector reads a device's data points from a **point library**: a TOML file
listing the nodes of a device type, with each point's datatype, access and node id. Today an
engineer writes it by hand from UaExpert screenshots. opcua-scan already discovers exactly these
facts (node ids, data types, access levels, subscribability, the namespace table) but can only
export them as JSON, NDJSON or CSV, and it has to be given the connection a second time with its
own flags, although the gateway's tedge-dot connector config already holds it.

tedge-dot deliberately keeps discovery out of its executable; its namespace-table change names
"the planned address and namespace discovery tool" as the producer of libraries and tables.
opcua-scan is that tool.

## What Changes

- **Connect from a tedge-dot connector config.** `--tedge-dot-config <FILE>` (with `--device
  <NAME>`, optional when the file has exactly one device) fills opcua-scan's connection settings
  from the file's `[connection]` table and the device's `protocol_address`: endpoint, security
  policy and mode (with tedge-dot's precedence rules), user/password or password file, X.509 user
  identity, the gateway's application certificate, key, name and URI (so a server that trusts the
  connector trusts the scan), and `trust_any_server_certificate`. Server-certificate trust stays
  in opcua-scan's own PKI. Relative paths resolve against the config file's directory.
  Explicit CLI flags still win. Available on every command that connects.
- **Write a point library from a scan.** `browse --output tedge-dot` writes the matched nodes as
  a tedge-dot OPC UA point library (`[library]` + `[[point]]`) to `--file` or stdout. Datatypes,
  access and `subscribe` are derived from the node's attributes, names/descriptions are carried
  over, ids are unique snake_case names derived from the browse path, and `[library] namespaces`
  records the server's namespace table. Nodes that cannot be points are skipped and listed in a
  header comment. An empty result is an error, never an empty library.
- **Write a point library from the TUI.** A new key (`t`) writes the marked nodes as a point
  library. `--library <FILE>` names the library; when it already exists, its points are
  pre-marked in the tree, and saving merges into the file instead of replacing it.
- **Merging never destroys hand edits.** Existing points are kept verbatim (including `transform`,
  `map`, `meta`, ...), new nodes are appended, and in the TUI a point is removed only when its node
  is shown in the tree and unmarked. Node ids of new points are written in the library's own
  namespace table, extending it when the server has a namespace the table lacks.

Not in this change: other protocols, `nsu=` addresses (dropped by tedge-dot), writing a full
connector config or a device entry, structure fields (`field`) and array elements (`index`).

## Capabilities

### New Capabilities
- `tedge-dot-connection`: connecting opcua-scan with the settings of a device in a tedge-dot OPC
  UA connector config: device selection, the mapping of each setting, precedence against CLI
  flags, and the settings opcua-scan refuses or ignores.
- `tedge-dot-point-library`: producing a tedge-dot OPC UA point library from discovered nodes, by
  CLI export and from the TUI: the file format, the per-node mapping and skip rules, point ids,
  the namespace table, and merging into an existing library.

### Modified Capabilities
<!-- none: openspec/specs/ has no capabilities yet; the reverse-engineered docs are updated
     alongside (components/cli.md, export.md, tui.md) -->

## Impact

- Code: new `src/tedge_dot/` module (config mapping, library writer/merger); `src/cli/mod.rs`
  (connection flags, `Encoding::TedgeDot`), `src/cli/browse.rs`, `src/opcua/client.rs`
  (application identity and certificate from the config, literal password), `src/tui/mod.rs`
  (`t` key, `--library` pre-marking and merge).
- Dependencies: none new (`toml`, `toml_edit` are already used).
- Tests: unit tests for mapping, ids, datatypes and merge; CLI e2e tests against the in-process
  server (connect via a tedge-dot config, `--output tedge-dot`, merge).
- Docs: README, `openspec/components/{cli,export,tui}.md`.
- Users: purely additive; existing flags and outputs are unchanged.
