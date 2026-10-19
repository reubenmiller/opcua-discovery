//! Shared test fixtures: an in-process OPC UA server with a small, known
//! address space, plus helpers to invoke the compiled `opcua-scan` binary.
//!
//! The server exposes:
//!
//! ```text
//! Objects/Factory/Line1/Motor/Temperature  (Double, 25.0)
//! Objects/Factory/Line1/Motor/Speed         (Float, 1500.0)
//! ```
//!
//! Tests can also point at an external server by setting the
//! `OPCUA_TEST_ENDPOINT` environment variable, in which case the in-process
//! server is not started (the external server is expected to expose a similar
//! address space for the assertions to hold).

#![allow(dead_code)]

use std::process::{Command, Output};
use std::time::{Duration, Instant};

use opcua::crypto::SecurityPolicy;
use opcua::server::address_space::{
    DataTypeBuilder, MethodBuilder, ObjectBuilder, VariableBuilder,
};
use opcua::server::diagnostics::NamespaceMetadata;
use opcua::server::node_manager::memory::{SimpleNodeManager, simple_node_manager};
use opcua::server::{
    ANONYMOUS_USER_TOKEN_ID, ServerBuilder, ServerEndpoint, ServerHandle, ServerUserToken,
};
use opcua::types::{
    Argument, DataTypeDefinition, DataTypeId, EnumDefinition, EnumField, MessageSecurityMode,
    NodeId, ObjectTypeId, StructureDefinition, StructureField, StructureType,
};

/// Namespace URI used by the test address space.
pub const TEST_NS: &str = "urn:opcua-scan-test";

/// Key under which the username/password token is registered on the server.
pub const TEST_USER_TOKEN_ID: &str = "opcua-scan-test-user";
/// Username accepted by the in-process server.
pub const TEST_USER: &str = "operator";
/// Password accepted by the in-process server.
pub const TEST_PASSWORD: &str = "s3cr3t-pass";

/// The secured policies the test server offers (CLI name, policy), each with
/// Sign and SignAndEncrypt endpoints.
pub const SECURE_POLICIES: &[(&str, SecurityPolicy)] = &[
    ("Basic128Rsa15", SecurityPolicy::Basic128Rsa15),
    ("Basic256", SecurityPolicy::Basic256),
    ("Basic256Sha256", SecurityPolicy::Basic256Sha256),
    ("Aes128Sha256RsaOaep", SecurityPolicy::Aes128Sha256RsaOaep),
    ("Aes256Sha256RsaPss", SecurityPolicy::Aes256Sha256RsaPss),
];

/// Leaf variables created under `Motor`, with their browse names.
pub const MOTOR_VARIABLES: &[&str] = &[
    "Temperature",
    "Speed",
    "Torque",
    "Power",
    "Running",
    "Label",
    "Counter",
    "TempHistory",
];

/// Start (or locate) a test server and return its endpoint URL.
///
/// The in-process server is started **once per test binary** and shared by all
/// tests in that binary. The address space is read-only, so sharing is safe and
/// it keeps resource use low: spawning a separate full OPC UA server (with
/// secure endpoints and crypto handshakes) for every test otherwise starves the
/// CPU under parallel load and makes timing-sensitive tests flaky.
///
/// If `OPCUA_TEST_ENDPOINT` is set, that endpoint is returned and no in-process
/// server is started.
pub fn start() -> String {
    use std::sync::OnceLock;
    static ENDPOINT: OnceLock<String> = OnceLock::new();
    ENDPOINT
        .get_or_init(|| {
            if let Ok(ep) = std::env::var("OPCUA_TEST_ENDPOINT") {
                return ep;
            }

            let port = free_port();
            let endpoint = format!("opc.tcp://127.0.0.1:{port}/");

            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .expect("build test runtime");
                rt.block_on(async move {
                    run_server(port).await;
                });
            });

            wait_until_listening(port);
            endpoint
        })
        .clone()
}

