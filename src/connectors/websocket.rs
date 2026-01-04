//! WebSocket client for Polymarket with reconnection logic.
//!
//! Key requirements:
//! 1. Heartbeat monitoring: Ping every 10s
//! 2. Exponential backoff: 1s, 2s, 4s, 8s, 16s, max 60s
//! 3. State rebuild on reconnect: Fetch REST snapshot
//! 4. Never blocks other assets

use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{interval, Instant};
use tokio_tungstenite::{
    connect_async, tungstenite::protocol::Message, MaybeTlsStream, WebSocketStream,
};
use tracing::{debug, error, info, warn};

use crate::assets::Asset;
use crate::events::RawMarketData;

use super::auth::ApiCredentials;

/// Default WebSocket URL for Polymarket.
const DEFAULT_WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";

/// Heartbeat interval - send ping every 10 seconds.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

/// Maximum time to wait for pong response.
const PONG_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum reconnection backoff.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Initial reconnection backoff.
const INITIAL_BACKOFF: Duration = Duration::from_secs(1);

#[derive(Debug, Error)]
pub enum WebSocketError {
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Send failed: {0}")]
    SendFailed(String),

    #[error("Receive failed: {0}")]
    ReceiveFailed(String),

    #[error("Heartbeat timeout")]
    HeartbeatTimeout,

    #[error("Connection closed by server: {0}")]
    ConnectionClosed(String),

    #[error("Subscription failed: {0}")]
    SubscriptionFailed(String),
}

/// Connection state for the WebSocket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
}

/// WebSocket client for Polymarket with automatic reconnection.
pub struct PolymarketWebSocket {
    asset: Asset,
    url: String,
    credentials: Option<ApiCredentials>,
    connection: Option<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    state: ConnectionState,
    last_pong: Instant,
    current_backoff: Duration,
    subscribed_tokens: Vec<String>,
    raw_data_tx: mpsc::Sender<RawMarketData>,
}

impl PolymarketWebSocket {
    /// Creates a new WebSocket client for the given asset.
    pub fn new(
        asset: Asset,
        credentials: Option<ApiCredentials>,
        raw_data_tx: mpsc::Sender<RawMarketData>,
    ) -> Self {
        Self {
            asset,
            url: DEFAULT_WS_URL.to_string(),
            credentials,
            connection: None,
            state: ConnectionState::Disconnected,
            last_pong: Instant::now(),
            current_backoff: INITIAL_BACKOFF,
            subscribed_tokens: Vec::new(),
            raw_data_tx,
        }
    }

    /// Creates a new WebSocket client with a custom URL.
    pub fn with_url(
        asset: Asset,
        url: String,
        credentials: Option<ApiCredentials>,
        raw_data_tx: mpsc::Sender<RawMarketData>,
    ) -> Self {
        Self {
            asset,
            url,
            credentials,
            connection: None,
            state: ConnectionState::Disconnected,
            last_pong: Instant::now(),
            current_backoff: INITIAL_BACKOFF,
            subscribed_tokens: Vec::new(),
            raw_data_tx,
        }
    }

    /// Returns the current connection state.
    pub fn state(&self) -> ConnectionState {
        self.state
    }

    /// Establishes a WebSocket connection.
    pub async fn connect(&mut self) -> Result<(), WebSocketError> {
        info!("[{}] Connecting to WebSocket: {}", self.asset, self.url);
        self.state = ConnectionState::Connecting;

        match connect_async(&self.url).await {
            Ok((ws_stream, _response)) => {
                info!("[{}] WebSocket connected successfully", self.asset);
                self.connection = Some(ws_stream);
                self.state = ConnectionState::Connected;
                self.last_pong = Instant::now();
                self.current_backoff = INITIAL_BACKOFF;
                Ok(())
            }
            Err(e) => {
                error!("[{}] WebSocket connection failed: {}", self.asset, e);
                self.state = ConnectionState::Disconnected;
                Err(WebSocketError::ConnectionFailed(e.to_string()))
            }
        }
    }

    /// Subscribes to orderbook updates for the given token IDs.
    pub async fn subscribe(&mut self, token_ids: Vec<String>) -> Result<(), WebSocketError> {
        let conn = self
            .connection
            .as_mut()
            .ok_or_else(|| WebSocketError::SubscriptionFailed("Not connected".to_string()))?;

        let subscribe_msg = SubscribeMessage {
            assets_ids: token_ids.clone(),
            type_: "market".to_string(),
        };

        let msg_json = serde_json::to_string(&subscribe_msg)
            .map_err(|e| WebSocketError::SubscriptionFailed(e.to_string()))?;

        debug!("[{}] Sending subscription: {}", self.asset, msg_json);

        conn.send(Message::Text(msg_json))
            .await
            .map_err(|e| WebSocketError::SendFailed(e.to_string()))?;

        self.subscribed_tokens = token_ids;
        info!(
            "[{}] Subscribed to {} token(s)",
            self.asset,
            self.subscribed_tokens.len()
        );

        Ok(())
    }

