use crate::{aggr::time::DataPoint, chart::indicator::KlineIndicator};
use exchange::{
    Kline, Trade, UnixMs,
    unit::price::{Price, PriceStep},
    unit::qty::Qty,
};

use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

pub mod drawing {
    use super::{SessionProfileMode, SessionProfilePlacement};
    use exchange::{UnixMs, unit::Price};
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
    pub struct DrawingColor {
        pub r: f32,
        pub g: f32,
        pub b: f32,
        pub a: f32,
    }

    impl DrawingColor {
        pub const BLUE: Self = Self::rgb(0.23, 0.57, 0.96);
        pub const ORANGE: Self = Self::rgb(0.95, 0.55, 0.18);
        pub const GREEN: Self = Self::rgb(0.20, 0.72, 0.45);
        pub const RED: Self = Self::rgb(0.90, 0.30, 0.30);

        pub const fn rgb(r: f32, g: f32, b: f32) -> Self {
            Self { r, g, b, a: 1.0 }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub enum DrawingTool {
        Select,
        Pen,
        HorizontalLine,
        VerticalLine,
        Rectangle,
        Fibonacci,
        TrendLine,
        Text,
        FixedRangeVolumeProfile,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub enum DrawingX {
        Time(UnixMs),
        Tick(u64),
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct DrawingAnchor {
        pub x: DrawingX,
        pub price: Price,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct FibonacciLevel {
        pub value: f32,
        #[serde(default = "default_true")]
        pub visible: bool,
    }

    /// Settings owned by one fixed-range volume-profile drawing.
    ///
    /// They intentionally mirror the visual options of the session volume
    /// profile without sharing that indicator's state or its time interval.
    #[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
    #[serde(default)]
    pub struct FixedRangeVolumeProfileConfig {
        pub placement: SessionProfilePlacement,
        pub mode: SessionProfileMode,
        pub value_area_percent: f32,
        pub width_percent: f32,
        pub row_size_ticks: u16,
        pub show_poc: bool,
        pub show_value_area: bool,
        pub show_vwap: bool,
        pub show_range_high_low: bool,
    }

    impl Default for FixedRangeVolumeProfileConfig {
        fn default() -> Self {
            Self {
                placement: SessionProfilePlacement::Left,
                mode: SessionProfileMode::Volume,
                value_area_percent: 70.0,
                width_percent: 35.0,
                row_size_ticks: 1,
                show_poc: true,
                show_value_area: true,
                show_vwap: true,
                show_range_high_low: true,
            }
        }
    }

    const fn default_true() -> bool {
        true
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct DrawingStyle {
        pub color: DrawingColor,
        pub stroke_width: f32,
        pub opacity: f32,
        pub fill_color: DrawingColor,
        pub fill_opacity: f32,
        pub text_size: f32,
        #[serde(default = "default_text_scale")]
        pub text_scale: f32,
        pub show_labels: bool,
        #[serde(default = "default_fibonacci_levels")]
        pub fibonacci_levels: Vec<FibonacciLevel>,
        #[serde(default)]
        pub fixed_range_volume_profile: FixedRangeVolumeProfileConfig,
    }

    impl Default for DrawingStyle {
        fn default() -> Self {
            Self {
                color: DrawingColor::BLUE,
                stroke_width: 2.0,
                opacity: 1.0,
                fill_color: DrawingColor::BLUE,
                fill_opacity: 0.12,
                text_size: 14.0,
                text_scale: 1.0,
                show_labels: true,
                fibonacci_levels: default_fibonacci_levels(),
                fixed_range_volume_profile: FixedRangeVolumeProfileConfig::default(),
            }
        }
    }

    const fn default_text_scale() -> f32 {
        1.0
    }

    fn default_fibonacci_levels() -> Vec<FibonacciLevel> {
        [
            0.0, 0.236, 0.382, 0.5, 0.618, 0.786, 1.0, 1.272, 1.618, 2.0, 2.618,
        ]
        .into_iter()
        .map(|value| FibonacciLevel {
            value,
            visible: value <= 1.0,
        })
        .collect()
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub enum DrawingGeometry {
        Freehand {
            points: Vec<DrawingAnchor>,
        },
        HorizontalLine {
            price: Price,
        },
        VerticalLine {
            x: DrawingX,
        },
        Rectangle {
            first: DrawingAnchor,
            second: DrawingAnchor,
        },
        Fibonacci {
            first: DrawingAnchor,
            second: DrawingAnchor,
        },
        TrendLine {
            first: DrawingAnchor,
            second: DrawingAnchor,
        },
        Text {
            anchor: DrawingAnchor,
            content: String,
        },
        FixedRangeVolumeProfile {
            first: UnixMs,
            second: UnixMs,
        },
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    pub struct Drawing {
        pub id: u64,
        pub geometry: DrawingGeometry,
        #[serde(default)]
        pub style: DrawingStyle,
        #[serde(default = "default_true")]
        pub visible: bool,
    }
}

#[derive(Clone)]
pub struct KlineDataPoint {
    pub kline: Kline,
    pub footprint: KlineTrades,
    pub bubble_summary: BubbleVolumeSummary,
    /// Completeness of the raw directional executions for this bucket.
    pub trade_coverage: TradeCoverage,
    /// Raw executions retained in arrival order so indicators can reconstruct
    /// a real intrabar path when the bucket has complete historical coverage.
    pub trade_sequence: Vec<Trade>,
    /// Stable exchange IDs already aggregated into this candle. Unlike the
    /// chart-level raw buffer, this lives as long as the candle bucket.
    pub trade_ids: FxHashSet<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TradeCoverage {
    #[default]
    Unknown,
    Partial,
    Complete,
}

impl KlineDataPoint {
    pub fn max_cluster_qty(&self, cluster_kind: ClusterKind, highest: Price, lowest: Price) -> Qty {
        self.footprint
            .max_cluster_qty(cluster_kind, highest, lowest)
    }

    pub fn add_trade(&mut self, trade: &Trade, step: PriceStep) {
        // Keep the datapoint invariant even for callers which do not pass through the UI chart's
        // ingestion filter. Zero/negative prices would poison footprint autoscaling.
        if trade.price.units <= 0 {
            return;
        }
        if let Some(id) = trade.id
            && !self.trade_ids.insert(id)
        {
            return;
        }
        self.footprint.add_trade_to_nearest_bin(trade, step);
        self.trade_sequence.push(*trade);
        if self.trade_coverage == TradeCoverage::Unknown {
            self.trade_coverage = TradeCoverage::Partial;
        }
    }

    pub fn poc_price(&self) -> Option<Price> {
        self.footprint.poc_price()
    }

    pub fn set_poc_status(&mut self, status: NPoc) {
        self.footprint.set_poc_status(status);
    }

    pub fn clear_trades(&mut self) {
        self.footprint.clear();
        self.trade_sequence.clear();
        self.trade_ids.clear();
    }

    pub fn set_bubble_summary(&mut self, summary: BubbleVolumeSummary) {
        self.bubble_summary = summary;
    }

    pub fn calculate_poc(&mut self) {
        self.footprint.calculate_poc();
    }

    pub fn last_trade_time(&self) -> Option<UnixMs> {
        self.footprint.last_trade_t()
    }

    pub fn first_trade_time(&self) -> Option<UnixMs> {
        self.footprint.first_trade_t()
    }

    pub fn volume_delta(&self) -> Qty {
        if self.kline.volume.is_directional() {
            self.kline.volume.delta()
        } else if !self.footprint.trades.is_empty() {
            self.footprint
                .trades
                .values()
                .fold(Qty::ZERO, |acc, group| acc + group.delta_qty())
        } else {
            Qty::ZERO
        }
    }

    /// Whether this datapoint has directional (buy vs sell) data.
    pub fn is_directional(&self) -> bool {
        !self.footprint.trades.is_empty() || self.kline.volume.is_directional()
    }
}

impl DataPoint for KlineDataPoint {
    fn add_trade(&mut self, trade: &Trade, step: PriceStep) {
        self.add_trade(trade, step);
    }

    fn clear_trades(&mut self) {
        self.clear_trades();
    }

    fn last_trade_time(&self) -> Option<UnixMs> {
        self.last_trade_time()
    }

    fn first_trade_time(&self) -> Option<UnixMs> {
        self.first_trade_time()
    }

    fn last_price(&self) -> Price {
        self.kline.close
    }

    fn kline(&self) -> Option<&Kline> {
        Some(&self.kline)
    }

    fn value_high(&self) -> Price {
        self.kline.high
    }

    fn value_low(&self) -> Price {
        self.kline.low
    }
}

#[derive(Debug, Clone, Default)]
pub struct GroupedTrades {
    pub buy_qty: Qty,
    pub sell_qty: Qty,
    pub first_time: UnixMs,
    pub last_time: UnixMs,
    pub buy_count: usize,
    pub sell_count: usize,
}

impl GroupedTrades {
    fn new(trade: &Trade) -> Self {
        Self {
            buy_qty: if trade.is_sell {
                Qty::default()
            } else {
                trade.qty
            },
            sell_qty: if trade.is_sell {
                trade.qty
            } else {
                Qty::default()
            },
            first_time: trade.time,
            last_time: trade.time,
            buy_count: if trade.is_sell { 0 } else { 1 },
            sell_count: if trade.is_sell { 1 } else { 0 },
        }
    }

    fn add_trade(&mut self, trade: &Trade) {
        if trade.is_sell {
            self.sell_qty += trade.qty;
            self.sell_count += 1;
        } else {
            self.buy_qty += trade.qty;
            self.buy_count += 1;
        }
        self.last_time = trade.time;
    }

    pub fn total_qty(&self) -> Qty {
        self.buy_qty + self.sell_qty
    }

    pub fn delta_qty(&self) -> Qty {
        self.buy_qty - self.sell_qty
    }

    pub fn max_cluster_qty(&self, cluster_kind: ClusterKind) -> Qty {
        match cluster_kind {
            ClusterKind::BidAsk | ClusterKind::Table => self.buy_qty.max(self.sell_qty),
            ClusterKind::DeltaProfile => self.buy_qty.abs_diff(self.sell_qty),
            ClusterKind::VolumeProfile => self.total_qty(),
        }
    }
}

pub type VolumeBubbleClusterId = u64;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VolumeBubbleCluster {
    pub id: VolumeBubbleClusterId,
    pub candle_time: UnixMs,
    pub first_time: UnixMs,
    pub last_time: UnixMs,
    pub weighted_time: UnixMs,
    pub vwap_price: Price,
    pub total_qty: Qty,
    pub buy_qty: Qty,
    pub sell_qty: Qty,
    pub delta_qty: Qty,
    pub trade_count: usize,
    pub largest_trade_qty: Qty,
    #[serde(default)]
    pub percentile_rank: f32,
    #[serde(default)]
    pub importance_score: f32,
}

/// Compatibility name used by the fetch/cache plumbing. V2 candidates are smart clusters.
pub type BubbleCandidate = VolumeBubbleCluster;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BubbleVolumeSummary {
    pub candle_time: UnixMs,
    #[serde(default = "bubble_summary_algorithm_version")]
    pub algorithm_version: u16,
    pub candidates: Vec<BubbleCandidate>,
}

impl Default for BubbleVolumeSummary {
    fn default() -> Self {
        Self {
            candle_time: UnixMs::ZERO,
            algorithm_version: BUBBLE_SUMMARY_ALGORITHM_VERSION,
            candidates: Vec::new(),
        }
    }
}

pub const BUBBLE_SUMMARY_ALGORITHM_VERSION: u16 = 2;
const fn bubble_summary_algorithm_version() -> u16 {
    BUBBLE_SUMMARY_ALGORITHM_VERSION
}

impl BubbleVolumeSummary {
    pub fn new(candle_time: UnixMs, candidates: Vec<BubbleCandidate>) -> Self {
        Self {
            candle_time,
            algorithm_version: BUBBLE_SUMMARY_ALGORITHM_VERSION,
            candidates,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum VolumeBubblePreset {
    Clean,
    #[default]
    Balanced,
    Detailed,
    Custom,
}

impl VolumeBubblePreset {
    pub const ALL: [Self; 4] = [Self::Clean, Self::Balanced, Self::Detailed, Self::Custom];
}

impl std::fmt::Display for VolumeBubblePreset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Clean => "Clean",
            Self::Balanced => "Balanced",
            Self::Detailed => "Detailed",
            Self::Custom => "Custom",
        })
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BubbleThresholdMode {
    Fixed,
    AdaptivePercentile,
    #[default]
    Hybrid,
}

impl BubbleThresholdMode {
    pub const ALL: [Self; 3] = [Self::Fixed, Self::AdaptivePercentile, Self::Hybrid];
}

impl std::fmt::Display for BubbleThresholdMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Fixed => "Fixed",
            Self::AdaptivePercentile => "Adaptive percentile",
            Self::Hybrid => "Hybrid",
        })
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BubbleLabelMode {
    None,
    #[default]
    ExtremeOnly,
    All,
}

impl BubbleLabelMode {
    pub const ALL: [Self; 3] = [Self::None, Self::ExtremeOnly, Self::All];
}

impl std::fmt::Display for BubbleLabelMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::None => "None",
            Self::ExtremeOnly => "Extreme only",
            Self::All => "All",
        })
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BubblePriceResponse {
    Pending,
    FollowThrough,
    Stalled,
    Reversed,
    #[default]
    Neutral,
}

#[derive(Debug, Clone, Default)]
pub struct KlineTrades {
    pub trades: FxHashMap<Price, GroupedTrades>,
    pub poc: Option<PointOfControl>,
}

impl KlineTrades {
    pub fn new() -> Self {
        Self {
            trades: FxHashMap::default(),
            poc: None,
        }
    }

    pub fn first_trade_t(&self) -> Option<UnixMs> {
        self.trades.values().map(|group| group.first_time).min()
    }

    pub fn last_trade_t(&self) -> Option<UnixMs> {
        self.trades.values().map(|group| group.last_time).max()
    }

    /// Add trade to the bin at the step multiple computed with side-based rounding.
    /// Intended for order-book ladder/quotes; Floor for sells, ceil for buys.
    /// Introduces side bias at bin edges and should not be used for OHLC/footprint aggregation
    pub fn add_trade_to_side_bin(&mut self, trade: &Trade, step: PriceStep) {
        let price = trade.price.round_to_side_step(trade.is_sell, step);

        self.trades
            .entry(price)
            .and_modify(|group| group.add_trade(trade))
            .or_insert_with(|| GroupedTrades::new(trade));
    }

    /// Add trade to the bin at the nearest step multiple (side-agnostic).
    /// Ties (exactly half a step) round up to the higher multiple.
    /// Intended for footprint/OHLC trade aggregation
    pub fn add_trade_to_nearest_bin(&mut self, trade: &Trade, step: PriceStep) {
        let price = trade.price.round_to_step(step);

        self.trades
            .entry(price)
            .and_modify(|group| group.add_trade(trade))
            .or_insert_with(|| GroupedTrades::new(trade));
    }

    /// Max of some extracted qty across price levels within [`lowest`, `highest`].
    pub fn max_qty_by<F>(&self, highest: Price, lowest: Price, f: F) -> Qty
    where
        F: Fn(&GroupedTrades) -> Qty,
    {
        let mut max_qty = Qty::default();
        for (price, group) in &self.trades {
            if *price >= lowest && *price <= highest {
                max_qty = max_qty.max(f(group));
            }
        }
        max_qty
    }

    /// Max cluster qty considering only price levels within [`lowest`, `highest`].
    /// Used by the aggregate visible-range computation where the viewport may
    /// show only a subset of each bar's full price range (y-pan/zoom).
    pub fn max_cluster_qty(&self, cluster_kind: ClusterKind, highest: Price, lowest: Price) -> Qty {
        self.max_qty_by(highest, lowest, |group| group.max_cluster_qty(cluster_kind))
    }

    /// Max cluster qty across all price levels in this bar (unfiltered).
    /// Used for per-bar individual scaling, the full bar should contribute.
    pub fn max_cluster_qty_all(&self, cluster_kind: ClusterKind) -> Qty {
        self.trades
            .values()
            .map(|group| group.max_cluster_qty(cluster_kind))
            .max()
            .unwrap_or_default()
    }

    pub fn calculate_poc(&mut self) {
        if self.trades.is_empty() {
            return;
        }

        let mut max_volume = Qty::ZERO;
        let mut poc_price = Price::from_f32(0.0);

        for (price, group) in &self.trades {
            let total_volume = group.total_qty();
            if total_volume > max_volume {
                max_volume = total_volume;
                poc_price = *price;
            }
        }

        self.poc = Some(PointOfControl {
            price: poc_price,
            volume: max_volume,
            status: NPoc::default(),
        });
    }

    pub fn set_poc_status(&mut self, status: NPoc) {
        if let Some(poc) = &mut self.poc {
            poc.status = status;
        }
    }

    pub fn poc_price(&self) -> Option<Price> {
        self.poc.map(|poc| poc.price)
    }

    pub fn clear(&mut self) {
        self.trades.clear();
        self.poc = None;
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FootprintSummary {
    pub buy: Qty,
    pub sell: Qty,
    pub total: Qty,
    pub delta: Qty,
    pub delta_pct: f64,
}

impl FootprintSummary {
    pub fn new(buy: Qty, sell: Qty) -> Self {
        let total = buy + sell;
        let delta = buy - sell;
        let total_f = total.to_f64();
        let delta_pct = if total_f > 0.0 {
            (delta.to_f64() / total_f) * 100.0
        } else {
            0.0
        };

        Self {
            buy,
            sell,
            total,
            delta,
            delta_pct,
        }
    }

    pub fn from_trades(footprint: &KlineTrades) -> Option<Self> {
        if footprint.trades.is_empty() {
            return None;
        }

        let (buy, sell) = footprint
            .trades
            .values()
            .fold((Qty::ZERO, Qty::ZERO), |(buy, sell), group| {
                (buy + group.buy_qty, sell + group.sell_qty)
            });

        Some(Self::new(buy, sell))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub enum KlineChartKind {
    #[default]
    Candles,
    Footprint {
        clusters: ClusterKind,
        #[serde(default)]
        scaling: ClusterScaling,
        studies: Vec<FootprintStudy>,
    },
}

impl KlineChartKind {
    pub fn allows_indicator(&self, indicator: KlineIndicator) -> bool {
        match self {
            KlineChartKind::Candles => !matches!(indicator, KlineIndicator::BarAnalysis),
            KlineChartKind::Footprint { .. } => !indicator.is_overlay(),
        }
    }

    pub fn min_scaling(&self) -> f32 {
        match self {
            KlineChartKind::Footprint { .. } => 0.4,
            KlineChartKind::Candles => 0.6,
        }
    }

    pub fn max_scaling(&self) -> f32 {
        match self {
            KlineChartKind::Footprint { .. } => 1.2,
            KlineChartKind::Candles => 2.5,
        }
    }

    pub fn max_cell_width(&self) -> f32 {
        match self {
            KlineChartKind::Footprint { .. } => 360.0,
            KlineChartKind::Candles => 16.0,
        }
    }

    pub fn min_cell_width(&self) -> f32 {
        match self {
            KlineChartKind::Footprint { .. } => 80.0,
            KlineChartKind::Candles => 1.0,
        }
    }

    pub fn max_cell_height(&self) -> f32 {
        match self {
            KlineChartKind::Footprint { .. } => 90.0,
            KlineChartKind::Candles => 8.0,
        }
    }

    pub fn min_cell_height(&self) -> f32 {
        match self {
            KlineChartKind::Footprint { .. } => 1.0,
            KlineChartKind::Candles => 0.001,
        }
    }

    pub fn default_cell_width(&self) -> f32 {
        match self {
            KlineChartKind::Footprint { .. } => 80.0,
            KlineChartKind::Candles => 4.0,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub enum ClusterKind {
    #[default]
    BidAsk,
    VolumeProfile,
    DeltaProfile,
    Table,
}

impl ClusterKind {
    pub const ALL: [ClusterKind; 4] = [
        ClusterKind::BidAsk,
        ClusterKind::VolumeProfile,
        ClusterKind::DeltaProfile,
        ClusterKind::Table,
    ];

    /// Minimum footprint cell width (in unscaled pixels) for the cluster rendering mode.
    pub fn min_footprint_width(self) -> f32 {
        match self {
            ClusterKind::VolumeProfile | ClusterKind::DeltaProfile => 80.0,
            ClusterKind::BidAsk => 120.0,
            ClusterKind::Table => 100.0,
        }
    }
}

impl std::fmt::Display for ClusterKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClusterKind::BidAsk => write!(f, "Bid/Ask"),
            ClusterKind::VolumeProfile => write!(f, "Volume Profile"),
            ClusterKind::DeltaProfile => write!(f, "Delta Profile"),
            ClusterKind::Table => write!(f, "Table"),
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    // Whether to show last value labels on top right/left when not hovering
    // e.g. OHLC/bar change values for the main chart, or last value of an indicator series
    pub data_labels_always_visible: bool,
    // Whether to show the footprint per-bar summary below each candle.
    pub show_footprint_summary: bool,
    // Whether to show a small candle next to footprint table clusters.
    pub show_footprint_table_candle: bool,
    // Optional main-chart order-flow bubbles for regular candlesticks.
    pub volume_bubbles: VolumeBubbleConfig,
    /// Session volume profile overlay for regular candlesticks.
    pub session_volume_profile: SessionVolumeProfileConfig,
    /// Settings owned by the VWAP overlay indicator.
    pub vwap: VwapConfig,
    /// Settings owned by the Smart Money Concepts overlay indicator.
    pub smc: SmcConfig,
    /// Settings owned by the CVD panel indicator.
    pub cvd: CvdConfig,
    /// Visual settings owned by configurable indicators.
    pub indicator_configs: IndicatorConfigs,
    /// Compatibility input for states saved before `indicator_configs`.
    #[serde(default, rename = "gex_levels", skip_serializing)]
    pub legacy_gex_levels: Option<crate::chart::gex::GexLevelsConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_labels_always_visible: false,
            show_footprint_summary: false,
            show_footprint_table_candle: true,
            volume_bubbles: VolumeBubbleConfig::default(),
            session_volume_profile: SessionVolumeProfileConfig::default(),
            vwap: VwapConfig::default(),
            smc: SmcConfig::default(),
            cvd: CvdConfig::default(),
            indicator_configs: IndicatorConfigs::default(),
            legacy_gex_levels: None,
        }
    }
}

impl Config {
    pub fn gex_levels(&self) -> crate::chart::gex::GexLevelsConfig {
        self.legacy_gex_levels
            .unwrap_or(self.indicator_configs.gex_levels)
    }

    pub fn with_gex_levels(mut self, config: crate::chart::gex::GexLevelsConfig) -> Self {
        self.indicator_configs.gex_levels = config;
        self.legacy_gex_levels = None;
        self
    }

    pub fn migrate_legacy_indicator_configs(&mut self) {
        if let Some(legacy) = self.legacy_gex_levels.take() {
            self.indicator_configs.gex_levels = legacy;
        }
        self.indicator_configs.gex_levels.migrate_legacy_defaults();
    }
}

#[derive(Debug, Default, Copy, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct IndicatorConfigs {
    pub gex_levels: crate::chart::gex::GexLevelsConfig,
}

#[derive(Debug, Copy, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct CvdConfig {
    pub render_style: CvdRenderStyle,
    pub candle_width_percent: f32,
    pub show_wicks: bool,
    pub line_width: f32,
    pub reset: CvdReset,
}

impl Default for CvdConfig {
    fn default() -> Self {
        Self {
            render_style: CvdRenderStyle::Candlesticks,
            candle_width_percent: 70.0,
            show_wicks: false,
            line_width: 1.0,
            reset: CvdReset::DailyUtc,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub enum CvdReset {
    #[default]
    DailyUtc,
    Continuous,
}

impl CvdReset {
    pub const ALL: [Self; 2] = [Self::DailyUtc, Self::Continuous];
}

impl std::fmt::Display for CvdReset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::DailyUtc => "Daily UTC",
            Self::Continuous => "Continuous",
        })
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub enum CvdRenderStyle {
    Line,
    #[default]
    Candlesticks,
}

impl CvdRenderStyle {
    pub const ALL: [Self; 2] = [Self::Candlesticks, Self::Line];
}

impl std::fmt::Display for CvdRenderStyle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Line => "Line",
            Self::Candlesticks => "Candlesticks",
        })
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct VwapConfig {
    pub anchor: SessionProfileInterval,
    pub line_width: f32,
    pub show_bands: bool,
    pub band_multiplier: f32,
    pub show_labels: bool,
}

impl Default for VwapConfig {
    fn default() -> Self {
        Self {
            anchor: SessionProfileInterval::Daily,
            line_width: 1.6,
            show_bands: true,
            band_multiplier: 1.0,
            show_labels: true,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct SmcConfig {
    /// Lookback period for swing detection
    pub swing_length: i32,
    /// Display swing structure (BOS/CHoCH)
    pub show_swing_structure: bool,
    /// Display internal structure (smaller lookback)
    pub show_internal_structure: bool,
    /// Display order blocks
    pub show_swing_order_blocks: bool,
    pub show_internal_order_blocks: bool,
    /// Number of order blocks to show
    pub order_blocks_count: i32,
    /// Display equal highs/lows
    pub show_equal_highs_lows: bool,
    /// Bars confirmation for equal highs/lows
    pub equal_highs_lows_length: i32,
    /// Sensitivity threshold for equal highs/lows
    pub equal_highs_lows_threshold: f32,
    /// Display fair value gaps
    pub show_fair_value_gaps: bool,
    /// Display premium/discount zones
    pub show_premium_discount_zones: bool,
    /// Display trailing strong/weak high/low
    pub show_strong_weak_high_low: bool,
}

impl Default for SmcConfig {
    fn default() -> Self {
        Self {
            swing_length: 50,
            show_swing_structure: true,
            show_internal_structure: true,
            show_swing_order_blocks: true,
            show_internal_order_blocks: true,
            order_blocks_count: 5,
            show_equal_highs_lows: true,
            equal_highs_lows_length: 3,
            equal_highs_lows_threshold: 0.1,
            show_fair_value_gaps: false,
            show_premium_discount_zones: false,
            show_strong_weak_high_low: true,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct SessionVolumeProfileConfig {
    pub enabled: bool,
    pub interval: SessionProfileInterval,
    pub placement: SessionProfilePlacement,
    pub mode: SessionProfileMode,
    /// Percentage of session volume enclosed by VAH/VAL.
    pub value_area_percent: f32,
    /// Maximum profile width as percentage of the session width.
    pub width_percent: f32,
    /// Number of chart ticks aggregated into one profile row.
    pub row_size_ticks: u16,
    pub show_poc: bool,
    pub show_value_area: bool,
    pub show_vwap: bool,
    pub show_session_high_low: bool,
}

impl Default for SessionVolumeProfileConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval: SessionProfileInterval::Hourly,
            placement: SessionProfilePlacement::Left,
            mode: SessionProfileMode::Volume,
            value_area_percent: 70.0,
            width_percent: 35.0,
            row_size_ticks: 1,
            show_poc: true,
            show_value_area: true,
            show_vwap: true,
            show_session_high_low: true,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub enum SessionProfileInterval {
    Minutes30,
    #[default]
    Hourly,
    Hours4,
    Daily,
    Weekly,
}

impl SessionProfileInterval {
    pub const ALL: [Self; 5] = [
        Self::Minutes30,
        Self::Hourly,
        Self::Hours4,
        Self::Daily,
        Self::Weekly,
    ];

    pub fn milliseconds(self) -> u64 {
        match self {
            Self::Minutes30 => 30 * 60_000,
            Self::Hourly => 60 * 60_000,
            Self::Hours4 => 4 * 60 * 60_000,
            Self::Daily => 24 * 60 * 60_000,
            Self::Weekly => 7 * 24 * 60 * 60_000,
        }
    }
}

impl std::fmt::Display for SessionProfileInterval {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Minutes30 => "30 minutes",
            Self::Hourly => "Hourly",
            Self::Hours4 => "4 hours",
            Self::Daily => "Daily",
            Self::Weekly => "Weekly",
        })
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub enum SessionProfilePlacement {
    #[default]
    Left,
    Right,
}

impl SessionProfilePlacement {
    pub const ALL: [Self; 2] = [Self::Left, Self::Right];
}

impl std::fmt::Display for SessionProfilePlacement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Left => "Left / session open",
            Self::Right => "Right / session close",
        })
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub enum SessionProfileMode {
    #[default]
    Volume,
    Delta,
}

impl SessionProfileMode {
    pub const ALL: [Self; 2] = [Self::Volume, Self::Delta];
}

impl std::fmt::Display for SessionProfileMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Volume => "Volume",
            Self::Delta => "Delta",
        })
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Serialize)]
pub struct VolumeBubbleConfig {
    pub enabled: bool,
    pub preset: VolumeBubblePreset,
    pub threshold_mode: BubbleThresholdMode,
    pub min_qty: f64,
    pub adaptive_window_minutes: u64,
    pub display_percentile: f32,
    pub label_percentile: f32,
    pub cluster_window_ms: u32,
    pub cluster_price_ticks: u32,
    pub max_bubbles_per_bar: usize,
    pub max_bubbles_in_view: usize,
    pub max_labels_in_view: usize,
    pub max_candidates_per_candle: usize,
    pub history_window_minutes: u64,
    pub use_raw_trades_when_available: bool,
    pub min_radius_px: f32,
    pub max_radius_px: f32,
    pub fill_enabled: bool,
    pub three_dimensional: bool,
    pub fill_intensity: f32,
    pub border_opacity: f32,
    pub hover_opacity: f32,
    pub age_fading: bool,
    pub label_mode: BubbleLabelMode,
    pub color_mode: BubbleColorMode,
    pub session: VolumeBubbleSession,
    pub min_center_distance_px: f32,
    pub price_response_enabled: bool,
    pub price_response_horizon_seconds: u32,
    pub minimum_side_dominance: f32,
    pub response_threshold_bps: f32,
}

impl Default for VolumeBubbleConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            preset: VolumeBubblePreset::Balanced,
            threshold_mode: BubbleThresholdMode::Hybrid,
            min_qty: 0.0,
            adaptive_window_minutes: 20,
            display_percentile: 95.0,
            label_percentile: 99.0,
            cluster_window_ms: 500,
            cluster_price_ticks: 1,
            max_bubbles_per_bar: 3,
            max_bubbles_in_view: 40,
            max_labels_in_view: 6,
            max_candidates_per_candle: 32,
            history_window_minutes: 30,
            use_raw_trades_when_available: true,
            min_radius_px: 5.0,
            max_radius_px: 16.0,
            fill_enabled: true,
            three_dimensional: false,
            fill_intensity: 1.0,
            border_opacity: 0.90,
            hover_opacity: 0.26,
            age_fading: true,
            label_mode: BubbleLabelMode::ExtremeOnly,
            color_mode: BubbleColorMode::Delta,
            session: VolumeBubbleSession::Auto,
            min_center_distance_px: 12.0,
            price_response_enabled: false,
            price_response_horizon_seconds: 10,
            minimum_side_dominance: 0.65,
            response_threshold_bps: 1.5,
        }
    }
}

impl VolumeBubbleConfig {
    pub fn for_preset(preset: VolumeBubblePreset) -> Self {
        let mut config = Self {
            preset,
            ..Self::default()
        };
        match preset {
            VolumeBubblePreset::Clean => {
                config.display_percentile = 99.0;
                config.label_percentile = 100.0;
                config.cluster_window_ms = 750;
                config.cluster_price_ticks = 2;
                config.max_bubbles_per_bar = 1;
                config.max_bubbles_in_view = 25;
                config.max_labels_in_view = 0;
                config.label_mode = BubbleLabelMode::None;
            }
            VolumeBubblePreset::Balanced | VolumeBubblePreset::Custom => {}
            VolumeBubblePreset::Detailed => {
                config.display_percentile = 90.0;
                config.label_percentile = 98.0;
                config.cluster_window_ms = 250;
                config.max_bubbles_per_bar = 5;
                config.max_bubbles_in_view = 70;
                config.max_labels_in_view = 10;
            }
        }
        config
    }

    pub fn customized(mut self) -> Self {
        self.preset = VolumeBubblePreset::Custom;
        self
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct VolumeBubbleConfigWire {
    enabled: bool,
    preset: Option<VolumeBubblePreset>,
    threshold_mode: Option<BubbleThresholdMode>,
    min_qty: f64,
    adaptive_window_minutes: Option<u64>,
    display_percentile: Option<f32>,
    label_percentile: Option<f32>,
    cluster_window_ms: Option<u32>,
    cluster_price_ticks: Option<u32>,
    max_bubbles_per_bar: Option<usize>,
    max_bubbles_in_view: Option<usize>,
    max_labels_in_view: Option<usize>,
    max_candidates_per_candle: Option<usize>,
    history_window_minutes: Option<u64>,
    use_raw_trades_when_available: Option<bool>,
    min_radius_px: Option<f32>,
    max_radius_px: Option<f32>,
    fill_enabled: Option<bool>,
    three_dimensional: Option<bool>,
    show_labels: Option<bool>,
    label_mode: Option<BubbleLabelMode>,
    color_mode: Option<BubbleColorMode>,
    session: Option<VolumeBubbleSession>,
    fill_intensity: Option<f32>,
    border_opacity: Option<f32>,
    hover_opacity: Option<f32>,
    age_fading: Option<bool>,
    min_center_distance_px: Option<f32>,
    price_response_enabled: Option<bool>,
    price_response_horizon_seconds: Option<u32>,
    minimum_side_dominance: Option<f32>,
    response_threshold_bps: Option<f32>,
}

impl<'de> Deserialize<'de> for VolumeBubbleConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = VolumeBubbleConfigWire::deserialize(deserializer)?;
        let legacy = wire.preset.is_none();
        let mut config = Self::default();
        config.enabled = wire.enabled;
        config.min_qty = wire.min_qty;
        config.preset = wire.preset.unwrap_or(VolumeBubblePreset::Custom);
        config.threshold_mode = wire.threshold_mode.unwrap_or(if legacy {
            BubbleThresholdMode::Fixed
        } else {
            config.threshold_mode
        });
        config.adaptive_window_minutes = wire
            .adaptive_window_minutes
            .unwrap_or(config.adaptive_window_minutes);
        config.display_percentile = wire.display_percentile.unwrap_or(config.display_percentile);
        config.label_percentile = wire.label_percentile.unwrap_or(config.label_percentile);
        config.cluster_window_ms = wire.cluster_window_ms.unwrap_or(config.cluster_window_ms);
        config.cluster_price_ticks = wire
            .cluster_price_ticks
            .unwrap_or(config.cluster_price_ticks);
        config.max_bubbles_per_bar = wire
            .max_bubbles_per_bar
            .unwrap_or(config.max_bubbles_per_bar);
        config.max_bubbles_in_view = wire
            .max_bubbles_in_view
            .unwrap_or(config.max_bubbles_in_view);
        config.max_labels_in_view = wire.max_labels_in_view.unwrap_or(config.max_labels_in_view);
        config.max_candidates_per_candle = wire
            .max_candidates_per_candle
            .unwrap_or(config.max_candidates_per_candle);
        config.history_window_minutes = wire
            .history_window_minutes
            .unwrap_or(config.history_window_minutes);
        config.use_raw_trades_when_available = wire
            .use_raw_trades_when_available
            .unwrap_or(config.use_raw_trades_when_available);
        config.min_radius_px = wire.min_radius_px.unwrap_or(config.min_radius_px);
        config.max_radius_px = wire.max_radius_px.unwrap_or(config.max_radius_px);
        config.fill_enabled = wire.fill_enabled.unwrap_or(config.fill_enabled);
        config.three_dimensional = wire.three_dimensional.unwrap_or(config.three_dimensional);
        config.label_mode = wire.label_mode.unwrap_or(match wire.show_labels {
            Some(true) => BubbleLabelMode::All,
            Some(false) => BubbleLabelMode::None,
            None => config.label_mode,
        });
        config.color_mode = wire.color_mode.unwrap_or(config.color_mode);
        config.session = wire.session.unwrap_or(config.session);
        config.fill_intensity = wire.fill_intensity.unwrap_or(config.fill_intensity);
        config.border_opacity = wire.border_opacity.unwrap_or(config.border_opacity);
        config.hover_opacity = wire.hover_opacity.unwrap_or(config.hover_opacity);
        config.age_fading = wire.age_fading.unwrap_or(config.age_fading);
        config.min_center_distance_px = wire
            .min_center_distance_px
            .unwrap_or(config.min_center_distance_px);
        config.price_response_enabled = wire
            .price_response_enabled
            .unwrap_or(config.price_response_enabled);
        config.price_response_horizon_seconds = wire
            .price_response_horizon_seconds
            .unwrap_or(config.price_response_horizon_seconds);
        config.minimum_side_dominance = wire
            .minimum_side_dominance
            .unwrap_or(config.minimum_side_dominance);
        config.response_threshold_bps = wire
            .response_threshold_bps
            .unwrap_or(config.response_threshold_bps);
        Ok(config)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdaptiveBubbleThreshold {
    pub effective: f64,
    pub adaptive: Option<f64>,
    pub warmed_up: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct StabilizedBubbleThreshold {
    pub value: f64,
    pub last_update: Option<UnixMs>,
}

impl StabilizedBubbleThreshold {
    pub fn update(&mut self, proposed: f64, now: UnixMs) -> f64 {
        if !proposed.is_finite() || proposed < 0.0 {
            return self.value.max(0.0);
        }
        if self
            .last_update
            .is_some_and(|last| now.as_u64().saturating_sub(last.as_u64()) < 1_000)
        {
            return self.value;
        }
        let relative_change = if self.value > f64::EPSILON {
            (proposed - self.value).abs() / self.value
        } else {
            1.0
        };
        if self.last_update.is_none() || relative_change >= 0.10 {
            self.value = proposed;
            self.last_update = Some(now);
        }
        self.value
    }
}

pub fn percentile(values: &[f64], percentile: f32) -> Option<f64> {
    let mut values = values
        .iter()
        .copied()
        .filter(|value| value.is_finite() && *value >= 0.0)
        .collect::<Vec<_>>();
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let rank =
        (f64::from(percentile.clamp(0.0, 100.0)) / 100.0) * (values.len().saturating_sub(1) as f64);
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    let fraction = rank - lower as f64;
    Some(values[lower] + ((values[upper] - values[lower]) * fraction))
}

pub fn adaptive_bubble_threshold(
    values: &[f64],
    config: &VolumeBubbleConfig,
    minimum_samples: usize,
) -> AdaptiveBubbleThreshold {
    let warmed_up = values.len() >= minimum_samples;
    let adaptive = warmed_up
        .then(|| percentile(values, config.display_percentile))
        .flatten();
    let effective = match config.threshold_mode {
        BubbleThresholdMode::Fixed => config.min_qty,
        BubbleThresholdMode::AdaptivePercentile => adaptive.unwrap_or(config.min_qty),
        BubbleThresholdMode::Hybrid => adaptive.unwrap_or(config.min_qty).max(config.min_qty),
    };
    AdaptiveBubbleThreshold {
        effective: if effective.is_finite() {
            effective.max(0.0)
        } else {
            config.min_qty.max(0.0)
        },
        adaptive,
        warmed_up,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BubbleThresholdBaselines {
    pub combined: AdaptiveBubbleThreshold,
    pub buy_dominant: AdaptiveBubbleThreshold,
    pub sell_dominant: AdaptiveBubbleThreshold,
}

pub fn adaptive_bubble_threshold_baselines(
    clusters: &[VolumeBubbleCluster],
    config: &VolumeBubbleConfig,
    minimum_samples_per_side: usize,
) -> BubbleThresholdBaselines {
    let combined_values = clusters
        .iter()
        .map(|cluster| cluster.total_qty.to_f64())
        .collect::<Vec<_>>();
    let combined = adaptive_bubble_threshold(&combined_values, config, minimum_samples_per_side);
    let side = |buy: bool| {
        let values = clusters
            .iter()
            .filter(|cluster| (cluster.buy_qty >= cluster.sell_qty) == buy)
            .map(|cluster| cluster.total_qty.to_f64())
            .collect::<Vec<_>>();
        if values.len() < minimum_samples_per_side {
            combined
        } else {
            adaptive_bubble_threshold(&values, config, minimum_samples_per_side)
        }
    };
    BubbleThresholdBaselines {
        combined,
        buy_dominant: side(true),
        sell_dominant: side(false),
    }
}

#[derive(Clone, Copy)]
struct ClusterAccumulator {
    first_time: UnixMs,
    last_time: UnixMs,
    anchor_price: Price,
    anchor_is_sell: bool,
    price_qty_units: f64,
    time_qty_ms: f64,
    total_qty: Qty,
    buy_qty: Qty,
    sell_qty: Qty,
    trade_count: usize,
    largest_trade_qty: Qty,
}

impl ClusterAccumulator {
    fn new(trade: Trade) -> Self {
        let qty = trade.qty.to_f64();
        Self {
            first_time: trade.time,
            last_time: trade.time,
            anchor_price: trade.price,
            anchor_is_sell: trade.is_sell,
            price_qty_units: trade.price.units as f64 * qty,
            time_qty_ms: trade.time.as_u64() as f64 * qty,
            total_qty: trade.qty,
            buy_qty: if trade.is_sell { Qty::ZERO } else { trade.qty },
            sell_qty: if trade.is_sell { trade.qty } else { Qty::ZERO },
            trade_count: 1,
            largest_trade_qty: trade.qty,
        }
    }

    fn add(&mut self, trade: Trade) {
        let qty = trade.qty.to_f64();
        self.last_time = self.last_time.max(trade.time);
        self.price_qty_units += trade.price.units as f64 * qty;
        self.time_qty_ms += trade.time.as_u64() as f64 * qty;
        self.total_qty += trade.qty;
        if trade.is_sell {
            self.sell_qty += trade.qty
        } else {
            self.buy_qty += trade.qty
        }
        self.trade_count += 1;
        self.largest_trade_qty = self.largest_trade_qty.max(trade.qty);
    }

    fn vwap(self, price_step: PriceStep) -> Price {
        let qty = self.total_qty.to_f64();
        if qty <= f64::EPSILON {
            Price::from_units(0)
        } else {
            Price::from_units((self.price_qty_units / qty).round() as i64).round_to_step(price_step)
        }
    }

    fn finish(self, candle_time: UnixMs, price_step: PriceStep) -> VolumeBubbleCluster {
        let qty = self.total_qty.to_f64().max(f64::EPSILON);
        let weighted_time = UnixMs::new((self.time_qty_ms / qty).round().max(0.0) as u64);
        let vwap_price = self.vwap(price_step);
        let id = stable_cluster_id(
            candle_time,
            self.first_time,
            self.anchor_price.round_to_step(price_step),
            self.anchor_is_sell,
        );
        VolumeBubbleCluster {
            id,
            candle_time,
            first_time: self.first_time,
            last_time: self.last_time,
            weighted_time,
            vwap_price,
            total_qty: self.total_qty,
            buy_qty: self.buy_qty,
            sell_qty: self.sell_qty,
            delta_qty: self.buy_qty - self.sell_qty,
            trade_count: self.trade_count,
            largest_trade_qty: self.largest_trade_qty,
            percentile_rank: 0.0,
            importance_score: 0.0,
        }
    }
}

fn stable_cluster_id(
    candle_time: UnixMs,
    first_time: UnixMs,
    price: Price,
    is_sell: bool,
) -> VolumeBubbleClusterId {
    // FNV-1a over stable cluster anchors. Quantities intentionally do not participate: the ID
    // remains unchanged while the current live burst grows.
    let mut hash = 0xcbf29ce484222325u64;
    for value in [
        candle_time.as_u64(),
        first_time.as_u64(),
        price.units as u64,
    ] {
        for byte in value.to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    hash ^ u64::from(is_sell)
}

pub fn cluster_volume_bubble_trades(
    trades: &[Trade],
    candle_time: UnixMs,
    timeframe_ms: u64,
    price_step: PriceStep,
    config: &VolumeBubbleConfig,
) -> Vec<VolumeBubbleCluster> {
    if timeframe_ms == 0 || price_step.units <= 0 {
        return Vec::new();
    }
    let candle_end = candle_time.saturating_add(timeframe_ms);
    let mut ordered = trades
        .iter()
        .copied()
        .filter(|trade| {
            trade.time >= candle_time
                && trade.time < candle_end
                && trade.qty.units > 0
                && trade.price.units > 0
        })
        .collect::<Vec<_>>();
    ordered.sort_by_key(|trade| (trade.time, trade.id));

    let max_price_distance = price_step
        .units
        .saturating_mul(i64::from(config.cluster_price_ticks));
    let mut accumulators: Vec<ClusterAccumulator> = Vec::new();
    for trade in ordered {
        let matching = accumulators.iter().rposition(|cluster| {
            trade
                .time
                .as_u64()
                .saturating_sub(cluster.last_time.as_u64())
                <= u64::from(config.cluster_window_ms)
                && (trade.price.units - cluster.vwap(price_step).units).abs() <= max_price_distance
        });
        if let Some(index) = matching {
            accumulators[index].add(trade);
        } else {
            accumulators.push(ClusterAccumulator::new(trade));
        }
    }
    accumulators
        .into_iter()
        .map(|cluster| cluster.finish(candle_time, price_step))
        .collect()
}

pub fn rank_volume_bubble_clusters(clusters: &mut [VolumeBubbleCluster], effective_threshold: f64) {
    let quantities = clusters
        .iter()
        .map(|cluster| cluster.total_qty.to_f64())
        .collect::<Vec<_>>();
    for cluster in clusters {
        let qty = cluster.total_qty.to_f64();
        let below_or_equal = quantities
            .iter()
            .filter(|candidate| **candidate <= qty)
            .count();
        cluster.percentile_rank = 100.0 * below_or_equal as f32 / quantities.len().max(1) as f32;
        let dominance = cluster.buy_qty.abs_diff(cluster.sell_qty).to_f64() / qty.max(f64::EPSILON);
        let threshold_ratio = qty / effective_threshold.max(f64::EPSILON);
        let count_bonus = (cluster.trade_count as f32).ln_1p().min(3.0) / 3.0;
        cluster.importance_score = cluster.percentile_rank
            + (threshold_ratio.ln_1p() as f32 * 8.0)
            + (dominance as f32 * 3.0)
            + count_bonus;
    }
}

pub fn apply_volume_bubble_budget(
    clusters: impl IntoIterator<Item = VolumeBubbleCluster>,
    config: &VolumeBubbleConfig,
    effective_threshold: f64,
) -> Vec<VolumeBubbleCluster> {
    let mut clusters = clusters
        .into_iter()
        .filter(|cluster| cluster.total_qty.to_f64() >= effective_threshold)
        .collect::<Vec<_>>();
    rank_volume_bubble_clusters(&mut clusters, effective_threshold);
    clusters.sort_by(|left, right| {
        right
            .importance_score
            .total_cmp(&left.importance_score)
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut per_candle = FxHashMap::<UnixMs, usize>::default();
    clusters.retain(|cluster| {
        let count = per_candle.entry(cluster.candle_time).or_default();
        if *count >= config.max_bubbles_per_bar {
            return false;
        }
        *count += 1;
        true
    });
    clusters.truncate(config.max_bubbles_in_view);
    clusters
}

pub fn volume_bubble_radius(
    qty: f64,
    effective_threshold: f64,
    reference_p99: f64,
    min_radius: f32,
    max_radius: f32,
) -> f32 {
    let min_radius = min_radius.max(0.0);
    let max_radius = max_radius.max(min_radius);
    let threshold = effective_threshold.max(f64::EPSILON);
    let denominator = (1.0 + reference_p99.max(threshold) / threshold).ln();
    let relative = if denominator > f64::EPSILON {
        (1.0 + qty.max(0.0) / threshold).ln() / denominator
    } else {
        0.0
    };
    let normalized = relative.clamp(0.0, 1.0) as f32;
    (min_radius.mul_add(
        min_radius,
        normalized * (max_radius * max_radius - min_radius * min_radius),
    ))
    .sqrt()
}

pub fn bubble_age_factor(age_ms: u64, enabled: bool) -> f32 {
    if !enabled || age_ms <= 30_000 {
        return 1.0;
    }
    if age_ms >= 120_000 {
        return 0.58;
    }
    1.0 - (age_ms - 30_000) as f32 / 90_000.0 * 0.42
}

pub fn classify_bubble_price_response(
    cluster: &VolumeBubbleCluster,
    future_price: Option<Price>,
    horizon_elapsed: bool,
    config: &VolumeBubbleConfig,
) -> BubblePriceResponse {
    if !config.price_response_enabled {
        return BubblePriceResponse::Neutral;
    }
    let total = cluster.total_qty.to_f64();
    let dominance = cluster.buy_qty.max(cluster.sell_qty).to_f64() / total.max(f64::EPSILON);
    if dominance < f64::from(config.minimum_side_dominance) {
        return BubblePriceResponse::Neutral;
    }
    if !horizon_elapsed || future_price.is_none() {
        return BubblePriceResponse::Pending;
    }
    let move_bps = (future_price.unwrap().to_f64() / cluster.vwap_price.to_f64() - 1.0) * 10_000.0;
    let directional = if cluster.buy_qty > cluster.sell_qty {
        move_bps
    } else {
        -move_bps
    };
    if directional >= f64::from(config.response_threshold_bps) {
        BubblePriceResponse::FollowThrough
    } else if directional <= -f64::from(config.response_threshold_bps) {
        BubblePriceResponse::Reversed
    } else {
        BubblePriceResponse::Stalled
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub enum BubbleColorMode {
    #[default]
    Delta,
    DominantSide,
}

impl BubbleColorMode {
    pub const ALL: [BubbleColorMode; 2] = [BubbleColorMode::Delta, BubbleColorMode::DominantSide];
}

impl std::fmt::Display for BubbleColorMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BubbleColorMode::Delta => write!(f, "Delta"),
            BubbleColorMode::DominantSide => write!(f, "Dominant side"),
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub enum VolumeBubbleSession {
    #[default]
    Auto,
    Asian,
    London,
    NewYork,
}

impl VolumeBubbleSession {
    pub const ALL: [VolumeBubbleSession; 4] = [
        VolumeBubbleSession::Auto,
        VolumeBubbleSession::Asian,
        VolumeBubbleSession::London,
        VolumeBubbleSession::NewYork,
    ];
}

impl std::fmt::Display for VolumeBubbleSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VolumeBubbleSession::Auto => write!(f, "Auto session"),
            VolumeBubbleSession::Asian => write!(f, "Asian"),
            VolumeBubbleSession::London => write!(f, "London"),
            VolumeBubbleSession::NewYork => write!(f, "New York"),
        }
    }
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
pub enum ClusterScaling {
    #[default]
    /// Scale based on the maximum quantity in the visible range.
    VisibleRange,
    /// Blend global VisibleRange and per-cluster Individual using a weight in [0.0, 1.0].
    /// weight = fraction of global contribution (1.0 == all-global, 0.0 == all-individual).
    Hybrid { weight: f32 },
    /// Scale based only on the maximum quantity inside the datapoint (per-candle).
    Datapoint,
}

impl ClusterScaling {
    pub const ALL: [ClusterScaling; 3] = [
        ClusterScaling::VisibleRange,
        ClusterScaling::Hybrid { weight: 0.2 },
        ClusterScaling::Datapoint,
    ];

    /// Blend the global visible-range max qty with the per-candle individual max qty
    /// according to the scaling strategy.
    pub fn effective_qty(self, visible_max: f64, individual_max: Qty) -> f64 {
        match self {
            ClusterScaling::VisibleRange => Qty::scale_or_one(visible_max),
            ClusterScaling::Datapoint => individual_max.to_scale_or_one(),
            ClusterScaling::Hybrid { weight } => {
                let w = weight.clamp(0.0, 1.0) as f64;
                Qty::scale_or_one(visible_max * w + individual_max.to_f64() * (1.0 - w))
            }
        }
    }
}

impl std::fmt::Display for ClusterScaling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClusterScaling::VisibleRange => write!(f, "Visible Range"),
            ClusterScaling::Hybrid { weight } => write!(f, "Hybrid (weight: {:.2})", weight),
            ClusterScaling::Datapoint => write!(f, "Per-candle"),
        }
    }
}

impl std::cmp::Eq for ClusterScaling {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum FootprintStudy {
    NPoC {
        lookback: usize,
    },
    Imbalance {
        threshold: usize,
        color_scale: Option<usize>,
        ignore_zeros: bool,
    },
}

impl FootprintStudy {
    pub fn is_same_type(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (FootprintStudy::NPoC { .. }, FootprintStudy::NPoC { .. })
                | (
                    FootprintStudy::Imbalance { .. },
                    FootprintStudy::Imbalance { .. }
                )
        )
    }
}

impl FootprintStudy {
    pub const ALL: [FootprintStudy; 2] = [
        FootprintStudy::NPoC { lookback: 80 },
        FootprintStudy::Imbalance {
            threshold: 200,
            color_scale: Some(400),
            ignore_zeros: true,
        },
    ];
}

impl std::fmt::Display for FootprintStudy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FootprintStudy::NPoC { .. } => write!(f, "Naked Point of Control"),
            FootprintStudy::Imbalance { .. } => write!(f, "Imbalance"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PointOfControl {
    pub price: Price,
    pub volume: Qty,
    pub status: NPoc,
}

impl Default for PointOfControl {
    fn default() -> Self {
        Self {
            price: Price::from_f32(0.0),
            volume: Qty::ZERO,
            status: NPoc::default(),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum NPoc {
    #[default]
    None,
    Naked,
    Filled {
        at: u64,
    },
}

impl NPoc {
    pub fn filled(&mut self, at: u64) {
        *self = NPoc::Filled { at };
    }

    pub fn unfilled(&mut self) {
        *self = NPoc::Naked;
    }
}

#[cfg(test)]
mod volume_bubble_tests {
    use super::*;

    fn step() -> PriceStep {
        PriceStep {
            units: Price::from_f64(0.1).units,
        }
    }

    fn trade(id: u64, time: u64, price: f64, qty: f64, is_sell: bool) -> Trade {
        Trade {
            id: Some(id),
            time: UnixMs::new(time),
            is_sell,
            price: Price::from_f64(price),
            qty: Qty::from_f64(qty),
        }
    }

    fn cluster(trades: &[Trade], config: &VolumeBubbleConfig) -> Vec<VolumeBubbleCluster> {
        cluster_volume_bubble_trades(trades, UnixMs::new(60_000), 60_000, step(), config)
    }

    #[test]
    fn clustering_preserves_volume_sides_vwap_count_largest_and_stable_id() {
        let config = VolumeBubbleConfig::default();
        let trades = [
            trade(1, 61_000, 100.0, 2.0, false),
            trade(2, 61_200, 100.1, 3.0, true),
        ];
        let first = cluster(&trades, &config);
        let second = cluster(&trades, &config);
        let growing = cluster(&trades[..1], &config);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].id, second[0].id);
        assert_eq!(first[0].id, growing[0].id);
        assert_eq!(first[0].total_qty.to_f64(), 5.0);
        assert_eq!(first[0].buy_qty.to_f64(), 2.0);
        assert_eq!(first[0].sell_qty.to_f64(), 3.0);
        assert_eq!(first[0].trade_count, 2);
        assert_eq!(first[0].largest_trade_qty.to_f64(), 3.0);
        assert!((first[0].vwap_price.to_f64() - 100.1).abs() < 0.001);
    }

    #[test]
    fn temporal_and_price_distance_split_clusters() {
        let config = VolumeBubbleConfig::default();
        assert_eq!(
            cluster(
                &[
                    trade(1, 61_000, 100.0, 1.0, false),
                    trade(2, 61_501, 100.0, 1.0, false),
                ],
                &config
            )
            .len(),
            2
        );
        assert_eq!(
            cluster(
                &[
                    trade(1, 61_000, 100.0, 1.0, false),
                    trade(2, 61_100, 100.2, 1.0, false),
                ],
                &config
            )
            .len(),
            2
        );
    }

    #[test]
    fn percentile_threshold_warmup_hybrid_and_empty_are_finite() {
        let mut config = VolumeBubbleConfig {
            min_qty: 50.0,
            ..VolumeBubbleConfig::default()
        };
        let values = (1..=100).map(f64::from).collect::<Vec<_>>();
        assert!((percentile(&values, 95.0).unwrap() - 95.05).abs() < 0.01);
        let hybrid = adaptive_bubble_threshold(&values, &config, 20);
        assert!(hybrid.warmed_up && hybrid.effective > 95.0);
        config.min_qty = 200.0;
        assert_eq!(
            adaptive_bubble_threshold(&values, &config, 20).effective,
            200.0
        );
        let warmup = adaptive_bubble_threshold(&[100.0], &config, 20);
        assert!(!warmup.warmed_up && warmup.effective == 200.0);
        assert!(
            adaptive_bubble_threshold(&[], &config, 20)
                .effective
                .is_finite()
        );
    }

    #[test]
    fn threshold_hysteresis_ignores_small_and_too_frequent_changes() {
        let mut state = StabilizedBubbleThreshold::default();
        assert_eq!(state.update(100.0, UnixMs::new(1_000)), 100.0);
        assert_eq!(state.update(120.0, UnixMs::new(1_500)), 100.0);
        assert_eq!(state.update(105.0, UnixMs::new(2_500)), 100.0);
        assert_eq!(state.update(120.0, UnixMs::new(3_500)), 120.0);
    }

    #[test]
    fn side_baseline_falls_back_to_combined_until_enough_samples() {
        let config = VolumeBubbleConfig::default();
        let clusters = cluster(
            &[
                trade(1, 61_000, 100.0, 10.0, false),
                trade(2, 62_000, 101.0, 20.0, true),
            ],
            &VolumeBubbleConfig {
                cluster_window_ms: 10,
                ..config
            },
        );
        let baselines = adaptive_bubble_threshold_baselines(&clusters, &config, 20);
        assert_eq!(baselines.buy_dominant, baselines.combined);
        assert_eq!(baselines.sell_dominant, baselines.combined);
    }

    #[test]
    fn ranking_respects_per_bar_and_viewport_budgets() {
        let config = VolumeBubbleConfig {
            max_bubbles_per_bar: 1,
            max_bubbles_in_view: 2,
            ..VolumeBubbleConfig::default()
        };
        let mut clusters = Vec::new();
        for (id, candle, qty) in [
            (1, 60_000, 10.0),
            (2, 60_000, 50.0),
            (3, 120_000, 30.0),
            (4, 180_000, 20.0),
        ] {
            clusters.push(
                cluster_volume_bubble_trades(
                    &[trade(id, candle + 1, 100.0, qty, false)],
                    UnixMs::new(candle),
                    60_000,
                    step(),
                    &config,
                )[0],
            );
        }
        let selected = apply_volume_bubble_budget(clusters, &config, 0.0);
        assert_eq!(selected.len(), 2);
        assert!(selected.iter().any(|item| item.total_qty.to_f64() == 50.0));
        assert!(selected.iter().any(|item| item.total_qty.to_f64() == 30.0));
    }

    #[test]
    fn radius_is_monotonic_bounded_finite_and_outlier_resistant() {
        let small = volume_bubble_radius(10.0, 10.0, 1_000.0, 5.0, 16.0);
        let medium = volume_bubble_radius(100.0, 10.0, 1_000.0, 5.0, 16.0);
        let huge = volume_bubble_radius(1e12, 10.0, 1_000.0, 5.0, 16.0);
        assert!(small.is_finite() && small >= 5.0);
        assert!(medium >= small && medium < 16.0);
        assert_eq!(huge, 16.0);
    }

    #[test]
    fn age_fading_is_progressive_and_never_invisible() {
        assert_eq!(bubble_age_factor(10_000, true), 1.0);
        assert!(bubble_age_factor(60_000, true) < 1.0);
        assert_eq!(bubble_age_factor(180_000, true), 0.58);
    }

    #[test]
    fn price_response_has_no_lookahead_and_is_symmetric() {
        let config = VolumeBubbleConfig {
            price_response_enabled: true,
            ..VolumeBubbleConfig::default()
        };
        let buy = cluster(&[trade(1, 61_000, 100.0, 10.0, false)], &config)[0];
        assert_eq!(
            classify_bubble_price_response(&buy, Some(Price::from_f64(101.0)), false, &config),
            BubblePriceResponse::Pending
        );
        assert_eq!(
            classify_bubble_price_response(&buy, Some(Price::from_f64(101.0)), true, &config),
            BubblePriceResponse::FollowThrough
        );
        assert_eq!(
            classify_bubble_price_response(&buy, Some(Price::from_f64(99.0)), true, &config),
            BubblePriceResponse::Reversed
        );
        assert_eq!(
            classify_bubble_price_response(&buy, Some(Price::from_f64(100.001)), true, &config),
            BubblePriceResponse::Stalled
        );
        let sell = cluster(&[trade(2, 61_000, 100.0, 10.0, true)], &config)[0];
        assert_eq!(
            classify_bubble_price_response(&sell, Some(Price::from_f64(99.0)), true, &config),
            BubblePriceResponse::FollowThrough
        );
    }

    #[test]
    fn legacy_label_and_threshold_migration_and_balanced_new_default() {
        let hidden: VolumeBubbleConfig =
            serde_json::from_str(r#"{"show_labels":false,"min_qty":12.0}"#).unwrap();
        let shown: VolumeBubbleConfig =
            serde_json::from_str(r#"{"show_labels":true,"min_qty":12.0}"#).unwrap();
        assert_eq!(hidden.label_mode, BubbleLabelMode::None);
        assert_eq!(shown.label_mode, BubbleLabelMode::All);
        assert_eq!(hidden.threshold_mode, BubbleThresholdMode::Fixed);
        assert_eq!(hidden.min_qty, 12.0);
        assert_eq!(
            VolumeBubbleConfig::default().preset,
            VolumeBubblePreset::Balanced
        );
    }

    #[test]
    fn balanced_defaults_match_the_product_contract() {
        let config = VolumeBubbleConfig::default();
        assert_eq!(config.threshold_mode, BubbleThresholdMode::Hybrid);
        assert_eq!(config.adaptive_window_minutes, 20);
        assert_eq!(config.display_percentile, 95.0);
        assert_eq!(config.label_percentile, 99.0);
        assert_eq!(config.cluster_window_ms, 500);
        assert_eq!(config.cluster_price_ticks, 1);
        assert_eq!(config.max_bubbles_per_bar, 3);
        assert_eq!(config.max_bubbles_in_view, 40);
        assert_eq!(config.max_labels_in_view, 6);
        assert_eq!((config.min_radius_px, config.max_radius_px), (5.0, 16.0));
        assert!(config.fill_enabled);
        assert!(!config.three_dimensional);
        assert_eq!(config.label_mode, BubbleLabelMode::ExtremeOnly);
        assert_eq!(config.color_mode, BubbleColorMode::Delta);
        assert!(config.age_fading && config.use_raw_trades_when_available);
        assert!(!config.price_response_enabled);
        assert_eq!(config.border_opacity, 0.90);
        assert_eq!(config.hover_opacity, 0.26);
    }

    #[test]
    fn three_dimensional_bubbles_round_trip_and_default_off() {
        let legacy: VolumeBubbleConfig = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert!(!legacy.three_dimensional);

        let configured = VolumeBubbleConfig {
            three_dimensional: true,
            ..VolumeBubbleConfig::default()
        };
        let json = serde_json::to_string(&configured).unwrap();
        let decoded: VolumeBubbleConfig = serde_json::from_str(&json).unwrap();
        assert!(decoded.three_dimensional);
    }

    #[test]
    fn fixed_range_volume_profile_settings_are_backward_compatible() {
        let mut legacy = serde_json::to_value(drawing::DrawingStyle::default()).unwrap();
        legacy
            .as_object_mut()
            .unwrap()
            .remove("fixed_range_volume_profile");
        let decoded: drawing::DrawingStyle = serde_json::from_value(legacy).unwrap();
        assert_eq!(
            decoded.fixed_range_volume_profile,
            drawing::FixedRangeVolumeProfileConfig::default()
        );
        assert_eq!(decoded.fixed_range_volume_profile.width_percent, 35.0);
        assert!(decoded.fixed_range_volume_profile.show_poc);
    }
}
