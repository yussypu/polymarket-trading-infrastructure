//! REST API client for Polymarket.
//!
//! Provides access to:
//! - Gamma API: Market discovery, round information
//! - CLOB API: Order book snapshots, pricing

use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::Deserialize;
use std::cmp::Ordering;
use thiserror::Error;
use tracing::{debug, info, warn};

use crate::assets::Asset;
use crate::events::{OrderBookSnapshot, PriceLevel, RoundInfo};

use super::auth::ApiCredentials;

/// Default API endpoints.
const DEFAULT_CLOB_URL: &str = "https://clob.polymarket.com";
const DEFAULT_GAMMA_URL: &str = "https://gamma-api.polymarket.com";

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("HTTP request failed: {0}")]
    RequestFailed(#[from] reqwest::Error),

    #[error("Failed to parse response: {0}")]
    ParseError(String),

    #[error("Market not found for asset: {0}")]
    MarketNotFound(String),

    #[error("Rate limited, retry after: {0}ms")]
    RateLimited(u64),

    #[error("API error: {status} - {message}")]
    ApiError { status: u16, message: String },

    #[error("Authentication error: {0}")]
    AuthError(String),
}

/// Polymarket API client for REST operations.
#[derive(Clone)]
pub struct PolymarketApiClient {
    client: Client,
    clob_url: String,
    gamma_url: String,
    credentials: Option<ApiCredentials>,
}

impl PolymarketApiClient {
    /// Creates a new API client with default endpoints.
    pub fn new(credentials: Option<ApiCredentials>) -> Self {
        Self::with_endpoints(
            credentials,
            DEFAULT_CLOB_URL.to_string(),
            DEFAULT_GAMMA_URL.to_string(),
        )
    }

