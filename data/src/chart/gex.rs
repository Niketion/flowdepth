use exchange::{
    TickerInfo, UnixMs,
    options::{
        OptionContractMatchKey, OptionRight, OptionsProvider, OptionsUnderlying,
        RawOptionChainSnapshot, RawOptionContractSnapshot,
        derive::{DeriveMakerSide, DeriveMakerTrade},
        gex_monitor::GexProxyHistoryPoint,
        quantwheel::QuantWheelGexSnapshot,
    },
};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use std::{cmp::Ordering, collections::BTreeMap, f64::consts::PI, sync::Arc};

const MILLIS_PER_DAY: u64 = 86_400_000;
const MILLIS_PER_YEAR: f64 = 365.25 * MILLIS_PER_DAY as f64;
const MAX_VOLATILITY: f64 = 10.0;
const MIN_DENOMINATOR: f64 = 1.0e-12;
const DEFAULT_FLIP_RANGE_PERCENT: f64 = 30.0;
const FLIP_SCAN_STEPS: usize = 240;
const FLIP_BISECTION_STEPS: usize = 60;
pub const DEFAULT_SCENARIO_POINTS: usize = 512;
pub const NATIVE_GAMMA_MAX_AGE_MS: u64 = 90_000;

macro_rules! display_enum {
    ($name:ident, $($variant:ident => $label:literal),+ $(,)?) => {
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(match self { $(Self::$variant => $label),+ })
            }
        }
    };
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum GexScenarioResolution {
    Samples128,
    Samples256,
    #[default]
    #[serde(alias = "Auto")]
    Samples512,
}
impl GexScenarioResolution {
    pub const fn samples(self) -> usize {
        match self {
            Self::Samples128 => 128,
            Self::Samples256 => 256,
            Self::Samples512 => 512,
        }
    }
}
display_enum!(GexScenarioResolution, Samples128 => "128 samples", Samples256 => "256 samples", Samples512 => "512 samples");

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum GexGammaSource {
    BlackScholesDerived,
    #[default]
    ProviderNativePreferred,
}
impl GexGammaSource {
    pub const ALL: [Self; 2] = [Self::ProviderNativePreferred, Self::BlackScholesDerived];
}
display_enum!(GexGammaSource, BlackScholesDerived => "Black-Scholes derived", ProviderNativePreferred => "Provider native preferred");

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum GexGammaProvenance {
    Native,
    #[default]
    Derived,
    Mixed,
}
display_enum!(GexGammaProvenance, Native => "Provider native", Derived => "Black-Scholes derived", Mixed => "Mixed native / derived");

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum GexSignModel {
    AbsoluteGamma,
    #[default]
    CallPutOiProxy,
}

impl std::fmt::Display for GexSignModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AbsoluteGamma => f.write_str("Absolute Gamma"),
            Self::CallPutOiProxy => f.write_str("GEX OI Proxy"),
        }
    }
}

impl GexSignModel {
    pub const ALL: [Self; 2] = [Self::CallPutOiProxy, Self::AbsoluteGamma];
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum GexExpiryFilter {
    NextExpiry,
    OneDay,
    TwoDays,
    ThreeDays,
    #[default]
    SevenDays,
    ThirtyDays,
    All,
}

impl GexExpiryFilter {
    pub const ALL: [Self; 7] = [
        Self::NextExpiry,
        Self::OneDay,
        Self::TwoDays,
        Self::ThreeDays,
        Self::SevenDays,
        Self::ThirtyDays,
        Self::All,
    ];
}

impl std::fmt::Display for GexExpiryFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NextExpiry => f.write_str("Next expiry"),
            Self::OneDay => f.write_str("Next 1 day"),
            Self::TwoDays => f.write_str("Next 2 days"),
            Self::ThreeDays => f.write_str("Next 3 days"),
            Self::SevenDays => f.write_str("Next 7 days"),
            Self::ThirtyDays => f.write_str("Next 30 days"),
            Self::All => f.write_str("All expiries"),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum GexLevelColor {
    Cyan,
    Magenta,
    Primary,
    Success,
    Danger,
    #[default]
    Warning,
    Secondary,
}

impl GexLevelColor {
    pub const ALL: [Self; 7] = [
        Self::Cyan,
        Self::Magenta,
        Self::Primary,
        Self::Success,
        Self::Danger,
        Self::Warning,
        Self::Secondary,
    ];
}

impl std::fmt::Display for GexLevelColor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Cyan => "Cyan",
            Self::Magenta => "Red / magenta",
            Self::Primary => "Primary",
            Self::Success => "Success",
            Self::Danger => "Danger",
            Self::Warning => "Warning",
            Self::Secondary => "Secondary",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct GexLevelsConfig {
    pub expiry_filter: GexExpiryFilter,
    pub gamma_source: GexGammaSource,
    pub minimum_open_interest: f64,
    pub minimum_absolute_gex: f64,
    pub positive_color: GexLevelColor,
    pub negative_color: GexLevelColor,
    pub minimum_zone_strength: f32,
    /// Maximum relative price distance used only when rendering consecutive levels.
    pub level_match_tolerance_percent: f32,
    pub max_positive_zones: u8,
    pub max_negative_zones: u8,
    pub history_minutes: u16,
    pub persistent_lookback_minutes: u16,
    pub fade_buckets: u8,
    pub show_historical_zones: bool,
    pub show_active_projection: bool,
    pub show_current_profile: bool,
    pub current_profile_width_percent: f32,
    pub show_gamma_flip_marker: bool,
    pub show_call_wall_marker: bool,
    pub show_put_wall_marker: bool,
    pub show_hover_tooltip: bool,
}

impl Default for GexLevelsConfig {
    fn default() -> Self {
        Self {
            expiry_filter: GexExpiryFilter::SevenDays,
            gamma_source: GexGammaSource::ProviderNativePreferred,
            minimum_open_interest: 0.0,
            minimum_absolute_gex: 0.0,
            positive_color: GexLevelColor::Cyan,
            negative_color: GexLevelColor::Magenta,
            minimum_zone_strength: 0.12,
            level_match_tolerance_percent: 0.10,
            max_positive_zones: 6,
            max_negative_zones: 6,
            history_minutes: 1440,
            persistent_lookback_minutes: 15,
            fade_buckets: 3,
            show_historical_zones: true,
            show_active_projection: true,
            show_current_profile: true,
            current_profile_width_percent: 5.0,
            show_gamma_flip_marker: true,
            show_call_wall_marker: true,
            show_put_wall_marker: true,
            show_hover_tooltip: true,
        }
    }
}

impl GexLevelsConfig {
    pub fn migrate_legacy_defaults(&mut self) {
        if self.history_minutes == 240 {
            self.history_minutes = 1440;
        }
        self.minimum_zone_strength = self.minimum_zone_strength.clamp(0.01, 1.0);
        self.level_match_tolerance_percent = self.level_match_tolerance_percent.clamp(0.0, 1.0);
        self.max_positive_zones = self.max_positive_zones.clamp(1, 6);
        self.max_negative_zones = self.max_negative_zones.clamp(1, 6);
        self.fade_buckets = self.fade_buckets.clamp(1, 3);
        self.current_profile_width_percent = self.current_profile_width_percent.clamp(4.0, 7.0);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub sign_model: GexSignModel,
    pub expiry_filter: GexExpiryFilter,
    pub gamma_source: GexGammaSource,
    pub scenario_resolution: GexScenarioResolution,
    pub max_native_gamma_instruments: usize,
    pub min_open_interest: f64,
    pub min_absolute_gex: f64,
    pub max_visible_strikes: usize,
    pub price_range_percent: f64,
    pub show_call_gex: bool,
    pub show_put_gex: bool,
    pub show_net_gex: bool,
    pub show_absolute_gamma: bool,
    pub show_current_price: bool,
    pub show_call_wall: bool,
    pub show_put_wall: bool,
    pub show_gamma_flip: bool,
    pub show_summary: bool,
    pub show_header_net_gex: bool,
    pub show_header_absolute_gex: bool,
    pub show_header_gamma_flip: bool,
    pub show_header_call_wall: bool,
    pub show_header_put_wall: bool,
    pub show_header_expiry: bool,
    pub show_header_freshness: bool,
    pub show_header_snapshot: bool,
    pub show_header_model: bool,
    pub show_header_derive_flow: bool,
    pub show_intrinsic_stress_panel: bool,
    pub show_gamma_vega_panel: bool,
    pub show_gamma_liquidity_panel: bool,
    pub liquidity_depth_bps: f32,
    pub liquidity_reference_follow_link_group: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sign_model: GexSignModel::CallPutOiProxy,
            expiry_filter: GexExpiryFilter::SevenDays,
            gamma_source: GexGammaSource::ProviderNativePreferred,
            scenario_resolution: GexScenarioResolution::Samples512,
            max_native_gamma_instruments: 128,
            min_open_interest: 0.0,
            min_absolute_gex: 0.0,
            max_visible_strikes: 40,
            price_range_percent: 15.0,
            show_call_gex: true,
            show_put_gex: true,
            show_net_gex: true,
            show_absolute_gamma: false,
            show_current_price: true,
            show_call_wall: true,
            show_put_wall: true,
            show_gamma_flip: true,
            show_summary: true,
            show_header_net_gex: true,
            show_header_absolute_gex: false,
            show_header_gamma_flip: true,
            show_header_call_wall: false,
            show_header_put_wall: false,
            show_header_expiry: true,
            show_header_freshness: true,
            show_header_snapshot: false,
            show_header_model: true,
            show_header_derive_flow: false,
            show_intrinsic_stress_panel: true,
            show_gamma_vega_panel: true,
            show_gamma_liquidity_panel: true,
            liquidity_depth_bps: 25.0,
            liquidity_reference_follow_link_group: true,
        }
    }
}

pub const INTRINSIC_STRESS_MILD_RATIO: f64 = 0.02;
pub const INTRINSIC_STRESS_ELEVATED_RATIO: f64 = 0.05;
pub const INTRINSIC_STRESS_HIGH_RATIO: f64 = 0.10;
pub const GAMMA_VEGA_BALANCED_LOW: f64 = 0.80;
pub const GAMMA_VEGA_BALANCED_HIGH: f64 = 1.25;
pub const GAMMA_LIQUIDITY_MODERATE_RATIO: f64 = 0.25;
pub const GAMMA_LIQUIDITY_ELEVATED_RATIO: f64 = 0.75;
pub const GAMMA_LIQUIDITY_HIGH_RATIO: f64 = 1.50;
pub const GEX_PROXY_BALANCED_SHARE: f64 = 0.05;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum IntrinsicStressLevel {
    Low,
    #[default]
    Mild,
    Elevated,
    High,
}

impl std::fmt::Display for IntrinsicStressLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Low => "Low",
            Self::Mild => "Mild",
            Self::Elevated => "Elevated",
            Self::High => "High",
        })
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum GammaVegaRegime {
    VegaDominant,
    #[default]
    Balanced,
    GammaDominant,
    Unavailable,
}

