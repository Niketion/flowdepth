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
    SmartMoney,
}

impl Indicator for KlineIndicator {
    fn for_market(market: MarketKind) -> &'static [Self] {
        match market {
            MarketKind::Spot => &Self::FOR_SPOT,
            MarketKind::LinearPerps | MarketKind::InversePerps => &Self::FOR_PERPS,
        }
    }
}

impl KlineIndicator {
    // Indicator togglers on UI menus depend on these arrays.
    // Every variant needs to be in either SPOT, PERPS or both.
    /// Indicators that can be used with spot market tickers
    const FOR_SPOT: [KlineIndicator; 8] = [
        KlineIndicator::Volume,
        KlineIndicator::BarAnalysis,
        KlineIndicator::CumulativeDelta,
        KlineIndicator::VolumeBubbles,
        KlineIndicator::SessionVolumeProfile,
        KlineIndicator::Vwap,
        KlineIndicator::GexLevels,
        KlineIndicator::SmartMoney,
    ];
    /// Indicators that can be used with perpetual swap market tickers
    const FOR_PERPS: [KlineIndicator; 9] = [
        KlineIndicator::Volume,
        KlineIndicator::BarAnalysis,
        KlineIndicator::CumulativeDelta,
        KlineIndicator::OpenInterest,
        KlineIndicator::VolumeBubbles,
        KlineIndicator::SessionVolumeProfile,
        KlineIndicator::Vwap,
        KlineIndicator::GexLevels,
        KlineIndicator::SmartMoney,
    ];

    pub fn placement(self) -> IndicatorPlacement {
        if matches!(
            self,
            Self::VolumeBubbles
                | Self::SessionVolumeProfile
                | Self::Vwap
                | Self::GexLevels
                | Self::SmartMoney
        ) {
            IndicatorPlacement::Overlay
        } else {
            IndicatorPlacement::Panel
        }
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
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            KlineIndicator::Volume => write!(f, "Volume"),
            KlineIndicator::BarAnalysis => write!(f, "Bar Analysis"),
            KlineIndicator::CumulativeDelta => write!(f, "CVD"),
            KlineIndicator::OpenInterest => write!(f, "Open Interest"),
            KlineIndicator::VolumeBubbles => write!(f, "Volume Bubbles"),
            KlineIndicator::SessionVolumeProfile => write!(f, "Session Volume Profile"),
            KlineIndicator::Vwap => write!(f, "VWAP"),
            KlineIndicator::GexLevels => write!(f, "GEX Overlay"),
            KlineIndicator::SmartMoney => write!(f, "Smart Money Concepts"),
        }
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

#[derive(Debug, Clone, Copy)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smart_money_uses_ohlc_without_requiring_trades() {
        assert!(!KlineIndicator::SmartMoney.requires_trades(Exchange::BinanceLinear));
    }
}
