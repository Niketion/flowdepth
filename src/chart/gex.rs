use data::chart::gex::{
    Config, GEX_PROXY_BALANCED_SHARE, GammaLiquidityRegime, GexFreshness, GexLiquidityMetrics,
    GexProxyDirection, GexSignModel, GexSnapshot, gamma_liquidity_regime,
};
use exchange::{
    TickerInfo, UnixMs,
    adapter::MarketKind,
    depth::Depth,
    options::OptionsUnderlying,
    unit::qty::{SizeUnit, volume_size_unit},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub const LIQUIDITY_STALE_AFTER_MS: u64 = 10_000;
pub const LIQUIDITY_RECALCULATION_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum LiquidityDepthState {
    #[default]
    NoReference,
    WaitingForDepth,
    InvalidDepth,
    Ready,
    Stale,
}

#[derive(Debug, Clone, Copy)]
pub enum Message {
    ZoomIn,
    ZoomOut,
    AutoFit,
    Scrolled(iced::mouse::ScrollDelta),
    DragStarted,
    Dragged(iced::Point),
    DragEnded,
    SelectLiquidityReference,
}

#[derive(Debug, Clone, Copy)]
pub enum Action {
    ViewChanged,
}

pub struct GexChart {
    underlying: OptionsUnderlying,
    snapshot: Option<Arc<GexSnapshot>>,
    derive_flow: Option<Arc<data::chart::gex::DeriveMakerGammaFlow>>,
    freshness: GexFreshness,
    config: Config,
    visible_fraction: f64,
    center_offset: isize,
    dragging: bool,
    last_drag_y: Option<f32>,
    drag_remainder: f32,
    last_tick: Instant,
    error: Option<Arc<str>>,
    liquidity_reference: Option<TickerInfo>,
    liquidity_metrics: Option<GexLiquidityMetrics>,
    last_depth: Option<(Depth, UnixMs)>,
    last_liquidity_recalculation: Option<Instant>,
    logged_depth_state: Option<LiquidityDepthState>,
}

impl GexChart {
    pub fn new(
        underlying: OptionsUnderlying,
        config: Option<Config>,
        liquidity_reference: Option<TickerInfo>,
    ) -> Self {
        Self {
            underlying,
            snapshot: None,
            derive_flow: None,
            freshness: GexFreshness::Loading,
            config: config.unwrap_or_default(),
            visible_fraction: 1.0,
            center_offset: 0,
            dragging: false,
            last_drag_y: None,
            drag_remainder: 0.0,
            last_tick: Instant::now(),
            error: None,
            liquidity_reference,
            liquidity_metrics: None,
            last_depth: None,
            last_liquidity_recalculation: None,
            logged_depth_state: None,
        }
    }

    pub fn underlying(&self) -> OptionsUnderlying {
        self.underlying
    }

    pub fn set_underlying(&mut self, underlying: OptionsUnderlying) {
        if self.underlying != underlying {
            self.underlying = underlying;
            self.snapshot = None;
            self.derive_flow = None;
            self.freshness = GexFreshness::Loading;
            self.error = None;
            self.liquidity_metrics = None;
            self.last_depth = None;
            self.last_liquidity_recalculation = None;
            self.auto_fit();
        }
    }

    pub fn set_snapshot(
        &mut self,
        snapshot: Option<Arc<GexSnapshot>>,
        freshness: GexFreshness,
        error: Option<Arc<str>>,
    ) {
        if snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.underlying != self.underlying)
        {
            return;
        }
        self.snapshot = snapshot;
        self.freshness = freshness;
        self.error = error;
        if self.liquidity_metrics.is_none() {
            self.recalculate_liquidity_at(Instant::now());
        }
        self.last_tick = Instant::now();
    }

    pub fn snapshot(&self) -> Option<&Arc<GexSnapshot>> {
        self.snapshot.as_ref()
    }

    pub fn set_derive_flow(&mut self, flow: Option<Arc<data::chart::gex::DeriveMakerGammaFlow>>) {
        self.derive_flow = flow;
        self.last_tick = Instant::now();
    }

    pub fn derive_flow(&self) -> Option<&data::chart::gex::DeriveMakerGammaFlow> {
        self.derive_flow.as_deref()
    }

    pub fn freshness(&self) -> GexFreshness {
        self.freshness
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn set_config(&mut self, config: Config) {
        let liquidity_changed = self.config.liquidity_depth_bps != config.liquidity_depth_bps
            || self.config.sign_model != config.sign_model;
        let liquidity_panel_toggled =
            self.config.show_gamma_liquidity_panel != config.show_gamma_liquidity_panel;
        self.config = config;
        if liquidity_panel_toggled {
            self.liquidity_metrics = None;
            self.last_liquidity_recalculation = None;
            if self.config.show_gamma_liquidity_panel {
                self.recalculate_liquidity_at(Instant::now());
                // The first book received by the newly-created stream must also
                // publish immediately, even if an older retained book was usable.
                self.last_liquidity_recalculation = None;
            }
        } else if liquidity_changed {
            self.last_liquidity_recalculation = None;
            self.recalculate_liquidity_at(Instant::now());
        }
        self.last_tick = Instant::now();
    }

    pub fn liquidity_reference(&self) -> Option<TickerInfo> {
        self.liquidity_reference
    }

    pub fn proxy_target_spot(&self) -> Option<f64> {
        let price = self.last_depth.as_ref()?.0.mid_price()?.to_f64();
        (price.is_finite() && price > 0.0).then_some(price)
    }

    pub fn set_liquidity_reference(&mut self, reference: Option<TickerInfo>) {
        if self.liquidity_reference == reference {
            return;
        }
        self.liquidity_reference = reference;
        self.liquidity_metrics = None;
        self.last_depth = None;
        self.last_liquidity_recalculation = None;
        self.log_depth_state_transition();
        self.last_tick = Instant::now();
    }

    pub fn insert_depth(&mut self, depth: &Depth, observed_at: UnixMs) {
        self.insert_depth_at(depth, observed_at, Instant::now());
    }

    fn insert_depth_at(&mut self, depth: &Depth, observed_at: UnixMs, received_at: Instant) {
        self.last_depth = Some((depth.clone(), observed_at));
        if self.liquidity_reference.is_none() || !self.config.show_gamma_liquidity_panel {
            return;
        }
        let interval_elapsed = self.last_liquidity_recalculation.is_none_or(|last| {
            received_at.saturating_duration_since(last) >= LIQUIDITY_RECALCULATION_INTERVAL
        });
        if self.liquidity_metrics.is_none() || interval_elapsed {
            self.recalculate_liquidity_at(received_at);
            self.log_depth_state_transition();
            self.last_tick = received_at;
        }
    }

    pub fn liquidity_metrics(&self) -> Option<&GexLiquidityMetrics> {
        self.liquidity_metrics.as_ref()
    }

    pub fn liquidity_depth_state(&self) -> LiquidityDepthState {
        if self.liquidity_reference.is_none() {
            LiquidityDepthState::NoReference
        } else if let Some((_, observed_at)) = &self.last_depth {
            if UnixMs::now().saturating_diff(*observed_at) > LIQUIDITY_STALE_AFTER_MS {
                LiquidityDepthState::Stale
            } else if self.liquidity_metrics.is_some() {
                LiquidityDepthState::Ready
            } else {
                LiquidityDepthState::InvalidDepth
            }
        } else {
            LiquidityDepthState::WaitingForDepth
        }
    }

    fn recalculate_liquidity_at(&mut self, recalculated_at: Instant) {
        let (Some(reference), Some(snapshot), Some((depth, observed_at))) = (
            self.liquidity_reference,
            self.snapshot.as_deref(),
            self.last_depth.as_ref(),
        ) else {
            self.liquidity_metrics = None;
            return;
        };
        self.last_liquidity_recalculation = Some(recalculated_at);
        self.liquidity_metrics = calculate_liquidity_metrics(
            reference,
            *observed_at,
            depth,
            self.config.liquidity_depth_bps,
            snapshot,
            volume_size_unit(),
        );
        self.log_depth_state_transition();
    }

    fn log_depth_state_transition(&mut self) {
        let state = self.liquidity_depth_state();
        if self.logged_depth_state == Some(state) {
            return;
        }
        self.logged_depth_state = Some(state);
        if let Some(reference) = self.liquidity_reference {
            let (symbol, _) = reference.ticker.display_symbol_and_type();
            log::debug!(
                "GEX LiquidityDepthState ticker={} exchange={} state={}",
                symbol,
                reference.exchange(),
                match state {
                    LiquidityDepthState::NoReference => "no_reference",
                    LiquidityDepthState::WaitingForDepth => "waiting",
                    LiquidityDepthState::InvalidDepth => "invalid",
                    LiquidityDepthState::Ready => "ready",
                    LiquidityDepthState::Stale => "stale",
                }
            );
        }
    }

    pub fn last_tick(&self) -> Instant {
        self.last_tick
    }

    pub fn update(&mut self, message: Message) -> Action {
        match message {
            Message::ZoomIn if self.can_zoom_in() => {
                self.visible_fraction = (self.visible_fraction * 0.8).max(0.1);
            }
            Message::ZoomOut if self.can_zoom_out() => {
                self.visible_fraction = (self.visible_fraction * 1.25).min(1.0);
            }
            Message::ZoomIn | Message::ZoomOut => {}
            Message::AutoFit => self.auto_fit(),
            Message::Scrolled(delta) => {
                let rows = match delta {
                    iced::mouse::ScrollDelta::Lines { y, .. } => y.round() as isize,
                    iced::mouse::ScrollDelta::Pixels { y, .. } => (y / 32.0).round() as isize,
                };
                if rows != 0 {
                    self.center_offset = self.center_offset.saturating_add(rows);
                }
            }
            Message::DragStarted => {
                self.dragging = true;
                self.last_drag_y = None;
                self.drag_remainder = 0.0;
            }
            Message::Dragged(point) if self.dragging => {
                if let Some(previous) = self.last_drag_y {
                    self.drag_remainder += point.y - previous;
                    let rows = (self.drag_remainder / 12.0).trunc() as isize;
                    if rows != 0 {
                        self.center_offset = self.center_offset.saturating_add(rows);
                        self.drag_remainder -= rows as f32 * 12.0;
                    }
                }
                self.last_drag_y = Some(point.y);
            }
            Message::Dragged(_) => {}
            Message::DragEnded => {
                self.dragging = false;
                self.last_drag_y = None;
                self.drag_remainder = 0.0;
            }
            Message::SelectLiquidityReference => {}
        }
        self.clamp_center_offset();
        self.last_tick = Instant::now();
        Action::ViewChanged
    }

    fn auto_fit(&mut self) {
        self.visible_fraction = 1.0;
        self.center_offset = 0;
        self.dragging = false;
        self.last_drag_y = None;
        self.drag_remainder = 0.0;
    }

    pub fn visible_strikes(&self) -> &[data::chart::gex::GexStrike] {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return &[];
        };
        let strikes = snapshot.strikes.as_ref();
        if strikes.is_empty() {
            return strikes;
        }
        let range = (self.config.price_range_percent.max(0.0) / 100.0).min(1.0);
        let minimum = snapshot.source_spot * (1.0 - range);
        let maximum = snapshot.source_spot * (1.0 + range);
        let filtered_start = strikes.partition_point(|strike| strike.strike < minimum);
        let filtered_end = strikes.partition_point(|strike| strike.strike <= maximum);
        let filtered = &strikes[filtered_start..filtered_end];
        if filtered.is_empty() {
            return filtered;
        }
        let count = self.visible_count(filtered.len());
        let spot_index = filtered
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                (a.strike - snapshot.source_spot)
                    .abs()
                    .total_cmp(&(b.strike - snapshot.source_spot).abs())
            })
            .map_or(0, |(index, _)| index);
        let center = spot_index
            .saturating_add_signed(self.center_offset)
            .min(filtered.len() - 1);
        let start = center
            .saturating_sub(count / 2)
            .min(filtered.len().saturating_sub(count));
        &filtered[start..start + count]
    }

    pub fn can_zoom_in(&self) -> bool {
        let len = self.filtered_strikes().len();
        self.visible_count(len) > len.min(5)
    }

    pub fn can_zoom_out(&self) -> bool {
        let len = self.filtered_strikes().len();
        self.visible_count(len) < self.config.max_visible_strikes.max(1).min(len)
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging
    }

    fn filtered_strikes(&self) -> &[data::chart::gex::GexStrike] {
        let Some(snapshot) = self.snapshot.as_ref() else {
            return &[];
        };
        let strikes = snapshot.strikes.as_ref();
        let range = (self.config.price_range_percent.max(0.0) / 100.0).min(1.0);
        let minimum = snapshot.source_spot * (1.0 - range);
        let maximum = snapshot.source_spot * (1.0 + range);
        let start = strikes.partition_point(|strike| strike.strike < minimum);
        let end = strikes.partition_point(|strike| strike.strike <= maximum);
        &strikes[start..end]
    }

    fn visible_count(&self, available: usize) -> usize {
        let configured = self.config.max_visible_strikes.max(1).min(available);
        ((configured as f64 * self.visible_fraction).round() as usize)
            .max(available.min(5))
            .min(available)
    }

    fn clamp_center_offset(&mut self) {
        let Some(snapshot) = self.snapshot.as_ref() else {
            self.center_offset = 0;
            return;
        };
        let filtered = self.filtered_strikes();
        if filtered.is_empty() {
            self.center_offset = 0;
            return;
        }
        let spot = snapshot.source_spot;
        let spot_index = filtered
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| (a.strike - spot).abs().total_cmp(&(b.strike - spot).abs()))
            .map_or(0, |(index, _)| index);
        self.center_offset = self.center_offset.clamp(
            -(spot_index as isize),
            (filtered.len() - 1 - spot_index) as isize,
        );
    }

    pub fn view(&self) -> iced::Element<'_, Message> {
        crate::widget::chart::gex::view(self)
    }
}

