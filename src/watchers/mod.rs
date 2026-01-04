//! Watcher subsystems for market monitoring.
//!
//! Each watcher is responsible for a specific aspect of market observation
//! and emits normalized MarketEvents. All watchers operate independently
//! per asset.

mod orderbook_watcher;
mod round_watcher;

pub use orderbook_watcher::OrderBookWatcher;
pub use round_watcher::RoundTransitionWatcher;
