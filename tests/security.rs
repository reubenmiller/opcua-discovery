//! Security, authentication and certificate-trust integration tests.
//!
//! The in-process server (see [`common`]) exposes `None` plus Sign and
//! SignAndEncrypt endpoints for every secured policy async-opcua implements
//! ([`common::SECURE_POLICIES`]), and accepts both anonymous and
//! username/password identities. These tests exercise the
//! SecureChannel handshake and Session activation across security modes and
//! identity tokens, plus the server-certificate trust decision.

mod common;

use common::{SECURE_POLICIES, TEST_NS, TEST_PASSWORD, TEST_USER, run_cli, run_cli_ok};

#[test]
fn sign_handshake_succeeds_when_server_trusted() {
    let endpoint = common::start();
    let out = run_cli_ok(&[
        "connect",
        &endpoint,
        "--security-policy",
        "Basic256Sha256",
        "--security-mode",
        "Sign",
        "--trust-server",
    ]);
    assert!(
        out.contains(TEST_NS),
        "missing namespace in metadata:\n{out}"
    );
}

#[test]
fn sign_and_encrypt_handshake_succeeds_when_server_trusted() {
    let endpoint = common::start();
    let out = run_cli_ok(&[
        "connect",
        &endpoint,
        "--security-policy",
        "Basic256Sha256",
        "--security-mode",
        "SignAndEncrypt",
        "--trust-server",
    ]);
    assert!(out.contains("Endpoints"), "missing endpoints:\n{out}");
}

#[test]
fn browse_works_over_encrypted_channel() {
    let endpoint = common::start();
    // End-to-end discovery over a SignAndEncrypt channel: prove that real
    // service calls (Browse + Read) succeed through the secure transport, not
    // just the handshake.
    let out = run_cli_ok(&[
        "browse",
        &endpoint,
        "--security-policy",
        "Basic256Sha256",
        "--security-mode",
        "SignAndEncrypt",
        "--trust-server",
        "--depth",
        "unlimited",
        "--output",
        "json",
    ]);
    let recs = common::json_records(out.as_bytes());
    assert!(
        recs.iter()
            .any(|r| r.get("browse_name").and_then(|v| v.as_str()) == Some("Temperature")),
        "Temperature not discovered over encrypted channel: {recs:?}"
    );
}

#[test]
fn secure_connection_without_trust_is_rejected() {
    let endpoint = common::start();
    // Without `--trust-server`, the (self-signed) server certificate is not in
    // the client trust list, so a secure handshake must fail rather than
    // silently connecting insecurely.
    let out = run_cli(&[
        "connect",
        &endpoint,
        "--security-policy",
        "Basic256Sha256",
        "--security-mode",
        "SignAndEncrypt",
        "--timeout",
        "5s",
    ]);
    assert!(
        !out.status.success(),
        "secure connect unexpectedly succeeded without trusting the server cert"
    );
}

#[test]
fn username_password_authentication_succeeds() {
    let endpoint = common::start();
    // Username/password tokens are only safe over an encrypted channel, which
    // is how the tool is expected to be used.
    let out = run_cli_ok(&[
        "connect",
        &endpoint,
        "--security-policy",
        "Basic256Sha256",
        "--security-mode",
        "SignAndEncrypt",
        "--trust-server",
        "--user",
        TEST_USER,
        "--password",
        TEST_PASSWORD,
    ]);
    assert!(
        out.contains(TEST_NS),
        "missing namespace in metadata:\n{out}"
    );
}

#[test]
fn wrong_password_is_rejected() {
    let endpoint = common::start();
    let out = run_cli(&[
        "connect",
        &endpoint,
        "--security-policy",
        "Basic256Sha256",
        "--security-mode",
        "SignAndEncrypt",
        "--trust-server",
        "--user",
        TEST_USER,
        "--password",
        "definitely-the-wrong-password",
        "--timeout",
        "5s",
    ]);
    assert!(
        !out.status.success(),
        "connect succeeded with an incorrect password"
    );
}

/// Connect with `policy`/`mode`, optionally as the test user; the deprecated
/// policies get their opt-in.
fn connect_args<'a>(endpoint: &'a str, policy: &'a str, mode: &'a str, user: bool) -> Vec<&'a str> {
    let mut args = vec![
        "connect",
        endpoint,
        "--security-policy",
        policy,
        "--security-mode",
        mode,
        "--trust-server",
        "--allow-deprecated-security",
    ];
    if user {
        args.extend_from_slice(&["--user", TEST_USER, "--password", TEST_PASSWORD]);
    }
    args
}

#[test]
fn every_policy_and_mode_connects() {
    let endpoint = common::start();
    let combinations: Vec<(&str, &str, bool)> = SECURE_POLICIES
        .iter()
        .flat_map(|(policy, _)| {
            ["Sign", "SignAndEncrypt"]
                .into_iter()
                .flat_map(move |mode| [false, true].map(|user| (*policy, mode, user)))
        })
        .collect();
    // Each handshake takes seconds; run them side by side.
    let failures: Vec<String> = std::thread::scope(|scope| {
        let handles: Vec<_> = combinations
            .iter()
            .map(|&(policy, mode, user)| {
                let endpoint = &endpoint;
                scope.spawn(move || {
                    let out = run_cli(&connect_args(endpoint, policy, mode, user));
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    (!out.status.success() || !stdout.contains(TEST_NS)).then(|| {
                        format!(
                            "{policy}/{mode} user={user}: {}",
                            String::from_utf8_lossy(&out.stderr).trim()
                        )
                    })
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|h| h.join().unwrap())
            .collect()
    });
    assert_eq!(combinations.len(), 20);
    assert!(
        failures.is_empty(),
        "failed combinations:\n{}",
        failures.join("\n")
    );
}

#[test]
fn deprecated_policy_needs_opt_in() {
    let endpoint = common::start();
    let out = run_cli(&[
        "connect",
        &endpoint,
        "--security-policy",
        "Basic256",
        "--trust-server",
    ]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--allow-deprecated-security"), "{stderr}");
}

#[test]
fn policy_without_mode_uses_sign_and_encrypt() {
    let endpoint = common::start();
    let out = run_cli_ok(&[
        "connect",
        &endpoint,
        "--security-policy",
        "Aes256_Sha256_RsaPss",
        "--trust-server",
    ]);
    assert!(out.contains(TEST_NS), "{out}");
}

#[test]
fn plaintext_password_on_none_channel_is_warned_about() {
    let endpoint = common::start();
    let out = common::run_cli_with_env(
        &[
            "connect",
            &endpoint,
            "--user",
            TEST_USER,
            "--password",
            TEST_PASSWORD,
        ],
        &[("RUST_LOG", "warn")],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("sent unprotected"), "{stderr}");
}
