//! Polymarket Arbitrage Bot - Main Entry Point
//!
//! Spawns 4 independent asset executors for BTC, ETH, SOL, and XRP.
//! Each executor operates in complete isolation - a failure in one
//! asset does NOT affect any other asset.

use tracing::{error, info, warn};

use polymarket_bot::assets::{Asset, AssetExecutor};
use polymarket_bot::connectors::ApiCredentials;
use polymarket_bot::utils::init_telemetry;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load environment variables from .env file if present
    if let Err(e) = dotenvy::dotenv() {
        eprintln!("Note: No .env file found or error loading it: {}", e);
    }

    // Initialize telemetry/logging
    init_telemetry();

    info!("╔════════════════════════════════════════════════════════╗");
    info!("║   Polymarket Arbitrage Bot - Milestone 1               ║");
    info!("║   Infrastructure Foundation                            ║");
    info!("╚════════════════════════════════════════════════════════╝");
    info!("");

    // Load API credentials
    let credentials = ApiCredentials::from_env();
    if credentials.is_none() {
        warn!("No API credentials found in environment");
        warn!("Set POLYMARKET_API_KEY and POLYMARKET_API_SECRET for authenticated access");
        warn!("Continuing with unauthenticated access (read-only markets data)");
    } else {
        info!("API credentials loaded successfully");
    }

    info!("");
    info!("Starting 4 independent asset executors...");
    info!("");

    // Get all assets
    let assets = Asset::all();
    let mut handles = Vec::new();

    // Spawn an executor for each asset
    for asset in assets {
        let creds = credentials.clone();

        info!("[{}] Spawning executor for {}", asset, asset.name());

        let handle = tokio::spawn(async move {
            let executor = AssetExecutor::new(asset, creds);

            // Run the executor - this loops forever unless there's a fatal error
            executor.run().await;

            // If we get here, the executor stopped
            error!("[{}] Executor stopped unexpectedly!", asset);
        });

        handles.push((asset, handle));
    }

    info!("");
    info!("All asset executors started. Monitoring for events...");
    info!("Press Ctrl+C to stop.");
    info!("");

    // Wait for all tasks - they should run forever
    // If any task completes, it's an error
    for (asset, handle) in handles {
        match handle.await {
            Ok(()) => {
                error!("[{}] Executor task completed unexpectedly", asset);
            }
            Err(e) => {
                if e.is_cancelled() {
                    info!("[{}] Executor was cancelled", asset);
                } else {
                    error!("[{}] Executor task panicked: {:?}", asset, e);
                }
            }
        }
    }

    info!("All executors stopped. Shutting down.");
    Ok(())
}

/// Signal handler for graceful shutdown (future enhancement).
#[allow(dead_code)]
async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("Failed to install CTRL+C signal handler");
    info!("Shutdown signal received");
}