    /// Creates a new API client with custom endpoints.
    pub fn with_endpoints(
        credentials: Option<ApiCredentials>,
        clob_url: String,
        gamma_url: String,
    ) -> Self {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            clob_url,
            gamma_url,
            credentials,
        }
    }

    /// Fetches the current active 15-minute market for an asset.
    ///
    /// Uses direct slug construction instead of scanning all markets.
    /// Slug format: {asset}-updown-15m-{round_start_unix_timestamp}
    pub async fn get_current_round(&self, asset: Asset) -> Result<Option<RoundInfo>, ApiError> {
        debug!("[{}] Fetching current round...", asset);

        let asset_slug = match asset {
            Asset::BTC => "btc",
            Asset::ETH => "eth",
            Asset::SOL => "sol",
            Asset::XRP => "xrp",
        };

        // Calculate current 15-minute round timestamp
        // Rounds start at 0, 15, 30, 45 minutes past each hour
        let now = Utc::now();
        let current_ts = now.timestamp();
        let round_start_ts = (current_ts / 900) * 900; // 900 seconds = 15 minutes

        // Try current round first, then previous round (in case we're at the boundary)
        for offset in [0i64, -1i64] {
            let ts = round_start_ts + (offset * 900);
            let slug = format!("{}-updown-15m-{}", asset_slug, ts);

            debug!("[{}] Trying slug: {}", asset, slug);

            let url = format!("{}/markets/slug/{}", self.gamma_url, slug);

            let response = self.client.get(&url).send().await?;

            if response.status() == reqwest::StatusCode::NOT_FOUND {
                debug!("[{}] Slug {} not found, trying next", asset, slug);
                continue;
            }

            if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(1000);
                warn!("[{}] Rate limited. Retry after {}ms", asset, retry_after);
                return Err(ApiError::RateLimited(retry_after));
            }

            if !response.status().is_success() {
                let status = response.status().as_u16();
                let message = response.text().await.unwrap_or_default();
                debug!(
                    "[{}] API error for slug {}: {} - {}",
                    asset, slug, status, message
                );
                continue;
            }

            let market: GammaMarket = response.json().await.map_err(|e| {
                ApiError::ParseError(format!("Failed to parse market response: {}", e))
            })?;

            // Verify it's active
            if market.active != Some(true) || market.closed == Some(true) {
                debug!("[{}] Market {} is not active, trying next", asset, slug);
                continue;
            }

            // Parse token IDs
            let (yes_token, no_token) = parse_token_ids(&market.clob_token_ids)?;

            // Calculate actual round times from the timestamp
            let start_time = DateTime::from_timestamp(ts, 0)
                .unwrap_or(now)
                .with_timezone(&Utc);
            let end_time = start_time + chrono::Duration::minutes(15);

            info!(
                "[{}] Found active round: {} (Question: {}, Ends: {})",
                asset, market.condition_id, market.question, end_time
            );

            return Ok(Some(RoundInfo {
                round_id: slug,
                condition_id: market.condition_id,
                yes_token_id: yes_token,
                no_token_id: no_token,
                start_time,
                end_time,
                question: market.question,
            }));
        }

        warn!("[{}] No active 15-minute market found", asset);
        Ok(None)
    }

    /// Fetches any active market for demo/testing purposes.
    /// This is used when 15-minute markets aren't available.
    pub async fn get_demo_market(&self, asset: Asset) -> Result<Option<RoundInfo>, ApiError> {
        // Get high-liquidity active markets
        let url = format!(
            "{}/markets?limit=50&active=true&closed=false&order=liquidityNum&ascending=false",
            self.gamma_url
        );

        let response = self.client.get(&url).send().await?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let message = response.text().await.unwrap_or_default();
            return Err(ApiError::ApiError { status, message });
        }

        let markets: Vec<GammaMarket> = response.json().await.map_err(|e| {
            ApiError::ParseError(format!("Failed to parse markets response: {}", e))
        })?;

        // Find first market with valid token IDs
        for market in markets.iter().take(10) {
            if market.clob_token_ids.is_empty() {
                continue;
            }

            if let Ok((yes_token, no_token)) = parse_token_ids(&market.clob_token_ids) {
                // Parse end date
                let end_time = market
                    .end_date
                    .as_ref()
                    .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
                    .map(|d| d.with_timezone(&Utc))
                    .unwrap_or_else(|| Utc::now() + chrono::Duration::days(30));

                let start_time = market
                    .start_date
                    .as_ref()
                    .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
                    .map(|d| d.with_timezone(&Utc))
                    .unwrap_or_else(|| Utc::now() - chrono::Duration::days(1));

                info!(
                    "[{}] DEMO: Using market '{}' (ID: {})",
                    asset,
                    market.question.chars().take(50).collect::<String>(),
                    market.id
                );

                return Ok(Some(RoundInfo {
                    round_id: format!("demo-{}-{}", asset, market.id),
                    condition_id: market.condition_id.clone(),
                    yes_token_id: yes_token,
                    no_token_id: no_token,
                    start_time,
                    end_time,
                    question: format!("[DEMO] {}", market.question),
                }));
            }
        }

        warn!("[{}] DEMO: Could not find any suitable market", asset);
        Ok(None)
    }

    /// Fetches the order book for a specific token.
    pub async fn get_order_book(&self, token_id: &str) -> Result<BookResponse, ApiError> {
        let url = format!("{}/book?token_id={}", self.clob_url, token_id);

        debug!("Fetching order book for token: {}", token_id);

        let response = self.client.get(&url).send().await?;

        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(1000);
            return Err(ApiError::RateLimited(retry_after));
        }

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let message = response.text().await.unwrap_or_default();
            return Err(ApiError::ApiError { status, message });
        }

        response
            .json()
            .await
            .map_err(|e| ApiError::ParseError(e.to_string()))
    }

    /// Fetches order book snapshots for both YES and NO tokens.
    pub async fn get_full_order_book(
        &self,
        yes_token_id: &str,
        no_token_id: &str,
    ) -> Result<OrderBookSnapshot, ApiError> {
        // Fetch both order books concurrently
        let (yes_book, no_book) = tokio::try_join!(
            self.get_order_book(yes_token_id),
            self.get_order_book(no_token_id)
        )?;

        // Helper to find best bid (highest price)
        let find_best_bid = |levels: &[BookLevel]| -> Option<PriceLevel> {
            levels
                .iter()
                .filter_map(|l| {
                    let price = l.price.parse::<f64>().ok()?;
                    let size = l.size.parse::<f64>().ok()?;
                    Some(PriceLevel { price, size })
                })
                .max_by(|a, b| a.price.partial_cmp(&b.price).unwrap_or(Ordering::Equal))
        };

        // Helper to find best ask (lowest price)
        let find_best_ask = |levels: &[BookLevel]| -> Option<PriceLevel> {
            levels
                .iter()
                .filter_map(|l| {
                    let price = l.price.parse::<f64>().ok()?;
                    let size = l.size.parse::<f64>().ok()?;
                    Some(PriceLevel { price, size })
                })
                .min_by(|a, b| a.price.partial_cmp(&b.price).unwrap_or(Ordering::Equal))
        };

        Ok(OrderBookSnapshot {
            yes_best_bid: find_best_bid(&yes_book.bids),
            yes_best_ask: find_best_ask(&yes_book.asks),
            no_best_bid: find_best_bid(&no_book.bids),
            no_best_ask: find_best_ask(&no_book.asks),
            yes_token_id: yes_token_id.to_string(),
            no_token_id: no_token_id.to_string(),
            timestamp: Utc::now(),
        })
    }

    /// Returns the CLOB API base URL.
    pub fn clob_url(&self) -> &str {
        &self.clob_url
    }

    /// Returns the Gamma API base URL.
    pub fn gamma_url(&self) -> &str {
        &self.gamma_url
    }
}

