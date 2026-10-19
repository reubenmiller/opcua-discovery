# Spec Delta

## REMOVED Requirements

### Requirement: Security policy and mode follow tedge-dot precedence
**Reason**: it refused the deprecated policies outright; replaced by "Security policy and mode follow tedge-dot rules", which allows them behind `allow_deprecated_security` like tedge-dot.
**Migration**: none; configs that failed with "not supported by opcua-scan" now work when they set `allow_deprecated_security = true`.

## MODIFIED Requirements

### Requirement: Connector-only settings are ignored
Settings that only affect the connector's runtime (`connect_timeout_s`, `request_timeout_s`, `queue_size`, `namespaces`, device `poll_interval`, points, ...) SHALL NOT cause an error and SHALL NOT change opcua-scan's behaviour.

#### Scenario: Full production config
- **WHEN** the file is a complete tedge-dot OPC UA config with `[connector]`, `[mqtt]`, points and `points_from`
- **THEN** opcua-scan connects using only the connection settings

## ADDED Requirements

### Requirement: Security policy and mode follow tedge-dot rules
The policy SHALL be the device's `security_policy`, else `[connection]`'s, else `None`. The mode SHALL be the device's `security_mode`; else, when the device sets a policy, the policy's default (`none` for `None`, `sign_and_encrypt` otherwise); else `[connection]`'s mode; else the policy's default. Names SHALL be accepted as tedge-dot accepts them. `Basic128Rsa15` and `Basic256` SHALL need `allow_deprecated_security = true` (device over `[connection]`).

#### Scenario: Device policy without mode
- **WHEN** `[connection]` has `security_mode = "sign"` and the device has `security_policy = "Basic256Sha256"` and no mode
- **THEN** opcua-scan connects with Basic256Sha256 and SignAndEncrypt

#### Scenario: Connection defaults
- **WHEN** `[connection]` has `security_policy = "Aes256_Sha256_RsaPss"` and `security_mode = "sign"` and the device sets neither
- **THEN** opcua-scan connects with Aes256Sha256RsaPss and Sign

#### Scenario: Policy given as URI
- **WHEN** the device has `security_policy = "http://opcfoundation.org/UA/SecurityPolicy#Basic256Sha256"`
- **THEN** it is treated as Basic256Sha256

#### Scenario: Deprecated policy without opt-in
- **WHEN** the effective policy is `Basic128Rsa15` or `Basic256` and `allow_deprecated_security` is not true on the device or `[connection]`
- **THEN** the command fails with an error saying the policy is deprecated and to set `allow_deprecated_security = true`

#### Scenario: Deprecated policy with opt-in
- **WHEN** the device has `security_policy = "Basic256"` and `allow_deprecated_security = true`
- **THEN** opcua-scan connects with Basic256

#### Scenario: Inconsistent pair
- **WHEN** the effective policy is `None` and the effective mode is `sign`
- **THEN** the command fails with an error naming `security_mode`

### Requirement: Plaintext passwords follow tedge-dot
With `--tedge-dot-config`, a username identity whose password would travel unprotected (channel mode none, or sign with a username token policy naming `None`) SHALL be refused before activating the session with an error starting `plaintext password refused`, unless `allow_plaintext_password` is true on the device or `[connection]`; then a warning SHALL be logged.

#### Scenario: Password on a None channel
- **WHEN** the device has `user` and `password` and policy `None`, without `allow_plaintext_password`
- **THEN** the command fails with `plaintext password refused: ...` and the password is never sent

#### Scenario: Opted in
- **WHEN** the same device has `allow_plaintext_password = true`
- **THEN** opcua-scan connects and logs a warning

#### Scenario: Encrypted channel
- **WHEN** the device uses Basic256Sha256 with SignAndEncrypt and a username
- **THEN** opcua-scan connects without a warning
