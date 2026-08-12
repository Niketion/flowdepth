//! Public options analytics providers.
//!
//! Options providers are deliberately separate from chartable exchanges: they
//! do not participate in normal trade, depth, or kline streaming.

pub mod deribit;
pub mod derive;
pub mod gex_monitor;
pub mod quantwheel;

use crate::{Ticker, UnixMs};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum OptionsProvider {
    Deribit,
    QuantWheel,
}

impl std::fmt::Display for OptionsProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Deribit => f.write_str("Deribit"),
            Self::QuantWheel => f.write_str("QuantWheel"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum OptionsUnderlying {
    Btc,
    Eth,
    Gld,
    Ndx,
}

impl OptionsUnderlying {
    pub const ALL: [Self; 2] = [Self::Btc, Self::Eth];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Btc => "BTC",
            Self::Eth => "ETH",
            Self::Gld => "GLD",
            Self::Ndx => "NDX",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GexPriceMapping {
    SpotRatio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GexSource {
    Native {
        provider: OptionsProvider,
        underlying: OptionsUnderlying,
    },
    Proxy {
        provider: OptionsProvider,
        source_symbol: OptionsUnderlying,
        target_symbol: &'static str,
        price_mapping: GexPriceMapping,
    },
}

impl GexSource {
    pub const fn deribit(underlying: OptionsUnderlying) -> Self {
        Self::Native {
            provider: OptionsProvider::Deribit,
            underlying,
        }
    }

    pub const fn xaut_gld() -> Self {
        Self::Proxy {
            provider: OptionsProvider::QuantWheel,
            source_symbol: OptionsUnderlying::Gld,
            target_symbol: "XAUT",
            price_mapping: GexPriceMapping::SpotRatio,
        }
    }

    pub const fn nq_ndx() -> Self {
        Self::Proxy {
            provider: OptionsProvider::QuantWheel,
            source_symbol: OptionsUnderlying::Ndx,
            target_symbol: "NQ",
            price_mapping: GexPriceMapping::SpotRatio,
        }
    }

    pub const fn source_underlying(self) -> OptionsUnderlying {
        match self {
            Self::Native { underlying, .. } => underlying,
            Self::Proxy { source_symbol, .. } => source_symbol,
        }
    }

    pub const fn for_chart_underlying(underlying: OptionsUnderlying) -> Self {
        match underlying {
            OptionsUnderlying::Gld => Self::xaut_gld(),
            OptionsUnderlying::Ndx => Self::nq_ndx(),
            OptionsUnderlying::Btc | OptionsUnderlying::Eth => Self::deribit(underlying),
        }
    }
}

impl From<OptionsUnderlying> for GexSource {
    fn from(value: OptionsUnderlying) -> Self {
        Self::deribit(value)
    }
}

impl std::fmt::Display for OptionsUnderlying {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum OptionRight {
    Call,
    Put,
}

/// Venue-neutral identity used only for exact option-contract matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub struct OptionContractMatchKey {
    pub underlying: OptionsUnderlying,
    pub expiry_utc_day: i32,
    pub strike_cents: i64,
    pub right: OptionRight,
}

impl OptionContractMatchKey {
    pub fn new(
        underlying: OptionsUnderlying,
        expiration_timestamp: UnixMs,
        strike: f64,
        right: OptionRight,
    ) -> Option<Self> {
        if !strike.is_finite() || strike <= 0.0 {
            return None;
        }
        let strike_cents = (strike * 100.0).round();
        if !(i64::MIN as f64..=i64::MAX as f64).contains(&strike_cents) {
            return None;
        }
        let expiry_utc_day = i32::try_from(expiration_timestamp.as_u64() / 86_400_000).ok()?;
        Some(Self {
            underlying,
            expiry_utc_day,
            strike_cents: strike_cents as i64,
            right,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct OptionInstrument {
    pub instrument_name: String,
    pub underlying: OptionsUnderlying,
    pub expiration_timestamp: UnixMs,
    pub strike: f64,
    pub right: OptionRight,
    pub contract_size: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct OptionMarketPoint {
    pub instrument_name: String,
    pub open_interest_underlying: f64,
    pub mark_iv_percent: f64,
    pub underlying_price: f64,
    pub interest_rate: f64,
    pub observed_at: UnixMs,
    #[serde(default)]
    pub native_gamma: Option<f64>,
    #[serde(default)]
    pub native_gamma_observed_at: Option<UnixMs>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct RawOptionContractSnapshot {
    pub instrument: OptionInstrument,
    pub market: OptionMarketPoint,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct RawOptionChainSnapshot {
    pub provider: OptionsProvider,
    pub underlying: OptionsUnderlying,
    pub source_spot: f64,
    pub contracts: Arc<[RawOptionContractSnapshot]>,
    pub observed_at: UnixMs,
}

/// Resolve a normal FlowSurface market ticker to a supported options
/// underlying. Only explicit base/quote combinations are accepted.
pub fn resolve_options_underlying(ticker: Ticker) -> Option<OptionsUnderlying> {
    let (symbol, _) = ticker.display_symbol_and_type();
    resolve_symbol(&symbol)
}

/// Resolve a chart market to the options/GEX source that should supply it.
pub fn resolve_gex_source(ticker: Ticker) -> Option<GexSource> {
    let (symbol, _) = ticker.display_symbol_and_type();
    if is_xaut_symbol(&symbol) {
        Some(GexSource::xaut_gld())
    } else if is_nq_symbol(&symbol) {
        Some(GexSource::nq_ndx())
    } else {
        resolve_symbol(&symbol).map(GexSource::deribit)
    }
}

fn resolve_symbol(symbol: &str) -> Option<OptionsUnderlying> {
    let normalized = symbol.to_ascii_uppercase();
    const BTC: &[&str] = &["BTCUSD", "BTCUSDT", "BTCUSDC"];
    const ETH: &[&str] = &["ETHUSD", "ETHUSDT", "ETHUSDC"];

    let without_known_suffix = normalized
        .strip_suffix("-PERP")
        .or_else(|| normalized.strip_suffix("_PERP"))
        .or_else(|| normalized.strip_suffix("PERP"))
        .unwrap_or(&normalized);

    if BTC.contains(&without_known_suffix) {
        Some(OptionsUnderlying::Btc)
    } else if ETH.contains(&without_known_suffix) {
        Some(OptionsUnderlying::Eth)
    } else {
        None
    }
}

fn is_xaut_symbol(symbol: &str) -> bool {
    let normalized = symbol.to_ascii_uppercase();
    let normalized = normalized
        .strip_suffix("-PERP")
        .or_else(|| normalized.strip_suffix("_PERP"))
        .or_else(|| normalized.strip_suffix("PERP"))
        .unwrap_or(&normalized);
    ["XAUTUSD", "XAUTUSDT", "XAUTUSDC"].contains(&normalized)
}

fn is_nq_symbol(symbol: &str) -> bool {
    let normalized = symbol.to_ascii_uppercase();
    let normalized = normalized
        .strip_suffix("-PERP")
        .or_else(|| normalized.strip_suffix("_PERP"))
        .or_else(|| normalized.strip_suffix("PERP"))
        .unwrap_or(&normalized);
    if [
        "NDX",
        "NDXUSD",
        "NDXUSDT",
        "NDXUSDC",
        "NAS100",
        "NAS100USD",
        "NAS100USDT",
        "NAS100USDC",
        "NAS100_USD",
        "NAS100_USDT",
        "NAS100_USDC",
        "NQ",
        "NQUSD",
        "NQUSDT",
        "NQUSDC",
    ]
    .contains(&normalized)
    {
        return true;
    }
    // Listed futures commonly carry a month code and one/two-digit year (NQU6/NQZ26).
    normalized.strip_prefix("NQ").is_some_and(|suffix| {
        let mut chars = suffix.chars();
        matches!(
            chars.next(),
            Some('F' | 'G' | 'H' | 'J' | 'K' | 'M' | 'N' | 'Q' | 'U' | 'V' | 'X' | 'Z')
        ) && chars.as_str().len() <= 2
            && !chars.as_str().is_empty()
            && chars.as_str().chars().all(|c| c.is_ascii_digit())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::Exchange;

    fn ticker(symbol: &str, exchange: Exchange) -> Ticker {
        Ticker::new(symbol, exchange)
    }

    #[test]
    fn strict_underlying_resolver() {
        assert_eq!(
            resolve_options_underlying(ticker("BTCUSDT", Exchange::BinanceLinear)),
            Some(OptionsUnderlying::Btc)
        );
        assert_eq!(
            resolve_options_underlying(ticker("BTCUSD", Exchange::BinanceInverse)),
            Some(OptionsUnderlying::Btc)
        );
        assert_eq!(
            resolve_options_underlying(ticker("ETHUSDT", Exchange::BybitLinear)),
            Some(OptionsUnderlying::Eth)
        );
        assert_eq!(
            resolve_options_underlying(ticker("ETHUSDC", Exchange::HyperliquidSpot)),
            Some(OptionsUnderlying::Eth)
        );
        assert_eq!(
            resolve_options_underlying(ticker("SOLUSDT", Exchange::BinanceLinear)),
            None
        );
        assert_eq!(
            resolve_options_underlying(ticker("WBTCUSDT", Exchange::BinanceSpot)),
            None
        );
        assert_eq!(
            resolve_options_underlying(ticker("1000BTCUSDT", Exchange::BinanceLinear)),
            None
        );
        assert_eq!(
            resolve_options_underlying(ticker("BTC2LUSDT", Exchange::BinanceSpot)),
            None
        );
    }

    #[test]
    fn gex_source_resolves_xaut_independently_of_exchange() {
        for exchange in [Exchange::BinanceLinear, Exchange::BybitLinear] {
            assert_eq!(
                resolve_gex_source(ticker("XAUTUSDT", exchange)),
                Some(GexSource::xaut_gld())
            );
        }
        assert_eq!(
            resolve_gex_source(ticker("BTCUSDT", Exchange::BinanceLinear)),
            Some(GexSource::deribit(OptionsUnderlying::Btc))
        );
        assert_eq!(
            resolve_gex_source(ticker("SOLUSDT", Exchange::BinanceLinear)),
            None
        );
    }

    #[test]
    fn gex_source_resolves_ndx_proxy_markets() {
        for symbol in ["NAS100_USDT", "NAS100USD", "NDX", "NQU6", "NQZ26"] {
            assert_eq!(
                resolve_gex_source(ticker(symbol, Exchange::MexcLinear)),
                Some(GexSource::nq_ndx()),
                "symbol {symbol}"
            );
        }
        assert_eq!(
            resolve_gex_source(ticker("NQABC", Exchange::MexcLinear)),
            None
        );
    }
}
