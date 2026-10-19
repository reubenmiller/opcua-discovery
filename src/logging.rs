//! Logging / tracing setup.
//!
//! Logs go to **stderr** so that machine-readable output on **stdout** stays
//! clean and pipe-friendly. Verbosity is controlled by `-v/--verbose` and
//! `-q/--quiet` flags, or overridden entirely by the `RUST_LOG` environment
//! variable for advanced filtering.

use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// Initialise the global tracing subscriber.
///
/// * `verbosity` – number of `-v` flags (0 = warn, 1 = info, 2 = debug, 3+ = trace).
/// * `quiet` – suppress all but errors.
///
/// If `RUST_LOG` is set it takes precedence over the derived level.
pub fn init(verbosity: u8, quiet: bool) {
    let default_directive = if quiet {
        "error"
    } else {
        match verbosity {
            0 => "warn",
            1 => "info",
            2 => "debug",
            _ => "trace",
        }
    };

    // `opcua` itself is very chatty at debug/trace, so unless the user explicitly
    // asks for a lot of verbosity we keep the library at `warn`.
    let crate_filter = if verbosity >= 3 {
        default_directive.to_string()
    } else {
        format!("{default_directive},opcua=warn,opcua_client=warn")
    };

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(crate_filter));

    let fmt_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(false)
        .compact();

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .init();
}
