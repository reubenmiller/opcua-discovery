# Tasks

## 1. Policies and modes

- [x] 1.1 `SecurityPolicyArg`: add `Basic128Rsa15`, `Basic256`; one `parse` for CLI names, tedge-dot names and URIs; use it in clap, acquire configs, profiles
- [x] 1.2 `security_mode` as `Option`, effective mode (secured policy → SignAndEncrypt), consistency errors
- [x] 1.3 `--allow-deprecated-security` and the gate in `Connection::connect`
- [x] 1.4 Map all policies to async-opcua `SecurityPolicy`; keep `gen_connection`/acquire generation in sync

## 2. tedge-dot config

- [x] 2.1 Honour `allow_deprecated_security` (device over connection) with tedge-dot's message
- [x] 2.2 `allow_plaintext_password` → plaintext refusal/warning

## 3. Plaintext check

- [x] 3.1 Before connecting a username identity: GetEndpoints, find the endpoint and its UserName token policy, decide unprotected; error (tedge-dot without opt-in) or warn

## 4. Tests

- [x] 4.1 Unit tests: parsing, effective mode, gates, tedge-dot flags
- [x] 4.2 Test server: all policies × Sign/SignAndEncrypt
- [x] 4.3 E2E: every policy/mode anonymous and with username; deprecated without/with opt-in; tedge-dot plaintext refusal and opt-in

## 5. Docs

- [x] 5.1 README security section, `openspec/components/cli.md`; archive the change