// ============ Response Types ============

/// Market data from the Gamma API.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GammaMarket {
    pub id: String,
    pub question: String,
    pub condition_id: String,
    pub slug: String,
    #[serde(default)]
    pub clob_token_ids: String,
    pub active: Option<bool>,
    pub closed: Option<bool>,
    pub end_date: Option<String>,
    pub start_date: Option<String>,
    #[serde(default)]
    pub outcomes: String,
    #[serde(default)]
    pub outcome_prices: String,
}

/// Order book response from the CLOB API.
#[derive(Debug, Clone, Deserialize)]
pub struct BookResponse {
    pub market: Option<String>,
    pub asset_id: Option<String>,
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
    pub hash: Option<String>,
    pub timestamp: Option<String>,
}

/// A single level in the order book.
#[derive(Debug, Clone, Deserialize)]
pub struct BookLevel {
    pub price: String,
    pub size: String,
}

// ============ Helper Functions ============

/// Parses the token IDs string into YES and NO token IDs.
fn parse_token_ids(token_ids_str: &str) -> Result<(String, String), ApiError> {
    // The token IDs are typically in format: ["yes_id", "no_id"] or "yes_id,no_id"
    let cleaned = token_ids_str
        .trim_matches(|c| c == '[' || c == ']' || c == '"')
        .replace('"', "");

    let parts: Vec<&str> = cleaned.split(',').map(|s| s.trim()).collect();

    if parts.len() >= 2 {
        Ok((parts[0].to_string(), parts[1].to_string()))
    } else if parts.len() == 1 && !parts[0].is_empty() {
        // If there's only one token, we might need to derive the other
        // For now, return an error
        Err(ApiError::ParseError(
            "Expected two token IDs, found one".to_string(),
        ))
    } else {
        Err(ApiError::ParseError(
            "Failed to parse token IDs".to_string(),
        ))
    }
}

impl std::fmt::Debug for PolymarketApiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolymarketApiClient")
            .field("clob_url", &self.clob_url)
            .field("gamma_url", &self.gamma_url)
            .field("has_credentials", &self.credentials.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_token_ids() {
        let result = parse_token_ids("[\"123\", \"456\"]").unwrap();
        assert_eq!(result.0, "123");
        assert_eq!(result.1, "456");

        let result = parse_token_ids("123,456").unwrap();
        assert_eq!(result.0, "123");
        assert_eq!(result.1, "456");
    }

    #[test]
    fn test_round_timestamp_calculation() {
        // Test that timestamp rounds correctly to 15-minute intervals
        let ts: i64 = 1767482100; // Known good timestamp
        let rounded = (ts / 900) * 900;
        assert_eq!(rounded, 1767482100);

        // Test mid-round timestamp
        let mid_ts: i64 = 1767482500; // 400 seconds into a round
        let mid_rounded = (mid_ts / 900) * 900;
        assert_eq!(mid_rounded, 1767482400); // Should round to next 15-min boundary
    }
}