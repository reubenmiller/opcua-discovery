//! Per-service and CLI-contract integration tests driven through the compiled
//! `opcua-scan` binary against the in-process OPC UA server defined in
//! [`common`].
//!
//! These complement the smoke tests in `cli.rs` by exercising specific OPC UA
//! services (Browse continuation, attribute Reads, Subscriptions) and the
//! documented CLI contracts (exit codes, machine-clean stdout, namespace
//! handling) that previously lacked coverage.

mod common;

use serde_json::Value;

use common::{MOTOR_VARIABLES, TEST_NS, json_records, run_cli, run_cli_ok};

/// Collect the `browse_name` of every record in a JSON array.
fn browse_names(records: &[Value]) -> Vec<String> {
    records
        .iter()
        .filter_map(|r| r.get("browse_name").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

#[test]
fn browse_follows_continuation_points_with_small_page_size() {
    let endpoint = common::start();
    // A page size of 2 forces the server to return the eight Motor children
    // across multiple Browse/BrowseNext calls, exercising continuation points.
    let out = run_cli_ok(&[
        "browse",
        &endpoint,
        "--depth",
        "unlimited",
        "--type",
        "Variable",
        "--page-size",
        "2",
        "--output",
        "json",
    ]);
    let recs = json_records(out.as_bytes());
    let names = browse_names(&recs);
    for expected in MOTOR_VARIABLES {
        assert!(
            names.iter().any(|n| n == expected),
            "continuation paging dropped {expected:?}; found {names:?}"
        );
    }
}

#[test]
fn datatype_filter_selects_only_matching_variables() {
    let endpoint = common::start();
    let out = run_cli_ok(&[
        "browse",
        &endpoint,
        "--depth",
        "unlimited",
        "--type",
        "Variable",
        "--datatype",
        "Double",
        "--output",
        "json",
    ]);
    let recs = json_records(out.as_bytes());
    let names = browse_names(&recs);
    // Doubles in the fixture: Temperature, Torque, Power, TempHistory.
    assert!(names.iter().any(|n| n == "Temperature"), "{names:?}");
    assert!(names.iter().any(|n| n == "Torque"), "{names:?}");
    assert!(names.iter().any(|n| n == "Power"), "{names:?}");
    // Non-doubles must be excluded.
    assert!(
        !names.iter().any(|n| n == "Speed"),
        "Float leaked: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "Counter"),
        "Int32 leaked: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "Running"),
        "Boolean leaked: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "Label"),
        "String leaked: {names:?}"
    );
}

#[test]
fn historizing_filter_finds_only_history_node() {
    let endpoint = common::start();
    let out = run_cli_ok(&[
        "browse",
        &endpoint,
        "--depth",
        "unlimited",
        "--historizing",
        "--output",
        "json",
    ]);
    let recs = json_records(out.as_bytes());
    let names = browse_names(&recs);
    assert!(
        names.iter().any(|n| n == "TempHistory"),
        "historizing node not found: {names:?}"
    );
    assert!(
        names.iter().all(|n| n == "TempHistory"),
        "non-historizing nodes leaked: {names:?}"
    );
}

#[test]
fn writable_filter_returns_nothing_for_read_only_server() {
    let endpoint = common::start();
    // Every Factory variable is read-only (Plant has a writable enum for the
    // tedge-dot tests). Scope the query to Factory in the custom namespace so
    // no other writable node counts; the writable predicate must then yield
    // an empty result set.
    let out = run_cli_ok(&[
        "browse",
        &endpoint,
        "--depth",
        "unlimited",
        "--ns-uri",
        TEST_NS,
        "--include",
        "Factory/**",
        "--writable",
        "--output",
        "json",
    ]);
    let recs = json_records(out.as_bytes());
    assert!(recs.is_empty(), "expected no writable nodes, got {recs:?}");
}

#[test]
fn records_carry_namespace_uri_for_custom_nodes() {
    let endpoint = common::start();
    let out = run_cli_ok(&[
        "browse",
        &endpoint,
        "--depth",
        "unlimited",
        "--type",
        "Variable",
        "--output",
        "json",
    ]);
    let recs = json_records(out.as_bytes());
    let temperature = recs
        .iter()
        .find(|r| r.get("browse_name").and_then(Value::as_str) == Some("Temperature"))
        .expect("Temperature present");
    assert_eq!(
        temperature.get("namespace_uri").and_then(Value::as_str),
        Some(TEST_NS),
        "custom node should report its namespace URI: {temperature}"
    );
}

#[test]
fn ns_uri_filter_restricts_to_custom_namespace() {
    let endpoint = common::start();
    let out = run_cli_ok(&[
        "browse",
        &endpoint,
        "--depth",
        "unlimited",
        "--type",
        "Variable",
        "--ns-uri",
        TEST_NS,
        "--output",
        "json",
    ]);
    let recs = json_records(out.as_bytes());
    assert!(!recs.is_empty(), "expected nodes in the custom namespace");
    for r in &recs {
        assert_eq!(
            r.get("namespace_uri").and_then(Value::as_str),
            Some(TEST_NS),
            "node outside requested namespace: {r}"
        );
    }
}

#[test]
fn sample_subscription_emits_initial_values() {
    let endpoint = common::start();
    // Subscriptions deliver the initial value on creation, so a short sample
    // window is enough to receive at least one update without relying on the
    // value changing.
    let out = run_cli(&[
        "sample",
        &endpoint,
        "--depth",
        "unlimited",
        "--include",
        "**/Temperature",
        "--duration",
        "2s",
    ]);
    assert!(
        out.status.success(),
        "sample failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Temperature"),
        "no subscription update for Temperature:\n{stdout}"
    );
}

#[test]
fn machine_output_is_clean_json_on_stdout() {
    let endpoint = common::start();
    // With RUST_LOG set to a noisy level, diagnostics must still go to stderr
    // and never pollute the machine-readable JSON on stdout.
    let out = common::run_cli_with_env(
        &[
            "browse",
            &endpoint,
            "--depth",
            "unlimited",
            "--output",
            "json",
        ],
        &[("RUST_LOG", "debug")],
    );
    assert!(out.status.success());
    // stdout must parse as a JSON array with nothing else mixed in.
    let recs = json_records(&out.stdout);
    assert!(!recs.is_empty(), "expected records on stdout");
}

#[test]
fn connection_failure_uses_exit_code_three() {
    // A well-formed opc.tcp endpoint that is not listening should map to the
    // stable "connection failure" exit code (3), distinct from usage errors.
    let out = run_cli(&["connect", "opc.tcp://127.0.0.1:1/", "--timeout", "2s"]);
    assert!(!out.status.success());
    assert_eq!(
        out.status.code(),
        Some(3),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn unknown_flag_is_a_usage_error() {
    let endpoint = common::start();
    let out = run_cli(&["browse", &endpoint, "--definitely-not-a-flag"]);
    assert!(!out.status.success());
    // clap argument parsing failures exit with code 2.
    assert_eq!(out.status.code(), Some(2));
}