    /// Unsubscribes from the given token IDs.
    pub async fn unsubscribe(&mut self, token_ids: Vec<String>) -> Result<(), WebSocketError> {
        let conn = self
            .connection
            .as_mut()
            .ok_or_else(|| WebSocketError::SubscriptionFailed("Not connected".to_string()))?;

        let unsubscribe_msg = UnsubscribeMessage {
            assets_ids: token_ids.clone(),
            operation: "unsubscribe".to_string(),
        };

        let msg_json = serde_json::to_string(&unsubscribe_msg)
            .map_err(|e| WebSocketError::SubscriptionFailed(e.to_string()))?;

        debug!("[{}] Sending unsubscribe: {}", self.asset, msg_json);

        conn.send(Message::Text(msg_json))
            .await
            .map_err(|e| WebSocketError::SendFailed(e.to_string()))?;

        self.subscribed_tokens.retain(|id| !token_ids.contains(id));
        Ok(())
    }

    /// Sends a ping message to keep the connection alive.
    pub async fn send_ping(&mut self) -> Result<(), WebSocketError> {
        let conn = self
            .connection
            .as_mut()
            .ok_or_else(|| WebSocketError::SendFailed("Not connected".to_string()))?;

        conn.send(Message::Text("PING".to_string()))
            .await
            .map_err(|e| WebSocketError::SendFailed(e.to_string()))?;

        debug!("[{}] Sent PING", self.asset);
        Ok(())
    }

    /// Checks if the heartbeat has timed out.
    pub fn is_heartbeat_timeout(&self) -> bool {
        self.last_pong.elapsed() > HEARTBEAT_INTERVAL + PONG_TIMEOUT
    }

    /// Attempts to reconnect with exponential backoff.
    pub async fn reconnect(&mut self) -> Result<(), WebSocketError> {
        info!(
            "[{}] Attempting reconnection with {}ms backoff",
            self.asset,
            self.current_backoff.as_millis()
        );

        self.state = ConnectionState::Reconnecting;

        // Close existing connection if any
        if let Some(mut conn) = self.connection.take() {
            let _ = conn.close(None).await;
        }

        // Wait for backoff duration
        tokio::time::sleep(self.current_backoff).await;

        // Attempt to reconnect
        match self.connect().await {
            Ok(()) => {
                // Resubscribe to previous tokens
                if !self.subscribed_tokens.is_empty() {
                    let tokens = self.subscribed_tokens.clone();
                    self.subscribe(tokens).await?;
                }
                info!("[{}] Reconnection successful", self.asset);
                Ok(())
            }
            Err(e) => {
                // Increase backoff for next attempt
                self.current_backoff = std::cmp::min(self.current_backoff * 2, MAX_BACKOFF);
                Err(e)
            }
        }
    }

    /// Runs the WebSocket event loop.
    ///
    /// This method handles:
    /// - Receiving messages and forwarding to the raw data channel
    /// - Sending periodic heartbeats
    /// - Detecting connection issues and triggering reconnection
    ///
    /// Returns when the connection is lost (caller should call reconnect).
    pub async fn run_until_disconnect(&mut self) -> Result<(), WebSocketError> {
        let mut heartbeat_interval = interval(HEARTBEAT_INTERVAL);

        loop {
            let conn = match self.connection.as_mut() {
                Some(conn) => conn,
                None => return Err(WebSocketError::ConnectionFailed("No connection".to_string())),
            };

            tokio::select! {
                // Handle incoming messages
                msg_result = conn.next() => {
                    match msg_result {
                        Some(Ok(msg)) => {
                            if let Err(e) = self.handle_message(msg).await {
                                warn!("[{}] Error handling message: {}", self.asset, e);
                            }
                        }
                        Some(Err(e)) => {
                            error!("[{}] WebSocket receive error: {}", self.asset, e);
                            return Err(WebSocketError::ReceiveFailed(e.to_string()));
                        }
                        None => {
                            info!("[{}] WebSocket stream ended", self.asset);
                            return Err(WebSocketError::ConnectionClosed("Stream ended".to_string()));
                        }
                    }
                }

                // Send periodic heartbeats
                _ = heartbeat_interval.tick() => {
                    // Check for heartbeat timeout first
                    if self.is_heartbeat_timeout() {
                        warn!("[{}] Heartbeat timeout detected", self.asset);
                        return Err(WebSocketError::HeartbeatTimeout);
                    }

                    // Send ping
                    if let Err(e) = self.send_ping().await {
                        error!("[{}] Failed to send ping: {}", self.asset, e);
                        return Err(e);
                    }
                }
            }
        }
    }

