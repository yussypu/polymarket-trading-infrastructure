//! Connectors for Polymarket APIs.
//!
//! This module provides low-level clients for interacting with Polymarket's
//! REST and WebSocket APIs. All data fetched here is raw and must be
//! normalized through the events layer before use.

mod polymarket;
pub mod websocket;
mod auth;

pub use polymarket::PolymarketApiClient;
pub use websocket::{PolymarketWebSocket, ConnectionState, BookMessage};
pub use auth::ApiCredentials;
