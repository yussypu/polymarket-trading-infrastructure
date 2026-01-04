//! Order book watcher for real-time price streaming.
//!
//! Owns the WebSocket connection and emits normalized price events.
//! Handles:
//! - WebSocket lifecycle management
//! - Price normalization from raw JSON
//! - Reconnection with state reconciliation
//! - REST snapshot fetching on reconnect

use chrono::Utc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::assets::Asset;
use crate::connectors::{ApiCredentials, PolymarketApiClient, PolymarketWebSocket, BookMessage};
use crate::events::{MarketEvent, OrderBookSnapshot, PriceLevel, RawMarketData};

/// Configuration for the order book watcher.
pub struct OrderBookWatcherConfig {
    /// Maximum number of reconnection attempts before giving up.
    pub max_reconnect_attempts: u32,
    /// How long to wait between checking for new raw data.
    pub poll_interval: Duration,
}

impl Default for OrderBookWatcherConfig {
    fn default() -> Self {
        Self {
            max_reconnect_attempts: 100, // Essentially unlimited
            poll_interval: Duration::from_millis(10),
        }
    }
}

/// Watches the order book for a specific asset.
///
/// Responsibilities:
/// - Manages WebSocket connection for orderbook streaming
/// - Normalizes raw WebSocket data into MarketEvents
/// - Handles disconnection/reconnection with state rebuild
/// - Fetches REST snapshots on reconnect
pub struct OrderBookWatcher {
    asset: Asset,
    config: OrderBookWatcherConfig,
    websocket: Option<PolymarketWebSocket>,
    api_client: PolymarketApiClient,
    credentials: Option<ApiCredentials>,
    event_tx: mpsc::Sender<MarketEvent>,
    raw_data_rx: Option<mpsc::Receiver<RawMarketData>>,
    raw_data_tx: mpsc::Sender<RawMarketData>,
    current_yes_token: Option<String>,
    current_no_token: Option<String>,
    current_snapshot: Option<OrderBookSnapshot>,
    reconnect_attempts: u32,
}

impl OrderBookWatcher {
    /// Creates a new order book watcher for the given asset.
    pub fn new(
        asset: Asset,
        api_client: PolymarketApiClient,
        credentials: Option<ApiCredentials>,
        event_tx: mpsc::Sender<MarketEvent>,
    ) -> Self {
        let (raw_data_tx, raw_data_rx) = mpsc::channel(1000);

        Self {
            asset,
            config: OrderBookWatcherConfig::default(),
            websocket: None,
            api_client,
            credentials,
            event_tx,
            raw_data_rx: Some(raw_data_rx),
            raw_data_tx,
            current_yes_token: None,
            current_no_token: None,
            current_snapshot: None,
            reconnect_attempts: 0,
        }
    }

    /// Creates a new order book watcher with custom configuration.
    pub fn with_config(
        asset: Asset,
        config: OrderBookWatcherConfig,
        api_client: PolymarketApiClient,
        credentials: Option<ApiCredentials>,
        event_tx: mpsc::Sender<MarketEvent>,
    ) -> Self {
        let mut watcher = Self::new(asset, api_client, credentials, event_tx);
        watcher.config = config;
        watcher
    }

    /// Sets the token IDs to subscribe to.
    pub fn set_tokens(&mut self, yes_token: String, no_token: String) {
        self.current_yes_token = Some(yes_token);
        self.current_no_token = Some(no_token);
    }

    /// Clears the current token subscriptions.
    pub fn clear_tokens(&mut self) {
        self.current_yes_token = None;
        self.current_no_token = None;
    }

    /// Returns whether the watcher has active token subscriptions.
    pub fn has_tokens(&self) -> bool {
        self.current_yes_token.is_some() && self.current_no_token.is_some()
    }

    /// Runs the order book watcher.
    ///
    /// This is the main event loop that:
    /// 1. Establishes WebSocket connection
    /// 2. Subscribes to token updates
    /// 3. Processes incoming data
    /// 4. Handles reconnection on failure
    pub async fn run(mut self) {
        info!("[{}] OrderBookWatcher starting", self.asset);

        // Take ownership of the receiver
        let mut raw_data_rx = self.raw_data_rx.take().expect("Receiver already taken");

        loop {
            // Ensure we have tokens to subscribe to
            if !self.has_tokens() {
                debug!("[{}] No tokens configured, waiting...", self.asset);
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }

            // Connect and subscribe
            match self.connect_and_subscribe().await {
                Ok(()) => {
                    self.reconnect_attempts = 0;

                    // Emit connected event
                    let _ = self.event_tx.send(MarketEvent::WebSocketConnected {
                        asset: self.asset,
                        timestamp: Utc::now(),
                    }).await;

                    // Run the WebSocket event loop
                    self.run_event_loop(&mut raw_data_rx).await;
                }
                Err(e) => {
                    error!("[{}] Connection failed: {}", self.asset, e);
                }
            }

            // Handle reconnection
            self.reconnect_attempts += 1;
            if self.reconnect_attempts > self.config.max_reconnect_attempts {
                error!(
                    "[{}] Max reconnection attempts ({}) exceeded, giving up",
                    self.asset, self.config.max_reconnect_attempts
                );
                break;
            }

            // Emit disconnected event
            let _ = self.event_tx.send(MarketEvent::WebSocketDisconnected {
                asset: self.asset,
                reason: "Connection lost".to_string(),
                timestamp: Utc::now(),
            }).await;

            // Wait before attempting reconnection
            if let Some(ref mut ws) = self.websocket {
                if let Err(e) = ws.reconnect().await {
                    warn!(
                        "[{}] Reconnection attempt {} failed: {}",
                        self.asset, self.reconnect_attempts, e
                    );
                    continue;
                }

                // Fetch REST snapshot to reconcile state
                if let Err(e) = self.fetch_and_emit_snapshot().await {
                    warn!("[{}] Failed to fetch snapshot after reconnect: {}", self.asset, e);
                }

                // Emit reconnected event
                let _ = self.event_tx.send(MarketEvent::WebSocketReconnected {
                    asset: self.asset,
                    timestamp: Utc::now(),
                }).await;
            }
        }

        info!("[{}] OrderBookWatcher stopped", self.asset);
    }