pub fn quantity_to_quote_notional(
    market_kind: MarketKind,
    size_unit: SizeUnit,
    price: f64,
    quantity: f64,
) -> f64 {
    if !price.is_finite() || price <= 0.0 || !quantity.is_finite() || quantity <= 0.0 {
        return 0.0;
    }
    match (market_kind, size_unit) {
        (MarketKind::InversePerps, _) | (_, SizeUnit::Quote) => quantity,
        (_, SizeUnit::Base) => quantity * price,
    }
}

pub fn effective_liquidity(bid_depth_usd: f64, ask_depth_usd: f64) -> f64 {
    if !bid_depth_usd.is_finite()
        || !ask_depth_usd.is_finite()
        || bid_depth_usd <= 0.0
        || ask_depth_usd <= 0.0
    {
        return 0.0;
    }
    2.0 * bid_depth_usd * ask_depth_usd / (bid_depth_usd + ask_depth_usd)
}

fn calculate_liquidity_metrics(
    reference_ticker: TickerInfo,
    observed_at: UnixMs,
    depth: &Depth,
    depth_range_bps: f32,
    snapshot: &GexSnapshot,
    size_unit: SizeUnit,
) -> Option<GexLiquidityMetrics> {
    let best_bid = depth.bids.last_key_value()?.0.to_f64();
    let best_ask = depth.asks.first_key_value()?.0.to_f64();
    if !best_bid.is_finite() || !best_ask.is_finite() || best_bid <= 0.0 || best_ask < best_bid {
        return None;
    }
    let mid_price = (best_bid + best_ask) * 0.5;
    let spread_bps = (best_ask - best_bid) / mid_price * 10_000.0;
    let depth_range_bps = f64::from(depth_range_bps.clamp(1.0, 500.0));
    let fraction = depth_range_bps / 10_000.0;
    let bid_floor = mid_price * (1.0 - fraction);
    let ask_ceiling = mid_price * (1.0 + fraction);
    let market_kind = reference_ticker.market_type();
    let bid_depth_usd = depth
        .bids
        .iter()
        .filter(|(price, _)| price.to_f64() >= bid_floor)
        .map(|(price, qty)| {
            quantity_to_quote_notional(market_kind, size_unit, price.to_f64(), qty.to_f64())
        })
        .sum::<f64>();
    let ask_depth_usd = depth
        .asks
        .iter()
        .filter(|(price, _)| price.to_f64() <= ask_ceiling)
        .map(|(price, qty)| {
            quantity_to_quote_notional(market_kind, size_unit, price.to_f64(), qty.to_f64())
        })
        .sum::<f64>();
    let effective_liquidity_usd = effective_liquidity(bid_depth_usd, ask_depth_usd);
    let gamma_exposure_usd = match snapshot.model {
        GexSignModel::CallPutOiProxy => snapshot.net_gex_1pct.unwrap_or(0.0).abs(),
        GexSignModel::AbsoluteGamma => snapshot.absolute_gex_1pct,
    };
    let impact_ratio = if effective_liquidity_usd > 0.0 {
        gamma_exposure_usd / effective_liquidity_usd
    } else {
        0.0
    };
    let proxy_direction = if snapshot.model == GexSignModel::AbsoluteGamma {
        GexProxyDirection::NotApplicable
    } else {
        let net = snapshot.net_gex_1pct.unwrap_or(0.0);
        if snapshot.absolute_gex_1pct <= 0.0
            || net.abs() / snapshot.absolute_gex_1pct < GEX_PROXY_BALANCED_SHARE
        {
            GexProxyDirection::Balanced
        } else if net > 0.0 {
            GexProxyDirection::Positive
        } else {
            GexProxyDirection::Negative
        }
    };
    Some(GexLiquidityMetrics {
        reference_ticker,
        observed_at,
        mid_price,
        spread_bps,
        bid_depth_usd,
        ask_depth_usd,
        effective_liquidity_usd,
        gamma_exposure_usd,
        impact_ratio,
        regime: if effective_liquidity_usd > 0.0 {
            gamma_liquidity_regime(impact_ratio)
        } else {
            GammaLiquidityRegime::Unavailable
        },
        proxy_direction,
        depth_range_bps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use data::chart::gex::{GexSignModel, GexStrike};
    use exchange::{
        Ticker, UnixMs,
        adapter::Exchange,
        options::OptionsProvider,
        unit::{Price, Qty},
    };

    fn ticker(exchange: Exchange) -> TickerInfo {
        TickerInfo::new(Ticker::new("BTCUSDT", exchange), 0.01, 0.001, None)
    }

    fn depth(levels: &[(f64, f64)], asks: &[(f64, f64)]) -> Depth {
        Depth {
            bids: levels
                .iter()
                .map(|(price, qty)| (Price::from_f64(*price), Qty::from_f64(*qty)))
                .collect(),
            asks: asks
                .iter()
                .map(|(price, qty)| (Price::from_f64(*price), Qty::from_f64(*qty)))
                .collect(),
        }
    }

    fn chart() -> GexChart {
        let mut chart = GexChart::new(
            OptionsUnderlying::Btc,
            Some(Config {
                max_visible_strikes: 20,
                price_range_percent: 100.0,
                ..Config::default()
            }),
            None,
        );
        let strikes = (50..=150)
            .step_by(5)
            .map(|strike| GexStrike {
                strike: f64::from(strike),
                call_gex_1pct: 1.0,
                put_gex_1pct: -1.0,
                net_gex_1pct: 0.0,
                absolute_gamma_1pct: 2.0,
                call_open_interest: 1.0,
                put_open_interest: 1.0,
                expiration_count: 1,
                gamma_provenance: data::chart::gex::GexGammaProvenance::Derived,
            })
            .collect::<Vec<_>>();
        chart.set_snapshot(
            Some(Arc::new(GexSnapshot {
                provider: OptionsProvider::Deribit,
                underlying: OptionsUnderlying::Btc,
                model: GexSignModel::CallPutOiProxy,
                expiry_filter: data::chart::gex::GexExpiryFilter::SevenDays,
                gamma_source: data::chart::gex::GexGammaSource::BlackScholesDerived,
                gamma_provenance: data::chart::gex::GexGammaProvenance::Derived,
                source_spot: 100.0,
                observed_at: UnixMs::new(1),
                calculated_at: UnixMs::new(1),
                net_gex_1pct: Some(0.0),
                absolute_gex_1pct: 1.0,
                call_wall: Some(120.0),
                put_wall: Some(80.0),
                gamma_flip: Some(95.0),
                intrinsic_stress: Default::default(),
                gamma_vega: Default::default(),
                strikes: strikes.into(),
                expiry_strikes: Arc::default(),
                scenario_curve: Arc::default(),
                scale_p95: 2.0,
                proxy: None,
            })),
            GexFreshness::Fresh,
            None,
        );
        chart
    }

    #[test]
    fn scroll_pans_higher_and_lower_without_zooming() {
        let mut chart = chart();
        let fraction = chart.visible_fraction;
        chart.update(Message::Scrolled(iced::mouse::ScrollDelta::Lines {
            x: 0.0,
            y: 2.0,
        }));
        assert_eq!(chart.center_offset, 2);
        chart.update(Message::Scrolled(iced::mouse::ScrollDelta::Pixels {
            x: 0.0,
            y: -32.0,
        }));
        assert_eq!(chart.center_offset, 1);
        assert_eq!(chart.visible_fraction, fraction);
        assert!(!chart.visible_strikes().is_empty());
    }

    #[test]
    fn zoom_buttons_disable_at_limits_and_windows_stay_nonempty() {
        let mut chart = chart();
        while chart.can_zoom_in() {
            chart.update(Message::ZoomIn);
            assert!(!chart.visible_strikes().is_empty());
        }
        assert!(!chart.can_zoom_in());
        while chart.can_zoom_out() {
            chart.update(Message::ZoomOut);
            assert!(!chart.visible_strikes().is_empty());
        }
        assert!(!chart.can_zoom_out());
        assert_eq!(chart.visible_strikes().len(), 20);
    }

    #[test]
    fn pan_is_clamped_to_available_strikes() {
        let mut chart = chart();
        for _ in 0..100 {
            chart.update(Message::Scrolled(iced::mouse::ScrollDelta::Lines {
                x: 0.0,
                y: 10.0,
            }));
        }
        assert!(!chart.visible_strikes().is_empty());
        assert!(chart.visible_strikes().last().unwrap().strike <= 150.0);
    }

    #[test]
    fn quantity_conversion_handles_base_quote_and_inverse() {
        assert_eq!(
            quantity_to_quote_notional(MarketKind::LinearPerps, SizeUnit::Base, 100.0, 2.0),
            200.0
        );
        assert_eq!(
            quantity_to_quote_notional(MarketKind::LinearPerps, SizeUnit::Quote, 100.0, 200.0),
            200.0
        );
        assert_eq!(
            quantity_to_quote_notional(MarketKind::InversePerps, SizeUnit::Base, 100.0, 200.0),
            200.0
        );
    }

    #[test]
    fn harmonic_liquidity_penalizes_imbalance_and_empty_side() {
        assert!((effective_liquidity(100.0, 300.0) - 150.0).abs() < 1.0e-12);
        assert_eq!(effective_liquidity(100.0, 0.0), 0.0);
    }

    #[test]
    fn liquidity_range_spread_sums_and_impact_are_calculated() {
        let chart = chart();
        let book = depth(
            &[(99.99, 2.0), (99.0, 50.0)],
            &[(100.01, 3.0), (101.0, 50.0)],
        );
        let metrics = calculate_liquidity_metrics(
            ticker(Exchange::BinanceLinear),
            UnixMs::new(10),
            &book,
            25.0,
            chart.snapshot().expect("snapshot"),
            SizeUnit::Base,
        )
        .expect("metrics");
        assert!((metrics.mid_price - 100.0).abs() < 1.0e-12);
        assert!((metrics.spread_bps - 2.0).abs() < 1.0e-9);
        assert!((metrics.bid_depth_usd - 199.98).abs() < 1.0e-9);
        assert!((metrics.ask_depth_usd - 300.03).abs() < 1.0e-9);
        assert!(
            (metrics.effective_liquidity_usd - effective_liquidity(199.98, 300.03)).abs() < 1.0e-9
        );
        assert_eq!(metrics.gamma_exposure_usd, 0.0);
        assert_eq!(metrics.impact_ratio, 0.0);
    }

    #[test]
    fn proxy_target_spot_uses_live_reference_depth_midpoint() {
        let reference = ticker(Exchange::BybitLinear);
        let mut chart = GexChart::new(OptionsUnderlying::Gld, None, Some(reference));
        assert_eq!(chart.proxy_target_spot(), None);
        chart.insert_depth(&depth(&[(3_999.0, 2.0)], &[(4_001.0, 3.0)]), UnixMs::now());
        assert_eq!(chart.proxy_target_spot(), Some(4_000.0));
    }

    #[test]
    fn stale_depth_and_reference_changes_are_runtime_only() {
        let mut chart = chart();
        assert_eq!(
            chart.liquidity_depth_state(),
            LiquidityDepthState::NoReference
        );
        let reference = ticker(Exchange::BinanceLinear);
        chart.set_liquidity_reference(Some(reference));
        assert_eq!(
            chart.liquidity_depth_state(),
            LiquidityDepthState::WaitingForDepth
        );
        let now = UnixMs::now();
        let stale_at = UnixMs::new(now.as_u64().saturating_sub(LIQUIDITY_STALE_AFTER_MS + 1));
        chart.insert_depth(&depth(&[(99.99, 2.0)], &[(100.01, 3.0)]), stale_at);
        assert!(chart.liquidity_metrics().is_some());
        assert_eq!(chart.liquidity_depth_state(), LiquidityDepthState::Stale);
        let next = ticker(Exchange::BybitLinear);
        chart.set_liquidity_reference(Some(next));
        assert_eq!(chart.liquidity_reference(), Some(next));
        assert!(chart.liquidity_metrics().is_none());
        assert_eq!(
            chart.liquidity_depth_state(),
            LiquidityDepthState::WaitingForDepth
        );

        chart.insert_depth(&depth(&[(99.99, 2.0)], &[]), UnixMs::now());
        assert_eq!(
            chart.liquidity_depth_state(),
            LiquidityDepthState::InvalidDepth
        );
        chart.insert_depth(&depth(&[(99.99, 2.0)], &[(100.01, 3.0)]), UnixMs::now());
        assert_eq!(chart.liquidity_depth_state(), LiquidityDepthState::Ready);
    }

    #[test]
    fn first_depth_is_immediate_and_updates_are_throttled_to_latest_book() {
        let mut chart = chart();
        chart.set_liquidity_reference(Some(ticker(Exchange::BinanceLinear)));
        let started = Instant::now();
        let first_observed = UnixMs::new(1_000);
        let latest_observed = UnixMs::new(2_000);
        let first = depth(&[(99.99, 2.0)], &[(100.01, 3.0)]);
        let latest = depth(&[(99.99, 8.0)], &[(100.01, 9.0)]);

        chart.insert_depth_at(&first, first_observed, started);
        assert_eq!(
            chart.liquidity_metrics().expect("first metric").observed_at,
            first_observed
        );
        let first_liquidity = chart
            .liquidity_metrics()
            .expect("first metric")
            .effective_liquidity_usd;

        chart.insert_depth_at(&latest, latest_observed, started + Duration::from_secs(59));
        assert_eq!(
            chart
                .liquidity_metrics()
                .expect("throttled metric")
                .observed_at,
            first_observed
        );
        assert_eq!(
            chart.last_depth.as_ref().expect("latest depth").1,
            latest_observed
        );

        chart.insert_depth_at(
            &latest,
            latest_observed,
            started + LIQUIDITY_RECALCULATION_INTERVAL,
        );
        let sampled = chart.liquidity_metrics().expect("sampled metric");
        assert_eq!(sampled.observed_at, latest_observed);
        assert_ne!(sampled.effective_liquidity_usd, first_liquidity);
    }

    #[test]
    fn continuous_depth_ticks_cannot_debounce_periodic_recalculation() {
        let mut chart = chart();
        chart.set_liquidity_reference(Some(ticker(Exchange::BinanceLinear)));
        let started = Instant::now();
        let book = depth(&[(99.99, 2.0)], &[(100.01, 3.0)]);
        chart.insert_depth_at(&book, UnixMs::new(1), started);

        for second in 1..60 {
            chart.insert_depth_at(
                &book,
                UnixMs::new(second * 1_000),
                started + Duration::from_secs(second),
            );
            assert_eq!(
                chart.liquidity_metrics().expect("metric").observed_at,
                UnixMs::new(1)
            );
        }
        chart.insert_depth_at(
            &book,
            UnixMs::new(60_000),
            started + Duration::from_secs(60),
        );
        assert_eq!(
            chart
                .liquidity_metrics()
                .expect("periodic metric")
                .observed_at,
            UnixMs::new(60_000)
        );
        assert_eq!(
            chart.last_liquidity_recalculation,
            Some(started + LIQUIDITY_RECALCULATION_INTERVAL)
        );
    }

    #[test]
    fn reference_change_and_panel_reenable_reset_liquidity_throttle() {
        let mut chart = chart();
        chart.set_liquidity_reference(Some(ticker(Exchange::BinanceLinear)));
        let started = Instant::now();
        let book = depth(&[(99.99, 2.0)], &[(100.01, 3.0)]);
        chart.insert_depth_at(&book, UnixMs::new(1), started);

        chart.set_liquidity_reference(Some(ticker(Exchange::BybitLinear)));
        assert!(chart.last_depth.is_none());
        assert!(chart.liquidity_metrics().is_none());
        assert!(chart.last_liquidity_recalculation.is_none());
        chart.insert_depth_at(&book, UnixMs::new(2), started + Duration::from_secs(1));
        assert_eq!(
            chart
                .liquidity_metrics()
                .expect("new reference metric")
                .observed_at,
            UnixMs::new(2)
        );

        let mut disabled = *chart.config();
        disabled.show_gamma_liquidity_panel = false;
        chart.set_config(disabled);
        assert!(chart.liquidity_metrics().is_none());
        assert!(chart.last_liquidity_recalculation.is_none());

        let mut enabled = disabled;
        enabled.show_gamma_liquidity_panel = true;
        chart.set_config(enabled);
        assert_eq!(
            chart
                .liquidity_metrics()
                .expect("reenabled metric")
                .observed_at,
            UnixMs::new(2)
        );
        assert!(chart.last_liquidity_recalculation.is_none());
        chart.insert_depth_at(&book, UnixMs::new(3), started + Duration::from_secs(2));
        assert_eq!(
            chart
                .liquidity_metrics()
                .expect("first new stream metric")
                .observed_at,
            UnixMs::new(3)
        );
    }

    #[test]
    fn absolute_gamma_has_no_directional_proxy_label() {
        let mut snapshot = chart().snapshot().expect("snapshot").as_ref().clone();
        snapshot.model = GexSignModel::AbsoluteGamma;
        snapshot.net_gex_1pct = None;
        snapshot.absolute_gex_1pct = 10.0;
        let metrics = calculate_liquidity_metrics(
            ticker(Exchange::BinanceInverse),
            UnixMs::new(10),
            &depth(&[(99.99, 200.0)], &[(100.01, 300.0)]),
            25.0,
            &snapshot,
            SizeUnit::Base,
        )
        .expect("metrics");
        assert_eq!(metrics.proxy_direction, GexProxyDirection::NotApplicable);
        assert_eq!(metrics.gamma_exposure_usd, 10.0);
    }
}
