//! `--tedge-dot-config`: fill [`ConnectionArgs`] from a tedge-dot OPC UA
//! connector config (`[connection]` + one device's `protocol_address`).
//!
//! The rules mirror tedge-dot's `connector-opcua/src/config.rs`
//! (`device_security`, `identity`): the same precedence, the same policy and
//! mode spellings, the same identity errors. Settings that only affect the
//! connector's runtime are ignored. Errors never quote a password.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::debug;

use crate::cli::{ConnectionArgs, Redacted, SecurityModeArg, SecurityPolicyArg};
use crate::error::{Result, ScanError};

/// Where tedge-dot keeps its PKI when `[connection] pki_dir` is not set.
pub const DEFAULT_PKI_DIR: &str = "/var/lib/tedge-dot/opcua/pki";
/// The application certificate and key inside a tedge-dot PKI directory.
const OWN_CERTIFICATE: &str = "own/certs/cert.der";
const OWN_PRIVATE_KEY: &str = "own/private/key.pem";
/// tedge-dot's application identity when `[connection]` does not set one.
const DEFAULT_APP_NAME: &str = "tedge-dot";
const DEFAULT_APP_URI: &str = "urn:tedge-dot";

const POLICY_URI_PREFIX: &str = "http://opcfoundation.org/UA/SecurityPolicy#";
const POLICY_NAMES: &str =
    "None, Basic128Rsa15, Basic256, Basic256Sha256, Aes128_Sha256_RsaOaep, Aes256_Sha256_RsaPss";

/// `[connection]`: only the keys opcua-scan uses; everything else is ignored.
#[derive(Debug, Default, Deserialize)]
struct Connection {
    application_name: Option<String>,
    application_uri: Option<String>,
    security_policy: Option<String>,
    security_mode: Option<String>,
    pki_dir: Option<String>,
    certificate: Option<String>,
    private_key: Option<String>,
    #[serde(default)]
    trust_any_server_certificate: bool,
    #[serde(default)]
    allow_deprecated_security: bool,
    #[serde(default)]
    allow_plaintext_password: bool,
}

/// `device.protocol_address`.
#[derive(Debug, Default, Deserialize)]
struct Endpoint {
    endpoint: Option<String>,
    security_policy: Option<String>,
    security_mode: Option<String>,
    user: Option<String>,
    password: Option<toml::Value>,
    password_file: Option<String>,
    user_certificate: Option<String>,
    user_private_key: Option<String>,
    trust_any_server_certificate: Option<bool>,
    allow_deprecated_security: Option<bool>,
    allow_plaintext_password: Option<bool>,
}

/// Who the session is activated as.
#[derive(Debug, PartialEq, Eq)]
enum Identity {
    Anonymous,
    UserName {
        user: String,
        password: Redacted,
    },
    X509 {
        certificate: PathBuf,
        private_key: PathBuf,
    },
}

