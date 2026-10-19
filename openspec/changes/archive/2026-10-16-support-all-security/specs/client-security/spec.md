# Spec Delta

## Purpose

Defines which OPC UA security policies, message security modes and identities every connecting
command of opcua-scan supports, and the safeguards around deprecated policies and unprotected
passwords.

## ADDED Requirements

### Requirement: All security policies
`--security-policy` SHALL accept `None`, `Basic128Rsa15`, `Basic256`, `Basic256Sha256`, `Aes128Sha256RsaOaep` and `Aes256Sha256RsaPss`, case-insensitively, also in tedge-dot's spelling (`Aes128_Sha256_RsaOaep`, `Aes256_Sha256_RsaPss`) and as the full `http://opcfoundation.org/UA/SecurityPolicy#...` URI. Acquire configs and profiles SHALL accept the same names.

#### Scenario: Every policy connects
- **WHEN** a server offers an endpoint for each of the six policies with Sign and SignAndEncrypt (and `None`/None)
- **THEN** `opcua-scan connect` succeeds for every policy and mode the server offers, anonymously and with a username

#### Scenario: tedge-dot spelling
- **WHEN** `--security-policy Aes128_Sha256_RsaOaep` is given
- **THEN** it is treated as Aes128Sha256RsaOaep

### Requirement: Deprecated policies need an opt-in
`Basic128Rsa15` and `Basic256` SHALL only be used when `--allow-deprecated-security` is given (or the profile/config equivalent). Otherwise the command SHALL fail before connecting with an error naming the policy and the flag.

#### Scenario: Without opt-in
- **WHEN** `opcua-scan connect <ep> --security-policy Basic256` runs
- **THEN** it fails with an error saying `Basic256` is deprecated and to pass `--allow-deprecated-security`

#### Scenario: With opt-in
- **WHEN** `opcua-scan connect <ep> --security-policy Basic128Rsa15 --allow-deprecated-security --trust-server` runs against a server offering it
- **THEN** it connects

### Requirement: Mode defaults and consistency
A secured policy without `--security-mode` SHALL use SignAndEncrypt. `--security-mode None` with a secured policy, and `Sign` or `SignAndEncrypt` with policy `None`, SHALL be usage errors.

#### Scenario: Policy alone
- **WHEN** `--security-policy Basic256Sha256` is given without `--security-mode`
- **THEN** opcua-scan connects with Basic256Sha256 and SignAndEncrypt

#### Scenario: Sign without policy
- **WHEN** `--security-mode Sign` is given without `--security-policy`
- **THEN** the command fails with an error saying Sign needs a policy other than None

### Requirement: Identities
Every policy and mode SHALL support the anonymous, username/password and X.509 user identities. A username token SHALL be protected as the endpoint's user token policy requires.

#### Scenario: Username on every secured endpoint
- **WHEN** a username identity connects to each secured endpoint of the test server
- **THEN** the session is activated as that user

### Requirement: Plaintext password warning
When a password would travel unprotected (channel mode None, or Sign with a username token policy that names `None`) and no tedge-dot config is in use, opcua-scan SHALL log a warning and connect.

#### Scenario: Username on a None channel
- **WHEN** `opcua-scan connect <ep> --user op --password env:PW` uses policy None
- **THEN** a warning says the password is sent unprotected, and the connection proceeds