/// Build, populate and run the in-process server until the runtime ends.
async fn run_server(port: u16) {
    let pki = std::env::temp_dir().join(format!("opcua-scan-test-pki-{port}"));
    let tokens = [
        ANONYMOUS_USER_TOKEN_ID.to_string(),
        TEST_USER_TOKEN_ID.to_string(),
    ];
    let mut builder = ServerBuilder::new();
    // Every secured policy async-opcua implements, signed and encrypted.
    for (name, policy) in SECURE_POLICIES {
        for (suffix, mode) in [
            ("sign", MessageSecurityMode::Sign),
            ("signencrypt", MessageSecurityMode::SignAndEncrypt),
        ] {
            builder = builder.add_endpoint(
                format!("{}_{suffix}", name.to_ascii_lowercase()),
                ServerEndpoint::new("/", *policy, mode, &tokens),
            );
        }
    }
    let (server, handle) = builder
        .application_name("opcua-scan-test-server")
        .application_uri("urn:opcua-scan-test-server")
        .host("127.0.0.1")
        .port(port)
        .discovery_urls(vec![format!("opc.tcp://127.0.0.1:{port}/")])
        .add_user_token(
            TEST_USER_TOKEN_ID,
            ServerUserToken::user_pass(TEST_USER, TEST_PASSWORD),
        )
        .add_endpoint("none", ServerEndpoint::new_none("/", &tokens))
        .with_node_manager(simple_node_manager(
            NamespaceMetadata {
                namespace_uri: TEST_NS.to_owned(),
                ..Default::default()
            },
            "test",
        ))
        .create_sample_keypair(true)
        .trust_client_certs(true)
        .pki_dir(pki)
        .build()
        .expect("build test server");

    populate(&handle);

    let _ = server.run().await;
}

/// Populate the address space with the known factory hierarchy.
fn populate(handle: &ServerHandle) {
    let nm = handle
        .node_managers()
        .get_of_type::<SimpleNodeManager>()
        .expect("simple node manager");
    let ns = handle
        .get_namespace_index(TEST_NS)
        .expect("test namespace registered");

    let address_space = nm.address_space();
    let mut a = address_space.write();

    let factory = NodeId::new(ns, "Factory");
    let _ = a.add_folder(&factory, "Factory", "Factory", &NodeId::objects_folder_id());
    let line = NodeId::new(ns, "Line1");
    let _ = a.add_folder(&line, "Line1", "Line1", &factory);
    let motor = NodeId::new(ns, "Motor");
    let _ = a.add_folder(&motor, "Motor", "Motor", &line);

    // A spread of scalar data types so type/datatype filtering and value
    // conversion can be exercised end to end. The eight children also make
    // Browse continuation points reachable with a small `--page-size`.
    VariableBuilder::new(
        &NodeId::new(ns, "Temperature"),
        "Temperature",
        "Temperature",
    )
    .data_type(DataTypeId::Double)
    .value(25.0f64)
    .organized_by(&motor)
    .insert(&mut *a);
    VariableBuilder::new(&NodeId::new(ns, "Speed"), "Speed", "Speed")
        .data_type(DataTypeId::Float)
        .value(1500.0f32)
        .organized_by(&motor)
        .insert(&mut *a);
    VariableBuilder::new(&NodeId::new(ns, "Torque"), "Torque", "Torque")
        .data_type(DataTypeId::Double)
        .value(8.5f64)
        .organized_by(&motor)
        .insert(&mut *a);
    VariableBuilder::new(&NodeId::new(ns, "Power"), "Power", "Power")
        .data_type(DataTypeId::Double)
        .value(3200.0f64)
        .organized_by(&motor)
        .insert(&mut *a);
    VariableBuilder::new(&NodeId::new(ns, "Running"), "Running", "Running")
        .data_type(DataTypeId::Boolean)
        .value(true)
        .organized_by(&motor)
        .insert(&mut *a);
    VariableBuilder::new(&NodeId::new(ns, "Label"), "Label", "Label")
        .data_type(DataTypeId::String)
        .value("M-100")
        .organized_by(&motor)
        .insert(&mut *a);
    VariableBuilder::new(&NodeId::new(ns, "Counter"), "Counter", "Counter")
        .data_type(DataTypeId::Int32)
        .value(42i32)
        .organized_by(&motor)
        .insert(&mut *a);
    // A historizing variable so the `--historizing` predicate has a target.
    VariableBuilder::new(
        &NodeId::new(ns, "TempHistory"),
        "TempHistory",
        "TempHistory",
    )
    .data_type(DataTypeId::Double)
    .value(25.0f64)
    .historizing(true)
    .history_readable()
    .organized_by(&motor)
    .insert(&mut *a);

    populate_plant(&mut a, ns);
}