/// The connection settings of one device, resolved and validated.
#[derive(Debug, PartialEq, Eq)]
pub struct DeviceSettings {
    endpoint: String,
    policy: SecurityPolicyArg,
    mode: SecurityModeArg,
    identity: Identity,
    trust_any_server_certificate: bool,
    /// Deprecated policies were allowed (`allow_deprecated_security`).
    allow_deprecated_security: bool,
    /// A password may travel unprotected (`allow_plaintext_password`).
    allow_plaintext_password: bool,
    /// The gateway's application certificate + key, name and URI (secured policies only).
    application: Option<Application>,
    /// The device's `type`.
    device_type: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct Application {
    name: String,
    uri: String,
    certificate: PathBuf,
    private_key: PathBuf,
}

/// Apply `--tedge-dot-config` (when given) to `args`. Values given on the
/// command line win.
pub fn apply(args: &mut ConnectionArgs) -> Result<()> {
    let Some(path) = args.tedge_dot_config.clone() else {
        return Ok(());
    };
    let settings = load(&path, args.device.as_deref())?;
    settings.apply_to(args);
    Ok(())
}

/// Read `path` and resolve the settings of `device` (or of the only device).
pub fn load(path: &Path, device: Option<&str>) -> Result<DeviceSettings> {
    let config_err = |reason: String| ScanError::Config {
        path: path.to_path_buf(),
        reason,
    };
    let text = std::fs::read_to_string(path).map_err(|e| config_err(e.to_string()))?;
    let doc: toml::Table = text
        .parse()
        .map_err(|e: toml::de::Error| config_err(e.to_string()))?;
    // Relative paths in the file are relative to the file, wherever we run.
    let base_dir = path
        .parent()
        .map(|p| {
            if p.as_os_str().is_empty() {
                Path::new(".")
            } else {
                p
            }
        })
        .and_then(|p| std::fs::canonicalize(p).ok());
    resolve(&doc, device, base_dir.as_deref()).map_err(config_err)
}

/// Resolve the settings of one device from a parsed config.
fn resolve(
    doc: &toml::Table,
    device: Option<&str>,
    base_dir: Option<&Path>,
) -> std::result::Result<DeviceSettings, String> {
    if let Some(protocol) = doc
        .get("connector")
        .and_then(|c| c.get("protocol"))
        .and_then(toml::Value::as_str)
        && protocol != "opcua"
    {
        return Err(format!(
            "this is a tedge-dot config for '{protocol}', not 'opcua'"
        ));
    }

    let connection: Connection = match doc.get("connection") {
        None => Connection::default(),
        Some(v) => v
            .clone()
            .try_into()
            .map_err(|e| format!("[connection]: {e}"))?,
    };

    let table = select_device(doc, device)?;
    let name = table
        .get("name")
        .and_then(toml::Value::as_str)
        .unwrap_or("?")
        .to_string();
    let device_err = |e: String| format!("device '{name}': {e}");
    let endpoint: Endpoint = match table.get("protocol_address") {
        None => Endpoint::default(),
        Some(v) => v
            .clone()
            .try_into()
            .map_err(|e| device_err(format!("protocol_address: {e}")))?,
    };
    let url = endpoint
        .endpoint
        .clone()
        .ok_or_else(|| device_err("protocol_address has no endpoint".into()))?;

    let (policy, mode) = security(&connection, &endpoint).map_err(device_err)?;
    let allow_deprecated_security = endpoint
        .allow_deprecated_security
        .unwrap_or(connection.allow_deprecated_security);
    if policy.is_deprecated() && !allow_deprecated_security {
        return Err(device_err(format!(
            "security_policy '{}' is deprecated (SHA-1); set allow_deprecated_security = true to use it",
            policy.name()
        )));
    }
    let identity = identity(&endpoint, base_dir).map_err(device_err)?;
    let application = if policy == SecurityPolicyArg::None {
        None
    } else {
        application(&connection, base_dir)?
    };

    Ok(DeviceSettings {
        endpoint: url,
        policy,
        mode,
        identity,
        trust_any_server_certificate: endpoint
            .trust_any_server_certificate
            .unwrap_or(connection.trust_any_server_certificate),
        allow_deprecated_security,
        allow_plaintext_password: endpoint
            .allow_plaintext_password
            .unwrap_or(connection.allow_plaintext_password),
        application,
        device_type: table
            .get("type")
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string),
    })
}

