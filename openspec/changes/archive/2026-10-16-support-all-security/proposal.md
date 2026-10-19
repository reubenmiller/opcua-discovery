# Proposal

## Why

opcua-scan offers only four security policies, refuses the deprecated `Basic128Rsa15` and
`Basic256` outright, and ignores tedge-dot's `allow_deprecated_security` and
`allow_plaintext_password`. Older PLCs and gateways in the field still only offer the deprecated
policies, so the scan cannot reach every server the tedge-dot connector can, and a scan driven
by a connector config can send a password the connector itself would refuse to send.

## What Changes

- The CLI (`--security-policy`), acquire configs and profiles accept every policy async-opcua
  implements: `None`, `Basic128Rsa15`, `Basic256`, `Basic256Sha256`, `Aes128Sha256RsaOaep`,
  `Aes256Sha256RsaPss`, also in tedge-dot's spelling (`Aes128_Sha256_RsaOaep`, ...) and as the
  full policy URI.
- The deprecated policies need an explicit opt-in: `--allow-deprecated-security` on the CLI,
  `allow_deprecated_security` (connection or device) in a tedge-dot config.
- A secured policy without a mode uses SignAndEncrypt (tedge-dot's policy default) instead of
  failing to find a `None`-mode endpoint. An explicit `--security-mode None` with a secured
  policy, or `Sign`/`SignAndEncrypt` with `None`, is a usage error.
- **tedge-dot plaintext rule:** with `--tedge-dot-config`, a password that would travel
  unprotected (channel `none`, or `sign` with a username token policy naming `None`) is refused
  unless `allow_plaintext_password` is set, as the connector does. Without a tedge-dot config the
  scan keeps connecting and logs a warning (no behaviour change for existing CLI use).
- The in-process test server offers every policy and mode, and every combination is tested end
  to end with anonymous and username identities.

Not possible: ECC policies (`ECC_nistP256`, ...). Neither async-opcua 0.18 nor tedge-dot
implements them.

## Capabilities

### New Capabilities
- `client-security`: the security policies, modes, deprecated-policy opt-in, default mode and
  plaintext-password warning of every connecting command.

### Modified Capabilities
- `tedge-dot-connection`: deprecated policies are honoured with `allow_deprecated_security`
  instead of refused; `allow_plaintext_password` is enforced; both leave the "ignored" list.

## Impact

- Code: `src/cli/mod.rs` (`SecurityPolicyArg`, `--allow-deprecated-security`), `src/opcua/client.rs`
  (policy mapping, default mode, plaintext check), `src/tedge_dot/connection.rs`,
  `src/acquire/config.rs` (policy names).
- Tests: `tests/common` server endpoints for all policies; `tests/security.rs` matrix.
- Users: additive. A secured policy without `--security-mode` now connects with SignAndEncrypt
  instead of failing.
