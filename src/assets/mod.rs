//! Asset definitions for supported cryptocurrencies.
//!
//! Each asset operates in complete isolation with its own:
//! - Async execution task
//! - State machine instance
//! - Watcher instances
//! - Order flows
//! - Exposure accounting

mod executor;

pub use executor::AssetExecutor;

use std::fmt;

/// Supported cryptocurrency assets for 15-minute prediction markets.
///
/// CRITICAL: Each asset MUST operate in complete isolation.
/// A failure in one asset must NOT affect any other asset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Asset {
    BTC,
    ETH,
    SOL,
    XRP,
}

impl Asset {
    /// Returns all supported assets.
    pub fn all() -> Vec<Asset> {
        vec![Asset::BTC, Asset::ETH, Asset::SOL, Asset::XRP]
    }

    /// Returns the asset's display name for logging.
    pub fn name(&self) -> &'static str {
        match self {
            Asset::BTC => "Bitcoin",
            Asset::ETH => "Ethereum",
            Asset::SOL => "Solana",
            Asset::XRP => "XRP",
        }
    }

    /// Returns the asset's ticker symbol.
    pub fn ticker(&self) -> &'static str {
        match self {
            Asset::BTC => "BTC",
            Asset::ETH => "ETH",
            Asset::SOL => "SOL",
            Asset::XRP => "XRP",
        }
    }

    /// Returns the search tag used to find this asset's 15-minute markets
    /// in the Polymarket Gamma API.
    ///
    /// Note: The actual condition IDs and token IDs are dynamic and
    /// must be fetched via the markets API for the current round.
    pub fn market_search_tag(&self) -> &'static str {
        match self {
            Asset::BTC => "bitcoin-15-minute",
            Asset::ETH => "ethereum-15-minute",
            Asset::SOL => "solana-15-minute",
            Asset::XRP => "xrp-15-minute",
        }
    }
}

impl fmt::Display for Asset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.ticker())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_assets_returns_four() {
        assert_eq!(Asset::all().len(), 4);
    }

    #[test]
    fn test_asset_display() {
        assert_eq!(format!("{}", Asset::BTC), "BTC");
        assert_eq!(format!("{}", Asset::ETH), "ETH");
        assert_eq!(format!("{}", Asset::SOL), "SOL");
        assert_eq!(format!("{}", Asset::XRP), "XRP");
    }
}
