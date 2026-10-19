# Design

## Context

async-opcua 0.18 implements six policies (`SecurityPolicy::{None, Basic128Rsa15, Basic256,
Basic256Sha256, Aes128Sha256RsaOaep, Aes256Sha256RsaPss}`), no ECC. opcua-scan exposes four.
tedge-dot (`connector-opcua/src/config.rs`, connector spec §3.3, §6.2) allows the deprecated two
behind `allow_deprecated_security` and refuses unprotected passwords unless
`allow_plaintext_password`.

## Decisions

**One policy parser.** `SecurityPolicyArg` gets the two deprecated variants; a single
`SecurityPolicyArg::parse` accepts the CLI names, tedge-dot names and URIs, used by clap
(`value_parser`), acquire configs and the tedge-dot mapper. Profiles deserialize through it too.

**Default mode at connect time.** `--security-mode` keeps its `None` default in clap, but
`Connection::connect` resolves the effective mode: a secured policy with mode `None` *that was not
given explicitly* becomes SignAndEncrypt. To tell "not given" from "given as None",
`security_mode` becomes `Option<SecurityModeArg>`; existing call sites read it through
`ConnectionArgs::effective_mode()`.

**Deprecated opt-in** is a `ConnectionArgs` flag (`--allow-deprecated-security`), checked in
`Connection::connect`, so every command and every source (CLI, profile, acquire config,
tedge-dot config) goes through one gate. The tedge-dot mapper sets the flag from
`allow_deprecated_security` (device over connection) and keeps tedge-dot's own error text.

**Plaintext check** needs the user token policy of the endpoint that will be used, which
`connect_to_matching_endpoint` picks internally. For a username identity, opcua-scan reads the
endpoints first (one extra GetEndpoints, which `connect_to_matching_endpoint` does anyway),
finds the endpoint with the requested URL path, policy and mode, and its UserName token policy.
Unprotected = mode None, or mode Sign and the token policy's `security_policy_uri` is the None
URI. With a tedge-dot config and no `allow_plaintext_password` → error; otherwise → warning.
Without a tedge-dot config the result is only a warning, so existing CLI use is unchanged.

**Test server** offers None plus Sign and SignAndEncrypt for all five secured policies, with the
anonymous and username tokens. The server side of async-opcua accepts deprecated policies when
configured explicitly.

## Risks / Trade-offs

- The deprecated policies use SHA-1 and short keys; the opt-in flag and a warning on every
  connect keep that visible.
- Some servers reject the deprecated policies with keys > 2048 bits; opcua-scan's generated
  certificate is 2048-bit RSA, which fits every policy.
