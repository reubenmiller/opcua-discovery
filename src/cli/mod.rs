//! Command-line interface definition (clap derive).
//!
//! The CLI is split into shared argument groups (connection, traversal, filters,
//! output) that are reused across subcommands so that flags behave consistently
//! everywhere.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};

pub mod acquire;
pub mod browse;
pub mod connect;
pub mod sample;
pub mod search;

use crate::cache::Cache;
use crate::error::Result;
use crate::opcua::namespaces::NamespaceTable;

/// Read the namespace table for `endpoint`, using the metadata cache when the
/// `--cache` flag is set (and populating it on a miss).
pub async fn load_namespaces(
    session: &opcua::client::Session,
    connection: &ConnectionArgs,
    endpoint: &str,
) -> Result<NamespaceTable> {
    if let Some(dir) = &connection.cache {
        let cache = Cache::open(dir);
        if let Some(uris) = cache.load_namespaces(endpoint) {
            return Ok(NamespaceTable::from_uris(uris));
        }
        let table = NamespaceTable::read(session).await?;
        cache.store_namespaces(endpoint, table.uris());
        return Ok(table);
    }
    NamespaceTable::read(session).await
}

/// `opcua-scan` — efficiently scan and discover an OPC UA namespace.
#[derive(Debug, Parser)]
#[command(
    name = "opcua-scan",
    version,
    about = "Efficiently scan and discover an OPC UA namespace.",
    long_about = "opcua-scan connects to an OPC UA server and browses its address \
space efficiently, with include/exclude patterns, depth control, rich metadata \
filters, subscribable detection and multiple export formats. Designed for large \
industrial servers (10k-500k+ nodes)."
)]
pub struct Cli {
    /// Increase logging verbosity (repeatable: -v info, -vv debug, -vvv trace).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Suppress all logging except errors.
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Load defaults from a named profile or profile file. CLI flags override it.
    #[arg(long, global = true, value_name = "NAME|PATH")]
    pub profile: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

/// Top level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Verify connectivity and print server metadata and endpoints.
    Connect(connect::ConnectArgs),
    /// Browse and discover nodes in the address space (primary command).
    Browse(browse::BrowseArgs),
    /// Search the address space for nodes matching a query (fuzzy).
    Search(search::SearchArgs),
    /// Subscribe to matching variables and print live value updates.
    Sample(sample::SampleArgs),
    /// Run config-driven data acquisition (scan, poll, subscribe, history).
    Acquire(acquire::AcquireArgs),
}

/// Connection, security and authentication arguments.
#[derive(Debug, Args, Clone)]
pub struct ConnectionArgs {
    /// Endpoint URL, e.g. opc.tcp://10.0.0.10:4840
    #[arg(value_name = "ENDPOINT")]
    pub endpoint: Option<String>,

    /// Username for user/password authentication.
    #[arg(long, value_name = "USER")]
    pub user: Option<String>,

    /// Password. Use `env:VAR` to read from an environment variable, or omit to
    /// be prompted securely. Secrets are never logged or printed.
    #[arg(long, value_name = "PASSWORD|env:VAR", requires = "user")]
    pub password: Option<String>,

    /// Client certificate (PEM/DER) for certificate authentication / secure channel.
    #[arg(long, value_name = "FILE")]
    pub cert: Option<PathBuf>,

    /// Client private key (PEM) matching `--cert`.
    #[arg(long, value_name = "FILE", requires = "cert")]
    pub key: Option<PathBuf>,

    /// Security policy to request (also tedge-dot names and policy URIs).
    #[arg(long, value_enum, default_value_t = SecurityPolicyArg::None, ignore_case = true)]
    pub security_policy: SecurityPolicyArg,

    /// Message security mode to request [default: SignAndEncrypt with a
    /// security policy, else None].
    #[arg(long, value_enum, ignore_case = true)]
    pub security_mode: Option<SecurityModeArg>,

    /// Allow the deprecated (SHA-1) policies Basic128Rsa15 and Basic256.
    #[arg(long)]
    pub allow_deprecated_security: bool,

    /// Automatically trust the server certificate (INSECURE; for lab use only).
    #[arg(long)]
    pub trust_server: bool,