/// The `[[device]]` named `device`, or the only one.
pub(crate) fn select_device<'a>(
    doc: &'a toml::Table,
    device: Option<&str>,
) -> std::result::Result<&'a toml::Table, String> {
    let devices: Vec<&toml::Table> = doc
        .get("device")
        .and_then(toml::Value::as_array)
        .map(|a| a.iter().filter_map(toml::Value::as_table).collect())
        .unwrap_or_default();
    let names = || {
        devices
            .iter()
            .filter_map(|d| d.get("name").and_then(toml::Value::as_str))
            .collect::<Vec<_>>()
            .join(", ")
    };
    match device {
        Some(wanted) => devices
            .iter()
            .find(|d| d.get("name").and_then(toml::Value::as_str) == Some(wanted))
            .copied()
            .ok_or_else(|| format!("no device '{wanted}' (devices: {})", names())),
        None => match devices.as_slice() {
            [only] => Ok(only),
            [] => Err("the config has no [[device]]".into()),
            _ => Err(format!(
                "the config has several devices; choose one with --device ({})",
                names()
            )),
        },
    }
}

/// Policy and mode with tedge-dot's precedence: a device that sets its own
/// policy and no mode gets the policy's default mode, not the connection's.
fn security(
    conn: &Connection,
    ep: &Endpoint,
) -> std::result::Result<(SecurityPolicyArg, SecurityModeArg), String> {
    let policy = match ep
        .security_policy
        .as_deref()
        .or(conn.security_policy.as_deref())
    {
        None => SecurityPolicyArg::None,
        Some(text) => parse_policy(text)?,
    };
    let mode_text = match (&ep.security_mode, &ep.security_policy) {
        (Some(mode), _) => Some(mode.as_str()),
        (None, Some(_)) => None,
        (None, None) => conn.security_mode.as_deref(),
    };
    let secure = policy != SecurityPolicyArg::None;
    let mode = match mode_text {
        None if secure => SecurityModeArg::SignAndEncrypt,
        None => SecurityModeArg::None,
        Some(text) => parse_mode(text)?,
    };
    match (secure, mode) {
        (false, SecurityModeArg::None)
        | (true, SecurityModeArg::Sign | SecurityModeArg::SignAndEncrypt) => Ok((policy, mode)),
        (false, _) => Err(format!(
            "security_mode '{}' needs a security_policy other than None",
            mode_name(mode)
        )),
        (true, SecurityModeArg::None) => {
            Err("security_mode 'none' cannot be used with a security_policy other than None".into())
        }
    }
}

/// A tedge-dot policy name or URI.
fn parse_policy(text: &str) -> std::result::Result<SecurityPolicyArg, String> {
    match text.strip_prefix(POLICY_URI_PREFIX).unwrap_or(text) {
        "None" => Ok(SecurityPolicyArg::None),
        "Basic128Rsa15" => Ok(SecurityPolicyArg::Basic128Rsa15),
        "Basic256" => Ok(SecurityPolicyArg::Basic256),
        "Basic256Sha256" => Ok(SecurityPolicyArg::Basic256Sha256),
        "Aes128_Sha256_RsaOaep" => Ok(SecurityPolicyArg::Aes128Sha256RsaOaep),
        "Aes256_Sha256_RsaPss" => Ok(SecurityPolicyArg::Aes256Sha256RsaPss),
        _ => Err(format!(
            "security_policy '{text}' is not one of {POLICY_NAMES}"
        )),
    }
}

/// A mode in any case; `signandencrypt` is accepted like tedge-dot does.
fn parse_mode(text: &str) -> std::result::Result<SecurityModeArg, String> {
    match text.to_ascii_lowercase().as_str() {
        "none" => Ok(SecurityModeArg::None),
        "sign" => Ok(SecurityModeArg::Sign),
        "sign_and_encrypt" | "signandencrypt" => Ok(SecurityModeArg::SignAndEncrypt),
        _ => Err(format!(
            "security_mode '{text}' is not one of none, sign, sign_and_encrypt"
        )),
    }
}

fn mode_name(mode: SecurityModeArg) -> &'static str {
    match mode {
        SecurityModeArg::None => "none",
        SecurityModeArg::Sign => "sign",
        SecurityModeArg::SignAndEncrypt => "sign_and_encrypt",
    }
}

