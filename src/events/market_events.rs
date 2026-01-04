//! Normalized market events consumed by execution logic.
//!
//! CRITICAL: All events must include timestamps for deterministic replay.
//! The event normalization layer ensures raw WebSocket/REST data never
//! reaches the execution logic directly.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::assets::Asset;

/// Primary event enum - all execution logic consumes ONLY this type.
///
/// Every event includes the asset it belongs to, ensuring complete
/// isolation between asset execution contexts.
#[derive(Debug, Clone)]
pub enum MarketEvent {
    // ========== Round Lifecycle Events ==========
    /// Emitted when a new 15-minute round starts.
    /// Triggers reset of all per-round state.
    RoundStarted {
        asset: Asset,
        round_id: String,
        condition_id: String,
        yes_token_id: String,
        no_token_id: String,
        start_time: DateTime<Utc>,
        end_time: DateTime<Utc>,
    },

    /// Emitted when a round ends (either naturally or detected via API).
    RoundEnded {
        asset: Asset,
        round_id: String,
    },

    /// Emitted when round identity cannot be confirmed unambiguously.
    /// Trading MUST be disabled when this event is received.
    RoundAmbiguous {
        asset: Asset,
        reason: String,
        timestamp: DateTime<Utc>,
    },

    // ========== Order Book Events ==========
    /// Best bid/ask update from the order book watcher.
    /// Contains pricing for BOTH YES and NO outcomes.
    BestBidAskUpdate {
        asset: Asset,
        yes_best_bid: Option<f64>,
        yes_best_ask: Option<f64>,
        no_best_bid: Option<f64>,
        no_best_ask: Option<f64>,
        timestamp: DateTime<Utc>,
    },

    /// Full order book snapshot (received on connect/reconnect).
    OrderBookSnapshot {
        asset: Asset,
        snapshot: OrderBookSnapshot,
        timestamp: DateTime<Utc>,
    },

    // ========== WebSocket Infrastructure Events ==========
    /// WebSocket connection established.
    WebSocketConnected {
        asset: Asset,
        timestamp: DateTime<Utc>,
    },

    /// WebSocket connection lost.
    WebSocketDisconnected {
        asset: Asset,
        reason: String,
        timestamp: DateTime<Utc>,
    },

    /// WebSocket successfully reconnected after disconnect.
    /// State has been reconciled with REST snapshot.
    WebSocketReconnected {
        asset: Asset,
        timestamp: DateTime<Utc>,
    },

    /// Heartbeat/ping timeout - connection may be stale.
    HeartbeatTimeout {
        asset: Asset,
        last_heartbeat: DateTime<Utc>,
        timestamp: DateTime<Utc>,
    },

    // ========== Order Lifecycle Events (Milestone 2) ==========
    /// Order successfully placed.
    OrderPlaced {
        asset: Asset,
        order_id: String,
        side: OrderSide,
        price: f64,
        size: f64,
        timestamp: DateTime<Utc>,
    },

    /// Order partially or fully filled.
    OrderFilled {
        asset: Asset,
        order_id: String,
        filled_qty: f64,
        remaining_qty: f64,
        fill_price: f64,
        timestamp: DateTime<Utc>,
    },

    /// Order canceled (by user or system).
    OrderCanceled {
        asset: Asset,
        order_id: String,
        reason: String,
        timestamp: DateTime<Utc>,
    },

    /// Order rejected by exchange.
    OrderRejected {
        asset: Asset,
        order_id: String,
        reason: String,
        timestamp: DateTime<Utc>,
    },

    // ========== Error Events ==========
    /// API error that doesn't require shutdown.
    ApiError {
        asset: Asset,
        error: String,
        recoverable: bool,
        timestamp: DateTime<Utc>,
    },
}

impl MarketEvent {
    /// Returns the asset this event belongs to.
    pub fn asset(&self) -> Asset {
        match self {
            MarketEvent::RoundStarted { asset, .. } => *asset,
            MarketEvent::RoundEnded { asset, .. } => *asset,
            MarketEvent::RoundAmbiguous { asset, .. } => *asset,
            MarketEvent::BestBidAskUpdate { asset, .. } => *asset,
            MarketEvent::OrderBookSnapshot { asset, .. } => *asset,
            MarketEvent::WebSocketConnected { asset, .. } => *asset,
            MarketEvent::WebSocketDisconnected { asset, .. } => *asset,
            MarketEvent::WebSocketReconnected { asset, .. } => *asset,
            MarketEvent::HeartbeatTimeout { asset, .. } => *asset,
            MarketEvent::OrderPlaced { asset, .. } => *asset,
            MarketEvent::OrderFilled { asset, .. } => *asset,
            MarketEvent::OrderCanceled { asset, .. } => *asset,
            MarketEvent::OrderRejected { asset, .. } => *asset,
            MarketEvent::ApiError { asset, .. } => *asset,
        }
    }

