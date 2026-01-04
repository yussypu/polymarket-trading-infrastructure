//! Asset executor that orchestrates per-asset watcher tasks.
//!
//! Each asset operates in COMPLETE isolation:
//! - Separate async execution task
//! - Separate state machine instance (Milestone 2)
//! - Separate watcher instances
//! - Separate order flows (Milestone 2)
//! - Separate exposure accounting (Milestone 2)
//!
//! A failure in one asset MUST NOT affect any other asset.

use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::assets::Asset;
use crate::connectors::{ApiCredentials, PolymarketApiClient};
use crate::events::MarketEvent;
use crate::watchers::{OrderBookWatcher, RoundTransitionWatcher};

/// Per-asset executor that manages all components for a single asset.
///
/// This is the isolation boundary - everything inside operates
/// independently from other assets.
pub struct AssetExecutor {
    asset: Asset,
    api_client: PolymarketApiClient,
    credentials: Option<ApiCredentials>,
    event_rx: mpsc::Receiver<MarketEvent>,
    event_tx: mpsc::Sender<MarketEvent>,

    // Current round state (for coordinating watchers)
    current_round_id: Option<String>,
    current_yes_token: Option<String>,
    current_no_token: Option<String>,

    // Trading state flags
    trading_enabled: bool,
    round_ambiguous: bool,
}

impl AssetExecutor {
    /// Creates a new asset executor.
    pub fn new(asset: Asset, credentials: Option<ApiCredentials>) -> Self {
        let (event_tx, event_rx) = mpsc::channel(1000);

        let clob_url = std::env::var("POLYMARKET_CLOB_URL")
            .unwrap_or_else(|_| "https://clob.polymarket.com".to_string());
        let gamma_url = std::env::var("POLYMARKET_GAMMA_URL")
            .unwrap_or_else(|_| "https://gamma-api.polymarket.com".to_string());

        let api_client = PolymarketApiClient::with_endpoints(
            credentials.clone(),
            clob_url,
            gamma_url,
        );

        Self {
            asset,
            api_client,
            credentials,
            event_rx,
            event_tx,
            current_round_id: None,
            current_yes_token: None,
            current_no_token: None,
            trading_enabled: false,
            round_ambiguous: false,
        }
    }

    /// Creates a new asset executor with a custom API client.
    pub fn with_api_client(
        asset: Asset,
        api_client: PolymarketApiClient,
        credentials: Option<ApiCredentials>,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::channel(1000);

        Self {
            asset,
            api_client,
            credentials,
            event_rx,
            event_tx,
            current_round_id: None,
            current_yes_token: None,
            current_no_token: None,
            trading_enabled: false,
            round_ambiguous: false,
        }
    }

    /// Returns the asset this executor manages.
    pub fn asset(&self) -> Asset {
        self.asset
    }

    /// Returns whether trading is currently enabled.
    pub fn is_trading_enabled(&self) -> bool {
        self.trading_enabled && !self.round_ambiguous
    }

    /// Runs the asset executor.
    ///
    /// This spawns the watcher tasks and runs the main event loop.
    /// The executor runs forever unless a fatal error occurs.
    pub async fn run(mut self) {
        info!("[{}] AssetExecutor starting", self.asset);
        info!("[{}] CLOB URL: {}", self.asset, self.api_client.clob_url());
        info!("[{}] Gamma URL: {}", self.asset, self.api_client.gamma_url());

        // Create the round transition watcher
        let round_watcher = RoundTransitionWatcher::new(
            self.asset,
            self.api_client.clone(),
            self.event_tx.clone(),
        );

        // Spawn the round watcher task
        let mut round_task = tokio::spawn(async move {
            round_watcher.run().await;
        });

        // Note: OrderBookWatcher is started after we get round info with token IDs
        let mut orderbook_task: Option<tokio::task::JoinHandle<()>> = None;

        // Main event loop
        info!("[{}] Entering main event loop", self.asset);

        loop {
            tokio::select! {
                // Handle incoming events
                Some(event) = self.event_rx.recv() => {
                    self.handle_event(event, &mut orderbook_task).await;
                }

                // Round watcher task completed (should not happen normally)
                result = &mut round_task => {
                    match result {
                        Ok(()) => {
                            error!("[{}] Round watcher task ended unexpectedly", self.asset);
                        }
                        Err(e) => {
                            error!("[{}] Round watcher task panicked: {}", self.asset, e);
                        }
                    }
                    // Disable trading - round state is unknown
                    self.trading_enabled = false;
                    self.round_ambiguous = true;
                    break;
                }
            }
        }

        // Clean up orderbook task if running
        if let Some(task) = orderbook_task {
            task.abort();
        }

        info!("[{}] AssetExecutor stopped", self.asset);
    }