    /// Directory holding the client PKI (own cert, private key, trusted certs).
    #[arg(long, value_name = "DIR")]
    pub pki_dir: Option<PathBuf>,

    /// Per-request timeout, e.g. 5s, 500ms.
    #[arg(long, value_name = "DURATION", default_value = "10s", value_parser = parse_duration)]
    pub timeout: Duration,

    /// Number of retries for transient failures before giving up on a node.
    #[arg(long, default_value_t = 3, value_name = "N")]
    pub retry: u32,

    /// Directory for the best-effort metadata cache (caches the namespace table).
    #[arg(long, value_name = "DIR")]
    pub cache: Option<PathBuf>,

    /// Take the connection from a tedge-dot OPC UA connector config
    /// (`[connection]` + the device's `protocol_address`). CLI flags override it.
    #[arg(long, value_name = "FILE")]
    pub tedge_dot_config: Option<PathBuf>,

    /// Device (`[[device]] name`) in the tedge-dot config; optional when it has one device.
    #[arg(long, value_name = "NAME", requires = "tedge_dot_config")]
    pub device: Option<String>,

    /// Settings without a CLI flag, filled from `--tedge-dot-config`.
    #[arg(skip)]
    pub extras: ConnectionExtras,
}

impl ConnectionArgs {
    /// The policy and mode to connect with: a secured policy without a mode
    /// gets SignAndEncrypt. Inconsistent pairs and deprecated policies without
    /// `--allow-deprecated-security` are errors.
    pub fn effective_security(
        &self,
    ) -> std::result::Result<(SecurityPolicyArg, SecurityModeArg), String> {
        let policy = self.security_policy;
        if policy.is_deprecated() && !self.allow_deprecated_security {
            return Err(format!(
                "security policy {} is deprecated (SHA-1); pass --allow-deprecated-security to use it",
                policy.name()
            ));
        }
        let secure = policy != SecurityPolicyArg::None;
        match (secure, self.security_mode) {
            (false, None | Some(SecurityModeArg::None)) => Ok((policy, SecurityModeArg::None)),
            (true, None) => Ok((policy, SecurityModeArg::SignAndEncrypt)),
            (true, Some(SecurityModeArg::None)) => Err(format!(
                "security mode None cannot be used with security policy {} (use Sign or SignAndEncrypt)",
                policy.name()
            )),
            (true, Some(mode)) => Ok((policy, mode)),
            (false, Some(mode)) => Err(format!(
                "security mode {mode:?} needs a security policy other than None"
            )),
        }
    }
}

/// Connection settings that have no CLI flag. They are only set from a
/// tedge-dot connector config (see [`crate::tedge_dot::connection`]).
#[derive(Debug, Clone, Default)]
pub struct ConnectionExtras {
    /// A password taken literally (no `env:` lookup, no prompt).
    pub password: Option<Redacted>,
    /// Client application name presented to the server.
    pub application_name: Option<String>,
    /// Client application URI presented to the server.
    pub application_uri: Option<String>,
    /// Application instance certificate and its private key.
    pub certificate: Option<(PathBuf, PathBuf)>,
    /// The device's `type`, used as the point library type.
    pub device_type: Option<String>,
    /// Refuse (instead of warn about) a password that would travel
    /// unprotected: a tedge-dot config without `allow_plaintext_password`.
    pub refuse_plaintext_password: bool,
}

/// A secret string whose `Debug` output is redacted.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Redacted(pub String);

impl std::fmt::Debug for Redacted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("***")
    }
}

/// Traversal-control arguments shared by browse/search/sample.
#[derive(Debug, Args, Clone)]
pub struct TraversalArgs {
    /// Maximum traversal depth: a number, or `unlimited`. Root is depth 0.
    #[arg(long, default_value = "3", value_name = "N|unlimited", value_parser = parse_depth)]
    pub depth: Depth,

    /// Starting node id (e.g. `i=85` for Objects, `ns=2;s=Foo`). Defaults to Objects.
    #[arg(long, value_name = "NODEID")]
    pub root: Option<String>,

    /// Traversal strategy.
    #[arg(long, value_enum, default_value_t = Strategy::Bfs)]
    pub strategy: Strategy,

