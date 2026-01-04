//! Round transition watcher for detecting 15-minute round boundaries.
//!
//! Detects round transitions with <100ms latency goal (in production).
//! For Milestone 1, uses REST polling every 5 seconds.
//!
//! CRITICAL: If round identity cannot be confirmed unambiguously,
//! trading MUST be disabled.

use chrono::Utc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::assets::Asset;
use crate::connectors::PolymarketApiClient;
use crate::events::{MarketEvent, RoundInfo};

/// Configuration for the round transition watcher.
pub struct RoundWatcherConfig {
    /// How often to poll for round changes.
    pub poll_interval: Duration,
    /// How long before round end to warn about upcoming transition.
    pub pre_end_warning: Duration,
    /// Maximum consecutive API failures before raising an error.
    pub max_consecutive_failures: u32,
}

impl Default for RoundWatcherConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(5),
            pre_end_warning: Duration::from_secs(30),
            max_consecutive_failures: 3,
        }
    }
}

/// Watches for round transitions for a specific asset.
///
/// Responsibilities:
/// - Continuously monitors Polymarket round lifecycle
/// - Detects round rollover immediately
/// - Emits RoundStarted/RoundEnded events
/// - Raises RoundAmbiguous if state is unclear
pub struct RoundTransitionWatcher {
    asset: Asset,
    config: RoundWatcherConfig,
    api_client: PolymarketApiClient,
    event_tx: mpsc::Sender<MarketEvent>,
    current_round: Option<RoundInfo>,
    consecutive_failures: u32,
    is_running: bool,
}

impl RoundTransitionWatcher {
    /// Creates a new round transition watcher.
    pub fn new(
        asset: Asset,
        api_client: PolymarketApiClient,
        event_tx: mpsc::Sender<MarketEvent>,
    ) -> Self {
        Self {
            asset,
            config: RoundWatcherConfig::default(),
            api_client,
            event_tx,
            current_round: None,
            consecutive_failures: 0,
            is_running: false,
        }
    }

    /// Creates a new round transition watcher with custom configuration.
    pub fn with_config(
        asset: Asset,
        config: RoundWatcherConfig,
        api_client: PolymarketApiClient,
        event_tx: mpsc::Sender<MarketEvent>,
    ) -> Self {
        Self {
            asset,
            config,
            api_client,
            event_tx,
            current_round: None,
            consecutive_failures: 0,
            is_running: false,
        }
    }

    /// Returns the current round information if known.
    pub fn current_round(&self) -> Option<&RoundInfo> {
        self.current_round.as_ref()
    }

    /// Returns whether the watcher is actively running.
    pub fn is_running(&self) -> bool {
        self.is_running
    }

