//! Event system for the Polymarket arbitrage bot.
//!
//! All external signals MUST be converted into normalized internal events
//! BEFORE being consumed by execution logic. Raw WebSocket data must NEVER
//! drive execution logic directly.

mod market_events;

pub use market_events::{
    MarketEvent,
    OrderSide,
    RawMarketData,
    OrderBookSnapshot,
    PriceLevel,
    RoundInfo,
};