    /// Number of concurrent browse workers (keep modest to avoid server overload).
    #[arg(long, default_value_t = 8, value_name = "N")]
    pub workers: usize,

    /// References requested per browse call (server pagination page size).
    #[arg(long, default_value_t = 200, value_name = "N")]
    pub page_size: u32,

    /// Stop expanding a branch once it exceeds this many children; report truncation.
    #[arg(long, value_name = "N")]
    pub max_children: Option<usize>,
}

/// Filtering arguments shared by browse/search/sample.
#[derive(Debug, Args, Clone, Default)]
pub struct FilterArgs {
    /// Only traverse/keep branches whose browse path matches this glob (repeatable).
    #[arg(long, value_name = "GLOB")]
    pub include: Vec<String>,

    /// Like --include but using a regular expression against the browse path.
    #[arg(long = "include-regex", value_name = "REGEX")]
    pub include_regex: Vec<String>,

    /// Prune (do not browse children of) branches matching this glob (repeatable).
    #[arg(long, value_name = "GLOB")]
    pub exclude: Vec<String>,

    /// Like --exclude but using a regular expression.
    #[arg(long = "exclude-regex", value_name = "REGEX")]
    pub exclude_regex: Vec<String>,

    /// Only include nodes in these namespace URIs (repeatable).
    #[arg(long = "include-ns", value_name = "URI")]
    pub include_ns: Vec<String>,

    /// Restrict to a namespace URI.
    #[arg(long = "ns-uri", value_name = "URI")]
    pub ns_uri: Option<String>,

    /// Restrict to a namespace index (note: indexes can change per session).
    #[arg(long = "ns-index", value_name = "N")]
    pub ns_index: Option<u16>,

    /// Filter by node class (comma separated): Object,Variable,Method,ObjectType,...
    #[arg(
        long = "type",
        value_delimiter = ',',
        value_name = "CLASS",
        ignore_case = true
    )]
    pub node_class: Vec<NodeClassArg>,

    /// Filter Variables by data type (builtin name, NodeId, or namespace URI).
    #[arg(long, value_delimiter = ',', value_name = "TYPE")]
    pub datatype: Vec<String>,

    /// Only nodes that are readable (current-read bit of AccessLevel).
    #[arg(long)]
    pub readable: bool,

    /// Only nodes that are writable (current-write bit of AccessLevel).
    #[arg(long)]
    pub writable: bool,

    /// Only nodes that are historizing.
    #[arg(long)]
    pub historizing: bool,

    /// Only Variable nodes that look eligible for monitoring (subscribable).
    #[arg(long)]
    pub subscribable: bool,
}

/// Output / export arguments.
#[derive(Debug, Args, Clone)]
pub struct OutputArgs {
    /// Human-readable display format.
    #[arg(long, value_enum, default_value_t = Format::Tree)]
    pub format: Format,

    /// Machine-readable export encoding (use with --file or to stream to stdout).
    #[arg(long, value_enum, value_name = "ENCODING")]
    pub output: Option<Encoding>,

    /// Write export to this file instead of stdout.
    #[arg(long, value_name = "FILE")]
    pub file: Option<PathBuf>,

    /// Read and display current values for matching variables.
    #[arg(long)]
    pub preview_values: bool,

    /// Launch the interactive TUI explorer instead of streaming results.
    #[arg(long)]
    pub tui: bool,

    /// Repeat the scan every DURATION until interrupted (e.g. 30s, 5m).
    #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
    pub watch: Option<Duration>,
}

/// Traversal depth limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    /// Limited to the given number of levels (root = 0).
    Limited(u32),
    /// No depth limit.
    Unlimited,
}

impl Depth {
    /// Whether a node at `depth` may still have its children browsed.
    pub fn allows_children_at(&self, depth: u32) -> bool {
        match self {
            Depth::Unlimited => true,
            Depth::Limited(max) => depth < *max,
        }
    }
}

/// Traversal strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Strategy {
    /// Breadth-first (best for shallow discovery).
    Bfs,
    /// Depth-first (useful for targeted deep exploration).
    Dfs,
}