    /// Runs the round transition watcher.
    ///
    /// This continuously polls for round changes and emits events
    /// when transitions are detected.
    pub async fn run(mut self) {
        info!("[{}] RoundTransitionWatcher starting", self.asset);
        self.is_running = true;

        // Initial round fetch
        match self.fetch_current_round().await {
            Ok(Some(round)) => {
                info!(
                    "[{}] Initial round: {} (ends: {})",
                    self.asset, round.round_id, round.end_time
                );
                self.emit_round_started(&round).await;
                self.current_round = Some(round);
            }
            Ok(None) => {
                warn!("[{}] No active round found at startup", self.asset);
                self.emit_round_ambiguous("No active round found at startup").await;
            }
            Err(e) => {
                error!("[{}] Failed to fetch initial round: {}", self.asset, e);
                self.emit_round_ambiguous(&format!("Initial fetch failed: {}", e)).await;
            }
        }

        // Main polling loop
        let mut interval = tokio::time::interval(self.config.poll_interval);

        loop {
            interval.tick().await;

            // Check if current round has ended naturally
            if let Some(ref round) = self.current_round {
                let now = Utc::now();

                // Warn about upcoming end
                let time_to_end = round.end_time - now;
                if time_to_end.num_seconds() > 0
                    && time_to_end.num_seconds() <= self.config.pre_end_warning.as_secs() as i64
                {
                    debug!(
                        "[{}] Round {} ending in {} seconds",
                        self.asset, round.round_id, time_to_end.num_seconds()
                    );
                }

                // Check for natural end
                if now >= round.end_time {
                    info!("[{}] Round {} has ended naturally", self.asset, round.round_id);
                    self.emit_round_ended(&round.round_id).await;
                    self.current_round = None;
                }
            }

            // Poll for new/changed round
            match self.fetch_current_round().await {
                Ok(Some(new_round)) => {
                    self.consecutive_failures = 0;

                    if let Some(ref current) = self.current_round {
                        // Check if round has changed
                        if current.round_id != new_round.round_id {
                            info!(
                                "[{}] Round transition detected: {} -> {}",
                                self.asset, current.round_id, new_round.round_id
                            );

                            // Emit end for old round
                            self.emit_round_ended(&current.round_id).await;

                            // Emit start for new round
                            self.emit_round_started(&new_round).await;

                            self.current_round = Some(new_round);
                        }
                    } else {
                        // No current round, so this is a new round starting
                        info!(
                            "[{}] New round started: {} (ends: {})",
                            self.asset, new_round.round_id, new_round.end_time
                        );
                        self.emit_round_started(&new_round).await;
                        self.current_round = Some(new_round);
                    }
                }
                Ok(None) => {
                    self.consecutive_failures = 0;

                    // No active round found
                    if self.current_round.is_some() {
                        let round_id = self.current_round.as_ref().unwrap().round_id.clone();
                        info!("[{}] Round {} ended (no new round found)", self.asset, round_id);
                        self.emit_round_ended(&round_id).await;
                        self.current_round = None;
                    }

                    debug!("[{}] No active round, waiting...", self.asset);
                }
                Err(e) => {
                    self.consecutive_failures += 1;
                    warn!(
                        "[{}] Failed to fetch round (attempt {}): {}",
                        self.asset, self.consecutive_failures, e
                    );

                    if self.consecutive_failures >= self.config.max_consecutive_failures {
                        error!(
                            "[{}] Max consecutive failures reached, round state is ambiguous",
                            self.asset
                        );
                        self.emit_round_ambiguous(&format!(
                            "API failed {} times consecutively",
                            self.consecutive_failures
                        )).await;
                    }
                }
            }
        }
    }

    /// Fetches the current active round from the API.
    async fn fetch_current_round(&self) -> Result<Option<RoundInfo>, String> {
        self.api_client
            .get_current_round(self.asset)
            .await
            .map_err(|e| e.to_string())
    }

    /// Emits a RoundStarted event.
    async fn emit_round_started(&self, round: &RoundInfo) {
        let event = MarketEvent::RoundStarted {
            asset: self.asset,
            round_id: round.round_id.clone(),
            condition_id: round.condition_id.clone(),
            yes_token_id: round.yes_token_id.clone(),
            no_token_id: round.no_token_id.clone(),
            start_time: round.start_time,
            end_time: round.end_time,
        };

        if let Err(e) = self.event_tx.send(event).await {
            error!("[{}] Failed to emit RoundStarted: {}", self.asset, e);
        }
    }

    /// Emits a RoundEnded event.
    async fn emit_round_ended(&self, round_id: &str) {
        let event = MarketEvent::RoundEnded {
            asset: self.asset,
            round_id: round_id.to_string(),
        };

        if let Err(e) = self.event_tx.send(event).await {
            error!("[{}] Failed to emit RoundEnded: {}", self.asset, e);
        }
    }

    /// Emits a RoundAmbiguous event.
    ///
    /// This indicates that round identity cannot be confirmed and
    /// trading MUST be disabled.
    async fn emit_round_ambiguous(&self, reason: &str) {
        let event = MarketEvent::RoundAmbiguous {
            asset: self.asset,
            reason: reason.to_string(),
            timestamp: Utc::now(),
        };

        if let Err(e) = self.event_tx.send(event).await {
            error!("[{}] Failed to emit RoundAmbiguous: {}", self.asset, e);
        }
    }
}

impl std::fmt::Debug for RoundTransitionWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoundTransitionWatcher")
            .field("asset", &self.asset)
            .field("is_running", &self.is_running)
            .field("has_current_round", &self.current_round.is_some())
            .field("consecutive_failures", &self.consecutive_failures)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = RoundWatcherConfig::default();
        assert_eq!(config.poll_interval.as_secs(), 5);
        assert_eq!(config.pre_end_warning.as_secs(), 30);
        assert_eq!(config.max_consecutive_failures, 3);
    }
}