/// `Plant`: custom datatypes and methods, outside `Factory` so the factory
/// counts stay stable.
///
/// - `Plant/Pump1/Status`: `PumpStatus` structure { Speed: Double, Motor:
///   `MotorData` { Current: Float }, State: `PumpState`, History: Double[],
///   Blob: XmlElement }
/// - `Plant/Pump1/State`: `PumpState` enumeration { Stopped = 0, Running = 1,
///   Fault = 2 }, writable
/// - `Plant/Pump1/Level`: `Percent`, a subtype of Double without a definition
/// - `Plant/Machine`: methods `Multiply(a: Int32, b: Int32)`, `Reset()`,
///   `Locked()` (not executable), `Configure(cfg: MotorData)`
fn populate_plant(a: &mut opcua::server::address_space::AddressSpace, ns: u16) {
    let structure_field = |name: &str, data_type: NodeId, value_rank: i32| StructureField {
        name: name.into(),
        description: Default::default(),
        data_type,
        value_rank,
        array_dimensions: None,
        max_string_length: 0,
        is_optional: false,
    };
    let structure = |fields: Vec<StructureField>| {
        DataTypeDefinition::Structure(StructureDefinition {
            default_encoding_id: NodeId::null(),
            base_data_type: DataTypeId::Structure.into(),
            structure_type: StructureType::Structure,
            fields: Some(fields),
        })
    };

    let motor_data = NodeId::new(ns, "MotorData");
    DataTypeBuilder::new(&motor_data, "MotorData", "MotorData")
        .subtype_of(DataTypeId::Structure)
        .data_type_definition(structure(vec![structure_field(
            "Current",
            DataTypeId::Float.into(),
            -1,
        )]))
        .insert(a);
    let pump_state = NodeId::new(ns, "PumpState");
    DataTypeBuilder::new(&pump_state, "PumpState", "PumpState")
        .subtype_of(DataTypeId::Enumeration)
        .data_type_definition(DataTypeDefinition::Enum(EnumDefinition {
            fields: Some(
                ["Stopped", "Running", "Fault"]
                    .iter()
                    .enumerate()
                    .map(|(i, name)| EnumField {
                        value: i as i64,
                        display_name: (*name).into(),
                        description: Default::default(),
                        name: (*name).into(),
                    })
                    .collect(),
            ),
        }))
        .insert(a);
    let pump_status = NodeId::new(ns, "PumpStatus");
    DataTypeBuilder::new(&pump_status, "PumpStatus", "PumpStatus")
        .subtype_of(DataTypeId::Structure)
        .data_type_definition(structure(vec![
            structure_field("Speed", DataTypeId::Double.into(), -1),
            structure_field("Motor", motor_data.clone(), -1),
            structure_field("State", pump_state.clone(), -1),
            structure_field("History", DataTypeId::Double.into(), 1),
            structure_field("Blob", DataTypeId::XmlElement.into(), -1),
        ]))
        .insert(a);
    let percent = NodeId::new(ns, "Percent");
    DataTypeBuilder::new(&percent, "Percent", "Percent")
        .subtype_of(DataTypeId::Double)
        .insert(a);

    let plant = NodeId::new(ns, "Plant");
    let _ = a.add_folder(&plant, "Plant", "Plant", &NodeId::objects_folder_id());
    let pump = NodeId::new(ns, "Pump1");
    let _ = a.add_folder(&pump, "Pump1", "Pump1", &plant);
    VariableBuilder::new(&NodeId::new(ns, "Pump1.Status"), "Status", "Status")
        .data_type(pump_status)
        .organized_by(&pump)
        .insert(a);
    VariableBuilder::new(&NodeId::new(ns, "Pump1.State"), "State", "State")
        .data_type(pump_state)
        .value(1i32)
        .writable()
        .organized_by(&pump)
        .insert(a);
    VariableBuilder::new(&NodeId::new(ns, "Pump1.Level"), "Level", "Level")
        .data_type(percent)
        .value(42.0f64)
        .organized_by(&pump)
        .insert(a);

    let machine = NodeId::new(ns, "Machine");
    ObjectBuilder::new(&machine, "Machine", "Machine")
        .organized_by(&plant)
        .has_type_definition(ObjectTypeId::BaseObjectType)
        .insert(a);
    let argument = |name: &str, data_type: NodeId| Argument {
        name: name.into(),
        data_type,
        value_rank: -1,
        array_dimensions: None,
        description: Default::default(),
    };
    let multiply = NodeId::new(ns, "Machine.Multiply");
    MethodBuilder::new(&multiply, "Multiply", "Multiply")
        .component_of(machine.clone())
        .executable(true)
        .user_executable(true)
        .input_args(
            a,
            &NodeId::new(ns, "Machine.Multiply.InputArguments"),
            &[
                argument("a", DataTypeId::Int32.into()),
                argument("b", DataTypeId::Int32.into()),
            ],
        )
        .insert(a);
    MethodBuilder::new(&NodeId::new(ns, "Machine.Reset"), "Reset", "Reset")
        .component_of(machine.clone())
        .executable(true)
        .user_executable(true)
        .insert(a);
    MethodBuilder::new(&NodeId::new(ns, "Machine.Locked"), "Locked", "Locked")
        .component_of(machine.clone())
        .executable(false)
        .user_executable(false)
        .insert(a);
    let configure = NodeId::new(ns, "Machine.Configure");
    MethodBuilder::new(&configure, "Configure", "Configure")
        .component_of(machine)
        .executable(true)
        .user_executable(true)
        .input_args(
            a,
            &NodeId::new(ns, "Machine.Configure.InputArguments"),
            &[argument("cfg", motor_data)],
        )
        .insert(a);
}

