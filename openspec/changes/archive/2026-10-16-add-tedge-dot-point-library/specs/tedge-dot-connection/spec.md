# Spec Delta

## Purpose

Lets opcua-scan connect to an OPC UA server with the settings of one device in a tedge-dot OPC UA
connector configuration, so the gateway's connection is described once and the scan reaches the
server exactly as the connector does.

## ADDED Requirements

### Requirement: Connector config option
Every command that connects (`connect`, `browse`, `search`, `sample`) SHALL accept `--tedge-dot-config <FILE>` and `--device <NAME>`. `--device` SHALL require `--tedge-dot-config`. `--tedge-dot-config` SHALL conflict with `browse --config`.

#### Scenario: Connect via the config
- **WHEN** `opcua-scan connect --tedge-dot-config opcua.toml --device plc-7` runs and `plc-7` has `protocol_address = { endpoint = "opc.tcp://plc:4840/" }`
- **THEN** opcua-scan connects to `opc.tcp://plc:4840/` and prints the server metadata

#### Scenario: Device without config
- **WHEN** `--device plc-7` is given without `--tedge-dot-config`
- **THEN** the command fails with a usage error before connecting

### Requirement: Device selection
The device SHALL be chosen by its `name` from the file's `[[device]]` entries. When `--device` is absent and the file has exactly one device, that device SHALL be used. Otherwise the command SHALL fail with an error that lists the device names.

#### Scenario: Single device
- **WHEN** the file has one `[[device]]` and `--device` is not given
- **THEN** that device's settings are used

#### Scenario: Several devices, none chosen
- **WHEN** the file has devices `plc-7` and `plc-8` and `--device` is not given
- **THEN** the command fails with an error naming `plc-7` and `plc-8`

#### Scenario: Unknown device
- **WHEN** `--device plc-9` names no device in the file
- **THEN** the command fails with an error naming `plc-9` and the available devices

### Requirement: Only OPC UA configs
When the file declares `[connector] protocol`, it SHALL be `opcua`; any other value SHALL fail with an error naming the protocol. A file that is not valid TOML, or a device without `protocol_address.endpoint`, SHALL fail with an error naming the file or device.

#### Scenario: Modbus config
- **WHEN** the file has `[connector] protocol = "modbus"`
- **THEN** the command fails with an error saying the config is for `modbus`, not `opcua`

#### Scenario: Missing endpoint
- **WHEN** the chosen device's `protocol_address` has no `endpoint`
- **THEN** the command fails with an error naming the device and `endpoint`

### Requirement: Security policy and mode follow tedge-dot precedence
The policy SHALL be the device's `security_policy`, else `[connection]`'s, else `None`. The mode SHALL be the device's `security_mode`; else, when the device sets a policy, the policy's default (`none` for `None`, `sign_and_encrypt` otherwise); else `[connection]`'s mode; else the policy's default. Names SHALL be accepted as tedge-dot accepts them.

#### Scenario: Device policy without mode
- **WHEN** `[connection]` has `security_mode = "sign"` and the device has `security_policy = "Basic256Sha256"` and no mode
- **THEN** opcua-scan connects with Basic256Sha256 and SignAndEncrypt

#### Scenario: Connection defaults
- **WHEN** `[connection]` has `security_policy = "Aes256_Sha256_RsaPss"` and `security_mode = "sign"` and the device sets neither
- **THEN** opcua-scan connects with Aes256Sha256RsaPss and Sign

#### Scenario: Policy given as URI
- **WHEN** the device has `security_policy = "http://opcfoundation.org/UA/SecurityPolicy#Basic256Sha256"`
- **THEN** it is treated as Basic256Sha256

#### Scenario: Unsupported policy
- **WHEN** the effective policy is `Basic128Rsa15` or `Basic256`
- **THEN** the command fails with an error saying opcua-scan does not support that policy

#### Scenario: Inconsistent pair
- **WHEN** the effective policy is `None` and the effective mode is `sign`
- **THEN** the command fails with an error naming `security_mode`

### Requirement: Identity
The identity SHALL be taken from the device: `user` with `password` or with the first line of `password_file` (an empty password when neither is set), or `user_certificate` with `user_private_key`, or anonymous when none is set. Setting both identity kinds, a password without `user`, or only one of the certificate pair SHALL be an error. Error messages SHALL never contain a password.