/// The device's identity, with tedge-dot's rules and messages.
fn identity(ep: &Endpoint, base_dir: Option<&Path>) -> std::result::Result<Identity, String> {
    let username_fields = ep.user.is_some() || ep.password.is_some() || ep.password_file.is_some();
    let cert_fields = ep.user_certificate.is_some() || ep.user_private_key.is_some();
    if username_fields && cert_fields {
        return Err(
            "user/password/password_file and user_certificate/user_private_key are both set; use one identity"
                .into(),
        );
    }
    if cert_fields {
        return match (&ep.user_certificate, &ep.user_private_key) {
            (Some(cert), Some(key)) => Ok(Identity::X509 {
                certificate: resolve_path(base_dir, cert),
                private_key: resolve_path(base_dir, key),
            }),
            (Some(_), None) => Err("user_certificate needs user_private_key".into()),
            _ => Err("user_private_key needs user_certificate".into()),
        };
    }
    let Some(user) = &ep.user else {
        if ep.password.is_some() || ep.password_file.is_some() {
            return Err("password/password_file needs user".into());
        }
        return Ok(Identity::Anonymous);
    };
    let password = match (&ep.password, &ep.password_file) {
        (Some(_), Some(_)) => return Err("password and password_file are both set; use one".into()),
        (Some(toml::Value::String(p)), None) => Redacted(p.clone()),
        (Some(_), None) => return Err("a password must be a string (or use password_file)".into()),
        (None, Some(file)) => read_password_file(&resolve_path(base_dir, file))?,
        (None, None) => Redacted::default(),
    };
    Ok(Identity::UserName {
        user: user.clone(),
        password,
    })
}

/// The first line of a password file. The error names the path only.
fn read_password_file(path: &Path) -> std::result::Result<Redacted, String> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        let reason = match e.kind() {
            std::io::ErrorKind::InvalidData => "not valid UTF-8".to_string(),
            _ => e.to_string(),
        };
        format!(
            "password_file '{}' cannot be read: {reason}",
            path.display()
        )
    })?;
    Ok(Redacted(
        text.lines().next().unwrap_or_default().to_string(),
    ))
}

/// The gateway's application certificate, when it exists. Explicitly
/// configured files must exist; the PKI directory's are optional.
fn application(
    conn: &Connection,
    base_dir: Option<&Path>,
) -> std::result::Result<Option<Application>, String> {
    let (certificate, private_key) = match (&conn.certificate, &conn.private_key) {
        (Some(cert), Some(key)) => {
            let (cert, key) = (resolve_path(base_dir, cert), resolve_path(base_dir, key));
            for (field, path) in [("certificate", &cert), ("private_key", &key)] {
                if !path.is_file() {
                    return Err(format!(
                        "[connection] {field} '{}' does not exist",
                        path.display()
                    ));
                }
            }
            (cert, key)
        }
        (Some(_), None) => return Err("[connection] certificate needs private_key".into()),
        (None, Some(_)) => return Err("[connection] private_key needs certificate".into()),
        (None, None) => {
            let pki = resolve_path(base_dir, conn.pki_dir.as_deref().unwrap_or(DEFAULT_PKI_DIR));
            let (cert, key) = (pki.join(OWN_CERTIFICATE), pki.join(OWN_PRIVATE_KEY));
            if !cert.is_file() || !key.is_file() {
                debug!(pki = %pki.display(), "no tedge-dot application certificate; using opcua-scan's own");
                return Ok(None);
            }
            (cert, key)
        }
    };
    Ok(Some(Application {
        name: conn
            .application_name
            .clone()
            .unwrap_or_else(|| DEFAULT_APP_NAME.into()),
        uri: conn
            .application_uri
            .clone()
            .unwrap_or_else(|| DEFAULT_APP_URI.into()),
        certificate,
        private_key,
    }))
}

/// `path` unchanged when absolute, joined to `base_dir` when relative.
fn resolve_path(base_dir: Option<&Path>, path: &str) -> PathBuf {
    let p = Path::new(path);
    match base_dir {
        Some(base) if p.is_relative() => base.join(p),
        _ => p.to_path_buf(),
    }
}