impl std::fmt::Display for GammaVegaRegime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::VegaDominant => "Vega dominant",
            Self::Balanced => "Balanced",
            Self::GammaDominant => "Gamma dominant",
            Self::Unavailable => "Unavailable",
        })
    }
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
pub struct IntrinsicStressMetrics {
    pub gross_intrinsic_usd: f64,
    pub total_oi_notional_usd: f64,
    pub intrinsic_ratio: f64,
    pub itm_contracts: usize,
    pub total_contracts: usize,
    pub level: IntrinsicStressLevel,
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
pub struct GammaVegaMetrics {
    pub gamma_shock_1pct_usd: f64,
    pub vega_shock_1vol_usd: f64,
    pub gamma_vega_ratio: Option<f64>,
    pub regime: GammaVegaRegime,
    pub top_gamma_expiry: Option<UnixMs>,
    pub top_vega_expiry: Option<UnixMs>,
    pub valid_contracts: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum GammaLiquidityRegime {
    LowImpact,
    Moderate,
    Elevated,
    HighImpact,
    #[default]
    Unavailable,
}

impl std::fmt::Display for GammaLiquidityRegime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::LowImpact => "Low impact",
            Self::Moderate => "Moderate",
            Self::Elevated => "Elevated",
            Self::HighImpact => "High impact",
            Self::Unavailable => "Unavailable",
        })
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum GexProxyDirection {
    Positive,
    Negative,
    Balanced,
    #[default]
    NotApplicable,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GexLiquidityMetrics {
    pub reference_ticker: TickerInfo,
    pub observed_at: UnixMs,
    pub mid_price: f64,
    pub spread_bps: f64,
    pub bid_depth_usd: f64,
    pub ask_depth_usd: f64,
    pub effective_liquidity_usd: f64,
    pub gamma_exposure_usd: f64,
    pub impact_ratio: f64,
    pub regime: GammaLiquidityRegime,
    pub proxy_direction: GexProxyDirection,
    pub depth_range_bps: f64,
}

pub fn gamma_liquidity_regime(ratio: f64) -> GammaLiquidityRegime {
    if !ratio.is_finite() || ratio < 0.0 {
        GammaLiquidityRegime::Unavailable
    } else if ratio < GAMMA_LIQUIDITY_MODERATE_RATIO {
        GammaLiquidityRegime::LowImpact
    } else if ratio < GAMMA_LIQUIDITY_ELEVATED_RATIO {
        GammaLiquidityRegime::Moderate
    } else if ratio < GAMMA_LIQUIDITY_HIGH_RATIO {
        GammaLiquidityRegime::Elevated
    } else {
        GammaLiquidityRegime::HighImpact
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct GexStrike {
    pub strike: f64,
    pub call_gex_1pct: f64,
    pub put_gex_1pct: f64,
    pub net_gex_1pct: f64,
    pub absolute_gamma_1pct: f64,
    pub call_open_interest: f64,
    pub put_open_interest: f64,
    pub expiration_count: usize,
    #[serde(default)]
    pub gamma_provenance: GexGammaProvenance,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct GexExpiryStrike {
    pub expiration: UnixMs,
    pub strike: f64,
    pub call_gex_1pct: f64,
    pub put_gex_1pct: f64,
    pub net_gex_1pct: f64,
    pub absolute_gamma_1pct: f64,
    pub call_open_interest: f64,
    pub put_open_interest: f64,
    #[serde(default)]
    pub gamma_provenance: GexGammaProvenance,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct GexScenarioPoint {
    pub price: f64,
    pub net_gex_1pct: f64,
    pub absolute_gex_1pct: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct GexProxyStrikeMapping {
    pub source_strike: f64,
    pub mapped_strike: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct GexProxyMetadata {
    pub source_symbol: String,
    pub target_symbol: String,
    pub source_spot: f64,
    pub target_spot: f64,
    /// Timestamped target anchors are not available from every chart feed yet.
    /// Keeping the field on the immutable mapping records the distinction and
    /// allows those feeds to opt in without remapping historical observations.
    #[serde(default)]
    pub target_observed_at: Option<UnixMs>,
    /// Concrete QuantWheel expirations used for this immutable observation.
    #[serde(default)]
    pub resolved_expirations: Arc<[String]>,
    /// Exact source-to-target strike coordinates for this immutable mapping.
    #[serde(default)]
    pub strike_mappings: Arc<[GexProxyStrikeMapping]>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct GexSnapshot {
    pub provider: OptionsProvider,
    pub underlying: OptionsUnderlying,
    pub model: GexSignModel,
    #[serde(default)]
    pub expiry_filter: GexExpiryFilter,
    #[serde(default)]
    pub gamma_source: GexGammaSource,
    #[serde(default)]
    pub gamma_provenance: GexGammaProvenance,
    pub source_spot: f64,
    pub observed_at: UnixMs,
    pub calculated_at: UnixMs,
    pub net_gex_1pct: Option<f64>,
    pub absolute_gex_1pct: f64,
    pub call_wall: Option<f64>,
    pub put_wall: Option<f64>,
    pub gamma_flip: Option<f64>,
    #[serde(default)]
    pub intrinsic_stress: IntrinsicStressMetrics,
    #[serde(default)]
    pub gamma_vega: GammaVegaMetrics,
    pub strikes: Arc<[GexStrike]>,
    #[serde(default)]
    pub expiry_strikes: Arc<[GexExpiryStrike]>,
    #[serde(default)]
    pub scenario_curve: Arc<[GexScenarioPoint]>,
    #[serde(default)]
    pub scale_p95: f64,
    #[serde(default)]
    pub proxy: Option<GexProxyMetadata>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum ObservedGammaDirection {
    LongGamma,
    ShortGamma,
    Balanced,
    #[default]
    Unavailable,
}

impl std::fmt::Display for ObservedGammaDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::LongGamma => "Long Γ",
            Self::ShortGamma => "Short Γ",
            Self::Balanced => "Balanced",
            Self::Unavailable => "Unavailable",
        })
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum FlowQuality {
    #[default]
    Low,
    Medium,
    High,
}

impl std::fmt::Display for FlowQuality {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
        })
    }
}

impl std::fmt::Display for OiProxyAgreement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Agree => "Agree",
            Self::Diverge => "Diverge",
            Self::Insufficient => "Insufficient",
        })
    }
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize, Serialize)]
pub struct MakerGammaFlowWindow {
    pub lookback_minutes: u16,
    pub signed_gamma_flow_1pct: f64,
    pub gross_gamma_flow_1pct: f64,
    pub imbalance: f64,
    pub matched_deribit_gex_share: f64,
    pub trade_count: usize,
    pub direction: ObservedGammaDirection,
    pub quality: FlowQuality,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct DeriveMakerGammaFlow {
    pub observed_at: UnixMs,
    pub five_minutes: MakerGammaFlowWindow,
    pub thirty_minutes: MakerGammaFlowWindow,
    pub two_hours: MakerGammaFlowWindow,
    #[serde(default)]
    pub oi_proxy_comparison_30m: MakerGammaFlowWindow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OiProxyAgreement {
    Agree,
    Diverge,
    Insufficient,
}

pub fn oi_proxy_agreement(
    snapshot: &GexSnapshot,
    flow: Option<&DeriveMakerGammaFlow>,
) -> OiProxyAgreement {
    let Some(flow) = flow else {
        return OiProxyAgreement::Insufficient;
    };
    let window = &flow.oi_proxy_comparison_30m;
    if window.quality == FlowQuality::Low
        || snapshot.absolute_gex_1pct <= 0.0
        || !snapshot.absolute_gex_1pct.is_finite()
    {
        return OiProxyAgreement::Insufficient;
    }
    let Some(net) = snapshot.net_gex_1pct.filter(|value| value.is_finite()) else {
        return OiProxyAgreement::Insufficient;
    };
    if net.abs() / snapshot.absolute_gex_1pct < 0.10
        || matches!(
            window.direction,
            ObservedGammaDirection::Balanced | ObservedGammaDirection::Unavailable
        )
    {
        return OiProxyAgreement::Insufficient;
    }
    let same_sign = matches!(
        (net.is_sign_positive(), window.direction),
        (true, ObservedGammaDirection::LongGamma) | (false, ObservedGammaDirection::ShortGamma)
    );
    if same_sign {
        OiProxyAgreement::Agree
    } else {
        OiProxyAgreement::Diverge
    }
}

pub type GexHeatmapSnapshot = GexSnapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
pub enum GexZoneSign {
    Positive,
    Negative,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub enum GexZoneState {
    #[default]
    Active,
    Fading,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct GexZoneBand {
    /// Immutable source coordinate inherited from a proxy-mapped strike.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_strike: Option<f64>,
    pub strike: f64,
    pub lower_price: f64,
    pub upper_price: f64,
    pub normalized_strength: f32,
    pub net_gex_1pct: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct GexZone {
    pub id: u64,
    pub observed_at: UnixMs,
    pub lower_price: f64,
    pub upper_price: f64,
    pub peak_price: f64,
    pub net_gex_1pct: f64,
    pub absolute_gex_1pct: f64,
    pub normalized_strength: f32,
    pub persistence_score: f32,
    pub sign: GexZoneSign,
    pub dominant_expiry: Option<UnixMs>,
    pub gamma_provenance: GexGammaProvenance,
    #[serde(default)]
    pub state: GexZoneState,
    #[serde(default)]
    pub missing_buckets: u8,
    #[serde(default)]
    pub bands: Arc<[GexZoneBand]>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct GexZoneFrame {
    pub bucket_start: UnixMs,
    /// QuantWheel frames end at the next exact observation. `None` is the
    /// semantically open final observation. Deribit frames always store their
    /// existing candle-bucket end here.
    #[serde(default)]
    pub interval_end: Option<UnixMs>,
    pub source_spot: f64,
    pub zones: Arc<[GexZone]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GexProxyZoneRole {
    PositivePrimary,
    PositiveSecondary,
    NegativePrimary,
    NegativeSecondary,
}

impl GexProxyZoneRole {
    pub const ALL: [Self; 4] = [
        Self::PositivePrimary,
        Self::PositiveSecondary,
        Self::NegativePrimary,
        Self::NegativeSecondary,
    ];

    pub const fn weight(self) -> f32 {
        match self {
            Self::PositivePrimary | Self::NegativePrimary => 1.0,
            Self::PositiveSecondary | Self::NegativeSecondary => 0.62,
        }
    }

    fn level(self, point: &GexProxyHistoryPoint) -> Option<f64> {
        match self {
            Self::PositivePrimary => point.positive_level_1,
            Self::PositiveSecondary => point.positive_level_2,
            Self::NegativePrimary => point.negative_level_1,
            Self::NegativeSecondary => point.negative_level_2,
        }
        .filter(|level| level.is_finite() && *level > 0.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GexProxyZone {
    pub role: GexProxyZoneRole,
    pub center_price: f64,
    pub lower_price: f64,
    pub upper_price: f64,
    pub strength: f32,
    pub wall_confirmed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GexProxyZoneFrame {
    pub bucket_start: UnixMs,
    pub bucket_end: UnixMs,
    pub observed_at: UnixMs,
    pub source_spot: f64,
    pub total_gex: f64,
    pub flip_level: Option<f64>,
    pub zones: Arc<[GexProxyZone]>,
}

impl GexZone {
    pub fn is_semantically_valid(&self) -> bool {
        self.id != 0
            && self.observed_at.as_u64() > 0
            && self.lower_price.is_finite()
            && self.upper_price.is_finite()
            && self.peak_price.is_finite()
            && self.lower_price > 0.0
            && self.lower_price <= self.peak_price
            && self.peak_price <= self.upper_price
            && self.net_gex_1pct.is_finite()
            && self.absolute_gex_1pct.is_finite()
            && self.absolute_gex_1pct >= 0.0
            && self.normalized_strength.is_finite()
            && (0.0..=1.0).contains(&self.normalized_strength)
            && self.persistence_score.is_finite()
            && (0.0..=1.0).contains(&self.persistence_score)
            && self.bands.iter().all(|band| {
                band.source_strike
                    .is_none_or(|strike| strike.is_finite() && strike > 0.0)
                    && band.strike.is_finite()
                    && band.lower_price.is_finite()
                    && band.upper_price.is_finite()
                    && band.lower_price > 0.0
                    && band.lower_price <= band.strike
                    && band.strike <= band.upper_price
                    && band.normalized_strength.is_finite()
                    && (0.0..=1.0).contains(&band.normalized_strength)
                    && band.net_gex_1pct.is_finite()
            })
    }
}

impl GexZoneFrame {
    pub fn is_semantically_valid(&self) -> bool {
        self.bucket_start.as_u64() > 0
            && self.source_spot.is_finite()
            && self.source_spot > 0.0
            && self.zones.iter().all(GexZone::is_semantically_valid)
    }
}

impl GexSnapshot {
    pub fn is_semantically_valid(&self) -> bool {
        self.source_spot.is_finite()
            && self.source_spot > 0.0
            && self.observed_at.as_u64() > 0
            && self.calculated_at.as_u64() > 0
            && self.strikes.iter().all(|strike| {
                strike.strike.is_finite()
                    && strike.strike > 0.0
                    && [
                        strike.call_gex_1pct,
                        strike.put_gex_1pct,
                        strike.net_gex_1pct,
                        strike.absolute_gamma_1pct,
                        strike.call_open_interest,
                        strike.put_open_interest,
                    ]
                    .iter()
                    .all(|value| value.is_finite())
            })
            && self.expiry_strikes.iter().all(|value| {
                value.expiration.as_u64() > 0
                    && value.strike.is_finite()
                    && value.strike > 0.0
                    && [
                        value.call_gex_1pct,
                        value.put_gex_1pct,
                        value.net_gex_1pct,
                        value.absolute_gamma_1pct,
                        value.call_open_interest,
                        value.put_open_interest,
                    ]
                    .iter()
                    .all(|number| number.is_finite())
            })
            && self
                .scenario_curve
                .windows(2)
                .all(|pair| pair[0].price < pair[1].price)
            && self.scenario_curve.iter().all(|point| {
                point.price.is_finite()
                    && point.price > 0.0
                    && point.net_gex_1pct.is_finite()
                    && point.absolute_gex_1pct.is_finite()
            })
            && self.proxy.as_ref().is_none_or(|proxy| {
                !proxy.source_symbol.is_empty()
                    && !proxy.target_symbol.is_empty()
                    && proxy.source_spot.is_finite()
                    && proxy.source_spot > 0.0
                    && proxy.target_spot.is_finite()
                    && proxy.target_spot > 0.0
                    && proxy.strike_mappings.iter().all(|mapping| {
                        mapping.source_strike.is_finite()
                            && mapping.source_strike > 0.0
                            && mapping.mapped_strike.is_finite()
                            && mapping.mapped_strike > 0.0
                    })
            })
    }
}

pub fn map_proxy_price(source_price: f64, source_spot: f64, target_spot: f64) -> Option<f64> {
    if !source_price.is_finite()
        || !source_spot.is_finite()
        || source_spot <= 0.0
        || !target_spot.is_finite()
        || target_spot <= 0.0
    {
        return None;
    }
    let mapped = source_price * (target_spot / source_spot);
    (mapped.is_finite() && mapped > 0.0).then_some(mapped)
}

pub fn map_proxy_snapshot(
    source: &GexSnapshot,
    source_symbol: &str,
    target_symbol: &str,
    target_spot: f64,
) -> Option<GexSnapshot> {
    let source_spot = source.source_spot;
    let map = |price| map_proxy_price(price, source_spot, target_spot);
    let mut mapped = source.clone();
    mapped.source_spot = target_spot;
    mapped.call_wall = match source.call_wall {
        Some(price) => Some(map(price)?),
        None => None,
    };
    mapped.put_wall = match source.put_wall {
        Some(price) => Some(map(price)?),
        None => None,
    };
    mapped.gamma_flip = match source.gamma_flip {
        Some(price) => Some(map(price)?),
        None => None,
    };
    mapped.strikes = source
        .strikes
        .iter()
        .cloned()
        .map(|mut strike| {
            strike.strike = map(strike.strike)?;
            Some(strike)
        })
        .collect::<Option<Vec<_>>>()?
        .into();
    mapped.expiry_strikes = source
        .expiry_strikes
        .iter()
        .cloned()
        .map(|mut strike| {
            strike.strike = map(strike.strike)?;
            Some(strike)
        })
        .collect::<Option<Vec<_>>>()?
        .into();
    mapped.scenario_curve = source
        .scenario_curve
        .iter()
        .cloned()
        .map(|mut point| {
            point.price = map(point.price)?;
            Some(point)
        })
        .collect::<Option<Vec<_>>>()?
        .into();
    mapped.proxy = Some(GexProxyMetadata {
        source_symbol: source_symbol.to_owned(),
        target_symbol: target_symbol.to_owned(),
        source_spot,
        target_spot,
        target_observed_at: None,
        resolved_expirations: Arc::from([]),
        strike_mappings: source
            .strikes
            .iter()
            .zip(mapped.strikes.iter())
            .map(|(source, mapped)| GexProxyStrikeMapping {
                source_strike: source.strike,
                mapped_strike: mapped.strike,
            })
            .collect::<Vec<_>>()
            .into(),
    });
    Some(mapped)
}

pub fn quantwheel_snapshot(
    source: QuantWheelGexSnapshot,
    expiry_filter: GexExpiryFilter,
    expiration_count: usize,
    calculated_at: UnixMs,
) -> GexSnapshot {
    let strikes = source
        .levels
        .iter()
        .map(|level| GexStrike {
            strike: level.strike,
            call_gex_1pct: level.call_gex,
            put_gex_1pct: -level.put_gex.abs(),
            net_gex_1pct: level.net_gex,
            absolute_gamma_1pct: level.call_gex.abs() + level.put_gex.abs(),
            call_open_interest: level.call_open_interest,
            put_open_interest: level.put_open_interest,
            expiration_count,
            gamma_provenance: GexGammaProvenance::Native,
        })
        .collect::<Vec<_>>();
    let absolute_gex_1pct = strikes
        .iter()
        .map(|strike| strike.absolute_gamma_1pct)
        .sum();
    let scale_p95 =
        gex_percentile_95(strikes.iter().map(|strike| strike.net_gex_1pct.abs())).unwrap_or(0.0);
    GexSnapshot {
        provider: source.provider,
        underlying: source.underlying,
        model: GexSignModel::CallPutOiProxy,
        expiry_filter,
        gamma_source: GexGammaSource::ProviderNativePreferred,
        gamma_provenance: GexGammaProvenance::Native,
        source_spot: source.stock_price,
        observed_at: source.observed_at,
        calculated_at,
        net_gex_1pct: Some(source.total_gex),
        absolute_gex_1pct,
        call_wall: source.call_wall.map(|wall| wall.strike),
        put_wall: source.put_wall.map(|wall| wall.strike),
        gamma_flip: source.gamma_inflection,
        intrinsic_stress: IntrinsicStressMetrics::default(),
        gamma_vega: GammaVegaMetrics::default(),
        strikes: strikes.into(),
        expiry_strikes: Arc::default(),
        scenario_curve: Arc::default(),
        scale_p95,
        proxy: None,
    }
}

#[cfg(test)]
mod proxy_mapping_tests {
    use super::*;

    fn snapshot() -> GexSnapshot {
        GexSnapshot {
            provider: OptionsProvider::QuantWheel,
            underlying: OptionsUnderlying::Gld,
            model: GexSignModel::CallPutOiProxy,
            expiry_filter: GexExpiryFilter::All,
            gamma_source: GexGammaSource::ProviderNativePreferred,
            gamma_provenance: GexGammaProvenance::Native,
            source_spot: 400.0,
            observed_at: UnixMs::new(1),
            calculated_at: UnixMs::new(1),
            net_gex_1pct: Some(9_689.0),
            absolute_gex_1pct: 10_000.0,
            call_wall: Some(410.0),
            put_wall: Some(390.0),
            gamma_flip: Some(402.0),
            intrinsic_stress: Default::default(),
            gamma_vega: Default::default(),
            strikes: Arc::from([GexStrike {
                strike: 405.0,
                call_gex_1pct: 11_413.0,
                put_gex_1pct: -1_724.0,
                net_gex_1pct: 9_689.0,
                absolute_gamma_1pct: 13_137.0,
                call_open_interest: 2_075.0,
                put_open_interest: 254.0,
                expiration_count: 1,
                gamma_provenance: GexGammaProvenance::Native,
            }]),
            expiry_strikes: Arc::from([GexExpiryStrike {
                expiration: UnixMs::new(2),
                strike: 405.0,
                call_gex_1pct: 10.0,
                put_gex_1pct: -2.0,
                net_gex_1pct: 8.0,
                absolute_gamma_1pct: 12.0,
                call_open_interest: 20.0,
                put_open_interest: 4.0,
                gamma_provenance: GexGammaProvenance::Native,
            }]),
            scenario_curve: Arc::from([GexScenarioPoint {
                price: 405.0,
                net_gex_1pct: 8.0,
                absolute_gex_1pct: 12.0,
            }]),
            scale_p95: 9_689.0,
            proxy: None,
        }
    }

    #[test]
    fn maps_gld_price_coordinates_and_preserves_gex_magnitudes() {
        assert_eq!(map_proxy_price(405.0, 400.0, 4_000.0), Some(4_050.0));
        let mapped =
            map_proxy_snapshot(&snapshot(), "GLD", "XAUTUSDT", 4_000.0).expect("valid mapping");
        assert_eq!(mapped.strikes[0].strike, 4_050.0);
        assert_eq!(mapped.proxy.as_ref().unwrap().strike_mappings.len(), 1);
        assert_eq!(
            mapped.proxy.as_ref().unwrap().strike_mappings[0].source_strike,
            405.0
        );
        assert_eq!(
            mapped.proxy.as_ref().unwrap().strike_mappings[0].mapped_strike,
            4_050.0
        );
        assert_eq!(mapped.call_wall, Some(4_100.0));
        assert_eq!(mapped.put_wall, Some(3_900.0));
        assert_eq!(mapped.gamma_flip, Some(4_020.0));
        assert_eq!(mapped.expiry_strikes[0].strike, 4_050.0);
        assert_eq!(mapped.scenario_curve[0].price, 4_050.0);
        assert_eq!(mapped.strikes[0].net_gex_1pct, 9_689.0);
        assert_eq!(mapped.net_gex_1pct, Some(9_689.0));
        assert_eq!(mapped.proxy.as_ref().unwrap().source_spot, 400.0);
    }

    #[test]
    fn rejects_zero_or_invalid_source_spot() {
        assert_eq!(map_proxy_price(405.0, 0.0, 4_000.0), None);
        assert_eq!(map_proxy_price(405.0, f64::NAN, 4_000.0), None);
        let mut invalid = snapshot();
        invalid.source_spot = 0.0;
        assert!(map_proxy_snapshot(&invalid, "GLD", "XAUTUSDT", 4_000.0).is_none());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GexFreshness {
    Loading,
    Fresh,
    Stale,
    Expired,
    Error,
}

#[derive(Default)]
struct StrikeAccumulator {
    strike: f64,
    call_gex: f64,
    put_gex_abs: f64,
    absolute: f64,
    call_oi: f64,
    put_oi: f64,
    expirations: FxHashSet<UnixMs>,
    native_gamma_count: usize,
    derived_gamma_count: usize,
}

pub fn normal_pdf(value: f64) -> Option<f64> {
    value
        .is_finite()
        .then(|| (-0.5 * value * value).exp() / (2.0 * PI).sqrt())
        .filter(|result| result.is_finite())
}

pub fn black_scholes_gamma(
    spot: f64,
    strike: f64,
    years_to_expiry: f64,
    interest_rate: f64,
    volatility: f64,
) -> Option<f64> {
    if ![spot, strike, years_to_expiry, interest_rate, volatility]
        .iter()
        .all(|value| value.is_finite())
        || spot <= 0.0
        || strike <= 0.0
        || years_to_expiry <= 0.0
        || volatility <= 0.0
        || volatility > MAX_VOLATILITY
    {
        return None;
    }
    let sqrt_time = years_to_expiry.sqrt();
    let denominator = volatility * sqrt_time;
    if denominator <= MIN_DENOMINATOR {
        return None;
    }
    let d1 = ((spot / strike).ln()
        + (interest_rate + 0.5 * volatility * volatility) * years_to_expiry)
        / denominator;
    let gamma_denominator = spot * denominator;
    if gamma_denominator <= MIN_DENOMINATOR {
        return None;
    }
    let gamma = normal_pdf(d1)? / gamma_denominator;
    (gamma.is_finite() && gamma >= 0.0).then_some(gamma)
}

pub fn black_scholes_vega(
    spot: f64,
    strike: f64,
    years_to_expiry: f64,
    interest_rate: f64,
    volatility: f64,
) -> Option<f64> {
    if ![spot, strike, years_to_expiry, interest_rate, volatility]
        .iter()
        .all(|value| value.is_finite())
        || spot <= 0.0
        || strike <= 0.0
        || years_to_expiry <= 0.0
        || volatility <= 0.0
        || volatility > MAX_VOLATILITY
    {
        return None;
    }
    let sqrt_time = years_to_expiry.sqrt();
    let denominator = volatility * sqrt_time;
    if denominator <= MIN_DENOMINATOR {
        return None;
    }
    let d1 = ((spot / strike).ln()
        + (interest_rate + 0.5 * volatility * volatility) * years_to_expiry)
        / denominator;
    let vega = spot * normal_pdf(d1)? * sqrt_time;
    (vega.is_finite() && vega >= 0.0).then_some(vega)
}

pub fn years_to_expiry(expiration: UnixMs, now: UnixMs) -> Option<f64> {
    expiration
        .as_u64()
        .checked_sub(now.as_u64())
        .map(|millis| millis as f64 / MILLIS_PER_YEAR)
        .filter(|years| years.is_finite() && *years > 0.0)
}

pub fn iv_percent_to_decimal(iv_percent: f64) -> Option<f64> {
    let volatility = iv_percent / 100.0;
    (iv_percent.is_finite() && volatility > 0.0 && volatility <= MAX_VOLATILITY)
        .then_some(volatility)
}

pub fn calculate_gex(chain: &RawOptionChainSnapshot, config: &Config) -> GexSnapshot {
    calculate_gex_at(chain, config, UnixMs::now())
}

pub fn calculate_gex_at(
    chain: &RawOptionChainSnapshot,
    config: &Config,
    calculated_at: UnixMs,
) -> GexSnapshot {
    let selected = select_contracts(
        chain,
        config.expiry_filter,
        config.min_open_interest,
        calculated_at,
    );
    let mut by_strike: FxHashMap<u64, StrikeAccumulator> = FxHashMap::default();
    let mut by_expiry_strike: FxHashMap<(UnixMs, u64), StrikeAccumulator> = FxHashMap::default();

    for contract in selected.iter().copied() {
        let oi = contract.market.open_interest_underlying;
        let Some((gex, provenance)) = current_contract_gex(
            contract,
            chain.source_spot,
            calculated_at,
            config.gamma_source,
        ) else {
            continue;
        };
        if gex < config.min_absolute_gex {
            continue;
        }
        let entry = by_strike
            .entry(contract.instrument.strike.to_bits())
            .or_insert_with(|| StrikeAccumulator {
                strike: contract.instrument.strike,
                ..StrikeAccumulator::default()
            });
        entry.absolute += gex;
        match provenance {
            GexGammaProvenance::Native => entry.native_gamma_count += 1,
            GexGammaProvenance::Derived => entry.derived_gamma_count += 1,
            GexGammaProvenance::Mixed => {}
        }
        entry
            .expirations
            .insert(contract.instrument.expiration_timestamp);
        match contract.instrument.right {
            OptionRight::Call => {
                entry.call_gex += gex;
                entry.call_oi += oi;
            }
            OptionRight::Put => {
                entry.put_gex_abs += gex;
                entry.put_oi += oi;
            }
        }
        let expiry_entry = by_expiry_strike
            .entry((
                contract.instrument.expiration_timestamp,
                contract.instrument.strike.to_bits(),
            ))
            .or_insert_with(|| StrikeAccumulator {
                strike: contract.instrument.strike,
                ..StrikeAccumulator::default()
            });
        expiry_entry.absolute += gex;
        match provenance {
            GexGammaProvenance::Native => expiry_entry.native_gamma_count += 1,
            GexGammaProvenance::Derived => expiry_entry.derived_gamma_count += 1,
            GexGammaProvenance::Mixed => {}
        }
        match contract.instrument.right {
            OptionRight::Call => {
                expiry_entry.call_gex += gex;
                expiry_entry.call_oi += oi;
            }
            OptionRight::Put => {
                expiry_entry.put_gex_abs += gex;
                expiry_entry.put_oi += oi;
            }
        }
    }

    let mut strikes = by_strike
        .into_values()
        .map(|entry| {
            let net = entry.call_gex - entry.put_gex_abs;
            GexStrike {
                strike: entry.strike,
                call_gex_1pct: entry.call_gex,
                put_gex_1pct: -entry.put_gex_abs,
                net_gex_1pct: net,
                absolute_gamma_1pct: entry.absolute,
                call_open_interest: entry.call_oi,
                put_open_interest: entry.put_oi,
                expiration_count: entry.expirations.len(),
                gamma_provenance: gamma_provenance(
                    entry.native_gamma_count,
                    entry.derived_gamma_count,
                ),
            }
        })
        .collect::<Vec<_>>();
    strikes.sort_by(|a, b| a.strike.partial_cmp(&b.strike).unwrap_or(Ordering::Equal));
    let mut expiry_strikes = by_expiry_strike
        .into_iter()
        .map(|((expiration, _), entry)| GexExpiryStrike {
            expiration,
            strike: entry.strike,
            call_gex_1pct: entry.call_gex,
            put_gex_1pct: -entry.put_gex_abs,
            net_gex_1pct: entry.call_gex - entry.put_gex_abs,
            absolute_gamma_1pct: entry.absolute,
            call_open_interest: entry.call_oi,
            put_open_interest: entry.put_oi,
            gamma_provenance: gamma_provenance(entry.native_gamma_count, entry.derived_gamma_count),
        })
        .collect::<Vec<_>>();
    expiry_strikes.sort_by(|a, b| {
        a.strike
            .total_cmp(&b.strike)
            .then_with(|| a.expiration.cmp(&b.expiration))
    });

    let absolute_gex_1pct = strikes
        .iter()
        .map(|strike| strike.absolute_gamma_1pct)
        .sum();
    let proxy_net = strikes.iter().map(|strike| strike.net_gex_1pct).sum();
    let call_wall = strikes
        .iter()
        .max_by(|a, b| {
            a.call_gex_1pct
                .partial_cmp(&b.call_gex_1pct)
                .unwrap_or(Ordering::Equal)
        })
        .filter(|strike| strike.call_gex_1pct > 0.0)
        .map(|strike| strike.strike);
    let put_wall = strikes
        .iter()
        .max_by(|a, b| {
            a.put_gex_1pct
                .abs()
                .partial_cmp(&b.put_gex_1pct.abs())
                .unwrap_or(Ordering::Equal)
        })
        .filter(|strike| strike.put_gex_1pct < 0.0)
        .map(|strike| strike.strike);
    let (scenario_curve, proxy_flip) = build_scenario_curve(
        &selected,
        chain.source_spot,
        calculated_at,
        config.price_range_percent,
        config.scenario_resolution.samples(),
    );
    let gamma_flip = (config.sign_model == GexSignModel::CallPutOiProxy)
        .then_some(proxy_flip)
        .flatten();
    let native_count = strikes
        .iter()
        .filter(|strike| strike.gamma_provenance == GexGammaProvenance::Native)
        .count();
    let derived_count = strikes
        .iter()
        .filter(|strike| strike.gamma_provenance == GexGammaProvenance::Derived)
        .count();
    let mixed_count = strikes.len().saturating_sub(native_count + derived_count);
    let scale_p95 = gex_percentile_95(strikes.iter().map(|strike| {
        if config.sign_model == GexSignModel::AbsoluteGamma {
            strike.absolute_gamma_1pct
        } else {
            strike.net_gex_1pct
        }
    }))
    .unwrap_or(0.0);
    let intrinsic_stress = calculate_intrinsic_stress(&selected, chain.source_spot);
    let gamma_vega = calculate_gamma_vega(
        &selected,
        chain.source_spot,
        calculated_at,
        absolute_gex_1pct,
    );

    GexSnapshot {
        provider: chain.provider,
        underlying: chain.underlying,
        model: config.sign_model,
        expiry_filter: config.expiry_filter,
        gamma_source: config.gamma_source,
        gamma_provenance: if mixed_count > 0 || (native_count > 0 && derived_count > 0) {
            GexGammaProvenance::Mixed
        } else {
            gamma_provenance(native_count, derived_count)
        },
        source_spot: chain.source_spot,
        observed_at: chain.observed_at,
        calculated_at,
        net_gex_1pct: (config.sign_model == GexSignModel::CallPutOiProxy).then_some(proxy_net),
        absolute_gex_1pct,
        call_wall,
        put_wall,
        gamma_flip,
        intrinsic_stress,
        gamma_vega,
        strikes: strikes.into(),
        expiry_strikes: expiry_strikes.into(),
        scenario_curve: scenario_curve.into(),
        scale_p95,
        proxy: None,
    }
}

fn select_contracts(
    chain: &RawOptionChainSnapshot,
    filter: GexExpiryFilter,
    min_open_interest: f64,
    now: UnixMs,
) -> Vec<&RawOptionContractSnapshot> {
    let next_expiry = chain
        .contracts
        .iter()
        .filter(|contract| contract.instrument.expiration_timestamp > now)
        .map(|contract| contract.instrument.expiration_timestamp)
        .min();
    let max_expiry = match filter {
        GexExpiryFilter::OneDay => Some(now.saturating_add(MILLIS_PER_DAY)),
        GexExpiryFilter::TwoDays => Some(now.saturating_add(2 * MILLIS_PER_DAY)),
        GexExpiryFilter::ThreeDays => Some(now.saturating_add(3 * MILLIS_PER_DAY)),
        GexExpiryFilter::SevenDays => Some(now.saturating_add(7 * MILLIS_PER_DAY)),
        GexExpiryFilter::ThirtyDays => Some(now.saturating_add(30 * MILLIS_PER_DAY)),
        GexExpiryFilter::NextExpiry | GexExpiryFilter::All => None,
    };
    chain
        .contracts
        .iter()
        .filter(|contract| {
            let expiration = contract.instrument.expiration_timestamp;
            let oi = contract.market.open_interest_underlying;
            if expiration <= now
                || !contract.instrument.strike.is_finite()
                || contract.instrument.strike <= 0.0
                || !oi.is_finite()
                || oi < 0.0
                || oi < min_open_interest.max(0.0)
            {
                return false;
            }
            match filter {
                GexExpiryFilter::NextExpiry => Some(expiration) == next_expiry,
                GexExpiryFilter::All => true,
                _ => max_expiry.is_some_and(|limit| expiration <= limit),
            }
        })
        .collect()
}

pub fn intrinsic_stress_level(ratio: f64) -> IntrinsicStressLevel {
    if !ratio.is_finite() || ratio < INTRINSIC_STRESS_MILD_RATIO {
        IntrinsicStressLevel::Low
    } else if ratio < INTRINSIC_STRESS_ELEVATED_RATIO {
        IntrinsicStressLevel::Mild
    } else if ratio < INTRINSIC_STRESS_HIGH_RATIO {
        IntrinsicStressLevel::Elevated
    } else {
        IntrinsicStressLevel::High
    }
}

fn calculate_intrinsic_stress(
    contracts: &[&RawOptionContractSnapshot],
    spot: f64,
) -> IntrinsicStressMetrics {
    if !spot.is_finite() || spot <= 0.0 {
        return IntrinsicStressMetrics {
            level: IntrinsicStressLevel::Low,
            ..IntrinsicStressMetrics::default()
        };
    }
    let mut gross_intrinsic_usd = 0.0;
    let mut total_oi_notional_usd = 0.0;
    let mut itm_contracts = 0;
    for contract in contracts {
        let oi = contract.market.open_interest_underlying;
        let intrinsic_per_unit = match contract.instrument.right {
            OptionRight::Call => (spot - contract.instrument.strike).max(0.0),
            OptionRight::Put => (contract.instrument.strike - spot).max(0.0),
        };
        if intrinsic_per_unit > 0.0 {
            itm_contracts += 1;
        }
        gross_intrinsic_usd += intrinsic_per_unit * oi;
        total_oi_notional_usd += oi * spot;
    }
    let gross_intrinsic_usd = finite_non_negative(gross_intrinsic_usd);
    let total_oi_notional_usd = finite_non_negative(total_oi_notional_usd);
    let intrinsic_ratio = if total_oi_notional_usd > MIN_DENOMINATOR {
        finite_non_negative(gross_intrinsic_usd / total_oi_notional_usd)
    } else {
        0.0
    };
    IntrinsicStressMetrics {
        gross_intrinsic_usd,
        total_oi_notional_usd,
        intrinsic_ratio,
        itm_contracts,
        total_contracts: contracts.len(),
        level: intrinsic_stress_level(intrinsic_ratio),
    }
}

pub fn gamma_vega_regime(ratio: Option<f64>) -> GammaVegaRegime {
    match ratio.filter(|value| value.is_finite() && *value >= 0.0) {
        Some(value) if value < GAMMA_VEGA_BALANCED_LOW => GammaVegaRegime::VegaDominant,
        Some(value) if value <= GAMMA_VEGA_BALANCED_HIGH => GammaVegaRegime::Balanced,
        Some(_) => GammaVegaRegime::GammaDominant,
        None => GammaVegaRegime::Unavailable,
    }
}

fn calculate_gamma_vega(
    contracts: &[&RawOptionContractSnapshot],
    spot: f64,
    now: UnixMs,
    gamma_shock_1pct_usd: f64,
) -> GammaVegaMetrics {
    let mut gamma_by_expiry: FxHashMap<UnixMs, f64> = FxHashMap::default();
    let mut vega_by_expiry: FxHashMap<UnixMs, f64> = FxHashMap::default();
    let mut vega_shock_1vol_usd = 0.0;
    let mut valid_contracts = 0;
    for contract in contracts {
        let Some(years) = years_to_expiry(contract.instrument.expiration_timestamp, now) else {
            continue;
        };
        let Some(volatility) = iv_percent_to_decimal(contract.market.mark_iv_percent) else {
            continue;
        };
        let Some(vega) = black_scholes_vega(
            spot,
            contract.instrument.strike,
            years,
            contract.market.interest_rate,
            volatility,
        ) else {
            continue;
        };
        let Some(gamma) = contract_gex(contract, spot, now) else {
            continue;
        };
        let vega_shock = vega * contract.market.open_interest_underlying * 0.01;
        if !vega_shock.is_finite() || vega_shock < 0.0 {
            continue;
        }
        let expiry = contract.instrument.expiration_timestamp;
        *gamma_by_expiry.entry(expiry).or_default() += gamma;
        *vega_by_expiry.entry(expiry).or_default() += vega_shock;
        vega_shock_1vol_usd += vega_shock;
        valid_contracts += 1;
    }
    let gamma_shock_1pct_usd = finite_non_negative(gamma_shock_1pct_usd);
    let vega_shock_1vol_usd = finite_non_negative(vega_shock_1vol_usd);
    let gamma_vega_ratio = (vega_shock_1vol_usd > MIN_DENOMINATOR)
        .then(|| gamma_shock_1pct_usd / vega_shock_1vol_usd)
        .filter(|value| value.is_finite() && *value >= 0.0);
    GammaVegaMetrics {
        gamma_shock_1pct_usd,
        vega_shock_1vol_usd,
        gamma_vega_ratio,
        regime: gamma_vega_regime(gamma_vega_ratio),
        top_gamma_expiry: largest_expiry_bucket(&gamma_by_expiry),
        top_vega_expiry: largest_expiry_bucket(&vega_by_expiry),
        valid_contracts,
    }
}

fn largest_expiry_bucket(values: &FxHashMap<UnixMs, f64>) -> Option<UnixMs> {
    values
        .iter()
        .filter(|(_, value)| value.is_finite())
        .max_by(|(expiry_a, value_a), (expiry_b, value_b)| {
            value_a
                .total_cmp(value_b)
                .then_with(|| expiry_b.cmp(expiry_a))
        })
        .map(|(expiry, _)| *expiry)
}

fn finite_non_negative(value: f64) -> f64 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn contract_gex(
    contract: &RawOptionContractSnapshot,
    spot: f64,
    calculated_at: UnixMs,
) -> Option<f64> {
    let years = years_to_expiry(contract.instrument.expiration_timestamp, calculated_at)?;
    let volatility = iv_percent_to_decimal(contract.market.mark_iv_percent)?;
    let gamma = black_scholes_gamma(
        spot,
        contract.instrument.strike,
        years,
        contract.market.interest_rate,
        volatility,
    )?;
    // Keep contract metadata explicit in the exposure formula. This also makes
    // the calculation valid for providers whose OI unit is not one underlying.
    let gex = gamma
        * contract.market.open_interest_underlying
        * contract.instrument.contract_size
        * spot
        * spot
        * 0.01;
    (gex.is_finite() && gex >= 0.0).then_some(gex)
}

fn current_contract_gex(
    contract: &RawOptionContractSnapshot,
    spot: f64,
    calculated_at: UnixMs,
    source: GexGammaSource,
) -> Option<(f64, GexGammaProvenance)> {
    if source == GexGammaSource::ProviderNativePreferred
        && let (Some(gamma), Some(observed_at)) = (
            contract.market.native_gamma,
            contract.market.native_gamma_observed_at,
        )
        && gamma.is_finite()
        && gamma >= 0.0
        && calculated_at.saturating_diff(observed_at) <= NATIVE_GAMMA_MAX_AGE_MS
    {
        let gex = gamma
            * contract.market.open_interest_underlying
            * contract.instrument.contract_size
            * spot
            * spot
            * 0.01;
        if gex.is_finite() && gex >= 0.0 {
            return Some((gex, GexGammaProvenance::Native));
        }
    }
    contract_gex(contract, spot, calculated_at).map(|gex| (gex, GexGammaProvenance::Derived))
}

/// Absolute gamma exposure of one contract for a one-percent spot move.
pub fn absolute_gamma_per_contract_1pct(gamma: f64, contract_size: f64, spot: f64) -> Option<f64> {
    if ![gamma, contract_size, spot]
        .iter()
        .all(|value| value.is_finite())
        || contract_size <= 0.0
        || spot <= 0.0
    {
        return None;
    }
    let value = gamma.abs() * contract_size * spot * spot * 0.01;
    (value.is_finite() && value >= 0.0).then_some(value)
}

pub fn calculate_derive_maker_gamma_flow(
    chain: &RawOptionChainSnapshot,
    trades: &[DeriveMakerTrade],
    config: &Config,
    observed_at: UnixMs,
) -> DeriveMakerGammaFlow {
    // Observed Derive flow is venue-independent evidence and must not disappear when the
    // Deribit visualization changes its expiry or minimum-OI filters. Matching still remains
    // exact, but it is performed against the complete non-expired Deribit chain.
    let selected = select_contracts(chain, GexExpiryFilter::All, 0.0, observed_at);
    let mut contracts: FxHashMap<OptionContractMatchKey, Vec<&RawOptionContractSnapshot>> =
        FxHashMap::default();
    let mut total_absolute_gex = 0.0;
    for contract in selected {
        let Some(key) = OptionContractMatchKey::new(
            chain.underlying,
            contract.instrument.expiration_timestamp,
            contract.instrument.strike,
            contract.instrument.right,
        ) else {
            continue;
        };
        if let Some((gex, _)) = current_contract_gex(
            contract,
            chain.source_spot,
            observed_at,
            config.gamma_source,
        ) {
            total_absolute_gex += gex;
        }
        contracts.entry(key).or_default().push(contract);
    }
    let calculate = |minutes| {
        calculate_flow_window(
            minutes,
            trades,
            &contracts,
            chain.source_spot,
            total_absolute_gex,
            config.gamma_source,
            observed_at,
        )
    };
    let comparison_selected = select_contracts(
        chain,
        config.expiry_filter,
        config.min_open_interest,
        observed_at,
    );
    let mut comparison_contracts: FxHashMap<
        OptionContractMatchKey,
        Vec<&RawOptionContractSnapshot>,
    > = FxHashMap::default();
    let mut comparison_absolute_gex = 0.0;
    for contract in comparison_selected {
        let Some(key) = OptionContractMatchKey::new(
            chain.underlying,
            contract.instrument.expiration_timestamp,
            contract.instrument.strike,
            contract.instrument.right,
        ) else {
            continue;
        };
        let Some((gex, _)) = current_contract_gex(
            contract,
            chain.source_spot,
            observed_at,
            config.gamma_source,
        ) else {
            continue;
        };
        if gex < config.min_absolute_gex {
            continue;
        }
        comparison_absolute_gex += gex;
        comparison_contracts.entry(key).or_default().push(contract);
    }
    DeriveMakerGammaFlow {
        observed_at,
        five_minutes: calculate(5),
        thirty_minutes: calculate(30),
        two_hours: calculate(120),
        oi_proxy_comparison_30m: calculate_flow_window(
            30,
            trades,
            &comparison_contracts,
            chain.source_spot,
            comparison_absolute_gex,
            config.gamma_source,
            observed_at,
        ),
    }
}

fn calculate_flow_window(
    lookback_minutes: u16,
    trades: &[DeriveMakerTrade],
    contracts: &FxHashMap<OptionContractMatchKey, Vec<&RawOptionContractSnapshot>>,
    spot: f64,
    total_absolute_gex: f64,
    gamma_source: GexGammaSource,
    observed_at: UnixMs,
) -> MakerGammaFlowWindow {
    const MAX_EXPIRY_DIFFERENCE_MS: u64 = 12 * 60 * 60 * 1_000;
    let cutoff = observed_at.saturating_sub(u64::from(lookback_minutes) * 60 * 1_000);
    let mut signed = 0.0;
    let mut gross = 0.0;
    let mut trade_count = 0usize;
    let mut matched_contracts = FxHashSet::default();
    for trade in trades
        .iter()
        .filter(|trade| trade.timestamp >= cutoff && trade.timestamp <= observed_at)
    {
        let Some(candidates) = contracts.get(&trade.key) else {
            continue;
        };
        let Some(contract) = candidates
            .iter()
            .copied()
            .filter(|contract| {
                contract
                    .instrument
                    .expiration_timestamp
                    .as_u64()
                    .abs_diff(trade.expiration_timestamp.as_u64())
                    <= MAX_EXPIRY_DIFFERENCE_MS
            })
            .min_by_key(|contract| {
                contract
                    .instrument
                    .expiration_timestamp
                    .as_u64()
                    .abs_diff(trade.expiration_timestamp.as_u64())
            })
        else {
            continue;
        };
        let gamma = current_contract_gamma(contract, spot, observed_at, gamma_source);
        let Some(per_contract) = gamma.and_then(|gamma| {
            absolute_gamma_per_contract_1pct(gamma, contract.instrument.contract_size, spot)
        }) else {
            continue;
        };
        let trade_gamma = per_contract * trade.amount;
        if !trade_gamma.is_finite() {
            continue;
        }
        signed += match trade.side {
            DeriveMakerSide::Buy => trade_gamma,
            DeriveMakerSide::Sell => -trade_gamma,
        };
        gross += trade_gamma.abs();
        trade_count += 1;
        matched_contracts.insert(contract.instrument.instrument_name.as_str());
    }
    let matched_gex = contracts
        .values()
        .flatten()
        .filter(|contract| matched_contracts.contains(contract.instrument.instrument_name.as_str()))
        .filter_map(|contract| {
            current_contract_gex(contract, spot, observed_at, gamma_source).map(|value| value.0)
        })
        .sum::<f64>();
    let matched_share = if total_absolute_gex > 0.0 {
        (matched_gex / total_absolute_gex).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let imbalance = if gross > 0.0 { signed / gross } else { 0.0 };
    let direction = if gross == 0.0 {
        ObservedGammaDirection::Unavailable
    } else if imbalance >= 0.20 {
        ObservedGammaDirection::LongGamma
    } else if imbalance <= -0.20 {
        ObservedGammaDirection::ShortGamma
    } else {
        ObservedGammaDirection::Balanced
    };
    let quality = if trade_count >= 5 && matched_share >= 0.10 && imbalance.abs() >= 0.35 {
        FlowQuality::High
    } else if trade_count >= 3 && matched_share >= 0.03 && imbalance.abs() >= 0.20 {
        FlowQuality::Medium
    } else {
        FlowQuality::Low
    };
    MakerGammaFlowWindow {
        lookback_minutes,
        signed_gamma_flow_1pct: signed,
        gross_gamma_flow_1pct: gross,
        imbalance,
        matched_deribit_gex_share: matched_share,
        trade_count,
        direction,
        quality,
    }
}

fn current_contract_gamma(
    contract: &RawOptionContractSnapshot,
    spot: f64,
    observed_at: UnixMs,
    source: GexGammaSource,
) -> Option<f64> {
    if source == GexGammaSource::ProviderNativePreferred
        && let (Some(gamma), Some(gamma_at)) = (
            contract.market.native_gamma,
            contract.market.native_gamma_observed_at,
        )
        && gamma.is_finite()
        && observed_at.saturating_diff(gamma_at) <= NATIVE_GAMMA_MAX_AGE_MS
    {
        return Some(gamma.abs());
    }
    let years = years_to_expiry(contract.instrument.expiration_timestamp, observed_at)?;
    let volatility = iv_percent_to_decimal(contract.market.mark_iv_percent)?;
    black_scholes_gamma(
        spot,
        contract.instrument.strike,
        years,
        contract.market.interest_rate,
        volatility,
    )
}

fn gamma_provenance(native: usize, derived: usize) -> GexGammaProvenance {
    match (native > 0, derived > 0) {
        (true, false) => GexGammaProvenance::Native,
        (false, true) | (false, false) => GexGammaProvenance::Derived,
        (true, true) => GexGammaProvenance::Mixed,
    }
}

fn proxy_total_at_price(
    contracts: &[&RawOptionContractSnapshot],
    price: f64,
    now: UnixMs,
) -> Option<f64> {
    proxy_totals_at_price(contracts, price, now).map(|(net, _)| net)
}

fn proxy_totals_at_price(
    contracts: &[&RawOptionContractSnapshot],
    price: f64,
    now: UnixMs,
) -> Option<(f64, f64)> {
    let mut net = 0.0;
    let mut absolute = 0.0;
    let mut valid = 0usize;
    for contract in contracts {
        let Some(gex) = contract_gex(contract, price, now) else {
            continue;
        };
        net += match contract.instrument.right {
            OptionRight::Call => gex,
            OptionRight::Put => -gex,
        };
        absolute += gex;
        valid += 1;
    }
    (valid > 0 && net.is_finite() && absolute.is_finite()).then_some((net, absolute))
}

fn build_scenario_curve(
    contracts: &[&RawOptionContractSnapshot],
    spot: f64,
    now: UnixMs,
    range_percent: f64,
    point_count: usize,
) -> (Vec<GexScenarioPoint>, Option<f64>) {
    if contracts.is_empty() || !spot.is_finite() || spot <= 0.0 || point_count < 2 {
        return (Vec::new(), None);
    }
    let fraction = (range_percent.max(DEFAULT_FLIP_RANGE_PERCENT) / 100.0).min(0.95);
    let low = spot * (1.0 - fraction);
    let high = spot * (1.0 + fraction);
    let step = (high - low) / (point_count - 1) as f64;
    let curve = (0..point_count)
        .filter_map(|index| {
            let price = low + step * index as f64;
            let (net_gex_1pct, absolute_gex_1pct) = proxy_totals_at_price(contracts, price, now)?;
            Some(GexScenarioPoint {
                price,
                net_gex_1pct,
                absolute_gex_1pct,
            })
        })
        .collect::<Vec<_>>();
    let gamma_flip = gamma_flip_from_curve(&curve, contracts, spot, now);
    (curve, gamma_flip)
}

fn gamma_flip_from_curve(
    curve: &[GexScenarioPoint],
    contracts: &[&RawOptionContractSnapshot],
    spot: f64,
    now: UnixMs,
) -> Option<f64> {
    let mut crossings = Vec::new();
    for pair in curve.windows(2) {
        let left = &pair[0];
        let right = &pair[1];
        if left.net_gex_1pct == 0.0 {
            crossings.push(left.price);
        } else if left.net_gex_1pct.signum() != right.net_gex_1pct.signum() {
            let (mut a, mut b, mut fa) = (left.price, right.price, left.net_gex_1pct);
            for _ in 0..FLIP_BISECTION_STEPS {
                let midpoint = (a + b) * 0.5;
                let Some(fm) = proxy_total_at_price(contracts, midpoint, now) else {
                    break;
                };
                if fm.abs() <= 1.0e-9 {
                    a = midpoint;
                    b = midpoint;
                    break;
                }
                if fa.signum() == fm.signum() {
                    a = midpoint;
                    fa = fm;
                } else {
                    b = midpoint;
                }
            }
            crossings.push((a + b) * 0.5);
        }
    }
    crossings
        .into_iter()
        .min_by(|a, b| (a - spot).abs().total_cmp(&(b - spot).abs()))
}

pub fn find_gamma_flip(
    contracts: &[&RawOptionContractSnapshot],
    spot: f64,
    now: UnixMs,
    range_percent: f64,
) -> Option<f64> {
    build_scenario_curve(contracts, spot, now, range_percent, FLIP_SCAN_STEPS + 1).1
}

#[derive(Clone, Copy)]
struct ZoneStrike<'a> {
    index: usize,
    strike: &'a GexStrike,
    source_strike: Option<f64>,
    sign: GexZoneSign,
    strength: f32,
    local_gap: f64,
}

pub fn extract_gex_zones(
    snapshot: &GexSnapshot,
    minimum_strength: f32,
    max_positive: u8,
    max_negative: u8,
) -> Vec<GexZone> {
    let source_strikes = snapshot
        .proxy
        .as_ref()
        .map(|proxy| {
            proxy
                .strike_mappings
                .iter()
                .map(|mapping| (mapping.mapped_strike.to_bits(), mapping.source_strike))
                .collect::<FxHashMap<_, _>>()
        })
        .unwrap_or_default();
    let mut strikes = snapshot.strikes.iter().collect::<Vec<_>>();
    strikes.sort_by(|a, b| a.strike.total_cmp(&b.strike));
    if strikes.is_empty() {
        return Vec::new();
    }
    let positive_scale = gex_percentile_95(
        strikes
            .iter()
            .map(|strike| strike.net_gex_1pct)
            .filter(|value| *value > 0.0),
    );
    let negative_scale = gex_percentile_95(
        strikes
            .iter()
            .map(|strike| strike.net_gex_1pct)
            .filter(|value| *value < 0.0),
    );
    let gaps = strikes
        .windows(2)
        .map(|pair| pair[1].strike - pair[0].strike)
        .filter(|gap| gap.is_finite() && *gap > 0.0)
        .collect::<Vec<_>>();
    let fallback_gap = median(&gaps).unwrap_or(snapshot.source_spot * 0.001);
    let threshold = minimum_strength.clamp(0.01, 1.0);
    let mut clusters: Vec<Vec<ZoneStrike<'_>>> = Vec::new();
    let mut current: Vec<ZoneStrike<'_>> = Vec::new();

    for (index, strike) in strikes.iter().enumerate() {
        let (sign, scale) = if strike.net_gex_1pct > 0.0 {
            (GexZoneSign::Positive, positive_scale)
        } else if strike.net_gex_1pct < 0.0 {
            (GexZoneSign::Negative, negative_scale)
        } else {
            if !current.is_empty() {
                clusters.push(std::mem::take(&mut current));
            }
            continue;
        };
        let Some(scale) = scale else {
            continue;
        };
        let strength =
            ((strike.net_gex_1pct.abs() / scale).asinh() / 1.0f64.asinh()).clamp(0.0, 1.0) as f32;
        if strength < threshold {
            if !current.is_empty() {
                clusters.push(std::mem::take(&mut current));
            }
            continue;
        }
        let local_gap = local_median_gap(&gaps, index, fallback_gap);
        let candidate = ZoneStrike {
            index,
            strike,
            source_strike: source_strikes.get(&strike.strike.to_bits()).copied(),
            sign,
            strength,
            local_gap,
        };
        let joins = current.last().is_some_and(|previous| {
            previous.sign == sign
                && index == previous.index + 1
                && strike.strike - previous.strike.strike
                    <= 1.5 * ((previous.local_gap + local_gap) * 0.5)
        });
        if !joins && !current.is_empty() {
            clusters.push(std::mem::take(&mut current));
        }
        current.push(candidate);
    }
    if !current.is_empty() {
        clusters.push(current);
    }

    let mut zones = clusters
        .into_iter()
        .map(|cluster| zone_from_cluster(snapshot, &cluster, fallback_gap))
        .collect::<Vec<_>>();
    let retain_best = |sign: GexZoneSign, limit: u8, zones: &mut Vec<GexZone>| {
        let mut indices = zones
            .iter()
            .enumerate()
            .filter(|(_, zone)| zone.sign == sign)
            .map(|(index, zone)| (index, zone.normalized_strength, zone.absolute_gex_1pct))
            .collect::<Vec<_>>();
        indices.sort_by(|a, b| {
            b.1.total_cmp(&a.1)
                .then_with(|| b.2.total_cmp(&a.2))
                .then_with(|| a.0.cmp(&b.0))
        });
        let keep = indices
            .into_iter()
            .take(usize::from(limit.clamp(1, 6)))
            .map(|value| value.0)
            .collect::<FxHashSet<_>>();
        for (index, zone) in zones.iter_mut().enumerate() {
            if zone.sign == sign && !keep.contains(&index) {
                zone.normalized_strength = -1.0;
            }
        }
    };
    retain_best(GexZoneSign::Positive, max_positive, &mut zones);
    retain_best(GexZoneSign::Negative, max_negative, &mut zones);
    zones.retain(|zone| zone.normalized_strength >= 0.0);
    zones.sort_by(|a, b| {
        a.peak_price
            .total_cmp(&b.peak_price)
            .then_with(|| a.sign.cmp(&b.sign))
    });
    zones
}

fn zone_from_cluster(
    snapshot: &GexSnapshot,
    cluster: &[ZoneStrike<'_>],
    fallback_gap: f64,
) -> GexZone {
    let peak = cluster
        .iter()
        .max_by(|a, b| {
            a.strength
                .total_cmp(&b.strength)
                .then_with(|| {
                    a.strike
                        .net_gex_1pct
                        .abs()
                        .total_cmp(&b.strike.net_gex_1pct.abs())
                })
                .then_with(|| b.strike.strike.total_cmp(&a.strike.strike))
        })
        .expect("non-empty GEX cluster");
    let bands = cluster
        .iter()
        .map(|value| {
            let local_gap = if value.local_gap.is_finite() && value.local_gap > 0.0 {
                value.local_gap
            } else {
                fallback_gap
            };
            let half_width = (0.18 * local_gap).clamp(
                snapshot.source_spot * 0.00025,
                snapshot.source_spot * 0.0015,
            );
            GexZoneBand {
                source_strike: value.source_strike,
                strike: value.strike.strike,
                lower_price: value.strike.strike - half_width,
                upper_price: value.strike.strike + half_width,
                normalized_strength: value.strength,
                net_gex_1pct: value.strike.net_gex_1pct,
            }
        })
        .collect::<Vec<_>>();
    let lower_price = bands
        .iter()
        .map(|band| band.lower_price)
        .min_by(f64::total_cmp)
        .expect("non-empty GEX cluster bands");
    let upper_price = bands
        .iter()
        .map(|band| band.upper_price)
        .max_by(f64::total_cmp)
        .expect("non-empty GEX cluster bands");
    let net_gex_1pct = cluster.iter().map(|value| value.strike.net_gex_1pct).sum();
    let absolute_gex_1pct = cluster
        .iter()
        .map(|value| value.strike.absolute_gamma_1pct)
        .sum();
    let cluster_prices = cluster
        .iter()
        .map(|value| value.strike.strike.to_bits())
        .collect::<FxHashSet<_>>();
    let mut expiry_contributions: FxHashMap<UnixMs, f64> = FxHashMap::default();
    for value in snapshot
        .expiry_strikes
        .iter()
        .filter(|value| cluster_prices.contains(&value.strike.to_bits()))
    {
        *expiry_contributions.entry(value.expiration).or_default() += value.net_gex_1pct.abs();
    }
    let dominant_expiry = expiry_contributions
        .into_iter()
        .max_by(|a, b| a.1.total_cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
        .map(|value| value.0);
    let native = cluster
        .iter()
        .any(|value| value.strike.gamma_provenance == GexGammaProvenance::Native);
    let derived = cluster
        .iter()
        .any(|value| value.strike.gamma_provenance == GexGammaProvenance::Derived);
    let mixed = cluster
        .iter()
        .any(|value| value.strike.gamma_provenance == GexGammaProvenance::Mixed);
    let gamma_provenance = if mixed || (native && derived) {
        GexGammaProvenance::Mixed
    } else if native {
        GexGammaProvenance::Native
    } else {
        GexGammaProvenance::Derived
    };
    GexZone {
        id: 0,
        observed_at: snapshot.observed_at,
        lower_price,
        upper_price,
        peak_price: peak.strike.strike,
        net_gex_1pct,
        absolute_gex_1pct,
        normalized_strength: peak.strength,
        persistence_score: peak.strength * 0.5,
        sign: peak.sign,
        dominant_expiry,
        gamma_provenance,
        state: GexZoneState::Active,
        missing_buckets: 0,
        bands: bands.into(),
    }
}

fn median(values: &[f64]) -> Option<f64> {
    let mut values = values
        .iter()
        .copied()
        .filter(|value| value.is_finite() && *value > 0.0)
        .collect::<Vec<_>>();
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    Some(if values.len() % 2 == 0 {
        (values[middle - 1] + values[middle]) * 0.5
    } else {
        values[middle]
    })
}

fn local_median_gap(gaps: &[f64], strike_index: usize, fallback: f64) -> f64 {
    let from = strike_index.saturating_sub(2).min(gaps.len());
    let to = (strike_index + 2).min(gaps.len());
    median(&gaps[from..to]).unwrap_or(fallback)
}

pub fn zone_overlap_ratio(a: &GexZone, b: &GexZone) -> f64 {
    let overlap = (a.upper_price.min(b.upper_price) - a.lower_price.max(b.lower_price)).max(0.0);
    let minimum_width = (a.upper_price - a.lower_price)
        .min(b.upper_price - b.lower_price)
        .max(f64::EPSILON);
    overlap / minimum_width
}

fn zone_matches(previous: &GexZone, current: &GexZone, spot: f64) -> bool {
    if previous.sign != current.sign {
        return false;
    }
    let overlap = zone_overlap_ratio(previous, current);
    let combined_half_width = ((previous.upper_price - previous.lower_price)
        + (current.upper_price - current.lower_price))
        * 0.25;
    overlap >= 0.30
        || (previous.peak_price - current.peak_price).abs()
            <= (spot * 0.002).max(combined_half_width)
}

#[derive(Debug)]
struct ZoneSourceIdentity {
    /// Canonical exact source-strike bits. Proxy mapping preserves the original
    /// `f64` values, so matching does not need fuzzy reverse mapping.
    members: Vec<u64>,
    representative: u64,
}

fn zone_source_identity(zone: &GexZone) -> Option<ZoneSourceIdentity> {
    let mut members = zone
        .bands
        .iter()
        .map(|band| band.source_strike.map(f64::to_bits))
        .collect::<Option<Vec<_>>>()?;
    if members.is_empty() {
        return None;
    }
    members.sort_unstable();
    members.dedup();
    let representative = zone
        .bands
        .iter()
        .find(|band| band.strike.to_bits() == zone.peak_price.to_bits())
        .and_then(|band| band.source_strike)
        .map(f64::to_bits)?;
    Some(ZoneSourceIdentity {
        members,
        representative,
    })
}

#[derive(Debug)]
struct QuantWheelMatchCandidate {
    current_index: usize,
    previous_index: usize,
    /// 3 exact membership, 2 strong Jaccard overlap, 1 representative match,
    /// 0 legacy spatial fallback when either side lacks source identity.
    source_tier: u8,
    source_similarity: f64,
    intersection_count: usize,
    representative_match: bool,
    spatial_overlap: f64,
    peak_distance: f64,
    current_strength: f64,
    previous_id: u64,
}

fn quantwheel_match_candidate(
    current_index: usize,
    current: &GexZone,
    current_source: Option<&ZoneSourceIdentity>,
    previous_index: usize,
    previous: &GexZone,
    previous_source: Option<&ZoneSourceIdentity>,
    spot: f64,
) -> Option<QuantWheelMatchCandidate> {
    if previous.sign != current.sign {
        return None;
    }
    let (source_tier, source_similarity, intersection_count, representative_match) =
        match (previous_source, current_source) {
            (Some(previous), Some(current)) => {
                if previous.members == current.members {
                    (3, 1.0, previous.members.len(), true)
                } else {
                    let intersection_count = previous
                        .members
                        .iter()
                        .filter(|member| current.members.binary_search(member).is_ok())
                        .count();
                    let union_count =
                        previous.members.len() + current.members.len() - intersection_count;
                    let jaccard = intersection_count as f64 / union_count.max(1) as f64;
                    let representative_match = previous.representative == current.representative;
                    if jaccard >= 0.50 {
                        (2, jaccard, intersection_count, representative_match)
                    } else if representative_match {
                        (1, jaccard, intersection_count, true)
                    } else {
                        // Explicit but unrelated source clusters must never match
                        // merely because their mapped target regions overlap.
                        return None;
                    }
                }
            }
            _ if zone_matches(previous, current, spot) => (0, 0.0, 0, false),
            _ => return None,
        };
    Some(QuantWheelMatchCandidate {
        current_index,
        previous_index,
        source_tier,
        source_similarity,
        intersection_count,
        representative_match,
        spatial_overlap: zone_overlap_ratio(previous, current),
        peak_distance: (previous.peak_price - current.peak_price).abs(),
        current_strength: current.absolute_gex_1pct,
        previous_id: previous.id,
    })
}

fn assign_quantwheel_zone_ids(
    current: &mut [GexZone],
    previous: &[GexZone],
    spot: f64,
) -> FxHashSet<usize> {
    let current_sources = current.iter().map(zone_source_identity).collect::<Vec<_>>();
    let previous_sources = previous
        .iter()
        .map(zone_source_identity)
        .collect::<Vec<_>>();
    let mut candidates = Vec::new();
    for (current_index, zone) in current.iter().enumerate() {
        for (previous_index, old) in previous.iter().enumerate() {
            if let Some(candidate) = quantwheel_match_candidate(
                current_index,
                zone,
                current_sources[current_index].as_ref(),
                previous_index,
                old,
                previous_sources[previous_index].as_ref(),
                spot,
            ) {
                candidates.push(candidate);
            }
        }
    }
    candidates.sort_by(|a, b| {
        b.source_tier
            .cmp(&a.source_tier)
            .then_with(|| b.source_similarity.total_cmp(&a.source_similarity))
            .then_with(|| b.intersection_count.cmp(&a.intersection_count))
            .then_with(|| b.representative_match.cmp(&a.representative_match))
            .then_with(|| b.spatial_overlap.total_cmp(&a.spatial_overlap))
            .then_with(|| a.peak_distance.total_cmp(&b.peak_distance))
            .then_with(|| b.current_strength.total_cmp(&a.current_strength))
            .then_with(|| a.previous_id.cmp(&b.previous_id))
            .then_with(|| a.current_index.cmp(&b.current_index))
    });
    let mut used_current = FxHashSet::default();
    let mut used_previous = FxHashSet::default();
    for candidate in candidates {
        if used_current.insert(candidate.current_index)
            && used_previous.insert(candidate.previous_index)
        {
            current[candidate.current_index].id = previous[candidate.previous_index].id;
        }
    }
    used_previous
}

fn same_quantwheel_series(previous: &GexSnapshot, current: &GexSnapshot) -> bool {
    previous.provider == current.provider
        && previous.underlying == current.underlying
        && previous.expiry_filter == current.expiry_filter
        && previous.gamma_source == current.gamma_source
        && match (&previous.proxy, &current.proxy) {
            (Some(previous), Some(current)) => {
                previous.source_symbol == current.source_symbol
                    && previous.target_symbol == current.target_symbol
            }
            (None, None) => true,
            _ => false,
        }
}

pub fn build_gex_zone_frames(
    history: &[Arc<GexSnapshot>],
    bucket_ms: u64,
    config: &GexLevelsConfig,
) -> Vec<GexZoneFrame> {
    let bucket_ms = bucket_ms.max(1);
    let mut snapshots = BTreeMap::new();
    for snapshot in history {
        snapshots.insert(
            gex_bucket_start(snapshot.observed_at, bucket_ms),
            snapshot.as_ref(),
        );
    }
    let mut frames = Vec::with_capacity(snapshots.len());
    let mut previous: Vec<GexZone> = Vec::new();
    let mut previous_bucket = None;
    let mut track_history: FxHashMap<u64, Vec<(UnixMs, f32)>> = FxHashMap::default();

    for (bucket_start, snapshot) in snapshots {
        let consecutive = previous_bucket
            .is_some_and(|value: UnixMs| bucket_start.saturating_diff(value) == bucket_ms);
        if !consecutive {
            previous.clear();
        }
        let mut current = extract_gex_zones(
            snapshot,
            config.minimum_zone_strength,
            config.max_positive_zones,
            config.max_negative_zones,
        );
        let mut candidates = Vec::new();
        if consecutive {
            for (current_index, zone) in current.iter().enumerate() {
                for (previous_index, old) in previous.iter().enumerate() {
                    if zone_matches(old, zone, snapshot.source_spot) {
                        candidates.push((
                            current_index,
                            previous_index,
                            zone_overlap_ratio(old, zone),
                            (old.peak_price - zone.peak_price).abs(),
                            zone.absolute_gex_1pct,
                            old.id,
                        ));
                    }
                }
            }
        }
        candidates.sort_by(|a, b| {
            b.2.total_cmp(&a.2)
                .then_with(|| a.3.total_cmp(&b.3))
                .then_with(|| b.4.total_cmp(&a.4))
                .then_with(|| a.5.cmp(&b.5))
                .then_with(|| a.0.cmp(&b.0))
        });
        let mut used_current = FxHashSet::default();
        let mut used_previous = FxHashSet::default();
        for (current_index, previous_index, ..) in candidates {
            if used_current.insert(current_index) && used_previous.insert(previous_index) {
                current[current_index].id = previous[previous_index].id;
            }
        }
        for (ordinal, zone) in current.iter_mut().enumerate() {
            if zone.id == 0 {
                zone.id = deterministic_zone_id(bucket_start, zone, ordinal);
            }
            let lookback_ms = u64::from(config.persistent_lookback_minutes.clamp(1, 60)) * 60_000;
            let cutoff = bucket_start.saturating_sub(lookback_ms);
            let entries = track_history.entry(zone.id).or_default();
            entries.retain(|(time, _)| *time >= cutoff);
            let expected = (lookback_ms / bucket_ms).max(1) as f32;
            let presence_ratio = (entries.len() as f32 / expected).clamp(0.0, 1.0);
            let average_strength = if entries.is_empty() {
                0.0
            } else {
                entries.iter().map(|(_, strength)| *strength).sum::<f32>() / entries.len() as f32
            };
            zone.persistence_score =
                (0.50 * zone.normalized_strength + 0.30 * presence_ratio + 0.20 * average_strength)
                    .clamp(0.0, 1.0);
            entries.push((bucket_start, zone.normalized_strength));
        }
        if consecutive {
            for (index, zone) in previous.iter().enumerate() {
                if used_previous.contains(&index) {
                    continue;
                }
                let missing = zone.missing_buckets.saturating_add(1);
                if missing < config.fade_buckets.clamp(1, 3) {
                    let mut fading = zone.clone();
                    fading.state = GexZoneState::Fading;
                    fading.missing_buckets = missing;
                    current.push(fading);
                }
            }
        }
        current.retain(|zone| {
            zone.normalized_strength >= config.minimum_zone_strength
                || zone.persistence_score >= 0.25
                || zone.state == GexZoneState::Fading
        });
        limit_tracked_zones(
            &mut current,
            GexZoneSign::Positive,
            config.max_positive_zones,
        );
        limit_tracked_zones(
            &mut current,
            GexZoneSign::Negative,
            config.max_negative_zones,
        );
        current.sort_by_key(|zone| zone.id);
        frames.push(GexZoneFrame {
            bucket_start,
            interval_end: Some(bucket_start.saturating_add(bucket_ms)),
            source_spot: snapshot.source_spot,
            zones: current.clone().into(),
        });
        previous = current;
        previous_bucket = Some(bucket_start);
    }
    frames
}

/// Builds knowledge-time intervals for QuantWheel observations.
///
/// Unlike [`build_gex_zone_frames`], this function never rounds, buckets, or
/// deduplicates observations. Each accepted observation starts at its exact
/// local response-completion timestamp and ends at the next observation. The
/// final interval is deliberately open-ended.
pub fn build_quantwheel_gex_intervals(
    history: &[Arc<GexSnapshot>],
    config: &GexLevelsConfig,
) -> Vec<GexZoneFrame> {
    let mut snapshots = history.iter().enumerate().collect::<Vec<_>>();
    snapshots.sort_by(|(left_index, left), (right_index, right)| {
        left.observed_at
            .cmp(&right.observed_at)
            .then_with(|| left_index.cmp(right_index))
    });
    let mut frames = Vec::with_capacity(snapshots.len());
    let mut previous: Vec<GexZone> = Vec::new();
    let mut track_history: FxHashMap<u64, Vec<(UnixMs, f32)>> = FxHashMap::default();

    for (index, (_, snapshot)) in snapshots.iter().enumerate() {
        let start = snapshot.observed_at;
        let end = snapshots.get(index + 1).map(|(_, next)| next.observed_at);
        if index > 0 && !same_quantwheel_series(snapshots[index - 1].1, snapshot) {
            previous.clear();
        }
        let mut current = extract_gex_zones(
            snapshot,
            config.minimum_zone_strength,
            config.max_positive_zones,
            config.max_negative_zones,
        );
        let used_previous =
            assign_quantwheel_zone_ids(&mut current, &previous, snapshot.source_spot);
        let lookback_ms = u64::from(config.persistent_lookback_minutes.clamp(1, 60)) * 60_000;
        let cutoff = start.saturating_sub(lookback_ms);
        let expected_observations = snapshots[..=index]
            .iter()
            .filter(|(_, value)| value.observed_at >= cutoff)
            .count()
            .max(1) as f32;
        for (ordinal, zone) in current.iter_mut().enumerate() {
            if zone.id == 0 {
                zone.id = deterministic_zone_id(start, zone, ordinal);
            }
            let entries = track_history.entry(zone.id).or_default();
            entries.retain(|(time, _)| *time >= cutoff);
            let presence_ratio = (entries.len() as f32 / expected_observations).clamp(0.0, 1.0);
            let average_strength = if entries.is_empty() {
                0.0
            } else {
                entries.iter().map(|(_, strength)| *strength).sum::<f32>() / entries.len() as f32
            };
            zone.persistence_score =
                (0.50 * zone.normalized_strength + 0.30 * presence_ratio + 0.20 * average_strength)
                    .clamp(0.0, 1.0);
            entries.push((start, zone.normalized_strength));
        }
        for (previous_index, zone) in previous.iter().enumerate() {
            if used_previous.contains(&previous_index) {
                continue;
            }
            let missing = zone.missing_buckets.saturating_add(1);
            if missing < config.fade_buckets.clamp(1, 3) {
                let mut fading = zone.clone();
                fading.state = GexZoneState::Fading;
                fading.missing_buckets = missing;
                current.push(fading);
            }
        }
        current.retain(|zone| {
            zone.normalized_strength >= config.minimum_zone_strength
                || zone.persistence_score >= 0.25
                || zone.state == GexZoneState::Fading
        });
        limit_tracked_zones(
            &mut current,
            GexZoneSign::Positive,
            config.max_positive_zones,
        );
        limit_tracked_zones(
            &mut current,
            GexZoneSign::Negative,
            config.max_negative_zones,
        );
        current.sort_by_key(|zone| zone.id);
        frames.push(GexZoneFrame {
            bucket_start: start,
            interval_end: end,
            source_spot: snapshot.source_spot,
            zones: current.clone().into(),
        });
        previous = current;
    }
    frames
}

fn limit_tracked_zones(zones: &mut Vec<GexZone>, sign: GexZoneSign, limit: u8) {
    let mut ranked = zones
        .iter()
        .enumerate()
        .filter(|(_, zone)| zone.sign == sign)
        .map(|(index, zone)| {
            (
                index,
                zone.persistence_score,
                zone.normalized_strength,
                zone.absolute_gex_1pct,
                zone.id,
            )
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then_with(|| b.2.total_cmp(&a.2))
            .then_with(|| b.3.total_cmp(&a.3))
            .then_with(|| a.4.cmp(&b.4))
    });
    let keep = ranked
        .into_iter()
        .take(usize::from(limit.clamp(1, 6)))
        .map(|value| value.0)
        .collect::<FxHashSet<_>>();
    let mut index = 0usize;
    zones.retain(|zone| {
        let retain = zone.sign != sign || keep.contains(&index);
        index += 1;
        retain
    });
}

fn deterministic_zone_id(bucket: UnixMs, zone: &GexZone, ordinal: usize) -> u64 {
    let sign = match zone.sign {
        GexZoneSign::Positive => 0x9e37_79b9_7f4a_7c15,
        GexZoneSign::Negative => 0xc2b2_ae3d_27d4_eb4f,
    };
    let mut value = bucket.as_u64() ^ zone.peak_price.to_bits().rotate_left(17) ^ sign;
    value ^= (ordinal as u64).wrapping_mul(0x1000_0000_01b3);
    value = value.wrapping_mul(0xff51_afd7_ed55_8ccd);
    value ^ (value >> 33)
}

pub fn gex_percentile_95(values: impl IntoIterator<Item = f64>) -> Option<f64> {
    let mut values = values
        .into_iter()
        .map(f64::abs)
        .filter(|value| value.is_finite() && *value > 0.0)
        .collect::<Vec<_>>();
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let index = ((values.len() as f64 * 0.95).ceil() as usize)
        .saturating_sub(1)
        .min(values.len() - 1);
    values.get(index).copied()
}

pub fn gex_bucket_start(timestamp: UnixMs, bucket_ms: u64) -> UnixMs {
    let bucket_ms = bucket_ms.max(1);
    UnixMs::new(timestamp.as_u64() / bucket_ms * bucket_ms)
}

fn gex_proxy_zones_for_point(point: &GexProxyHistoryPoint, p95: f64) -> Arc<[GexProxyZone]> {
    if !point.source_spot.is_finite() || point.source_spot <= 0.0 {
        return Arc::from([]);
    }
    let levels = GexProxyZoneRole::ALL
        .into_iter()
        .filter_map(|role| role.level(point).map(|level| (role, level)))
        .collect::<Vec<_>>();
    let normalized = if p95.is_finite() && p95 > f64::EPSILON {
        ((point.total_gex.abs() / p95).asinh() / 1.0_f64.asinh()).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    levels
        .iter()
        .enumerate()
        .map(|(index, (role, level))| {
            let nearest = levels
                .iter()
                .enumerate()
                .filter_map(|(candidate_index, (_, candidate))| {
                    (candidate_index != index).then_some((candidate - level).abs())
                })
                .min_by(f64::total_cmp);
            let half_width = nearest.map_or(point.source_spot * 0.001, |distance| {
                (distance * 0.12).clamp(point.source_spot * 0.0005, point.source_spot * 0.0025)
            });
            let wall_confirmed = match role {
                GexProxyZoneRole::PositivePrimary => point
                    .call_wall
                    .is_some_and(|wall| wall.is_finite() && (wall - level).abs() <= half_width),
                GexProxyZoneRole::NegativePrimary => point
                    .put_wall
                    .is_some_and(|wall| wall.is_finite() && (wall - level).abs() <= half_width),
                GexProxyZoneRole::PositiveSecondary | GexProxyZoneRole::NegativeSecondary => false,
            };
            let strength = (normalized * role.weight() + if wall_confirmed { 0.12 } else { 0.0 })
                .clamp(0.0, 1.0);
            GexProxyZone {
                role: *role,
                center_price: *level,
                lower_price: level - half_width,
                upper_price: level + half_width,
                strength,
                wall_confirmed,
            }
        })
        .collect::<Vec<_>>()
        .into()
}

pub fn build_gex_proxy_zone_frames(
    history: &[Arc<GexProxyHistoryPoint>],
    deribit_history: &[Arc<GexSnapshot>],
    chart_interval_ms: u64,
    latest_candle_time: UnixMs,
) -> Vec<GexProxyZoneFrame> {
    const SOURCE_INTERVAL_MS: u64 = 5 * 60 * 1_000;
    let chart_interval_ms = chart_interval_ms.max(1);
    let chart_end = latest_candle_time.saturating_add(chart_interval_ms);
    let covered_buckets = deribit_history
        .iter()
        .map(|snapshot| gex_bucket_start(snapshot.observed_at, chart_interval_ms))
        .collect::<FxHashSet<_>>();
    let p95 = gex_percentile_95(history.iter().map(|point| point.total_gex)).unwrap_or(0.0);
    let mut points = history
        .iter()
        .filter_map(|point| {
            let observed_at = u64::try_from(point.observed_at).ok()?;
            (observed_at < chart_end.as_u64()).then_some((UnixMs::new(observed_at), point.clone()))
        })
        .collect::<Vec<_>>();
    points.sort_by_key(|(observed_at, _)| *observed_at);
    points.dedup_by_key(|(observed_at, _)| *observed_at);

    if chart_interval_ms >= SOURCE_INTERVAL_MS {
        let mut grouped = BTreeMap::<UnixMs, Arc<GexProxyHistoryPoint>>::new();
        for (observed_at, point) in points {
            grouped.insert(gex_bucket_start(observed_at, chart_interval_ms), point);
        }
        return grouped
            .into_iter()
            .filter(|(bucket_start, _)| !covered_buckets.contains(bucket_start))
            .filter_map(|(bucket_start, point)| {
                let bucket_end = bucket_start
                    .saturating_add(chart_interval_ms)
                    .min(chart_end);
                let zones = gex_proxy_zones_for_point(&point, p95);
                let flip_level = point
                    .flip_level
                    .filter(|level| level.is_finite() && *level > 0.0);
                (bucket_end > bucket_start && (!zones.is_empty() || flip_level.is_some())).then(
                    || GexProxyZoneFrame {
                        bucket_start,
                        bucket_end,
                        observed_at: UnixMs::new(point.observed_at as u64),
                        source_spot: point.source_spot,
                        total_gex: point.total_gex,
                        flip_level,
                        zones,
                    },
                )
            })
            .collect();
    }

    let mut frames = Vec::new();
    for (index, (observed_at, point)) in points.iter().enumerate() {
        let source_end = observed_at
            .saturating_add(SOURCE_INTERVAL_MS)
            .min(points.get(index + 1).map_or(chart_end, |(next, _)| *next))
            .min(chart_end);
        let zones = gex_proxy_zones_for_point(point, p95);
        let flip_level = point
            .flip_level
            .filter(|level| level.is_finite() && *level > 0.0);
        if zones.is_empty() && flip_level.is_none() {
            continue;
        }
        let mut bucket = gex_bucket_start(*observed_at, chart_interval_ms);
        while bucket < source_end {
            let bucket_end = bucket.saturating_add(chart_interval_ms);
            let frame_start = (*observed_at).max(bucket);
            let frame_end = source_end.min(bucket_end);
            if frame_end > frame_start && !covered_buckets.contains(&bucket) {
                frames.push(GexProxyZoneFrame {
                    bucket_start: frame_start,
                    bucket_end: frame_end,
                    observed_at: *observed_at,
                    source_spot: point.source_spot,
                    total_gex: point.total_gex,
                    flip_level,
                    zones: zones.clone(),
                });
            }
            bucket = bucket_end;
        }
    }
    frames
}

pub fn dominant_expiry(values: &[GexExpiryStrike], strike: f64) -> Option<(UnixMs, f64)> {
    let matching = values
        .iter()
        .filter(|value| value.strike.to_bits() == strike.to_bits());
    let total = matching
        .clone()
        .map(|value| value.absolute_gamma_1pct)
        .sum::<f64>();
    let dominant = matching.max_by(|a, b| {
        a.absolute_gamma_1pct
            .total_cmp(&b.absolute_gamma_1pct)
            .then_with(|| b.expiration.cmp(&a.expiration))
    })?;
    let share = if total > 0.0 {
        (dominant.absolute_gamma_1pct / total).clamp(0.0, 1.0)
    } else {
        0.0
    };
    Some((dominant.expiration, share))
}

#[cfg(test)]
mod tests {
    use super::*;
    use exchange::options::{
        OptionInstrument, OptionMarketPoint,
        derive::{DeriveMakerSide, DeriveMakerTrade},
    };

    const NOW: UnixMs = UnixMs::new(1_700_000_000_000);

    fn contract(
        strike: f64,
        right: OptionRight,
        days: u64,
        oi: f64,
        contract_size: f64,
    ) -> RawOptionContractSnapshot {
        let name = format!("{strike:?}-{right:?}-{days}");
        RawOptionContractSnapshot {
            instrument: OptionInstrument {
                instrument_name: name.clone(),
                underlying: OptionsUnderlying::Btc,
                expiration_timestamp: NOW.saturating_add(days * MILLIS_PER_DAY),
                strike,
                right,
                contract_size,
            },
            market: OptionMarketPoint {
                instrument_name: name,
                open_interest_underlying: oi,
                mark_iv_percent: 50.0,
                underlying_price: 100.0,
                interest_rate: 0.01,
                observed_at: NOW,
                native_gamma: None,
                native_gamma_observed_at: None,
            },
        }
    }

    fn chain(contracts: Vec<RawOptionContractSnapshot>) -> RawOptionChainSnapshot {
        RawOptionChainSnapshot {
            provider: OptionsProvider::Deribit,
            underlying: OptionsUnderlying::Btc,
            source_spot: 100.0,
            contracts: contracts.into(),
            observed_at: NOW,
        }
    }

    #[test]
    fn normal_distribution_and_known_gamma() {
        assert!((normal_pdf(0.0).expect("pdf") - 0.398_942_280_4).abs() < 1.0e-10);
        let gamma = black_scholes_gamma(100.0, 100.0, 1.0, 0.05, 0.2).expect("gamma");
        assert!((gamma - 0.018_762).abs() < 1.0e-6);
        assert!(black_scholes_gamma(0.0, 100.0, 1.0, 0.0, 0.2).is_none());
        assert!(black_scholes_gamma(100.0, 100.0, 1.0, 0.0, f64::NAN).is_none());
        assert_eq!(iv_percent_to_decimal(55.0), Some(0.55));
    }

    #[test]
    fn known_black_scholes_vega_and_one_vol_point_conversion() {
        let vega = black_scholes_vega(100.0, 100.0, 1.0, 0.05, 0.2).expect("vega");
        assert!((vega - 37.524_034_69).abs() < 1.0e-6);
        assert!((vega * 10.0 * 0.01 - 3.752_403_469).abs() < 1.0e-6);
        assert_eq!(iv_percent_to_decimal(20.0), Some(0.2));
        assert!(black_scholes_vega(100.0, 100.0, 0.0, 0.0, 0.2).is_none());
        assert!(black_scholes_vega(100.0, 0.0, 1.0, 0.0, 0.2).is_none());
    }

    #[test]
    fn intrinsic_stress_covers_itm_otm_calls_and_puts() {
        let source = chain(vec![
            contract(90.0, OptionRight::Call, 7, 2.0, 1.0),
            contract(110.0, OptionRight::Call, 7, 3.0, 1.0),
            contract(110.0, OptionRight::Put, 7, 4.0, 1.0),
            contract(90.0, OptionRight::Put, 7, 5.0, 1.0),
        ]);
        let metrics = calculate_gex_at(&source, &Config::default(), NOW).intrinsic_stress;
        assert_eq!(metrics.gross_intrinsic_usd, 60.0);
        assert_eq!(metrics.total_oi_notional_usd, 1_400.0);
        assert!((metrics.intrinsic_ratio - 60.0 / 1_400.0).abs() < 1.0e-12);
        assert_eq!(metrics.itm_contracts, 2);
        assert_eq!(metrics.total_contracts, 4);
        assert_eq!(metrics.level, IntrinsicStressLevel::Mild);
    }

    #[test]
    fn intrinsic_zero_notional_is_safe_and_min_gex_independent() {
        let source = chain(vec![contract(90.0, OptionRight::Call, 7, 0.0, 1.0)]);
        let base = calculate_gex_at(&source, &Config::default(), NOW);
        let hidden_profile = calculate_gex_at(
            &source,
            &Config {
                min_absolute_gex: f64::MAX,
                ..Config::default()
            },
            NOW,
        );
        assert_eq!(base.intrinsic_stress, hidden_profile.intrinsic_stress);
        assert_eq!(base.intrinsic_stress.intrinsic_ratio, 0.0);
        assert!(base.intrinsic_stress.intrinsic_ratio.is_finite());
    }

    #[test]
    fn intrinsic_and_gamma_vega_classifications_use_named_boundaries() {
        assert_eq!(intrinsic_stress_level(0.019), IntrinsicStressLevel::Low);
        assert_eq!(intrinsic_stress_level(0.02), IntrinsicStressLevel::Mild);
        assert_eq!(intrinsic_stress_level(0.05), IntrinsicStressLevel::Elevated);
        assert_eq!(intrinsic_stress_level(0.10), IntrinsicStressLevel::High);
        assert_eq!(gamma_vega_regime(Some(0.79)), GammaVegaRegime::VegaDominant);
        assert_eq!(gamma_vega_regime(Some(0.80)), GammaVegaRegime::Balanced);
        assert_eq!(gamma_vega_regime(Some(1.25)), GammaVegaRegime::Balanced);
        assert_eq!(
            gamma_vega_regime(Some(1.26)),
            GammaVegaRegime::GammaDominant
        );
        assert_eq!(gamma_vega_regime(None), GammaVegaRegime::Unavailable);
        assert_eq!(
            gamma_liquidity_regime(0.24),
            GammaLiquidityRegime::LowImpact
        );
        assert_eq!(gamma_liquidity_regime(0.25), GammaLiquidityRegime::Moderate);
        assert_eq!(gamma_liquidity_regime(0.75), GammaLiquidityRegime::Elevated);
        assert_eq!(
            gamma_liquidity_regime(1.50),
            GammaLiquidityRegime::HighImpact
        );
    }

    #[test]
    fn gamma_vega_aggregates_calls_puts_and_expiry_leaders() {
        let source = chain(vec![
            contract(100.0, OptionRight::Call, 1, 10.0, 1.0),
            contract(100.0, OptionRight::Put, 1, 10.0, 1.0),
            contract(100.0, OptionRight::Call, 30, 10.0, 1.0),
            contract(100.0, OptionRight::Put, 30, 10.0, 1.0),
        ]);
        let snapshot = calculate_gex_at(
            &source,
            &Config {
                expiry_filter: GexExpiryFilter::All,
                ..Config::default()
            },
            NOW,
        );
        let metrics = snapshot.gamma_vega;
        assert_eq!(metrics.valid_contracts, 4);
        assert!(metrics.vega_shock_1vol_usd > 0.0);
        assert!(metrics.gamma_shock_1pct_usd > 0.0);
        assert!(metrics.gamma_vega_ratio.is_some());
        assert_eq!(
            metrics.top_gamma_expiry,
            Some(NOW.saturating_add(MILLIS_PER_DAY))
        );
        assert_eq!(
            metrics.top_vega_expiry,
            Some(NOW.saturating_add(30 * MILLIS_PER_DAY))
        );
    }

    #[test]
    fn gamma_vega_excludes_invalid_iv_and_expired_contracts() {
        let mut expired = contract(100.0, OptionRight::Call, 1, 5.0, 1.0);
        expired.instrument.expiration_timestamp = NOW;
        let mut invalid = contract(100.0, OptionRight::Put, 7, 5.0, 1.0);
        invalid.market.mark_iv_percent = f64::NAN;
        let metrics =
            calculate_gex_at(&chain(vec![expired, invalid]), &Config::default(), NOW).gamma_vega;
        assert_eq!(metrics.valid_contracts, 0);
        assert_eq!(metrics.vega_shock_1vol_usd, 0.0);
        assert_eq!(metrics.gamma_vega_ratio, None);
        assert_eq!(metrics.regime, GammaVegaRegime::Unavailable);
    }

    #[test]
    fn expiry_filters_use_real_timestamps() {
        let source = chain(vec![
            contract(90.0, OptionRight::Put, 1, 1.0, 1.0),
            contract(95.0, OptionRight::Put, 2, 1.0, 1.0),
            contract(97.0, OptionRight::Put, 3, 1.0, 1.0),
            contract(100.0, OptionRight::Call, 7, 1.0, 1.0),
            contract(110.0, OptionRight::Call, 30, 1.0, 1.0),
        ]);
        assert_eq!(
            select_contracts(&source, GexExpiryFilter::NextExpiry, 0.0, NOW).len(),
            1
        );
        assert_eq!(
            select_contracts(&source, GexExpiryFilter::OneDay, 0.0, NOW).len(),
            1
        );
        assert_eq!(
            select_contracts(&source, GexExpiryFilter::TwoDays, 0.0, NOW).len(),
            2
        );
        assert_eq!(
            select_contracts(&source, GexExpiryFilter::ThreeDays, 0.0, NOW).len(),
            3
        );
        assert_eq!(
            select_contracts(&source, GexExpiryFilter::SevenDays, 0.0, NOW).len(),
            4
        );
        assert_eq!(
            select_contracts(&source, GexExpiryFilter::ThirtyDays, 0.0, NOW).len(),
            5
        );
    }

    #[test]
    fn aggregates_proxy_absolute_walls_and_thresholds() {
        let source = chain(vec![
            contract(90.0, OptionRight::Put, 7, 20.0, 1.0),
            contract(100.0, OptionRight::Call, 7, 30.0, 1.0),
            contract(110.0, OptionRight::Call, 7, 5.0, 1.0),
        ]);
        let snapshot = calculate_gex_at(&source, &Config::default(), NOW);
        assert_eq!(snapshot.strikes.len(), 3);
        assert_eq!(snapshot.call_wall, Some(100.0));
        assert_eq!(snapshot.put_wall, Some(90.0));
        assert!(snapshot.net_gex_1pct.is_some());
        assert!(snapshot.absolute_gex_1pct > 0.0);

        let absolute = calculate_gex_at(
            &source,
            &Config {
                sign_model: GexSignModel::AbsoluteGamma,
                ..Config::default()
            },
            NOW,
        );
        assert!(absolute.net_gex_1pct.is_none());
        assert!(absolute.gamma_flip.is_none());

        let filtered = calculate_gex_at(
            &source,
            &Config {
                min_open_interest: 10.0,
                ..Config::default()
            },
            NOW,
        );
        assert_eq!(filtered.strikes.len(), 2);
        let none = calculate_gex_at(
            &source,
            &Config {
                min_absolute_gex: f64::MAX,
                ..Config::default()
            },
            NOW,
        );
        assert!(none.strikes.is_empty());
    }

    #[test]
    fn exposure_respects_provider_contract_size() {
        let one = chain(vec![contract(100.0, OptionRight::Call, 7, 10.0, 1.0)]);
        let ten = chain(vec![contract(100.0, OptionRight::Call, 7, 10.0, 10.0)]);
        let a = calculate_gex_at(&one, &Config::default(), NOW);
        let b = calculate_gex_at(&ten, &Config::default(), NOW);
        assert!((b.absolute_gex_1pct / a.absolute_gex_1pct - 10.0).abs() < 1.0e-9);
    }

    #[test]
    fn expired_and_non_finite_contracts_are_excluded() {
        let mut expired = contract(100.0, OptionRight::Call, 1, 1.0, 1.0);
        expired.instrument.expiration_timestamp = NOW;
        let mut invalid = contract(110.0, OptionRight::Call, 1, 1.0, 1.0);
        invalid.market.mark_iv_percent = f64::INFINITY;
        let snapshot = calculate_gex_at(&chain(vec![expired, invalid]), &Config::default(), NOW);
        assert!(snapshot.strikes.is_empty());
    }

    #[test]
    fn gamma_flip_is_scanned_and_bisected() {
        let source = chain(vec![
            contract(80.0, OptionRight::Call, 7, 50.0, 1.0),
            contract(120.0, OptionRight::Put, 7, 50.0, 1.0),
        ]);
        let snapshot = calculate_gex_at(&source, &Config::default(), NOW);
        assert!(snapshot.gamma_flip.is_some());

        let no_crossing = chain(vec![contract(100.0, OptionRight::Call, 7, 50.0, 1.0)]);
        assert!(
            calculate_gex_at(&no_crossing, &Config::default(), NOW)
                .gamma_flip
                .is_none()
        );
    }

    #[test]
    fn multiple_gamma_flips_choose_crossing_nearest_spot() {
        let source = chain(vec![
            contract(75.0, OptionRight::Call, 7, 30.0, 1.0),
            contract(90.0, OptionRight::Put, 7, 30.0, 1.0),
            contract(110.0, OptionRight::Call, 7, 30.0, 1.0),
            contract(125.0, OptionRight::Put, 7, 30.0, 1.0),
        ]);
        let selected = select_contracts(&source, GexExpiryFilter::SevenDays, 0.0, NOW);
        let mut crossings = Vec::new();
        let mut previous_price = 70.0;
        let mut previous = proxy_total_at_price(&selected, previous_price, NOW).expect("proxy");
        for price in 71..=130 {
            let price = f64::from(price);
            let value = proxy_total_at_price(&selected, price, NOW).expect("proxy");
            if previous.signum() != value.signum() {
                crossings.push((previous_price, price));
            }
            previous_price = price;
            previous = value;
        }
        assert!(crossings.len() >= 2);
        let flip = find_gamma_flip(&selected, source.source_spot, NOW, 30.0).expect("flip");
        let nearest = crossings
            .iter()
            .map(|(a, b)| (a + b) * 0.5)
            .min_by(|a, b| {
                (a - source.source_spot)
                    .abs()
                    .total_cmp(&(b - source.source_spot).abs())
            })
            .expect("crossing");
        assert!((flip - nearest).abs() <= 1.0);
    }

    #[test]
    fn incomplete_config_uses_defaults_and_unknown_fields_are_ignored() {
        let cfg: Config = serde_json::from_str(r#"{"price_range_percent":20,"future_field":true}"#)
            .expect("backwards compatible");
        assert_eq!(cfg.price_range_percent, 20.0);
        assert_eq!(cfg.max_visible_strikes, 40);
        assert!(cfg.show_header_net_gex);
        assert!(cfg.show_header_gamma_flip);
        assert!(cfg.show_header_expiry);
        assert!(cfg.show_header_freshness);
        assert!(cfg.show_header_model);
        assert!(!cfg.show_header_derive_flow);
        assert!(!cfg.show_header_absolute_gex);
        assert!(!cfg.show_header_call_wall);
        assert!(!cfg.show_header_put_wall);
        assert!(!cfg.show_header_snapshot);
        assert!(cfg.show_intrinsic_stress_panel);
        assert!(cfg.show_gamma_vega_panel);
        assert!(cfg.show_gamma_liquidity_panel);
        assert_eq!(cfg.liquidity_depth_bps, 25.0);
        assert!(cfg.liquidity_reference_follow_link_group);
    }

    #[test]
    fn legacy_overlay_modes_are_ignored() {
        for mode in ["Levels", "NetHeatmap", "AbsoluteHeatmap", "ScenarioHeatmap"] {
            let mut config: GexLevelsConfig = serde_json::from_value(serde_json::json!({
                "overlay_mode": mode,
                "current_profile_width_percent": 15.0
            }))
            .expect("legacy GEX overlay config");
            config.migrate_legacy_defaults();
            assert_eq!(config.current_profile_width_percent, 7.0);
        }
    }

    #[test]
    fn p95_normalization_is_robust_and_sign_agnostic() {
        let mut values = (1..=19).map(f64::from).collect::<Vec<_>>();
        values.push(1_000_000.0);
        assert_eq!(gex_percentile_95(values), Some(19.0));
        assert_eq!(gex_percentile_95([0.0, f64::NAN, f64::INFINITY]), None);
        assert_eq!(gex_percentile_95([-1.0, -2.0, -3.0]), Some(3.0));
    }

    #[test]
    fn expiry_breakdown_and_dominant_expiry_are_preserved() {
        let source = chain(vec![
            contract(100.0, OptionRight::Call, 1, 30.0, 1.0),
            contract(100.0, OptionRight::Put, 7, 5.0, 1.0),
        ]);
        let snapshot = calculate_gex_at(&source, &Config::default(), NOW);
        assert_eq!(snapshot.strikes.len(), 1);
        assert_eq!(snapshot.expiry_strikes.len(), 2);
        let (expiry, contribution) =
            dominant_expiry(&snapshot.expiry_strikes, 100.0).expect("dominant");
        assert_eq!(expiry, NOW.saturating_add(MILLIS_PER_DAY));
        assert!(contribution > 0.5 && contribution <= 1.0);
    }

    #[test]
    fn scenario_curve_is_sorted_finite_and_matches_precise_flip() {
        let source = chain(vec![
            contract(90.0, OptionRight::Put, 7, 30.0, 1.0),
            contract(110.0, OptionRight::Call, 7, 30.0, 1.0),
        ]);
        let selected = select_contracts(&source, GexExpiryFilter::SevenDays, 0.0, NOW);
        let old_flip = find_gamma_flip(&selected, source.source_spot, NOW, 30.0);
        let snapshot = calculate_gex_at(&source, &Config::default(), NOW);
        assert_eq!(snapshot.scenario_curve.len(), DEFAULT_SCENARIO_POINTS);
        assert!(
            snapshot
                .scenario_curve
                .windows(2)
                .all(|pair| pair[0].price < pair[1].price)
        );
        assert!(
            snapshot
                .scenario_curve
                .iter()
                .all(|point| point.price.is_finite()
                    && point.net_gex_1pct.is_finite()
                    && point.absolute_gex_1pct.is_finite())
        );
        assert!(
            (snapshot.gamma_flip.expect("new flip") - old_flip.expect("old flip")).abs() < 1.0e-6
        );
    }

    #[test]
    fn new_config_roundtrips_with_zone_defaults() {
        let config = GexLevelsConfig::default();
        assert_eq!(config.current_profile_width_percent, 5.0);
        assert_eq!(config.positive_color, GexLevelColor::Cyan);
        assert_eq!(config.negative_color, GexLevelColor::Magenta);
        assert_eq!(config.history_minutes, 1440);
        let encoded = serde_json::to_string(&config).expect("serialize");
        let decoded: GexLevelsConfig = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(decoded, config);
    }

    #[test]
    fn previous_default_history_window_migrates_to_twenty_four_hours() {
        let mut config = GexLevelsConfig {
            history_minutes: 240,
            ..GexLevelsConfig::default()
        };
        config.migrate_legacy_defaults();
        assert_eq!(config.history_minutes, 1440);
    }

    #[test]
    fn legacy_auto_resolution_migrates_to_explicit_512_samples() {
        let resolution: GexScenarioResolution =
            serde_json::from_str("\"Auto\"").expect("legacy resolution");
        assert_eq!(resolution, GexScenarioResolution::Samples512);
        assert_eq!(resolution.samples(), 512);
    }

    #[test]
    fn scenario_resolutions_and_range_are_dense_and_bounded() {
        let source = chain(vec![contract(100.0, OptionRight::Call, 7, 10.0, 1.0)]);
        for (resolution, expected) in [
            (GexScenarioResolution::Samples256, 256),
            (GexScenarioResolution::Samples512, 512),
        ] {
            let snapshot = calculate_gex_at(
                &source,
                &Config {
                    scenario_resolution: resolution,
                    ..Config::default()
                },
                NOW,
            );
            assert_eq!(snapshot.scenario_curve.len(), expected);
            assert!(snapshot.scenario_curve.first().expect("first").price <= 70.0);
            assert!(snapshot.scenario_curve.last().expect("last").price >= 130.0);
            assert!(
                snapshot
                    .scenario_curve
                    .iter()
                    .all(|point| point.absolute_gex_1pct >= 0.0)
            );
        }
    }

    fn zone_snapshot(observed_at: u64, values: &[(f64, f64)]) -> Arc<GexSnapshot> {
        let strikes = values
            .iter()
            .map(|(strike, net)| GexStrike {
                strike: *strike,
                call_gex_1pct: net.max(0.0),
                put_gex_1pct: net.min(0.0),
                net_gex_1pct: *net,
                absolute_gamma_1pct: net.abs(),
                call_open_interest: net.max(0.0),
                put_open_interest: (-net).max(0.0),
                expiration_count: 1,
                gamma_provenance: GexGammaProvenance::Derived,
            })
            .collect::<Vec<_>>();
        Arc::new(GexSnapshot {
            provider: OptionsProvider::Deribit,
            underlying: OptionsUnderlying::Btc,
            model: GexSignModel::CallPutOiProxy,
            expiry_filter: GexExpiryFilter::SevenDays,
            gamma_source: GexGammaSource::BlackScholesDerived,
            gamma_provenance: GexGammaProvenance::Derived,
            source_spot: 100_000.0,
            observed_at: UnixMs::new(observed_at),
            calculated_at: UnixMs::new(observed_at),
            net_gex_1pct: Some(values.iter().map(|(_, value)| value).sum()),
            absolute_gex_1pct: values.iter().map(|(_, value)| value.abs()).sum(),
            call_wall: None,
            put_wall: None,
            gamma_flip: None,
            intrinsic_stress: IntrinsicStressMetrics::default(),
            gamma_vega: GammaVegaMetrics::default(),
            strikes: strikes.into(),
            expiry_strikes: Arc::from([]),
            scenario_curve: Arc::from([]),
            scale_p95: 1.0,
            proxy: None,
        })
    }

    fn quantwheel_zone_snapshot(observed_at: u64, values: &[(f64, f64)]) -> Arc<GexSnapshot> {
        let mut snapshot = zone_snapshot(observed_at, values);
        {
            let value = Arc::make_mut(&mut snapshot);
            value.provider = OptionsProvider::QuantWheel;
            value.underlying = OptionsUnderlying::Ndx;
            value.gamma_source = GexGammaSource::ProviderNativePreferred;
            Arc::make_mut(&mut value.strikes)
                .iter_mut()
                .for_each(|strike| strike.gamma_provenance = GexGammaProvenance::Native);
            value.gamma_provenance = GexGammaProvenance::Native;
        }
        snapshot
    }

    fn mapped_quantwheel_zone_snapshot(
        observed_at: u64,
        source_spot: f64,
        target_spot: f64,
        values: &[(f64, f64)],
    ) -> Arc<GexSnapshot> {
        let mut source = quantwheel_zone_snapshot(observed_at, values);
        Arc::make_mut(&mut source).source_spot = source_spot;
        Arc::new(
            map_proxy_snapshot(&source, "NDX", "NAS100", target_spot)
                .expect("valid QuantWheel proxy mapping"),
        )
    }

    fn zone_source_members(zone: &GexZone) -> Vec<f64> {
        let mut members = zone
            .bands
            .iter()
            .filter_map(|band| band.source_strike)
            .collect::<Vec<_>>();
        members.sort_by(f64::total_cmp);
        members
    }

    fn active_zone_with_sources<'a>(frame: &'a GexZoneFrame, expected: &[f64]) -> &'a GexZone {
        frame
            .zones
            .iter()
            .find(|zone| {
                zone.state == GexZoneState::Active && zone_source_members(zone) == expected
            })
            .expect("active zone with expected source members")
    }

    fn proxy_point(
        observed_at: i64,
        total_gex: f64,
        levels: [Option<f64>; 4],
    ) -> Arc<GexProxyHistoryPoint> {
        Arc::new(GexProxyHistoryPoint {
            observed_at,
            source_spot: 100_000.0,
            total_gex,
            flip_level: Some(100_000.0),
            call_wall: Some(101_000.0),
            put_wall: Some(99_000.0),
            positive_level_1: levels[0],
            positive_level_2: levels[1],
            negative_level_1: levels[2],
            negative_level_2: levels[3],
        })
    }

    #[test]
    fn proxy_levels_map_to_four_synthetic_roles_without_invented_density() {
        let point = proxy_point(
            0,
            10.0,
            [
                Some(101_000.0),
                Some(102_000.0),
                Some(99_000.0),
                Some(98_000.0),
            ],
        );
        let frames = build_gex_proxy_zone_frames(&[point], &[], 5 * 60_000, UnixMs::new(0));
        let zones = &frames[0].zones;
        assert_eq!(zones.len(), 4);
        assert_eq!(
            zones.iter().map(|zone| zone.role).collect::<Vec<_>>(),
            GexProxyZoneRole::ALL
        );
        assert!(zones.iter().all(
            |zone| zone.lower_price < zone.center_price && zone.center_price < zone.upper_price
        ));
    }

    #[test]
    fn proxy_half_width_uses_nearest_level_and_strict_minimum_maximum() {
        let nearest = proxy_point(0, 1.0, [Some(100_000.0), Some(101_000.0), None, None]);
        let nearest_zones = gex_proxy_zones_for_point(&nearest, 1.0);
        assert!(
            (nearest_zones[0].upper_price - nearest_zones[0].center_price - 120.0).abs() < 1e-9
        );

        let minimum = proxy_point(0, 1.0, [Some(100_000.0), Some(100_100.0), None, None]);
        let minimum_zones = gex_proxy_zones_for_point(&minimum, 1.0);
        assert!((minimum_zones[0].upper_price - minimum_zones[0].center_price - 50.0).abs() < 1e-9);

        let maximum = proxy_point(0, 1.0, [Some(90_000.0), Some(110_000.0), None, None]);
        let maximum_zones = gex_proxy_zones_for_point(&maximum, 1.0);
        assert!(
            (maximum_zones[0].upper_price - maximum_zones[0].center_price - 250.0).abs() < 1e-9
        );

        let single = proxy_point(0, 1.0, [Some(100_000.0), None, None, None]);
        let single_zone = gex_proxy_zones_for_point(&single, 1.0);
        assert!((single_zone[0].upper_price - single_zone[0].center_price - 100.0).abs() < 1e-9);
    }

    #[test]
    fn proxy_wall_confirmation_and_p95_asinh_strength_are_bounded() {
        let history = (1..=19)
            .map(|value| {
                proxy_point(
                    i64::from(value) * 5 * 60_000,
                    f64::from(value),
                    [Some(101_000.0), Some(102_000.0), Some(99_000.0), None],
                )
            })
            .chain([proxy_point(
                20 * 5 * 60_000,
                1_000_000.0,
                [Some(101_000.0), Some(102_000.0), Some(99_000.0), None],
            )])
            .collect::<Vec<_>>();
        let p95 = gex_percentile_95(history.iter().map(|point| point.total_gex)).unwrap();
        assert_eq!(p95, 19.0);
        let zones = gex_proxy_zones_for_point(&history[18], p95);
        let positive = zones
            .iter()
            .find(|zone| zone.role == GexProxyZoneRole::PositivePrimary)
            .unwrap();
        let secondary = zones
            .iter()
            .find(|zone| zone.role == GexProxyZoneRole::PositiveSecondary)
            .unwrap();
        assert!(positive.wall_confirmed);
        assert_eq!(positive.strength, 1.0);
        assert!((secondary.strength - 0.62).abs() < 1e-6);
        assert!(
            zones
                .iter()
                .all(|zone| (0.0..=1.0).contains(&zone.strength))
        );
    }

    #[test]
    fn proxy_timeframes_cover_source_interval_and_full_large_bucket() {
        let point = proxy_point(0, 1.0, [Some(101_000.0), None, None, None]);
        let one_minute = build_gex_proxy_zone_frames(
            std::slice::from_ref(&point),
            &[],
            60_000,
            UnixMs::new(4 * 60_000),
        );
        assert_eq!(one_minute.len(), 5);
        assert_eq!(one_minute.first().unwrap().bucket_start, UnixMs::new(0));
        assert_eq!(
            one_minute.last().unwrap().bucket_end,
            UnixMs::new(5 * 60_000)
        );
        assert!(
            one_minute
                .windows(2)
                .all(|pair| pair[0].bucket_end == pair[1].bucket_start)
        );

        let point = proxy_point(7 * 60_000, 1.0, [Some(101_000.0), None, None, None]);
        let fifteen =
            build_gex_proxy_zone_frames(&[point], &[], 15 * 60_000, UnixMs::new(15 * 60_000));
        assert_eq!(fifteen.len(), 1);
        assert_eq!(fifteen[0].bucket_start, UnixMs::new(0));
        assert_eq!(fifteen[0].bucket_end, UnixMs::new(15 * 60_000));
    }

    #[test]
    fn proxy_bucket_precedence_excludes_only_actual_deribit_coverage() {
        let history = vec![
            proxy_point(0, 1.0, [Some(101_000.0), None, None, None]),
            proxy_point(5 * 60_000, 1.0, [Some(101_000.0), None, None, None]),
            proxy_point(10 * 60_000, 1.0, [Some(101_000.0), None, None, None]),
        ];
        let deribit = vec![zone_snapshot(5 * 60_000 + 1, &[])];
        let frames =
            build_gex_proxy_zone_frames(&history, &deribit, 5 * 60_000, UnixMs::new(10 * 60_000));
        assert_eq!(
            frames
                .iter()
                .map(|frame| frame.bucket_start.as_u64())
                .collect::<Vec<_>>(),
            vec![0, 10 * 60_000]
        );
        let covered = deribit
            .iter()
            .map(|snapshot| gex_bucket_start(snapshot.observed_at, 5 * 60_000))
            .collect::<FxHashSet<_>>();
        assert!(
            frames
                .iter()
                .all(|frame| !covered.contains(&frame.bucket_start))
        );
    }

    #[test]
    fn zone_clustering_groups_adjacent_sign_and_splits_sign_or_large_gap() {
        let adjacent = zone_snapshot(
            300_001,
            &[(69_900.0, 8.0), (70_000.0, 10.0), (70_100.0, 7.0)],
        );
        let zones = extract_gex_zones(&adjacent, 0.12, 6, 6);
        assert_eq!(zones.len(), 1);
        assert_eq!(zones[0].peak_price, 70_000.0);

        let sign_change = zone_snapshot(300_001, &[(69_900.0, 8.0), (70_000.0, -10.0)]);
        assert_eq!(extract_gex_zones(&sign_change, 0.12, 6, 6).len(), 2);

        let large_gap = zone_snapshot(
            300_001,
            &[(69_900.0, 8.0), (70_000.0, 10.0), (72_000.0, 9.0)],
        );
        assert_eq!(extract_gex_zones(&large_gap, 0.12, 6, 6).len(), 2);
    }

    #[test]
    fn isolated_zone_and_padding_are_strictly_bounded() {
        let isolated = zone_snapshot(300_001, &[(70_000.0, 10.0)]);
        let zone = extract_gex_zones(&isolated, 0.12, 6, 6).remove(0);
        let half = (zone.upper_price - zone.lower_price) * 0.5;
        assert!((25.0..=80.0).contains(&half));

        let cluster = zone_snapshot(300_001, &[(69_000.0, 8.0), (70_000.0, 10.0)]);
        let zone = extract_gex_zones(&cluster, 0.12, 6, 6).remove(0);
        assert_eq!(zone.lower_price, zone.bands[0].lower_price);
        assert_eq!(zone.upper_price, zone.bands[1].upper_price);
        assert!(zone.bands[0].upper_price < zone.bands[1].lower_price);
    }

    #[test]
    fn zone_cluster_builds_distinct_strike_weighted_bands() {
        let snapshot = zone_snapshot(
            300_001,
            &[(65_000.0, 3.0), (66_000.0, 6.0), (67_000.0, 10.0)],
        );
        let zone = extract_gex_zones(&snapshot, 0.12, 6, 6).remove(0);

        assert_eq!(zone.bands.len(), 3);
        assert!(
            zone.bands
                .windows(2)
                .all(|pair| pair[0].upper_price < pair[1].lower_price)
        );
        let weak = zone
            .bands
            .iter()
            .find(|band| band.strike == 65_000.0)
            .expect("65k band");
        let expected_weak = ((3.0_f64 / 10.0).asinh() / 1.0_f64.asinh()) as f32;
        assert!((weak.normalized_strength - expected_weak).abs() < 1.0e-6);
        assert!(weak.normalized_strength < zone.normalized_strength);
        assert_eq!(zone.peak_price, 67_000.0);
    }

    #[test]
    fn zone_limits_and_positive_negative_scales_are_independent() {
        let values = (0..20)
            .map(|index| {
                let value = if index % 2 == 0 {
                    10.0 + f64::from(index)
                } else {
                    -1_000.0 - f64::from(index)
                };
                (60_000.0 + f64::from(index) * 1_000.0, value)
            })
            .collect::<Vec<_>>();
        let snapshot = zone_snapshot(300_001, &values);
        let zones = extract_gex_zones(&snapshot, 0.12, 6, 6);
        assert!(
            zones
                .iter()
                .filter(|zone| zone.sign == GexZoneSign::Positive)
                .count()
                <= 6
        );
        assert!(
            zones
                .iter()
                .filter(|zone| zone.sign == GexZoneSign::Negative)
                .count()
                <= 6
        );
        assert!(zones.iter().any(|zone| zone.sign == GexZoneSign::Positive));
        assert!(zones.iter().any(|zone| zone.sign == GexZoneSign::Negative));
    }

    #[test]
    fn tracking_preserves_id_by_overlap_and_peak_distance() {
        let config = GexLevelsConfig::default();
        let overlap = vec![
            zone_snapshot(300_001, &[(70_000.0, 10.0), (70_100.0, 8.0)]),
            zone_snapshot(600_001, &[(70_050.0, 11.0), (70_150.0, 7.0)]),
        ];
        let frames = build_gex_zone_frames(&overlap, 300_000, &config);
        assert_eq!(frames[0].zones[0].id, frames[1].zones[0].id);

        let nearby = vec![
            zone_snapshot(300_001, &[(70_000.0, 10.0)]),
            zone_snapshot(600_001, &[(70_150.0, 9.0)]),
        ];
        let frames = build_gex_zone_frames(&nearby, 300_000, &config);
        assert_eq!(frames[0].zones[0].id, frames[1].zones[0].id);
    }

    #[test]
    fn frame_uses_latest_snapshot_inside_chart_bucket() {
        let history = vec![
            zone_snapshot(300_001, &[(70_000.0, 8.0)]),
            zone_snapshot(450_001, &[(72_000.0, 12.0)]),
        ];
        let frames = build_gex_zone_frames(&history, 300_000, &GexLevelsConfig::default());
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].zones[0].peak_price, 72_000.0);
    }

    #[test]
    fn quantwheel_intervals_preserve_exact_times_and_same_candle_observations() {
        let history = vec![
            quantwheel_zone_snapshot(81_677_000, &[(29_700.0, 8.0)]),
            quantwheel_zone_snapshot(84_000_000, &[(29_730.0, 12.0)]),
        ];
        let frames = build_quantwheel_gex_intervals(&history, &GexLevelsConfig::default());

        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].bucket_start, UnixMs::new(81_677_000));
        assert_eq!(frames[0].interval_end, Some(UnixMs::new(84_000_000)));
        assert_eq!(frames[1].bucket_start, UnixMs::new(84_000_000));
        assert_eq!(frames[1].interval_end, None);
        assert_eq!(frames[0].zones[0].peak_price, 29_700.0);
        assert_eq!(frames[1].zones[0].peak_price, 29_730.0);
    }

    #[test]
    fn quantwheel_interval_semantics_are_timeframe_independent_and_keep_equal_times() {
        let history = vec![
            quantwheel_zone_snapshot(81_000_000, &[(29_700.0, 8.0)]),
            quantwheel_zone_snapshot(81_000_000, &[(29_710.0, 9.0)]),
            quantwheel_zone_snapshot(84_000_000, &[(29_730.0, 12.0)]),
        ];
        let expected = build_quantwheel_gex_intervals(&history, &GexLevelsConfig::default());

        for _chart_timeframe_ms in [60_000, 300_000, 900_000, 3_600_000] {
            let actual = build_quantwheel_gex_intervals(&history, &GexLevelsConfig::default());
            assert_eq!(actual, expected);
        }
        assert_eq!(expected.len(), 3);
        assert_eq!(expected[0].interval_end, Some(UnixMs::new(81_000_000)));
        assert_eq!(expected[1].interval_end, Some(UnixMs::new(84_000_000)));
    }

    #[test]
    fn quantwheel_source_identity_beats_mapped_spatial_collision_and_survives_zone_cap() {
        let values = &[
            (29_750.0, 10.0),
            (29_760.0, 0.0),
            (29_770.0, 9.0),
            (29_775.0, 8.0),
        ];
        let history = vec![
            mapped_quantwheel_zone_snapshot(1_000, 30_000.0, 30_000.0, values),
            // This anchor places current 29_750 spatially on top of the old
            // 29_770 zone, reproducing the live collision.
            mapped_quantwheel_zone_snapshot(2_000, 30_000.0, 30_021.0, values),
        ];
        let config = GexLevelsConfig {
            max_positive_zones: 2,
            fade_buckets: 2,
            ..GexLevelsConfig::default()
        };
        let frames = build_quantwheel_gex_intervals(&history, &config);
        let previous_29_750 = active_zone_with_sources(&frames[0], &[29_750.0]);
        let previous_29_770 = active_zone_with_sources(&frames[0], &[29_770.0, 29_775.0]);
        let current_29_750 = active_zone_with_sources(&frames[1], &[29_750.0]);
        let current_29_770 = active_zone_with_sources(&frames[1], &[29_770.0, 29_775.0]);

        assert!(
            (current_29_750.peak_price - previous_29_770.peak_price).abs()
                < (current_29_750.peak_price - previous_29_750.peak_price).abs()
        );
        assert_eq!(current_29_750.id, previous_29_750.id);
        assert_eq!(current_29_770.id, previous_29_770.id);
        assert_eq!(frames[1].zones.len(), 2);
        assert!(
            frames[1]
                .zones
                .iter()
                .all(|zone| zone.state == GexZoneState::Active)
        );
    }

    #[test]
    fn quantwheel_source_overlap_tracks_small_cluster_membership_change() {
        let history = vec![
            mapped_quantwheel_zone_snapshot(
                1_000,
                30_000.0,
                30_000.0,
                &[
                    (29_870.0, 10.0),
                    (29_875.0, 9.0),
                    (29_880.0, 8.0),
                    (29_890.0, 7.0),
                    (29_900.0, 6.0),
                ],
            ),
            mapped_quantwheel_zone_snapshot(
                2_000,
                30_000.0,
                30_030.0,
                &[
                    (29_870.0, 10.0),
                    (29_875.0, 9.0),
                    (29_880.0, 8.0),
                    (29_890.0, 7.0),
                    // Retain the neighboring strike in raw extraction so the
                    // local-gap geometry is unchanged, but drop it below the
                    // configured strength threshold in this observation.
                    (29_900.0, 0.01),
                ],
            ),
        ];
        let frames = build_quantwheel_gex_intervals(&history, &GexLevelsConfig::default());
        let previous = active_zone_with_sources(
            &frames[0],
            &[29_870.0, 29_875.0, 29_880.0, 29_890.0, 29_900.0],
        );
        let current =
            active_zone_with_sources(&frames[1], &[29_870.0, 29_875.0, 29_880.0, 29_890.0]);
        assert_eq!(current.id, previous.id);
    }

    #[test]
    fn quantwheel_disjoint_explicit_sources_do_not_use_spatial_fallback() {
        let history = vec![
            mapped_quantwheel_zone_snapshot(
                1_000,
                30_000.0,
                30_000.0,
                &[(29_770.0, 10.0), (29_775.0, 9.0)],
            ),
            mapped_quantwheel_zone_snapshot(2_000, 30_000.0, 30_021.0, &[(29_750.0, 10.0)]),
        ];
        let frames = build_quantwheel_gex_intervals(&history, &GexLevelsConfig::default());
        let previous = active_zone_with_sources(&frames[0], &[29_770.0, 29_775.0]);
        let current = active_zone_with_sources(&frames[1], &[29_750.0]);

        assert!((current.peak_price - previous.peak_price).abs() < 2.0);
        assert_ne!(current.id, previous.id);
    }

    #[test]
    fn quantwheel_identity_does_not_cross_expiry_series() {
        let first = mapped_quantwheel_zone_snapshot(1_000, 30_000.0, 30_000.0, &[(29_750.0, 10.0)]);
        let mut second =
            mapped_quantwheel_zone_snapshot(2_000, 30_000.0, 30_000.0, &[(29_750.0, 10.0)]);
        Arc::make_mut(&mut second).expiry_filter = GexExpiryFilter::ThreeDays;
        let frames = build_quantwheel_gex_intervals(&[first, second], &GexLevelsConfig::default());

        assert_ne!(frames[0].zones[0].id, frames[1].zones[0].id);
        assert!(
            frames[1]
                .zones
                .iter()
                .all(|zone| zone.state == GexZoneState::Active)
        );
    }

    #[test]
    fn quantwheel_split_and_merge_have_single_deterministic_inheritance() {
        let split = vec![
            mapped_quantwheel_zone_snapshot(
                1_000,
                30_000.0,
                30_000.0,
                &[(29_770.0, 9.0), (29_775.0, 10.0)],
            ),
            mapped_quantwheel_zone_snapshot(
                2_000,
                30_000.0,
                30_000.0,
                &[(29_770.0, 9.0), (29_772.5, 0.0), (29_775.0, 10.0)],
            ),
        ];
        let split_frames = build_quantwheel_gex_intervals(&split, &GexLevelsConfig::default());
        let parent = active_zone_with_sources(&split_frames[0], &[29_770.0, 29_775.0]);
        assert_eq!(
            active_zone_with_sources(&split_frames[1], &[29_775.0]).id,
            parent.id
        );
        assert_ne!(
            active_zone_with_sources(&split_frames[1], &[29_770.0]).id,
            parent.id
        );

        let merge = vec![
            mapped_quantwheel_zone_snapshot(
                1_000,
                30_000.0,
                30_000.0,
                &[(29_770.0, 9.0), (29_772.5, 0.0), (29_775.0, 10.0)],
            ),
            mapped_quantwheel_zone_snapshot(
                2_000,
                30_000.0,
                30_000.0,
                &[(29_770.0, 9.0), (29_775.0, 10.0)],
            ),
        ];
        let merge_frames = build_quantwheel_gex_intervals(&merge, &GexLevelsConfig::default());
        let dominant_predecessor = active_zone_with_sources(&merge_frames[0], &[29_775.0]);
        let merged = active_zone_with_sources(&merge_frames[1], &[29_770.0, 29_775.0]);
        assert_eq!(merged.id, dominant_predecessor.id);
    }

    #[test]
    fn quantwheel_sign_flip_starts_a_new_active_identity() {
        let history = vec![
            mapped_quantwheel_zone_snapshot(1_000, 30_000.0, 30_000.0, &[(30_000.0, 10.0)]),
            mapped_quantwheel_zone_snapshot(2_000, 30_000.0, 30_000.0, &[(30_000.0, -10.0)]),
        ];
        let frames = build_quantwheel_gex_intervals(&history, &GexLevelsConfig::default());
        let positive_id = active_zone_with_sources(&frames[0], &[30_000.0]).id;
        let negative = active_zone_with_sources(&frames[1], &[30_000.0]);
        assert_eq!(negative.sign, GexZoneSign::Negative);
        assert_ne!(negative.id, positive_id);
        assert!(!frames[1].zones.iter().any(|zone| {
            zone.sign == GexZoneSign::Positive && zone.state == GexZoneState::Active
        }));
    }

    #[test]
    fn quantwheel_stable_source_identity_does_not_freeze_mapped_price() {
        let history = [29_737.342, 29_734.347, 29_751.322]
            .into_iter()
            .enumerate()
            .map(|(index, target_spot)| {
                mapped_quantwheel_zone_snapshot(
                    1_000 + index as u64 * 1_000,
                    29_700.0,
                    target_spot,
                    &[(29_700.0, 10.0)],
                )
            })
            .collect::<Vec<_>>();
        let frames = build_quantwheel_gex_intervals(&history, &GexLevelsConfig::default());
        let zones = frames
            .iter()
            .map(|frame| active_zone_with_sources(frame, &[29_700.0]))
            .collect::<Vec<_>>();

        assert!(zones.windows(2).all(|pair| pair[0].id == pair[1].id));
        let expected = [29_737.342, 29_734.347, 29_751.322];
        assert!(
            zones
                .iter()
                .zip(expected)
                .all(|(zone, expected)| (zone.peak_price - expected).abs() < 1.0e-9)
        );
    }

    #[test]
    fn deribit_bucket_frames_retain_existing_count_and_boundaries() {
        let history = vec![
            zone_snapshot(300_001, &[(70_000.0, 8.0)]),
            zone_snapshot(450_001, &[(72_000.0, 12.0)]),
            zone_snapshot(600_001, &[(73_000.0, 10.0)]),
        ];
        let frames = build_gex_zone_frames(&history, 300_000, &GexLevelsConfig::default());

        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].bucket_start, UnixMs::new(300_000));
        assert_eq!(frames[0].interval_end, Some(UnixMs::new(600_000)));
        assert_eq!(frames[0].zones[0].peak_price, 72_000.0);
        assert_eq!(frames[1].bucket_start, UnixMs::new(600_000));
        assert_eq!(frames[1].interval_end, Some(UnixMs::new(900_000)));
    }

    #[test]
    fn split_and_merge_choose_one_previous_id_deterministically() {
        let config = GexLevelsConfig::default();
        let split = vec![
            zone_snapshot(
                300_001,
                &[(69_900.0, 9.0), (70_000.0, 10.0), (70_100.0, 8.0)],
            ),
            zone_snapshot(
                600_001,
                &[(69_900.0, 9.0), (70_000.0, 0.0), (70_100.0, 8.0)],
            ),
        ];
        let frames = build_gex_zone_frames(&split, 300_000, &config);
        let old_id = frames[0].zones[0].id;
        assert_eq!(
            frames[1]
                .zones
                .iter()
                .filter(|zone| zone.id == old_id)
                .count(),
            1
        );

        let merge = vec![
            zone_snapshot(
                300_001,
                &[(69_900.0, 9.0), (70_000.0, 0.0), (70_100.0, 8.0)],
            ),
            zone_snapshot(
                600_001,
                &[(69_900.0, 9.0), (70_000.0, 10.0), (70_100.0, 8.0)],
            ),
        ];
        let first = build_gex_zone_frames(&merge, 300_000, &config);
        let second = build_gex_zone_frames(&merge, 300_000, &config);
        assert_eq!(first, second);
        let previous_ids = first[0]
            .zones
            .iter()
            .map(|zone| zone.id)
            .collect::<FxHashSet<_>>();
        assert!(previous_ids.contains(&first[1].zones[0].id));
    }

    #[test]
    fn persistence_fade_expiration_and_real_gap_are_bounded() {
        let config = GexLevelsConfig::default();
        let history = vec![
            zone_snapshot(300_001, &[(70_000.0, 10.0)]),
            zone_snapshot(600_001, &[(70_000.0, 10.0)]),
            zone_snapshot(900_001, &[(70_000.0, 10.0)]),
            zone_snapshot(1_200_001, &[]),
            zone_snapshot(1_500_001, &[]),
            zone_snapshot(1_800_001, &[]),
        ];
        let frames = build_gex_zone_frames(&history, 300_000, &config);
        assert!(frames[2].zones[0].persistence_score >= 0.65);
        assert_eq!(frames[3].zones[0].state, GexZoneState::Fading);
        assert_eq!(frames[3].zones[0].missing_buckets, 1);
        assert_eq!(frames[4].zones[0].missing_buckets, 2);
        assert!(frames[5].zones.is_empty());

        let gap = vec![
            zone_snapshot(300_001, &[(70_000.0, 10.0)]),
            zone_snapshot(1_200_001, &[(70_000.0, 10.0)]),
        ];
        let frames = build_gex_zone_frames(&gap, 300_000, &config);
        assert_ne!(frames[0].zones[0].id, frames[1].zones[0].id);
    }

    #[test]
    fn persistent_zone_remains_visible_when_current_strength_is_below_threshold() {
        let config = GexLevelsConfig {
            minimum_zone_strength: 0.90,
            ..GexLevelsConfig::default()
        };
        let history = vec![
            zone_snapshot(
                300_001,
                &[(70_000.0, 10.0), (70_500.0, 0.0), (71_000.0, 10.0)],
            ),
            zone_snapshot(
                600_001,
                &[(70_000.0, 0.1), (70_500.0, 0.0), (71_000.0, 10.0)],
            ),
        ];
        let frames = build_gex_zone_frames(&history, 300_000, &config);
        let old_id = frames[0]
            .zones
            .iter()
            .find(|zone| zone.peak_price == 70_000.0)
            .expect("initial zone")
            .id;
        let fading = frames[1]
            .zones
            .iter()
            .find(|zone| zone.id == old_id)
            .expect("persistent fading zone");
        assert_eq!(fading.state, GexZoneState::Fading);
        assert!(fading.persistence_score >= 0.25);
    }

    #[test]
    fn deterministic_five_minute_zone_render_fixture_has_gap_and_fade() {
        let history = vec![
            zone_snapshot(
                300_001,
                &[(65_000.0, -12.0), (70_000.0, 15.0), (72_000.0, 11.0)],
            ),
            zone_snapshot(
                600_001,
                &[(65_100.0, -13.0), (70_000.0, 16.0), (72_000.0, 10.0)],
            ),
            zone_snapshot(
                900_001,
                &[
                    (65_200.0, -11.0),
                    (69_950.0, 14.0),
                    (70_050.0, 15.0),
                    (72_000.0, 9.0),
                ],
            ),
            // No 1_200_000 bucket: this must remain a real visual gap.
            zone_snapshot(1_500_001, &[(65_300.0, -10.0), (70_000.0, 14.0)]),
            zone_snapshot(1_800_001, &[(65_400.0, -9.0)]),
        ];
        let frames = build_gex_zone_frames(&history, 300_000, &GexLevelsConfig::default());
        assert_eq!(frames.len(), 5);
        assert_eq!(
            frames[3]
                .bucket_start
                .saturating_diff(frames[2].bucket_start),
            600_000
        );
        assert!(
            frames[0]
                .zones
                .iter()
                .any(|zone| zone.sign == GexZoneSign::Positive)
        );
        assert!(
            frames[0]
                .zones
                .iter()
                .any(|zone| zone.sign == GexZoneSign::Negative)
        );
        assert!(
            frames[4]
                .zones
                .iter()
                .any(|zone| zone.state == GexZoneState::Fading)
        );
    }

    #[test]
    fn native_gamma_is_preferred_only_when_valid_and_fresh() {
        let mut native = contract(100.0, OptionRight::Call, 7, 10.0, 1.0);
        native.market.native_gamma = Some(0.25);
        native.market.native_gamma_observed_at = Some(NOW.saturating_sub(1_000));
        let fresh = calculate_gex_at(&chain(vec![native.clone()]), &Config::default(), NOW);
        assert_eq!(fresh.gamma_provenance, GexGammaProvenance::Native);

        native.market.native_gamma_observed_at =
            Some(NOW.saturating_sub(NATIVE_GAMMA_MAX_AGE_MS + 1));
        let stale = calculate_gex_at(&chain(vec![native.clone()]), &Config::default(), NOW);
        assert_eq!(stale.gamma_provenance, GexGammaProvenance::Derived);

        native.market.native_gamma = Some(f64::NAN);
        let invalid = calculate_gex_at(&chain(vec![native]), &Config::default(), NOW);
        assert_eq!(invalid.gamma_provenance, GexGammaProvenance::Derived);
    }

    #[test]
    fn partial_native_coverage_is_reported_as_mixed() {
        let mut native = contract(90.0, OptionRight::Call, 7, 10.0, 1.0);
        native.market.native_gamma = Some(0.25);
        native.market.native_gamma_observed_at = Some(NOW);
        let derived = contract(110.0, OptionRight::Put, 7, 10.0, 1.0);
        let snapshot = calculate_gex_at(&chain(vec![native, derived]), &Config::default(), NOW);
        assert_eq!(snapshot.gamma_provenance, GexGammaProvenance::Mixed);
        assert!(
            snapshot
                .scenario_curve
                .iter()
                .all(|point| point.absolute_gex_1pct >= 0.0)
        );
    }

    fn maker_trade(
        id: &str,
        contract: &RawOptionContractSnapshot,
        timestamp: UnixMs,
        side: DeriveMakerSide,
    ) -> DeriveMakerTrade {
        DeriveMakerTrade {
            trade_id: id.to_owned(),
            key: OptionContractMatchKey::new(
                contract.instrument.underlying,
                contract.instrument.expiration_timestamp,
                contract.instrument.strike,
                contract.instrument.right,
            )
            .expect("match key"),
            expiration_timestamp: contract.instrument.expiration_timestamp,
            timestamp,
            side,
            amount: 1.0,
            mark_price: 1.0,
            index_price: 100.0,
        }
    }

    #[test]
    fn derive_flow_matches_exact_contracts_and_deduplicates_matched_share() {
        let mut call = contract(100.0, OptionRight::Call, 7, 10.0, 1.0);
        call.market.native_gamma = Some(0.01);
        call.market.native_gamma_observed_at = Some(NOW);
        let mut put = contract(100.0, OptionRight::Put, 7, 10.0, 1.0);
        put.market.native_gamma = Some(0.01);
        put.market.native_gamma_observed_at = Some(NOW);
        let source = chain(vec![call.clone(), put.clone()]);
        let trades = (0..5)
            .map(|index| {
                maker_trade(
                    &format!("buy-{index}"),
                    &call,
                    NOW.saturating_sub(60_000 + index * 1_000),
                    DeriveMakerSide::Buy,
                )
            })
            .collect::<Vec<_>>();
        let flow = calculate_derive_maker_gamma_flow(&source, &trades, &Config::default(), NOW);
        assert_eq!(flow.five_minutes.trade_count, 5);
        assert_eq!(
            flow.thirty_minutes.direction,
            ObservedGammaDirection::LongGamma
        );
        assert_eq!(flow.thirty_minutes.quality, FlowQuality::High);
        assert!((flow.thirty_minutes.imbalance - 1.0).abs() < f64::EPSILON);
        assert!((flow.thirty_minutes.matched_deribit_gex_share - 0.5).abs() < 1.0e-12);

        let mut nearest = maker_trade(
            "nearest-is-forbidden",
            &call,
            NOW.saturating_sub(1_000),
            DeriveMakerSide::Buy,
        );
        nearest.key.strike_cents += 1;
        let mut beyond_expiry = maker_trade(
            "expiry-too-far",
            &call,
            NOW.saturating_sub(1_000),
            DeriveMakerSide::Buy,
        );
        beyond_expiry.expiration_timestamp = call
            .instrument
            .expiration_timestamp
            .saturating_sub(13 * 60 * 60 * 1_000);
        let unmatched = calculate_derive_maker_gamma_flow(
            &source,
            &[nearest, beyond_expiry],
            &Config::default(),
            NOW,
        );
        assert_eq!(unmatched.thirty_minutes.trade_count, 0);
        assert_eq!(
            unmatched.thirty_minutes.direction,
            ObservedGammaDirection::Unavailable
        );
    }

    #[test]
    fn maker_side_sign_is_identical_for_calls_and_puts_and_windows_are_bounded() {
        let call = contract(100.0, OptionRight::Call, 7, 10.0, 1.0);
        let put = contract(100.0, OptionRight::Put, 7, 10.0, 1.0);
        let source = chain(vec![call.clone(), put.clone()]);
        for contract in [&call, &put] {
            let buy = calculate_derive_maker_gamma_flow(
                &source,
                &[maker_trade(
                    "buy",
                    contract,
                    NOW.saturating_sub(1_000),
                    DeriveMakerSide::Buy,
                )],
                &Config::default(),
                NOW,
            );
            assert!(buy.five_minutes.signed_gamma_flow_1pct > 0.0);
            let sell = calculate_derive_maker_gamma_flow(
                &source,
                &[maker_trade(
                    "sell",
                    contract,
                    NOW.saturating_sub(1_000),
                    DeriveMakerSide::Sell,
                )],
                &Config::default(),
                NOW,
            );
            assert!(sell.five_minutes.signed_gamma_flow_1pct < 0.0);
        }
        let trades = [
            maker_trade(
                "recent",
                &call,
                NOW.saturating_sub(4 * 60_000),
                DeriveMakerSide::Buy,
            ),
            maker_trade(
                "mid",
                &call,
                NOW.saturating_sub(20 * 60_000),
                DeriveMakerSide::Buy,
            ),
            maker_trade(
                "old",
                &call,
                NOW.saturating_sub(90 * 60_000),
                DeriveMakerSide::Sell,
            ),
        ];
        let flow = calculate_derive_maker_gamma_flow(&source, &trades, &Config::default(), NOW);
        assert_eq!(flow.five_minutes.trade_count, 1);
        assert_eq!(flow.thirty_minutes.trade_count, 2);
        assert_eq!(flow.two_hours.trade_count, 3);

        let classified = [
            maker_trade("m1", &call, NOW.saturating_sub(1_000), DeriveMakerSide::Buy),
            maker_trade("m2", &call, NOW.saturating_sub(2_000), DeriveMakerSide::Buy),
            maker_trade("m3", &call, NOW.saturating_sub(3_000), DeriveMakerSide::Buy),
            maker_trade(
                "m4",
                &call,
                NOW.saturating_sub(4_000),
                DeriveMakerSide::Sell,
            ),
        ];
        let medium =
            calculate_derive_maker_gamma_flow(&source, &classified, &Config::default(), NOW);
        assert_eq!(
            medium.thirty_minutes.direction,
            ObservedGammaDirection::LongGamma
        );
        assert_eq!(medium.thirty_minutes.quality, FlowQuality::Medium);
        let balanced =
            calculate_derive_maker_gamma_flow(&source, &classified[2..], &Config::default(), NOW);
        assert_eq!(
            balanced.thirty_minutes.direction,
            ObservedGammaDirection::Balanced
        );
        assert_eq!(balanced.thirty_minutes.quality, FlowQuality::Low);
    }

    #[test]
    fn derive_flow_is_independent_from_deribit_visual_filters() {
        let contract = contract(100.0, OptionRight::Call, 14, 10.0, 1.0);
        let source = chain(vec![contract.clone()]);
        let trade = maker_trade(
            "outside-visible-expiry",
            &contract,
            NOW.saturating_sub(1_000),
            DeriveMakerSide::Buy,
        );
        let restrictive = Config {
            expiry_filter: GexExpiryFilter::OneDay,
            min_open_interest: f64::MAX,
            ..Config::default()
        };

        let flow = calculate_derive_maker_gamma_flow(&source, &[trade], &restrictive, NOW);

        assert_eq!(flow.five_minutes.trade_count, 1);
        assert_eq!(
            flow.five_minutes.direction,
            ObservedGammaDirection::LongGamma
        );
        assert_eq!(flow.oi_proxy_comparison_30m.trade_count, 0);
        assert_eq!(
            flow.oi_proxy_comparison_30m.direction,
            ObservedGammaDirection::Unavailable
        );
        let snapshot = calculate_gex_at(&source, &restrictive, NOW);
        assert_eq!(
            oi_proxy_agreement(&snapshot, Some(&flow)),
            OiProxyAgreement::Insufficient
        );
    }

    #[test]
    fn agreement_and_derive_calculation_leave_deribit_structure_unchanged() {
        let call = contract(90.0, OptionRight::Call, 7, 20.0, 1.0);
        let put = contract(110.0, OptionRight::Put, 7, 5.0, 1.0);
        let source = chain(vec![call.clone(), put]);
        let config = Config::default();
        let before = calculate_gex_at(&source, &config, NOW);
        let zones_before = build_gex_zone_frames(
            &[Arc::new(before.clone())],
            300_000,
            &GexLevelsConfig::default(),
        );
        let trades = (0..5)
            .map(|index| {
                maker_trade(
                    &format!("agreement-{index}"),
                    &call,
                    NOW.saturating_sub(index * 1_000),
                    DeriveMakerSide::Buy,
                )
            })
            .collect::<Vec<_>>();
        let flow = calculate_derive_maker_gamma_flow(&source, &trades, &config, NOW);
        let after = calculate_gex_at(&source, &config, NOW);
        assert_eq!(before, after);
        assert_eq!(
            zones_before,
            build_gex_zone_frames(
                &[Arc::new(after.clone())],
                300_000,
                &GexLevelsConfig::default(),
            )
        );
        assert_eq!(
            oi_proxy_agreement(&before, Some(&flow)),
            OiProxyAgreement::Agree
        );

        let mut opposite = flow.clone();
        opposite.oi_proxy_comparison_30m.direction = ObservedGammaDirection::ShortGamma;
        assert_eq!(
            oi_proxy_agreement(&before, Some(&opposite)),
            OiProxyAgreement::Diverge
        );
        opposite.oi_proxy_comparison_30m.quality = FlowQuality::Low;
        assert_eq!(
            oi_proxy_agreement(&before, Some(&opposite)),
            OiProxyAgreement::Insufficient
        );
    }
}
