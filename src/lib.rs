//! Polymarket Arbitrage Bot - Milestone 1: Infrastructure Foundation
//!
//! This crate provides the infrastructure layer for a capital-preservation-first
//! execution engine for Polymarket crypto "Up/Down" markets.
//!
//! # Architecture
//!
//! - **Event-driven**: All external signals are normalized into internal events
//! - **Asset isolation**: Each of 4 assets (BTC, ETH, SOL, XRP) operates independently
//! - **Deterministic**: All events include timestamps for future replay capability
//! - **No shared mutable state**: Execution correctness never depends on timing
//!
//! # Milestone 1 Scope
//!
//! This milestone delivers the infrastructure layer:
//! - Real-time market data streaming via WebSocket
//! - Zero-delay round transition detection
//! - Graceful WebSocket failure handling with automatic reconnection
//! - All raw data normalized to typed internal events
//!
//! # Usage
//!
//! ```no_run
//! use polymarket_bot::assets::{Asset, AssetExecutor};
//! use polymarket_bot::connectors::ApiCredentials;
//!
//! #[tokio::main]
//! async fn main() {
//!     // Load credentials
//!     let credentials = ApiCredentials::from_env();
//!
//!     // Create and run an executor for BTC
//!     let executor = AssetExecutor::new(Asset::BTC, credentials);
//!     executor.run().await;
//! }
//! ```

pub mod assets;
pub mod connectors;
pub mod events;
pub mod utils;
pub mod watchers;

// Re-export commonly used types
pub use assets::{Asset, AssetExecutor};
pub use connectors::{ApiCredentials, PolymarketApiClient};
pub use events::{MarketEvent, OrderSide, RoundInfo};
