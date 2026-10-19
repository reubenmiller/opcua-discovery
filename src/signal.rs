//! Graceful cancellation via Ctrl+C.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tracing::{info, warn};

/// Install a Ctrl+C handler and return a flag that flips to `true` on the first
/// interrupt. A second Ctrl+C aborts the process immediately.
pub fn install() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let handler_flag = flag.clone();
    tokio::spawn(async move {
        loop {
            if tokio::signal::ctrl_c().await.is_err() {
                warn!("failed to listen for Ctrl+C");
                return;
            }
            if handler_flag.swap(true, Ordering::SeqCst) {
                warn!("second interrupt received; aborting");
                std::process::exit(130);
            }
            info!("interrupt received; finishing in-flight work and shutting down cleanly");
        }
    });
    flag
}