/// Human display format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// Unicode tree.
    Tree,
    /// Aligned table.
    Table,
    /// Pretty JSON to stdout.
    Json,
}

/// Machine-readable export encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Encoding {
    /// JSON array.
    Json,
    /// Newline-delimited JSON.
    Ndjson,
    /// Comma separated values.
    Csv,
    /// tedge-dot OPC UA point library (TOML; `browse` only). Merges into an
    /// existing `--file`.
    #[value(name = "tedge-dot")]
    TedgeDot,
}

/// OPC UA node class selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum NodeClassArg {
    /// Object node.
    Object,
    /// Variable node.
    Variable,
    /// Method node.
    Method,
    /// ObjectType node.
    ObjectType,
    /// VariableType node.
    VariableType,
    /// ReferenceType node.
    ReferenceType,
    /// DataType node.
    DataType,
    /// View node.
    View,
}

/// Security policy selector. Every policy async-opcua implements; the
/// aliases are tedge-dot's spellings and the policy URIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SecurityPolicyArg {
    /// No security.
    #[value(alias = "http://opcfoundation.org/UA/SecurityPolicy#None")]
    None,
    /// Basic128Rsa15 (deprecated, needs --allow-deprecated-security).
    #[value(
        name = "Basic128Rsa15",
        alias = "http://opcfoundation.org/UA/SecurityPolicy#Basic128Rsa15"
    )]
    Basic128Rsa15,
    /// Basic256 (deprecated, needs --allow-deprecated-security).
    #[value(
        name = "Basic256",
        alias = "http://opcfoundation.org/UA/SecurityPolicy#Basic256"
    )]
    Basic256,
    /// Basic256Sha256.
    #[value(
        name = "Basic256Sha256",
        alias = "http://opcfoundation.org/UA/SecurityPolicy#Basic256Sha256"
    )]
    Basic256Sha256,
    /// Aes128Sha256RsaOaep.
    #[value(
        name = "Aes128Sha256RsaOaep",
        alias = "Aes128_Sha256_RsaOaep",
        alias = "http://opcfoundation.org/UA/SecurityPolicy#Aes128_Sha256_RsaOaep"
    )]
    Aes128Sha256RsaOaep,
    /// Aes256Sha256RsaPss.
    #[value(
        name = "Aes256Sha256RsaPss",
        alias = "Aes256_Sha256_RsaPss",
        alias = "http://opcfoundation.org/UA/SecurityPolicy#Aes256_Sha256_RsaPss"
    )]
    Aes256Sha256RsaPss,
}

impl SecurityPolicyArg {
    /// The CLI name.
    pub fn name(self) -> &'static str {
        match self {
            SecurityPolicyArg::None => "None",
            SecurityPolicyArg::Basic128Rsa15 => "Basic128Rsa15",
            SecurityPolicyArg::Basic256 => "Basic256",
            SecurityPolicyArg::Basic256Sha256 => "Basic256Sha256",
            SecurityPolicyArg::Aes128Sha256RsaOaep => "Aes128Sha256RsaOaep",
            SecurityPolicyArg::Aes256Sha256RsaPss => "Aes256Sha256RsaPss",
        }
    }

    /// The SHA-1 policies OPC UA deprecated.
    pub fn is_deprecated(self) -> bool {
        matches!(
            self,
            SecurityPolicyArg::Basic128Rsa15 | SecurityPolicyArg::Basic256
        )
    }

    /// Parse a CLI name, tedge-dot name or policy URI (any case).
    pub fn parse(text: &str) -> Option<SecurityPolicyArg> {
        <Self as ValueEnum>::from_str(text.trim(), true).ok()
    }
}

/// Message security mode selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SecurityModeArg {
    /// None.
    None,
    /// Sign.
    Sign,
    /// Sign and encrypt.
    #[value(
        name = "SignAndEncrypt",
        alias = "sign_and_encrypt",
        alias = "signandencrypt"
    )]
    SignAndEncrypt,
}

impl SecurityModeArg {
    /// Parse a CLI or tedge-dot mode name (any case).
    pub fn parse(text: &str) -> Option<SecurityModeArg> {
        <Self as ValueEnum>::from_str(text.trim(), true).ok()
    }
}