    /// Returns the timestamp of this event (for replay determinism).
    pub fn timestamp(&self) -> DateTime<Utc> {
        match self {
            MarketEvent::RoundStarted { start_time, .. } => *start_time,
            MarketEvent::RoundEnded { .. } => Utc::now(), // TODO: Add timestamp field
            MarketEvent::RoundAmbiguous { timestamp, .. } => *timestamp,
            MarketEvent::BestBidAskUpdate { timestamp, .. } => *timestamp,
            MarketEvent::OrderBookSnapshot { timestamp, .. } => *timestamp,
            MarketEvent::WebSocketConnected { timestamp, .. } => *timestamp,
            MarketEvent::WebSocketDisconnected { timestamp, .. } => *timestamp,
            MarketEvent::WebSocketReconnected { timestamp, .. } => *timestamp,
            MarketEvent::HeartbeatTimeout { timestamp, .. } => *timestamp,
            MarketEvent::OrderPlaced { timestamp, .. } => *timestamp,
            MarketEvent::OrderFilled { timestamp, .. } => *timestamp,
            MarketEvent::OrderCanceled { timestamp, .. } => *timestamp,
            MarketEvent::OrderRejected { timestamp, .. } => *timestamp,
            MarketEvent::ApiError { timestamp, .. } => *timestamp,
        }
    }
}

/// Order side for trading operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderSide {
    YesBuy,
    YesSell,
    NoBuy,
    NoSell,
}

impl OrderSide {
    /// Returns true if this is a buy order.
    pub fn is_buy(&self) -> bool {
        matches!(self, OrderSide::YesBuy | OrderSide::NoBuy)
    }

    /// Returns true if this is for the YES outcome.
    pub fn is_yes(&self) -> bool {
        matches!(self, OrderSide::YesBuy | OrderSide::YesSell)
    }
}

/// Raw market data before normalization.
///
/// This is an intermediate type used by the WebSocket layer.
/// It MUST be converted to a MarketEvent before being consumed
/// by execution logic.
#[derive(Debug, Clone)]
pub struct RawMarketData {
    pub asset: Asset,
    pub payload: serde_json::Value,
    pub received_at: DateTime<Utc>,
}

/// Order book snapshot containing best bid/ask for both outcomes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBookSnapshot {
    pub yes_best_bid: Option<PriceLevel>,
    pub yes_best_ask: Option<PriceLevel>,
    pub no_best_bid: Option<PriceLevel>,
    pub no_best_ask: Option<PriceLevel>,
    pub yes_token_id: String,
    pub no_token_id: String,
    pub timestamp: DateTime<Utc>,
}

/// A single price level in the order book.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceLevel {
    pub price: f64,
    pub size: f64,
}

/// Information about the current round.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoundInfo {
    pub round_id: String,
    pub condition_id: String,
    pub yes_token_id: String,
    pub no_token_id: String,
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub question: String,
}

impl RoundInfo {
    /// Returns true if the round is currently active.
    pub fn is_active(&self) -> bool {
        let now = Utc::now();
        now >= self.start_time && now < self.end_time
    }

    /// Returns the remaining time in this round.
    pub fn time_remaining(&self) -> chrono::Duration {
        let now = Utc::now();
        if now >= self.end_time {
            chrono::Duration::zero()
        } else {
            self.end_time - now
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_order_side_is_buy() {
        assert!(OrderSide::YesBuy.is_buy());
        assert!(OrderSide::NoBuy.is_buy());
        assert!(!OrderSide::YesSell.is_buy());
        assert!(!OrderSide::NoSell.is_buy());
    }

    #[test]
    fn test_order_side_is_yes() {
        assert!(OrderSide::YesBuy.is_yes());
        assert!(OrderSide::YesSell.is_yes());
        assert!(!OrderSide::NoBuy.is_yes());
        assert!(!OrderSide::NoSell.is_yes());
    }

    #[test]
    fn test_market_event_asset() {
        let event = MarketEvent::WebSocketConnected {
            asset: Asset::BTC,
            timestamp: Utc::now(),
        };
        assert_eq!(event.asset(), Asset::BTC);
    }
}