impl DeviceSettings {
    /// Fill `args` where the command line left a setting unset or at its default.
    pub fn apply_to(self, args: &mut ConnectionArgs) {
        if args.endpoint.is_none() {
            args.endpoint = Some(self.endpoint);
        }
        // Policy and mode only make sense as a pair: either both come from
        // the command line or both from the config.
        if args.security_policy == SecurityPolicyArg::None && args.security_mode.is_none() {
            args.security_policy = self.policy;
            args.security_mode = Some(self.mode);
        }
        args.allow_deprecated_security |= self.allow_deprecated_security;
        args.extras.refuse_plaintext_password = !self.allow_plaintext_password;
        // Likewise the identity: any identity flag on the command line wins whole.
        if args.user.is_none() && args.cert.is_none() {
            match self.identity {
                Identity::Anonymous => {}
                Identity::UserName { user, password } => {
                    args.user = Some(user);
                    args.extras.password = Some(password);
                }
                Identity::X509 {
                    certificate,
                    private_key,
                } => {
                    args.cert = Some(certificate);
                    args.key = Some(private_key);
                }
            }
        }
        args.trust_server |= self.trust_any_server_certificate;
        if let Some(app) = self.application {
            args.extras.application_name = Some(app.name);
            args.extras.application_uri = Some(app.uri);
            args.extras.certificate = Some((app.certificate, app.private_key));
        }
        args.extras.device_type = self.device_type;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn parse(text: &str) -> toml::Table {
        text.parse().expect("valid TOML")
    }

    fn settings(text: &str, device: Option<&str>) -> std::result::Result<DeviceSettings, String> {
        resolve(&parse(text), device, None)
    }

    const TWO_DEVICES: &str = r#"
        [connector]
        protocol = "opcua"
        [[device]]
        name = "plc-7"
        type = "acme-line"
        protocol_address = { endpoint = "opc.tcp://plc7:4840/" }
        [[device]]
        name = "plc-8"
        protocol_address = { endpoint = "opc.tcp://plc8:4840/" }
    "#;

    #[test]
    fn selects_the_named_device() {
        let s = settings(TWO_DEVICES, Some("plc-8")).unwrap();
        assert_eq!(s.endpoint, "opc.tcp://plc8:4840/");
        assert_eq!(s.device_type, None);
        let s = settings(TWO_DEVICES, Some("plc-7")).unwrap();
        assert_eq!(s.device_type.as_deref(), Some("acme-line"));
    }

    #[test]
    fn several_devices_need_a_choice() {
        let err = settings(TWO_DEVICES, None).unwrap_err();
        assert!(
            err.contains("--device") && err.contains("plc-7, plc-8"),
            "{err}"
        );
    }

    #[test]
    fn unknown_device_lists_the_available_ones() {
        let err = settings(TWO_DEVICES, Some("plc-9")).unwrap_err();
        assert!(
            err.contains("plc-9") && err.contains("plc-7, plc-8"),
            "{err}"
        );
    }

    #[test]
    fn single_device_needs_no_choice() {
        let s = settings(
            r#"[[device]]
               name = "only"
               protocol_address = { endpoint = "opc.tcp://x:4840" }"#,
            None,
        )
        .unwrap();
        assert_eq!(s.endpoint, "opc.tcp://x:4840");
        assert_eq!(s.policy, SecurityPolicyArg::None);
        assert_eq!(s.mode, SecurityModeArg::None);
        assert_eq!(s.identity, Identity::Anonymous);
    }

    #[test]
    fn other_protocols_are_refused() {
        let err = settings("[connector]\nprotocol = \"modbus\"\n", None).unwrap_err();
        assert!(err.contains("'modbus', not 'opcua'"), "{err}");
    }

    #[test]
    fn missing_endpoint_names_the_device() {
        let err = settings(
            "[[device]]\nname = \"plc-7\"\nprotocol_address = {}\n",
            None,
        )
        .unwrap_err();
        assert!(
            err.contains("device 'plc-7'") && err.contains("endpoint"),
            "{err}"
        );
    }

    /// One device with the given `[connection]` keys and `protocol_address` keys.
    fn with_security(conn: &str, ep: &str) -> std::result::Result<DeviceSettings, String> {
        settings(
            &format!(
                "[connection]\n{conn}\n[[device]]\nname = \"d\"\nprotocol_address = {{ endpoint = \"opc.tcp://x:4840\"{ep} }}\n"
            ),
            None,
        )
    }

    #[rstest]
    #[case::device_policy_does_not_inherit_mode(
        "security_mode = \"sign\"",
        ", security_policy = \"Basic256Sha256\"",
        SecurityPolicyArg::Basic256Sha256,
        SecurityModeArg::SignAndEncrypt
    )]
    #[case::connection_defaults(
        "security_policy = \"Aes256_Sha256_RsaPss\"\nsecurity_mode = \"sign\"",
        "",
        SecurityPolicyArg::Aes256Sha256RsaPss,
        SecurityModeArg::Sign
    )]
    #[case::device_mode_wins(
        "security_policy = \"Basic256Sha256\"\nsecurity_mode = \"sign\"",
        ", security_mode = \"SignAndEncrypt\"",
        SecurityPolicyArg::Basic256Sha256,
        SecurityModeArg::SignAndEncrypt
    )]
    #[case::policy_uri(
        "",
        ", security_policy = \"http://opcfoundation.org/UA/SecurityPolicy#Aes128_Sha256_RsaOaep\"",
        SecurityPolicyArg::Aes128Sha256RsaOaep,
        SecurityModeArg::SignAndEncrypt
    )]
    #[case::none_alone(
        "security_mode = \"sign\"",
        ", security_policy = \"None\"",
        SecurityPolicyArg::None,
        SecurityModeArg::None
    )]
    fn security_precedence(
        #[case] conn: &str,
        #[case] ep: &str,
        #[case] policy: SecurityPolicyArg,
        #[case] mode: SecurityModeArg,
    ) {
        let s = with_security(conn, ep).unwrap();
        assert_eq!((s.policy, s.mode), (policy, mode));
    }

    #[rstest]
    #[case::deprecated(
        "",
        ", security_policy = \"Basic256\"",
        "allow_deprecated_security = true"
    )]
    #[case::unknown("", ", security_policy = \"Basic512\"", "is not one of")]
    #[case::sign_without_policy("security_mode = \"sign\"", "", "security_mode")]
    #[case::none_with_policy(
        "",
        ", security_policy = \"Basic256Sha256\", security_mode = \"none\"",
        "security_mode 'none'"
    )]
    fn security_errors(#[case] conn: &str, #[case] ep: &str, #[case] needle: &str) {
        let err = with_security(conn, ep).unwrap_err();
        assert!(err.contains(needle), "{err}");
    }

    #[test]
    fn username_and_literal_password() {
        let s = with_security("", ", user = \"op\", password = \"env:FOO\"").unwrap();
        assert_eq!(
            s.identity,
            Identity::UserName {
                user: "op".into(),
                password: Redacted("env:FOO".into())
            }
        );
    }

    #[test]
    fn user_without_password_gets_an_empty_one() {
        let s = with_security("", ", user = \"op\"").unwrap();
        assert_eq!(
            s.identity,
            Identity::UserName {
                user: "op".into(),
                password: Redacted::default()
            }
        );
    }

    #[rstest]
    #[case::both(", user = \"op\", user_certificate = \"c.der\"", "use one identity")]
    #[case::password_without_user(", password = \"p\"", "needs user")]
    #[case::half_certificate(", user_certificate = \"c.der\"", "needs user_private_key")]
    #[case::password_twice(", user = \"op\", password = \"p\", password_file = \"f\"", "both set")]
    #[case::non_string_password(", user = \"op\", password = 1234", "must be a string")]
    fn identity_errors(#[case] ep: &str, #[case] needle: &str) {
        let err = with_security("", ep).unwrap_err();
        assert!(err.contains(needle), "{err}");
        assert!(!err.contains("1234"), "error quotes the password: {err}");
    }

    #[test]
    fn files_resolve_against_the_config_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("plc.pw"), "s3cret\nignored\n").unwrap();
        std::fs::write(
            dir.path().join("opcua.toml"),
            r#"[[device]]
               name = "d"
               protocol_address = { endpoint = "opc.tcp://x:4840", user = "op", password_file = "plc.pw" }"#,
        )
        .unwrap();
        let s = load(&dir.path().join("opcua.toml"), None).unwrap();
        assert_eq!(
            s.identity,
            Identity::UserName {
                user: "op".into(),
                password: Redacted("s3cret".into())
            }
        );
    }

    #[test]
    fn unreadable_password_file_names_the_path() {
        let err =
            with_security("", ", user = \"op\", password_file = \"/nonexistent/pw\"").unwrap_err();
        assert!(err.contains("/nonexistent/pw"), "{err}");
    }

    #[test]
    fn gateway_certificate_is_borrowed_for_secured_policies() {
        let dir = tempfile::tempdir().unwrap();
        let pki = dir.path().join("pki");
        std::fs::create_dir_all(pki.join("own/certs")).unwrap();
        std::fs::create_dir_all(pki.join("own/private")).unwrap();
        std::fs::write(pki.join(OWN_CERTIFICATE), b"cert").unwrap();
        std::fs::write(pki.join(OWN_PRIVATE_KEY), b"key").unwrap();
        let text = r#"
            [connection]
            pki_dir = "pki"
            application_uri = "urn:gw:tedge-dot"
            [[device]]
            name = "d"
            protocol_address = { endpoint = "opc.tcp://x:4840", security_policy = "Basic256Sha256" }
        "#;
        let s = resolve(&parse(text), None, Some(dir.path())).unwrap();
        let app = s.application.expect("gateway certificate");
        assert_eq!(app.uri, "urn:gw:tedge-dot");
        assert_eq!(app.name, DEFAULT_APP_NAME);
        assert_eq!(app.certificate, pki.join(OWN_CERTIFICATE));

        // Unsecured: the PKI is not looked at.
        let s = resolve(
            &parse(&text.replace("Basic256Sha256", "None")),
            None,
            Some(dir.path()),
        )
        .unwrap();
        assert_eq!(s.application, None);
    }

    #[test]
    fn missing_gateway_certificate_falls_back_to_own_identity() {
        let dir = tempfile::tempdir().unwrap();
        let s = resolve(
            &parse(
                r#"[connection]
                   pki_dir = "pki"
                   [[device]]
                   name = "d"
                   protocol_address = { endpoint = "opc.tcp://x:4840", security_policy = "Basic256Sha256" }"#,
            ),
            None,
            Some(dir.path()),
        )
        .unwrap();
        assert_eq!(s.application, None);
    }

    #[test]
    fn configured_certificate_must_exist() {
        let err = with_security(
            "certificate = \"/nonexistent/c.der\"\nprivate_key = \"/nonexistent/k.pem\"",
            ", security_policy = \"Basic256Sha256\"",
        )
        .unwrap_err();
        assert!(err.contains("/nonexistent/c.der"), "{err}");
    }

    #[rstest]
    #[case::device("", ", allow_deprecated_security = true")]
    #[case::connection("allow_deprecated_security = true", "")]
    fn deprecated_policy_with_opt_in(#[case] conn: &str, #[case] ep: &str) {
        let s = with_security(conn, &format!(", security_policy = \"Basic128Rsa15\"{ep}")).unwrap();
        assert_eq!(s.policy, SecurityPolicyArg::Basic128Rsa15);
        let mut args = cli_args(&[]);
        s.apply_to(&mut args);
        assert!(args.allow_deprecated_security);
        assert_eq!(
            args.effective_security().unwrap().0,
            SecurityPolicyArg::Basic128Rsa15
        );
    }

    #[test]
    fn device_opt_out_beats_connection_opt_in() {
        let err = with_security(
            "allow_deprecated_security = true",
            ", security_policy = \"Basic256\", allow_deprecated_security = false",
        )
        .unwrap_err();
        assert!(err.contains("deprecated"), "{err}");
    }

    #[test]
    fn plaintext_opt_in_lifts_the_refusal() {
        let s = with_security("allow_plaintext_password = true", ", user = \"op\"").unwrap();
        let mut args = cli_args(&[]);
        s.apply_to(&mut args);
        assert!(!args.extras.refuse_plaintext_password);
    }

    #[test]
    fn device_trust_overrides_connection() {
        let s = with_security(
            "trust_any_server_certificate = false",
            ", trust_any_server_certificate = true",
        )
        .unwrap();
        assert!(s.trust_any_server_certificate);
    }

    #[test]
    fn connector_only_settings_are_ignored() {
        let s = settings(
            r#"
            [connector]
            protocol = "opcua"
            poll_interval = "5s"
            [mqtt]
            host = "localhost"
            [connection]
            request_timeout_s = 3
            queue_size = 8
            allow_plaintext_password = true
            [[device]]
            name = "d"
            poll_interval = "1s"
            points_from = ["acme"]
            protocol_address = { endpoint = "opc.tcp://x:4840", namespaces = ["http://opcfoundation.org/UA/"], queue_size = 4 }
              [[device.point]]
              id = "t"
              datatype = "float64"
              address = { node_id = "ns=2;i=1" }
            "#,
            None,
        )
        .unwrap();
        assert_eq!(s.endpoint, "opc.tcp://x:4840");
    }

    fn cli_args(cli: &[&str]) -> ConnectionArgs {
        use clap::Parser;
        #[derive(Parser)]
        struct T {
            #[command(flatten)]
            c: ConnectionArgs,
        }
        let mut argv = vec!["t"];
        argv.extend_from_slice(cli);
        T::parse_from(argv).c
    }

    #[test]
    fn command_line_wins() {
        let s = with_security(
            "",
            ", security_policy = \"Basic256Sha256\", user = \"op\", password = \"p\"",
        )
        .unwrap();
        let mut args = cli_args(&[
            "opc.tcp://localhost:4840",
            "--security-policy",
            "Aes256Sha256RsaPss",
            "--security-mode",
            "Sign",
            "--user",
            "admin",
        ]);
        s.apply_to(&mut args);
        assert_eq!(args.endpoint.as_deref(), Some("opc.tcp://localhost:4840"));
        assert_eq!(args.security_policy, SecurityPolicyArg::Aes256Sha256RsaPss);
        assert_eq!(args.security_mode, Some(SecurityModeArg::Sign));
        assert_eq!(args.user.as_deref(), Some("admin"));
        assert_eq!(args.extras.password, None);
    }

    #[test]
    fn config_fills_unset_flags() {
        let s = with_security(
            "",
            ", security_policy = \"Basic256Sha256\", user = \"op\", password = \"p\"",
        )
        .unwrap();
        let mut args = cli_args(&[]);
        s.apply_to(&mut args);
        assert_eq!(args.endpoint.as_deref(), Some("opc.tcp://x:4840"));
        assert_eq!(args.security_policy, SecurityPolicyArg::Basic256Sha256);
        assert_eq!(args.security_mode, Some(SecurityModeArg::SignAndEncrypt));
        assert!(args.extras.refuse_plaintext_password);
        assert_eq!(args.user.as_deref(), Some("op"));
        assert_eq!(args.extras.password, Some(Redacted("p".into())));
    }
}