    /// Handles an incoming WebSocket message.
    async fn handle_message(&mut self, msg: Message) -> Result<(), WebSocketError> {
        match msg {
            Message::Text(text) => {
                // Check for PONG response
                if text == "PONG" || text.to_uppercase() == "PONG" {
                    debug!("[{}] Received PONG", self.asset);
                    self.last_pong = Instant::now();
                    return Ok(());
                }

                // Parse as JSON and forward to raw data channel
                match serde_json::from_str::<serde_json::Value>(&text) {
                    Ok(payload) => {
                        let raw_data = RawMarketData {
                            asset: self.asset,
                            payload,
                            received_at: Utc::now(),
                        };

                        // Non-blocking send - if channel is full, log warning
                        if let Err(e) = self.raw_data_tx.try_send(raw_data) {
                            warn!("[{}] Failed to forward raw data: {}", self.asset, e);
                        }
                    }
                    Err(e) => {
                        debug!("[{}] Non-JSON message received: {} ({})", self.asset, text, e);
                    }
                }
            }
            Message::Ping(data) => {
                // Respond to server ping with pong
                if let Some(conn) = self.connection.as_mut() {
                    let _ = conn.send(Message::Pong(data)).await;
                }
            }
            Message::Pong(_) => {
                debug!("[{}] Received Pong frame", self.asset);
                self.last_pong = Instant::now();
            }
            Message::Close(frame) => {
                let reason = frame
                    .map(|f| f.reason.to_string())
                    .unwrap_or_else(|| "Unknown".to_string());
                info!("[{}] WebSocket closed by server: {}", self.asset, reason);
                return Err(WebSocketError::ConnectionClosed(reason));
            }
            Message::Binary(_) => {
                debug!("[{}] Received binary message (ignored)", self.asset);
            }
            Message::Frame(_) => {
                // Raw frames are typically not seen at this level
            }
        }

        Ok(())
    }

    /// Gracefully closes the WebSocket connection.
    pub async fn close(&mut self) {
        if let Some(mut conn) = self.connection.take() {
            let _ = conn.close(None).await;
        }
        self.state = ConnectionState::Disconnected;
        info!("[{}] WebSocket closed", self.asset);
    }

    /// Returns the currently subscribed token IDs.
    pub fn subscribed_tokens(&self) -> &[String] {
        &self.subscribed_tokens
    }
}

// ============ Message Types ============

#[derive(Debug, Serialize)]
struct SubscribeMessage {
    assets_ids: Vec<String>,
    #[serde(rename = "type")]
    type_: String,
}

#[derive(Debug, Serialize)]
struct UnsubscribeMessage {
    assets_ids: Vec<String>,
    operation: String,
}

/// Parsed WebSocket message from Polymarket.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "event_type")]
pub enum WsMessage {
    #[serde(rename = "book")]
    Book(BookMessage),

    #[serde(rename = "price_change")]
    PriceChange(PriceChangeMessage),

    #[serde(rename = "last_trade_price")]
    LastTradePrice(LastTradePriceMessage),

    #[serde(rename = "tick_size_change")]
    TickSizeChange(TickSizeChangeMessage),
}

#[derive(Debug, Clone, Deserialize)]
pub struct BookMessage {
    pub asset_id: String,
    pub market: Option<String>,
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
    pub timestamp: Option<String>,
    pub hash: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BookLevel {
    pub price: String,
    pub size: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PriceChangeMessage {
    pub asset_id: String,
    pub market: Option<String>,
    pub price: String,
    pub size: String,
    pub side: String,
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LastTradePriceMessage {
    pub asset_id: String,
    pub market: Option<String>,
    pub price: String,
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TickSizeChangeMessage {
    pub asset_id: String,
    pub market: Option<String>,
    pub tick_size: String,
    pub timestamp: Option<String>,
}

impl std::fmt::Debug for PolymarketWebSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolymarketWebSocket")
            .field("asset", &self.asset)
            .field("url", &self.url)
            .field("state", &self.state)
            .field("subscribed_tokens", &self.subscribed_tokens.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_state_initial() {
        let (tx, _rx) = mpsc::channel(100);
        let ws = PolymarketWebSocket::new(Asset::BTC, None, tx);
        assert_eq!(ws.state(), ConnectionState::Disconnected);
    }

    #[test]
    fn test_subscribe_message_serialization() {
        let msg = SubscribeMessage {
            assets_ids: vec!["token1".to_string(), "token2".to_string()],
            type_: "market".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("assets_ids"));
        assert!(json.contains("token1"));
        assert!(json.contains("market"));
    }
}