#### Scenario: Password file
- **WHEN** the device has `user = "operator"` and `password_file = "secrets/plc7.pw"` and that file's first line is the password
- **THEN** opcua-scan activates the session as `operator` with that password, without prompting

#### Scenario: Password starting with env:
- **WHEN** the device has `password = "env:FOO"`
- **THEN** the literal string `env:FOO` is sent as the password; no environment variable is read

#### Scenario: Both identities
- **WHEN** the device sets `user` and `user_certificate`
- **THEN** the command fails with an error saying to use one identity

#### Scenario: Unreadable password file
- **WHEN** `password_file` names a file that does not exist
- **THEN** the command fails with an error naming the path and not containing any password

### Requirement: Gateway application certificate
For a secured policy, opcua-scan SHALL present the connector's application certificate: `[connection]` `certificate`/`private_key`, else `own/certs/cert.der` and `own/private/key.pem` under `pki_dir` (default `/var/lib/tedge-dot/opcua/pki`), together with `application_name`/`application_uri` (defaults `tedge-dot`/`urn:tedge-dot`). When neither certificate exists, opcua-scan's own identity SHALL be used. Under policy `None` the gateway's files SHALL NOT be read.

#### Scenario: Reuse the gateway's application certificate
- **WHEN** the policy is Basic256Sha256, `[connection]` has `pki_dir = "pki"` and `application_uri = "urn:gw:tedge-dot"`, and `<config dir>/pki/own/certs/cert.der` exists
- **THEN** opcua-scan presents that certificate and `urn:gw:tedge-dot`, so a server that trusts the connector trusts the scan

#### Scenario: No gateway certificate yet
- **WHEN** the policy is secured and no gateway certificate exists
- **THEN** opcua-scan connects with its own application identity

#### Scenario: Unsecured device
- **WHEN** the effective policy is `None`
- **THEN** opcua-scan does not read `pki_dir`, `certificate` or `private_key`

### Requirement: Server certificate trust
Server certificates SHALL be validated against opcua-scan's own PKI directory (`--pki-dir` or its default), never by writing to tedge-dot's PKI directory, whose trust-list layout differs. `trust_any_server_certificate` SHALL be the device's value, else `[connection]`'s.

#### Scenario: Trust override on the device
- **WHEN** `[connection]` has `trust_any_server_certificate = false` and the device has `true`
- **THEN** the server certificate is trusted automatically

#### Scenario: Gateway PKI untouched
- **WHEN** opcua-scan connects to a server whose certificate it does not trust, using a config with `pki_dir = "pki"`
- **THEN** the rejected certificate is stored under opcua-scan's PKI directory and nothing under `<config dir>/pki` changes

### Requirement: Relative paths
Relative `pki_dir`, `certificate`, `private_key`, `password_file`, `user_certificate` and `user_private_key` SHALL resolve against the directory of the config file, not the working directory.

#### Scenario: Run from another directory
- **WHEN** `/etc/tedge/plugins/ot/opcua.toml` has `password_file = "plc7.pw"` and opcua-scan runs in `/tmp`
- **THEN** the password is read from `/etc/tedge/plugins/ot/plc7.pw`

### Requirement: CLI flags win
An explicitly given CLI flag SHALL override the corresponding config value: the positional endpoint, `--user`, `--password`, `--cert`/`--key`, `--security-policy`, `--security-mode`, `--trust-server`.

#### Scenario: Endpoint override
- **WHEN** `opcua-scan connect opc.tcp://localhost:4840 --tedge-dot-config opcua.toml --device plc-7`
- **THEN** opcua-scan connects to `opc.tcp://localhost:4840` with the device's other settings

### Requirement: Connector-only settings are ignored
Settings that only affect the connector's runtime (`connect_timeout_s`, `request_timeout_s`, `queue_size`, `namespaces`, `allow_deprecated_security`, `allow_plaintext_password`, device `poll_interval`, points, ...) SHALL NOT cause an error and SHALL NOT change opcua-scan's behaviour.

#### Scenario: Full production config
- **WHEN** the file is a complete tedge-dot OPC UA config with `[connector]`, `[mqtt]`, points and `points_from`
- **THEN** opcua-scan connects using only the connection settings
