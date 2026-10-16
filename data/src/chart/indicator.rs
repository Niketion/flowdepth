pub mod catalog;

use std::fmt::{self, Debug, Display};

use enum_map::Enum;
use exchange::adapter::{Exchange, MarketKind};
use serde::{Deserialize, Serialize};

pub trait Indicator: PartialEq + Display + 'static {
    fn for_market(market: MarketKind) -> &'static [Self]
    where
        Self: Sized;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndicatorPlacement {
    Panel,
    Overlay,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize, Eq, Enum)]
pub enum KlineIndicator {
    Volume,
    BarAnalysis,
    CumulativeDelta,
    OpenInterest,
    VolumeBubbles,
    SessionVolumeProfile,
    Vwap,
    GexLevels,
    Rsi,
    Atr,
    Macd,
    Adx,
    Tpo,
}

impl Indicator for KlineIndicator {
    fn for_market(market: MarketKind) -> &'static [Self] {
        catalog::for_market(market)
    }
}

impl KlineIndicator {
    pub fn definition(self) -> &'static catalog::Definition {
        catalog::definition(self)
    }
    pub fn placement(self) -> IndicatorPlacement {
        self.definition().placement
    }
    pub fn is_overlay(self) -> bool {
        self.placement() == IndicatorPlacement::Overlay
    }

    pub fn requires_trades(self, exchange: Exchange) -> bool {
        matches!(
            self,
            Self::VolumeBubbles | Self::SessionVolumeProfile | Self::Vwap
        ) || (self == Self::CumulativeDelta
            && !matches!(
                exchange,
                Exchange::BinanceLinear | Exchange::BinanceInverse | Exchange::BinanceSpot
            ))
    }
}

impl Display for KlineIndicator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.definition().name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize, Eq, Enum)]
pub enum HeatmapIndicator {
    Volume,
}

impl Indicator for HeatmapIndicator {
    fn for_market(market: MarketKind) -> &'static [Self] {
        match market {
            MarketKind::Spot => &Self::FOR_SPOT,
            MarketKind::LinearPerps | MarketKind::InversePerps => &Self::FOR_PERPS,
        }
    }
}

impl HeatmapIndicator {
    // Indicator togglers on UI menus depend on these arrays.
    // Every variant needs to be in either SPOT, PERPS or both.
    /// Indicators that can be used with spot market tickers
    const FOR_SPOT: [HeatmapIndicator; 1] = [HeatmapIndicator::Volume];
    /// Indicators that can be used with perpetual swap market tickers
    const FOR_PERPS: [HeatmapIndicator; 1] = [HeatmapIndicator::Volume];
}

impl Display for HeatmapIndicator {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            HeatmapIndicator::Volume => write!(f, "Volume"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Temporary workaround,
/// represents any indicator type in the UI
pub enum UiIndicator {
    Heatmap(HeatmapIndicator),
    Kline(KlineIndicator),
}

impl From<KlineIndicator> for UiIndicator {
    fn from(k: KlineIndicator) -> Self {
        UiIndicator::Kline(k)
    }
}

impl From<HeatmapIndicator> for UiIndicator {
    fn from(h: HeatmapIndicator) -> Self {
        UiIndicator::Heatmap(h)
    }
}
