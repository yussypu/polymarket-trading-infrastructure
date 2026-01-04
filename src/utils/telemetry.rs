//! Telemetry and structured logging setup.
//!
//! Provides consistent logging across all components with:
//! - Asset-tagged log lines for filtering
//! - Structured output for log aggregation
//! - Configurable verbosity via RUST_LOG

use tracing_subscriber::{
    fmt::{self, format::FmtSpan},
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter,
};

/// Initializes the telemetry/logging system.
///
/// Uses RUST_LOG environment variable for configuration.
/// Defaults to INFO level if not set.
///
/// Example RUST_LOG values:
/// - `info` - All info and above
/// - `polymarket_bot=debug` - Debug for our crate, default for others
/// - `polymarket_bot=trace,tokio=warn` - Trace for us, warn for tokio
pub fn init_telemetry() {
    // Create env filter from RUST_LOG or use default
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| {
            // Default: INFO for everything, DEBUG for our crate
            EnvFilter::new("info,polymarket_bot=debug")
        });

    // Create the subscriber
    let subscriber = tracing_subscriber::registry()
        .with(env_filter)
        .with(
            fmt::layer()
                .with_target(true)
                .with_thread_ids(true)
                .with_level(true)
                .with_file(false)
                .with_line_number(false)
                .compact()
        );

    // Initialize
    subscriber.init();
}

/// Initializes telemetry with JSON output (for production).
pub fn init_telemetry_json() {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,polymarket_bot=debug"));

    let subscriber = tracing_subscriber::registry()
        .with(env_filter)
        .with(
            fmt::layer()
                .json()
                .with_span_events(FmtSpan::CLOSE)
        );

    subscriber.init();
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: Can only call init once per test run
    // #[test]
    // fn test_init_telemetry() {
    //     init_telemetry();
    // }
}
