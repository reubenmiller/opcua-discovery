//! Connection / scan **profiles**.
//!
//! A profile is a small YAML file holding default values for the connection,
//! traversal and filter options. It lets operators save the settings for a
//! given server (endpoint, security, common include/exclude patterns) and reuse
//! them with `--profile NAME`.
//!
//! Resolution order for `--profile VALUE`:
//! 1. If `VALUE` is an existing file path, it is loaded directly.
//! 2. Otherwise it is treated as a *name* and looked up as
//!    `<config-dir>/opcua-scan/profiles/<name>.yaml` (and `.yml`), where
//!    `<config-dir>` is the platform config directory (e.g. `~/.config` on
//!    Linux), falling back to `~/.opcua-scan/profiles/<name>.yaml`.
//!
//! Merge semantics: **CLI flags win**. A profile only fills values the user did
//! not provide. Detectable "unset" values are `Option` fields left as `None`,
//! empty repeatable (`Vec`) fields, and scalar fields still at their built-in
//! default. This keeps the behaviour predictable for a discovery tool.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::cli::{
    ConnectionArgs, Depth, FilterArgs, NodeClassArg, SecurityPolicyArg, Strategy, TraversalArgs,
};
use crate::error::{Result, ScanError};

/// A persisted set of default options for a server / scan.
///
/// Every field is optional; only the ones present in the file are applied.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Profile {
    // --- connection ---
    /// Default endpoint URL.
    pub endpoint: Option<String>,
    /// Default username.
    pub user: Option<String>,
    /// Default password (supports the same `env:VAR` form as the CLI flag).
    pub password: Option<String>,
    /// Security policy name (e.g. `Basic256Sha256`).
    pub security_policy: Option<String>,
    /// Security mode name (e.g. `SignAndEncrypt`).
    pub security_mode: Option<String>,
    /// Allow the deprecated policies `Basic128Rsa15` and `Basic256`.
    pub allow_deprecated_security: Option<bool>,
    /// Auto-trust the server certificate.
    pub trust_server: Option<bool>,
    /// PKI directory.
    pub pki_dir: Option<PathBuf>,
    /// Per-request timeout, e.g. `10s`.
    pub timeout: Option<String>,
    /// Retry count for transient failures.
    pub retry: Option<u32>,

    // --- traversal ---
    /// Traversal depth: a number or `unlimited`.
    pub depth: Option<String>,
    /// Default root node id.
    pub root: Option<String>,
    /// Traversal strategy (`bfs` / `dfs`).
    pub strategy: Option<String>,
    /// Concurrent browse workers.
    pub workers: Option<usize>,
    /// Browse page size.
    pub page_size: Option<u32>,
    /// Max children per branch before truncation.
    pub max_children: Option<usize>,

    // --- filters ---
    /// Include globs.
    #[serde(default)]
    pub include: Vec<String>,
    /// Include regexes.
    #[serde(default)]
    pub include_regex: Vec<String>,
    /// Exclude globs.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Exclude regexes.
    #[serde(default)]
    pub exclude_regex: Vec<String>,
    /// Restrict to these namespace URIs.
    #[serde(default)]
    pub include_ns: Vec<String>,
    /// Restrict to a single namespace URI.
    pub ns_uri: Option<String>,
    /// Restrict to a single namespace index.
    pub ns_index: Option<u16>,
    /// Node class filter names.
    #[serde(default)]
    pub node_class: Vec<String>,
    /// Data type filter values.
    #[serde(default)]
    pub datatype: Vec<String>,
}

impl Profile {
    /// Resolve and load a profile from a `--profile` value (path or name).
    pub fn load(value: &str) -> Result<Profile> {
        let path = resolve_path(value)?;
        let text = std::fs::read_to_string(&path).map_err(|e| ScanError::Config {
            path: path.clone(),
            reason: format!("cannot read profile: {e}"),
        })?;
        serde_yaml::from_str(&text).map_err(|e| ScanError::Config {
            path,
            reason: format!("invalid profile YAML: {e}"),
        })
    }

    /// Apply this profile's values onto parsed CLI args. CLI values win; the
    /// profile only fills unset (`None` / empty / still-default) fields.
    pub fn apply(
        &self,
        connection: &mut ConnectionArgs,
        traversal: &mut TraversalArgs,
        filters: &mut FilterArgs,
    ) -> Result<()> {
        self.apply_connection(connection)?;
        self.apply_traversal(traversal)?;
        self.apply_filters(filters)?;
        Ok(())
    }

