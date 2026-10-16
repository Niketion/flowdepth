//! Single registration point for discovery, placement, runtime and settings.
use super::{IndicatorPlacement, KlineIndicator};
use crate::chart::kline::scripted::ScriptKind;
use exchange::adapter::MarketKind;
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    OrderFlow,
    Momentum,
    Profiles,
}
impl Group {
    pub const ALL: [Self; 3] = [Self::OrderFlow, Self::Momentum, Self::Profiles];
    pub fn label(self) -> &'static str {
        match self {
            Self::OrderFlow => "Order flow",
            Self::Momentum => "Momentum",
            Self::Profiles => "Profiles & levels",
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub enum Icon {
    Volume,
    Delta,
    Momentum,
    Profile,
    Trend,
    Levels,
    Bars,
}
#[derive(Debug, Clone, Copy)]
pub enum Runtime {
    Volume,
    BarAnalysis,
    OpenInterest,
    Scripted(ScriptKind),
    Overlay,
}
#[derive(Debug, Clone, Copy)]
pub enum Editor {
    None,
    Cvd,
    Period(ScriptKind),
    Macd,
    Tpo,
    SessionProfile,
    Bubbles,
    Vwap,
    Gex,
}
#[derive(Debug, Clone, Copy)]
pub struct Definition {
    pub kind: KlineIndicator,
    pub name: &'static str,
    pub description: &'static str,
    pub group: Group,
    pub icon: Icon,
    pub runtime: Runtime,
    pub editor: Editor,
    pub placement: IndicatorPlacement,
}
macro_rules! define {
    ($id:ident, $name:literal, $description:literal, $group:ident, $icon:ident, $runtime:expr, $editor:expr) => {
        Definition {
            kind: KlineIndicator::$id,
            name: $name,
            description: $description,
            group: Group::$group,
            icon: Icon::$icon,
            runtime: $runtime,
            editor: $editor,
            placement: if matches!($runtime, Runtime::Overlay) {
                IndicatorPlacement::Overlay
            } else {
                IndicatorPlacement::Panel
            },
        }
    };
}
pub const DEFINITIONS: &[Definition] = &[
    define!(
        Volume,
        "Volume",
        "Traded volume for each bar.",
        OrderFlow,
        Volume,
        Runtime::Volume,
        Editor::None
    ),
    define!(
        BarAnalysis,
        "Bar Analysis",
        "Footprint statistics and directional volume inside each bar.",
        OrderFlow,
        Bars,
        Runtime::BarAnalysis,
        Editor::None
    ),
    define!(
        CumulativeDelta,
        "CVD",
        "Cumulative Volume Delta: continuous buy volume minus sell volume across the loaded history.",
        OrderFlow,
        Delta,
        Runtime::Scripted(ScriptKind::Cvd),
        Editor::Cvd
    ),
    define!(
        OpenInterest,
        "Open Interest",
        "Outstanding perpetual contracts. Availability depends on venue and timeframe.",
        OrderFlow,
        Trend,
        Runtime::OpenInterest,
        Editor::None
    ),
    define!(
        VolumeBubbles,
        "Volume Bubbles",
        "Large executions with adaptive volume thresholds and clustering.",
        OrderFlow,
        Volume,
        Runtime::Overlay,
        Editor::Bubbles
    ),
    define!(
        SessionVolumeProfile,
        "Session Volume Profile",
        "Volume distribution by price for each session, including POC and value area.",
        Profiles,
        Profile,
        Runtime::Overlay,
        Editor::SessionProfile
    ),
    define!(
        Vwap,
        "VWAP",
        "Volume weighted average price with session anchors and optional deviation bands.",
        Profiles,
        Trend,
        Runtime::Overlay,
        Editor::Vwap
    ),
    define!(
        GexLevels,
        "GEX Overlay",
        "Gamma exposure levels and options liquidity references on the price chart.",
        Profiles,
        Levels,
        Runtime::Overlay,
        Editor::Gex
    ),
    define!(
        Rsi,
        "RSI",
        "Relative Strength Index. Momentum and overbought or oversold conditions.",
        Momentum,
        Momentum,
        Runtime::Scripted(ScriptKind::Rsi),
        Editor::Period(ScriptKind::Rsi)
    ),
    define!(
        Atr,
        "ATR",
        "Average True Range. Volatility over the selected period.",
        Momentum,
        Momentum,
        Runtime::Scripted(ScriptKind::Atr),
        Editor::Period(ScriptKind::Atr)
    ),
    define!(
        Macd,
        "MACD",
        "Moving Average Convergence Divergence with signal line and histogram.",
        Momentum,
        Trend,
        Runtime::Scripted(ScriptKind::Macd),
        Editor::Macd
    ),
    define!(
        Adx,
        "ADX / DMI",
        "Average Directional Index and directional movement strength.",
        Momentum,
        Momentum,
        Runtime::Scripted(ScriptKind::Adx),
        Editor::Period(ScriptKind::Adx)
    ),
    define!(
        Tpo,
        "TPO",
        "Time Price Opportunity. Market profile with session letters and value area.",
        Profiles,
        Profile,
        Runtime::Overlay,
        Editor::Tpo
    ),
];

pub fn definition(kind: KlineIndicator) -> &'static Definition {
    &DEFINITIONS[kind as usize]
}
static SPOT: LazyLock<Vec<KlineIndicator>> = LazyLock::new(|| {
    DEFINITIONS
        .iter()
        .filter(|entry| entry.kind != KlineIndicator::OpenInterest)
        .map(|entry| entry.kind)
        .collect()
});
static PERPS: LazyLock<Vec<KlineIndicator>> =
    LazyLock::new(|| DEFINITIONS.iter().map(|entry| entry.kind).collect());
pub fn for_market(market: MarketKind) -> &'static [KlineIndicator] {
    match market {
        MarketKind::Spot => &SPOT,
        MarketKind::LinearPerps | MarketKind::InversePerps => &PERPS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_serializable_indicator_has_one_complete_registration() {
        let variants = enum_map::EnumMap::<KlineIndicator, ()>::default();
        assert_eq!(variants.len(), DEFINITIONS.len());
        for (kind, ()) in variants {
            let entry = definition(kind);
            assert_eq!(entry.kind, kind);
            assert!(!entry.name.is_empty() && !entry.description.is_empty());
            assert_eq!(
                DEFINITIONS
                    .iter()
                    .filter(|other| other.kind == kind)
                    .count(),
                1
            );
            let encoded = serde_json::to_value(kind).unwrap();
            assert_eq!(
                serde_json::from_value::<KlineIndicator>(encoded).unwrap(),
                kind
            );
        }
    }
    #[test]
    fn spot_excludes_only_open_interest_and_preserves_relative_order() {
        let expected: Vec<_> = for_market(MarketKind::LinearPerps)
            .iter()
            .copied()
            .filter(|kind| *kind != KlineIndicator::OpenInterest)
            .collect();
        assert_eq!(for_market(MarketKind::Spot), expected);
        assert_eq!(
            for_market(MarketKind::InversePerps),
            for_market(MarketKind::LinearPerps)
        );
        assert!(serde_json::from_str::<KlineIndicator>("\"SmartMoney\"").is_err());
    }
}