    /// Handles an incoming market event.
    async fn handle_event(
        &mut self,
        event: MarketEvent,
        orderbook_task: &mut Option<tokio::task::JoinHandle<()>>,
    ) {
        match event {
            MarketEvent::RoundStarted {
                round_id,
                condition_id,
                yes_token_id,
                no_token_id,
                start_time,
                end_time,
                ..
            } => {
                info!(
                    "[{}] ========== NEW ROUND ==========",
                    self.asset
                );
                info!(
                    "[{}] Round ID: {}",
                    self.asset, round_id
                );
                info!(
                    "[{}] Condition: {}",
                    self.asset, condition_id
                );
                info!(
                    "[{}] YES Token: {}",
                    self.asset, yes_token_id
                );
                info!(
                    "[{}] NO Token: {}",
                    self.asset, no_token_id
                );
                info!(
                    "[{}] Time: {} to {}",
                    self.asset, start_time, end_time
                );
                info!(
                    "[{}] ================================",
                    self.asset
                );

                // Update state
                self.current_round_id = Some(round_id);
                self.current_yes_token = Some(yes_token_id.clone());
                self.current_no_token = Some(no_token_id.clone());
                self.round_ambiguous = false;
                self.trading_enabled = true;

                // Start or restart the orderbook watcher with new tokens
                self.start_orderbook_watcher(
                    yes_token_id,
                    no_token_id,
                    orderbook_task,
                ).await;
            }

            MarketEvent::RoundEnded { round_id, .. } => {
                info!("[{}] Round {} ended", self.asset, round_id);

                // Disable trading until new round starts
                self.trading_enabled = false;
                self.current_round_id = None;
                self.current_yes_token = None;
                self.current_no_token = None;
            }

            MarketEvent::RoundAmbiguous { reason, .. } => {
                warn!("[{}] Round state is AMBIGUOUS: {}", self.asset, reason);
                warn!("[{}] Trading DISABLED until round is confirmed", self.asset);

                self.round_ambiguous = true;
                self.trading_enabled = false;
            }

            MarketEvent::BestBidAskUpdate {
                yes_best_bid,
                yes_best_ask,
                no_best_bid,
                no_best_ask,
                timestamp,
                ..
            } => {
                // Log price updates at debug level
                debug!(
                    "[{}] Prices @ {} - YES: bid={:?} ask={:?} | NO: bid={:?} ask={:?}",
                    self.asset,
                    timestamp.format("%H:%M:%S"),
                    yes_best_bid,
                    yes_best_ask,
                    no_best_bid,
                    no_best_ask
                );

                // Calculate implied probability and spread
                if let (Some(yes_bid), Some(no_ask)) = (yes_best_bid, no_best_ask) {
                    let combined = yes_bid + no_ask;
                    if combined < 1.0 {
                        info!(
                            "[{}] POTENTIAL ARB: YES bid ({:.4}) + NO ask ({:.4}) = {:.4} < 1.0",
                            self.asset, yes_bid, no_ask, combined
                        );
                    }
                }
            }

            MarketEvent::OrderBookSnapshot { snapshot, .. } => {
                info!(
                    "[{}] Order book snapshot received - YES: bid={:?} ask={:?} | NO: bid={:?} ask={:?}",
                    self.asset,
                    snapshot.yes_best_bid.as_ref().map(|l| l.price),
                    snapshot.yes_best_ask.as_ref().map(|l| l.price),
                    snapshot.no_best_bid.as_ref().map(|l| l.price),
                    snapshot.no_best_ask.as_ref().map(|l| l.price),
                );
            }

            MarketEvent::WebSocketConnected { timestamp, .. } => {
                info!("[{}] WebSocket connected @ {}", self.asset, timestamp);
            }

            MarketEvent::WebSocketDisconnected { reason, timestamp, .. } => {
                warn!(
                    "[{}] WebSocket disconnected @ {}: {}",
                    self.asset, timestamp, reason
                );
            }

            MarketEvent::WebSocketReconnected { timestamp, .. } => {
                info!("[{}] WebSocket reconnected @ {}", self.asset, timestamp);
            }

            MarketEvent::HeartbeatTimeout { last_heartbeat, .. } => {
                warn!(
                    "[{}] Heartbeat timeout! Last heartbeat: {}",
                    self.asset, last_heartbeat
                );
            }

            MarketEvent::ApiError { error, recoverable, .. } => {
                if recoverable {
                    warn!("[{}] Recoverable API error: {}", self.asset, error);
                } else {
                    error!("[{}] Non-recoverable API error: {}", self.asset, error);
                    self.trading_enabled = false;
                }
            }

            // Order lifecycle events - logged but not processed in Milestone 1
            MarketEvent::OrderPlaced { order_id, side, price, size, .. } => {
                info!(
                    "[{}] Order placed: {} {:?} @ {} x {}",
                    self.asset, order_id, side, price, size
                );
            }

            MarketEvent::OrderFilled { order_id, filled_qty, fill_price, .. } => {
                info!(
                    "[{}] Order filled: {} - {} @ {}",
                    self.asset, order_id, filled_qty, fill_price
                );
            }

            MarketEvent::OrderCanceled { order_id, reason, .. } => {
                info!("[{}] Order canceled: {} - {}", self.asset, order_id, reason);
            }

            MarketEvent::OrderRejected { order_id, reason, .. } => {
                warn!("[{}] Order rejected: {} - {}", self.asset, order_id, reason);
            }
        }
    }

    /// Starts or restarts the orderbook watcher with new token IDs.
    async fn start_orderbook_watcher(
        &self,
        yes_token_id: String,
        no_token_id: String,
        task_handle: &mut Option<tokio::task::JoinHandle<()>>,
    ) {
        // Abort existing task if running
        if let Some(handle) = task_handle.take() {
            info!("[{}] Stopping existing orderbook watcher", self.asset);
            handle.abort();
        }

        // Create new orderbook watcher
        let mut watcher = OrderBookWatcher::new(
            self.asset,
            self.api_client.clone(),
            self.credentials.clone(),
            self.event_tx.clone(),
        );

        // Configure tokens
        watcher.set_tokens(yes_token_id, no_token_id);

        // Spawn new task
        info!("[{}] Starting orderbook watcher", self.asset);
        *task_handle = Some(tokio::spawn(async move {
            watcher.run().await;
        }));
    }
}

impl std::fmt::Debug for AssetExecutor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssetExecutor")
            .field("asset", &self.asset)
            .field("current_round_id", &self.current_round_id)
            .field("trading_enabled", &self.trading_enabled)
            .field("round_ambiguous", &self.round_ambiguous)
            .finish()
    }
}