/// Parse a human duration like `5s`, `500ms`, `1m`.
pub fn parse_duration(s: &str) -> std::result::Result<Duration, String> {
    humantime::parse_duration(s).map_err(|e| format!("invalid duration '{s}': {e}"))
}

/// Parse a depth value: a non-negative integer or `unlimited`.
pub fn parse_depth(s: &str) -> std::result::Result<Depth, String> {
    if s.eq_ignore_ascii_case("unlimited") || s == "-1" {
        return Ok(Depth::Unlimited);
    }
    s.parse::<u32>()
        .map(Depth::Limited)
        .map_err(|_| format!("invalid depth '{s}': expected a number or 'unlimited'"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn args(cli: &[&str]) -> ConnectionArgs {
        #[derive(Parser)]
        struct T {
            #[command(flatten)]
            c: ConnectionArgs,
        }
        let mut argv = vec!["t"];
        argv.extend_from_slice(cli);
        T::try_parse_from(argv).expect("valid arguments").c
    }

    #[rstest]
    #[case("None", SecurityPolicyArg::None)]
    #[case("basic128rsa15", SecurityPolicyArg::Basic128Rsa15)]
    #[case("Basic256", SecurityPolicyArg::Basic256)]
    #[case("Basic256Sha256", SecurityPolicyArg::Basic256Sha256)]
    #[case("Aes128_Sha256_RsaOaep", SecurityPolicyArg::Aes128Sha256RsaOaep)]
    #[case("Aes256Sha256RsaPss", SecurityPolicyArg::Aes256Sha256RsaPss)]
    #[case(
        "http://opcfoundation.org/UA/SecurityPolicy#Aes256_Sha256_RsaPss",
        SecurityPolicyArg::Aes256Sha256RsaPss
    )]
    fn policy_names(#[case] text: &str, #[case] policy: SecurityPolicyArg) {
        assert_eq!(SecurityPolicyArg::parse(text), Some(policy));
        assert_eq!(args(&["--security-policy", text]).security_policy, policy);
    }

    #[rstest]
    #[case("sign_and_encrypt", SecurityModeArg::SignAndEncrypt)]
    #[case("SignAndEncrypt", SecurityModeArg::SignAndEncrypt)]
    #[case("sign", SecurityModeArg::Sign)]
    #[case("none", SecurityModeArg::None)]
    fn mode_names(#[case] text: &str, #[case] mode: SecurityModeArg) {
        assert_eq!(SecurityModeArg::parse(text), Some(mode));
    }

    #[rstest]
    #[case::defaults(&[], SecurityPolicyArg::None, SecurityModeArg::None)]
    #[case::policy_alone(
        &["--security-policy", "Basic256Sha256"],
        SecurityPolicyArg::Basic256Sha256,
        SecurityModeArg::SignAndEncrypt
    )]
    #[case::explicit_sign(
        &["--security-policy", "Aes128Sha256RsaOaep", "--security-mode", "Sign"],
        SecurityPolicyArg::Aes128Sha256RsaOaep,
        SecurityModeArg::Sign
    )]
    #[case::deprecated_with_opt_in(
        &["--security-policy", "Basic256", "--allow-deprecated-security"],
        SecurityPolicyArg::Basic256,
        SecurityModeArg::SignAndEncrypt
    )]
    fn effective(
        #[case] cli: &[&str],
        #[case] policy: SecurityPolicyArg,
        #[case] mode: SecurityModeArg,
    ) {
        assert_eq!(args(cli).effective_security(), Ok((policy, mode)));
    }

    #[rstest]
    #[case::deprecated(&["--security-policy", "Basic128Rsa15"], "--allow-deprecated-security")]
    #[case::none_with_policy(
        &["--security-policy", "Basic256Sha256", "--security-mode", "None"],
        "cannot be used"
    )]
    #[case::sign_without_policy(&["--security-mode", "Sign"], "needs a security policy")]
    fn effective_errors(#[case] cli: &[&str], #[case] needle: &str) {
        let err = args(cli).effective_security().unwrap_err();
        assert!(err.contains(needle), "{err}");
    }
}