    /// Apply only the connection-related defaults (used by `connect`).
    pub fn apply_connection(&self, c: &mut ConnectionArgs) -> Result<()> {
        fill_opt(&mut c.endpoint, self.endpoint.clone());
        fill_opt(&mut c.user, self.user.clone());
        fill_opt(&mut c.password, self.password.clone());
        fill_opt(&mut c.pki_dir, self.pki_dir.clone());
        if let Some(p) = &self.security_policy
            && c.security_policy == SecurityPolicyArg::None
        {
            c.security_policy = parse_enum(p, "security-policy")?;
        }
        if let Some(m) = &self.security_mode
            && c.security_mode.is_none()
        {
            c.security_mode = Some(parse_enum(m, "security-mode")?);
        }
        if let Some(allow) = self.allow_deprecated_security {
            c.allow_deprecated_security |= allow;
        }
        if let Some(t) = self.trust_server {
            c.trust_server = c.trust_server || t;
        }
        if let Some(t) = &self.timeout
            && c.timeout == default_timeout()
        {
            c.timeout = crate::cli::parse_duration(t).map_err(ScanError::InvalidArgument)?;
        }
        if let Some(r) = self.retry
            && c.retry == DEFAULT_RETRY
        {
            c.retry = r;
        }
        Ok(())
    }

    fn apply_traversal(&self, t: &mut TraversalArgs) -> Result<()> {
        if let Some(d) = &self.depth
            && t.depth == Depth::Limited(DEFAULT_DEPTH)
        {
            t.depth = crate::cli::parse_depth(d).map_err(ScanError::InvalidArgument)?;
        }
        fill_opt(&mut t.root, self.root.clone());
        if let Some(s) = &self.strategy
            && t.strategy == Strategy::Bfs
        {
            t.strategy = parse_enum(s, "strategy")?;
        }
        if let Some(w) = self.workers
            && t.workers == DEFAULT_WORKERS
        {
            t.workers = w;
        }
        if let Some(p) = self.page_size
            && t.page_size == DEFAULT_PAGE_SIZE
        {
            t.page_size = p;
        }
        if t.max_children.is_none() {
            t.max_children = self.max_children;
        }
        Ok(())
    }

    fn apply_filters(&self, f: &mut FilterArgs) -> Result<()> {
        fill_vec(&mut f.include, &self.include);
        fill_vec(&mut f.include_regex, &self.include_regex);
        fill_vec(&mut f.exclude, &self.exclude);
        fill_vec(&mut f.exclude_regex, &self.exclude_regex);
        fill_vec(&mut f.include_ns, &self.include_ns);
        fill_opt(&mut f.ns_uri, self.ns_uri.clone());
        if f.ns_index.is_none() {
            f.ns_index = self.ns_index;
        }
        if f.node_class.is_empty() {
            for name in &self.node_class {
                f.node_class.push(parse_enum::<NodeClassArg>(name, "type")?);
            }
        }
        fill_vec(&mut f.datatype, &self.datatype);
        Ok(())
    }
}

/// Built-in CLI defaults, mirrored here so profile merge can detect "unset".
const DEFAULT_RETRY: u32 = 3;
const DEFAULT_DEPTH: u32 = 3;
const DEFAULT_WORKERS: usize = 8;
const DEFAULT_PAGE_SIZE: u32 = 200;

fn default_timeout() -> std::time::Duration {
    std::time::Duration::from_secs(10)
}

fn fill_opt<T>(target: &mut Option<T>, value: Option<T>) {
    if target.is_none() {
        *target = value;
    }
}

fn fill_vec(target: &mut Vec<String>, value: &[String]) {
    if target.is_empty() && !value.is_empty() {
        *target = value.to_vec();
    }
}

/// Parse a clap `ValueEnum` from a string (case-insensitive), with a helpful error.
fn parse_enum<T: clap::ValueEnum>(s: &str, field: &str) -> Result<T> {
    T::from_str(s, true).map_err(|_| {
        let options: Vec<String> = T::value_variants()
            .iter()
            .filter_map(|v| v.to_possible_value().map(|p| p.get_name().to_string()))
            .collect();
        ScanError::InvalidArgument(format!(
            "invalid {field} value '{s}' in profile (expected one of: {})",
            options.join(", ")
        ))
    })
}