/// Grab an unused TCP port by binding to port 0 and reading the assigned port.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local addr").port()
}

/// Block until the server is accepting TCP connections (or time out).
fn wait_until_listening(port: u16) {
    // Generous: server start-up includes key generation, slow on shared CI runners.
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            // Give the OPC UA stack a brief moment after the socket is up.
            std::thread::sleep(Duration::from_millis(200));
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("test server did not start listening on port {port}");
}

/// Run the compiled `opcua-scan` binary with the given arguments.
///
/// A throwaway PKI directory is used so the client keypair does not clutter the
/// working directory.
pub fn run_cli(args: &[&str]) -> Output {
    run_cli_with_env(args, &[])
}

/// Like [`run_cli`] but with additional environment variables set on the child
/// process. `RUST_LOG=error` is always applied first (callers can override).
pub fn run_cli_with_env(args: &[&str], env: &[(&str, &str)]) -> Output {
    // A PKI directory unique to each invocation, so parallel secure-channel
    // tests never race on creating/reading the client sample keypair.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pki =
        std::env::temp_dir().join(format!("opcua-scan-client-pki-{}-{n}", std::process::id()));
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_opcua-scan"));
    cmd.args(args)
        .arg("--pki-dir")
        .arg(&pki)
        .env("RUST_LOG", "error");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("run opcua-scan binary")
}

/// Run the CLI and assert it exited successfully, returning stdout as a string.
///
/// On failure the assertion message includes both stdout and stderr so test
/// failures are easy to diagnose.
pub fn run_cli_ok(args: &[&str]) -> String {
    let out = run_cli(args);
    assert!(
        out.status.success(),
        "command {args:?} failed with code {:?}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Parse CLI stdout as a JSON array of records, with a helpful panic message.
pub fn json_records(stdout: &[u8]) -> Vec<serde_json::Value> {
    let text = String::from_utf8_lossy(stdout);
    serde_json::from_str::<Vec<serde_json::Value>>(text.trim())
        .unwrap_or_else(|e| panic!("expected JSON array, got error {e}; output was:\n{text}"))
}

/// Poll `check` until it returns `Some`, or panic after `timeout`.
///
/// This is the preferred way to wait for asynchronous conditions in tests: it
/// avoids fixed `sleep`s (slow and flaky) in favour of a bounded retry loop
/// with a short, deterministic poll interval.
pub fn wait_for<T>(timeout: Duration, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = check() {
            return v;
        }
        if Instant::now() >= deadline {
            panic!("condition not met within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
