# Design

## Context

opcua-scan already has the two building blocks: a scan that yields `NodeRecord`s (node id,
datatype name, ValueRank, AccessLevel/UserAccessLevel, subscribable, display name, description),
and a precedent for "seed connection from a file and pre-mark nodes in the TUI" (`browse
--config <acquire.toml>`, `AcquireConfig::apply_to`, `TuiOptions::selected_ids`). tedge-dot's
format is defined in `~/git/tedge-dot`: `connector-opcua/src/config.rs` (connection and
`protocol_address` fields, `device_security` precedence), `sdk/src/library.rs` (library keys,
empty-list rule), `sdk/src/descriptor.rs::is_valid_key` (`[A-Za-z0-9_]+`), and openspec change
`opcua-namespace-table` (`namespaces` semantics).

## Goals / Non-Goals

Goals: one connection description for gateway and scan; a library tedge-dot loads unchanged;
re-running or re-editing never loses hand edits.

Non-goals: a dependency on tedge-dot crates (it is not published, and the config surface we need
is small); writing devices or full connector configs; `field`/`index` points for structures and
arrays; reverse-locating library nodes in the TUI tree (see Open Questions).

## Decisions

**Module layout.** New `src/tedge_dot/` with `connection.rs` (parse the connector config, pick
the device, map onto `ConnectionArgs`) and `library.rs` (record → point mapping, ids, render,
merge). Pure functions over `NodeRecord` and `NamespaceTable`, unit-testable without a server.

**Where the mapping runs.** `--tedge-dot-config`/`--device` live in `ConnectionArgs`, so every
connecting command gets them. They are applied in `main.rs` right after `--profile`, by mutating
the parsed `ConnectionArgs` (only fields still unset/default, like `AcquireConfig::apply_to`).
Doing it before dispatch means `browse`'s acquire-config generation (`gen_connection`) sees the
mapped values too. The chosen device's `type` is kept on `ConnectionArgs` (`#[arg(skip)]`) for the
library writer.

**Settings without a CLI flag** (`application_name/uri`, `certificate`/`private_key`,
`create_certificate`, literal password) are `#[arg(skip)]` fields on `ConnectionArgs` that
`build_client` / `resolve_identity` honour. A file-sourced password must not go through
`--password`'s `env:` interpretation, hence the separate literal field. tedge-dot's default is an
empty password for `user` without one; we follow that instead of prompting.

**Policy names.** tedge-dot spells `Aes128_Sha256_RsaOaep`, opcua-scan `Aes128Sha256RsaOaep`;
the mapper accepts tedge-dot's names and full URIs. `Basic128Rsa15`/`Basic256` have no
opcua-scan flag and are rejected with a clear error rather than silently downgraded.

**Borrow the application certificate, not the PKI directory.** tedge-dot vendors a patched
async-opcua-crypto whose trust store uses the OPC UA Part 12 layout (`trusted/certs/`, CA chains,
CRLs) and `own/certs/cert.der` + `own/private/key.pem`; upstream async-opcua 0.18, which
opcua-scan uses, has a flat `trusted/` and `own/cert.der` + `private/private.pem`. Pointing
opcua-scan's `pki_dir` at the gateway's would make it write rejected/trusted certificates into
the gateway's directory in the wrong layout. So only the application certificate, key and
application URI/name are borrowed (explicit `certificate_path`/`private_key_path` on the client
builder), which is what a server checks when it trusts the connector; server-certificate trust
stays in opcua-scan's own PKI. The certificate is only looked up for secured policies, like the
connector itself, and only when it exists (a laptop run has no `/var/lib/tedge-dot`).

**Datatype mapping** is a new function next to the library writer rather than a change to
`canonical_data_type`, which feeds `--datatype` filtering and yields `datetime`/`guid` names
tedge-dot does not have. Duration (`i=290`) and UtcTime (`i=294`) are mapped by node id because
`builtin_datatype_name` only knows ids 1–25.

**Ids.** Shortest unique suffix of the snake_cased browse path, computed over the whole set
(existing ids reserved first), so ids are readable (`temperature`) and only grow where needed.

**Merge with `toml_edit`.** The existing file is parsed with `toml_edit::DocumentMut`, so comments
and layout survive; new points are appended as `[[point]]` tables; removals delete array entries.
Existing points are matched by resolving their `address` (`node_id` string, or
`namespace`+`identifier`) through the library's table to `(namespace URI, identifier)`, compared
with the scanned node's `(URI, identifier)`. Comparing by URI makes the match independent of
server renumbering. The generated header comment is only written when the file is created.

**TUI.** `t` reuses the existing marked-subtree collection (`collect_job`), generalised to return
`NodeRecord`s, which the acquire path maps to `GenNode`s as before. The removal set is computed on
the UI side from the arena: loaded Variable nodes that are neither marked nor under a mark.
Pre-marking reuses `selected_ids`: library points are translated to server node id strings once
the namespace table is read.

## Risks / Trade-offs

- Exported datatypes come from the declared DataType; a variable declared `BaseDataType` holding
  a Double is skipped. Acceptable: such nodes are rare and listed in the header.
- Pre-marked library nodes are only shown marked when their branch is expanded (no paths are
  stored in a library). The status line shows "M/N located" like the acquire flow.
- A merge never updates an existing point's datatype or access when the server changed; the user
  deletes the point to regenerate it. Chosen to protect hand edits.

## Open Questions

- Auto-revealing pre-marked library nodes would need inverse browsing from each node to the root;
  deferred until it is asked for.
- Reading `EngineeringUnits` properties for `unit` needs an extra browse per variable; the field is
  wired but stays empty until the scanner fills `engineering_units`.