/// Resolve a `--profile` value to a concrete file path.
fn resolve_path(value: &str) -> Result<PathBuf> {
    let direct = Path::new(value);
    if direct.is_file() {
        return Ok(direct.to_path_buf());
    }

    for dir in profile_dirs() {
        for ext in ["yaml", "yml"] {
            let candidate = dir.join(format!("{value}.{ext}"));
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    Err(ScanError::InvalidArgument(format!(
        "profile '{value}' not found (looked for a file path or a named profile in {})",
        profile_dirs()
            .iter()
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

/// Candidate directories that hold named profiles.
fn profile_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(cfg) = dirs::config_dir() {
        dirs.push(cfg.join("opcua-scan").join("profiles"));
    }
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".opcua-scan").join("profiles"));
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::SecurityPolicyArg;

    /// A baseline `ConnectionArgs` equivalent to "no CLI flags given" (all at
    /// their built-in defaults), so profile merge can be exercised.
    fn default_connection() -> ConnectionArgs {
        ConnectionArgs {
            endpoint: None,
            user: None,
            password: None,
            cert: None,
            key: None,
            security_policy: SecurityPolicyArg::None,
            security_mode: None,
            allow_deprecated_security: false,
            trust_server: false,
            pki_dir: None,
            timeout: default_timeout(),
            retry: DEFAULT_RETRY,
            cache: None,
            tedge_dot_config: None,
            device: None,
            extras: Default::default(),
        }
    }

    #[test]
    fn profile_parses_kebab_case_fields() {
        let yaml = r#"
endpoint: opc.tcp://host:4840/
user: operator
security-policy: Basic256Sha256
security-mode: SignAndEncrypt
trust-server: true
include:
  - "**/Temperature"
node-class:
  - Variable
"#;
        let p: Profile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(p.endpoint.as_deref(), Some("opc.tcp://host:4840/"));
        assert_eq!(p.user.as_deref(), Some("operator"));
        assert_eq!(p.security_policy.as_deref(), Some("Basic256Sha256"));
        assert_eq!(p.trust_server, Some(true));
        assert_eq!(p.include, vec!["**/Temperature".to_string()]);
        assert_eq!(p.node_class, vec!["Variable".to_string()]);
    }

    #[test]
    fn unknown_profile_field_is_rejected() {
        let yaml = "endpoint: opc.tcp://x/\nbogus-field: 1\n";
        assert!(serde_yaml::from_str::<Profile>(yaml).is_err());
    }

    #[test]
    fn apply_connection_fills_unset_values() {
        let profile: Profile = serde_yaml::from_str(
            "endpoint: opc.tcp://host:4840/\nsecurity-policy: Basic256Sha256\ntrust-server: true\nretry: 5\n",
        )
        .unwrap();
        let mut args = default_connection();
        profile.apply_connection(&mut args).unwrap();
        assert_eq!(args.endpoint.as_deref(), Some("opc.tcp://host:4840/"));
        assert_eq!(args.security_policy, SecurityPolicyArg::Basic256Sha256);
        assert!(args.trust_server);
        assert_eq!(args.retry, 5);
    }

    #[test]
    fn cli_values_win_over_profile() {
        let profile: Profile = serde_yaml::from_str(
            "endpoint: opc.tcp://from-profile/\nsecurity-policy: Basic256Sha256\nretry: 5\n",
        )
        .unwrap();
        let mut args = default_connection();
        // Simulate CLI-provided values.
        args.endpoint = Some("opc.tcp://from-cli/".to_string());
        args.security_policy = SecurityPolicyArg::Aes256Sha256RsaPss;
        args.retry = 9;
        profile.apply_connection(&mut args).unwrap();
        // None of the profile values should override the explicit CLI values.
        assert_eq!(args.endpoint.as_deref(), Some("opc.tcp://from-cli/"));
        assert_eq!(args.security_policy, SecurityPolicyArg::Aes256Sha256RsaPss);
        assert_eq!(args.retry, 9);
    }

    #[test]
    fn load_resolves_a_direct_file_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.yaml");
        std::fs::write(&path, "endpoint: opc.tcp://host:4840/\n").unwrap();
        let profile = Profile::load(path.to_str().unwrap()).unwrap();
        assert_eq!(profile.endpoint.as_deref(), Some("opc.tcp://host:4840/"));
    }

    #[test]
    fn load_reports_missing_profile() {
        let err = Profile::load("definitely-not-a-real-profile-xyz").unwrap_err();
        assert!(err.to_string().contains("not found"), "got: {err}");
    }
}