    /// Connects to WebSocket and subscribes to current tokens.
    async fn connect_and_subscribe(&mut self) -> Result<(), String> {
        // Create new WebSocket if needed
        if self.websocket.is_none() {
            self.websocket = Some(PolymarketWebSocket::new(
                self.asset,
                self.credentials.clone(),
                self.raw_data_tx.clone(),
            ));
        }

        let ws = self.websocket.as_mut().unwrap();

        // Connect
        ws.connect().await.map_err(|e| e.to_string())?;

        // Subscribe to tokens
        let mut tokens = Vec::new();
        if let Some(ref yes) = self.current_yes_token {
            tokens.push(yes.clone());
        }
        if let Some(ref no) = self.current_no_token {
            tokens.push(no.clone());
        }

        if !tokens.is_empty() {
            ws.subscribe(tokens).await.map_err(|e| e.to_string())?;
        }

        // Fetch initial snapshot
        self.fetch_and_emit_snapshot().await?;

        Ok(())
    }

    /// Fetches order book snapshot via REST and emits it.
    async fn fetch_and_emit_snapshot(&mut self) -> Result<(), String> {
        let yes_token = self.current_yes_token.as_ref()
            .ok_or_else(|| "No YES token configured".to_string())?;
        let no_token = self.current_no_token.as_ref()
            .ok_or_else(|| "No NO token configured".to_string())?;

        let snapshot = self.api_client
            .get_full_order_book(yes_token, no_token)
            .await
            .map_err(|e| e.to_string())?;

        debug!(
            "[{}] Fetched snapshot - YES bid: {:?}, NO ask: {:?}",
            self.asset,
            snapshot.yes_best_bid.as_ref().map(|l| l.price),
            snapshot.no_best_ask.as_ref().map(|l| l.price)
        );

        self.current_snapshot = Some(snapshot.clone());

        // Emit snapshot event
        let _ = self.event_tx.send(MarketEvent::OrderBookSnapshot {
            asset: self.asset,
            snapshot,
            timestamp: Utc::now(),
        }).await;

        Ok(())
    }

    /// Runs the main event loop, processing WebSocket messages.
    async fn run_event_loop(&mut self, raw_data_rx: &mut mpsc::Receiver<RawMarketData>) {
        if self.websocket.is_none() {
            return;
        }

        // Spawn a task to run the WebSocket
        let mut ws_handle = {
            // We need to move ownership temporarily
            let mut ws_owned = self.websocket.take().unwrap();
            tokio::spawn(async move {
                let result = ws_owned.run_until_disconnect().await;
                (ws_owned, result)
            })
        };

        loop {
            tokio::select! {
                // Process raw WebSocket data
                Some(raw_data) = raw_data_rx.recv() => {
                    self.process_raw_data(raw_data).await;
                }

                // WebSocket task completed (disconnected)
                result = &mut ws_handle => {
                    match result {
                        Ok((ws, ws_result)) => {
                            self.websocket = Some(ws);
                            if let Err(e) = ws_result {
                                warn!("[{}] WebSocket disconnected: {}", self.asset, e);
                            }
                        }
                        Err(e) => {
                            error!("[{}] WebSocket task panicked: {}", self.asset, e);
                        }
                    }
                    break;
                }
            }
        }
    }

