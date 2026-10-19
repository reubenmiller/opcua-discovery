# Tasks

## 1. Connection from a tedge-dot config

- [x] 1.1 Add `--tedge-dot-config` / `--device` and the `#[arg(skip)]` `ConnectionExtras` (literal password, application name/URI, certificate/key, device type) to `ConnectionArgs`; conflict with `browse --config`
- [x] 1.2 `src/tedge_dot/connection.rs`: parse the file, check `[connector] protocol`, select the device, map policy/mode (tedge-dot precedence and names), identity, gateway application certificate and trust, resolve relative paths
- [x] 1.3 Apply the mapping in `main.rs` after `--profile`, CLI values winning
- [x] 1.4 Honour the new fields in `build_client` and `resolve_identity`
- [x] 1.5 Unit tests: device selection, precedence, policy names, identity errors, path resolution, CLI override

## 2. Point library writer

- [x] 2.1 `src/tedge_dot/library.rs`: record → point (datatype, access, subscribe, name, description, unit) and skip reasons
- [x] 2.2 Point ids: snake_case, shortest unique path suffix, numeric fallback, reserved existing ids
- [x] 2.3 Render a new library (header comment, `[library]`, `[[point]]`), error on an empty result
- [x] 2.4 Merge into an existing library with `toml_edit`: resolve existing addresses through its table, skip covered nodes, append new points in the library's indexes, extend the table, remove a given set
- [x] 2.5 Unit tests for mapping, ids, render and merge

## 3. CLI export

- [x] 3.1 `Encoding::TedgeDot` and `--library-type`; write via the library writer in `browse` (stdout or `--file`, merge when the file exists)
- [x] 3.2 E2E tests against the in-process server: export, tedge-dot-config connect, merge keeps hand edits

## 4. TUI

- [x] 4.1 `--library` option, pre-mark library nodes via `selected_ids`
- [x] 4.2 Generalise `collect_job` to return `NodeRecord`s; `t` key writes/merges the library with the removal set; status and help text

## 5. Docs and checks

- [x] 5.1 README section; `openspec/components/{cli,export,tui}.md`
- [x] 5.2 `just ci` passes (fmt, clippy -D warnings, tests); `openspec validate --strict`