    /// Processes raw WebSocket data and emits normalized events.
    /// Processes raw WebSocket data and emits normalized events.
async fn process_raw_data(&mut self, raw: RawMarketData) {
    // The WebSocket can send either:
    // 1. A single object with event_type
    // 2. An array of objects (e.g., initial book snapshot sends both YES and NO books)
    
    if let Some(array) = raw.payload.as_array() {
        // Handle array of messages
        for item in array {
            self.process_single_message(item).await;
        }
    } else {
        // Handle single message
        self.process_single_message(&raw.payload).await;
    }
}

/// Processes a single WebSocket message object.
async fn process_single_message(&mut self, payload: &serde_json::Value) {
    let event_type = payload.get("event_type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    match event_type {
        "book" => {
            if let Ok(book_msg) = serde_json::from_value::<BookMessage>(payload.clone()) {
                self.handle_book_message(book_msg).await;
            } else {
                warn!("[{}] Failed to parse book message: {:?}", self.asset, payload);
            }
        }
        "price_change" => {
            self.handle_price_change(payload).await;
        }
        "last_trade_price" => {
            debug!("[{}] Trade: {:?}", self.asset, payload);
        }
        "tick_size_change" => {
            debug!("[{}] Tick size change: {:?}", self.asset, payload);
        }
        _ => {
            debug!("[{}] Unknown event type: {} - {:?}", self.asset, event_type, payload);
        }
    }
}

    /// Handles a full book snapshot message.
    /// Handles a full book snapshot message.
async fn handle_book_message(&mut self, book: BookMessage) {
    let is_yes_token = self.current_yes_token.as_ref()
        .map(|t| t == &book.asset_id)
        .unwrap_or(false);

    let is_no_token = self.current_no_token.as_ref()
        .map(|t| t == &book.asset_id)
        .unwrap_or(false);

    if !is_yes_token && !is_no_token {
        debug!("[{}] Ignoring book for unknown token: {}", self.asset, book.asset_id);
        return;
    }

    // Parse all bids and find the HIGHEST price (best bid)
    let best_bid = book.bids.iter()
        .filter_map(|l| {
            let price = l.price.parse::<f64>().ok()?;
            let size = l.size.parse::<f64>().ok()?;
            Some(PriceLevel { price, size })
        })
        .max_by(|a, b| a.price.partial_cmp(&b.price).unwrap_or(std::cmp::Ordering::Equal));

    // Parse all asks and find the LOWEST price (best ask)
    let best_ask = book.asks.iter()
        .filter_map(|l| {
            let price = l.price.parse::<f64>().ok()?;
            let size = l.size.parse::<f64>().ok()?;
            Some(PriceLevel { price, size })
        })
        .min_by(|a, b| a.price.partial_cmp(&b.price).unwrap_or(std::cmp::Ordering::Equal));

    // Update internal snapshot
    if let Some(ref mut snapshot) = self.current_snapshot {
        if is_yes_token {
            snapshot.yes_best_bid = best_bid;
            snapshot.yes_best_ask = best_ask;
        } else {
            snapshot.no_best_bid = best_bid;
            snapshot.no_best_ask = best_ask;
        }
        snapshot.timestamp = Utc::now();
    }

    // Emit update event
    self.emit_best_bid_ask().await;
}

    /// Handles a price change message.
    async fn handle_price_change(&mut self, payload: &serde_json::Value) {
        // Extract relevant fields
        let asset_id = payload.get("asset_id").and_then(|v| v.as_str());
        let price = payload.get("price").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok());
        let size = payload.get("size").and_then(|v| v.as_str()).and_then(|s| s.parse::<f64>().ok());
        let side = payload.get("side").and_then(|v| v.as_str());

        if let (Some(asset_id), Some(price), Some(_size), Some(side)) = (asset_id, price, size, side) {
            let is_yes_token = self.current_yes_token.as_ref().map(|t| t == asset_id).unwrap_or(false);
            let is_no_token = self.current_no_token.as_ref().map(|t| t == asset_id).unwrap_or(false);

            if !is_yes_token && !is_no_token {
                return;
            }

            // This is a simplified handler - in production we'd maintain the full book
            // and update it incrementally. For Milestone 1, we just emit updates.
            debug!(
                "[{}] Price change: {} side={} price={}",
                self.asset,
                if is_yes_token { "YES" } else { "NO" },
                side,
                price
            );

            // Emit update
            self.emit_best_bid_ask().await;
        }
    }

    /// Emits the current best bid/ask as a MarketEvent.
    async fn emit_best_bid_ask(&self) {
        let (yes_bid, yes_ask, no_bid, no_ask) = if let Some(ref snapshot) = self.current_snapshot {
            (
                snapshot.yes_best_bid.as_ref().map(|l| l.price),
                snapshot.yes_best_ask.as_ref().map(|l| l.price),
                snapshot.no_best_bid.as_ref().map(|l| l.price),
                snapshot.no_best_ask.as_ref().map(|l| l.price),
            )
        } else {
            (None, None, None, None)
        };

        let event = MarketEvent::BestBidAskUpdate {
            asset: self.asset,
            yes_best_bid: yes_bid,
            yes_best_ask: yes_ask,
            no_best_bid: no_bid,
            no_best_ask: no_ask,
            timestamp: Utc::now(),
        };

        if let Err(e) = self.event_tx.send(event).await {
            warn!("[{}] Failed to emit BestBidAskUpdate: {}", self.asset, e);
        }
    }
}

impl std::fmt::Debug for OrderBookWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrderBookWatcher")
            .field("asset", &self.asset)
            .field("has_yes_token", &self.current_yes_token.is_some())
            .field("has_no_token", &self.current_no_token.is_some())
            .field("reconnect_attempts", &self.reconnect_attempts)
            .finish()
    }
}
