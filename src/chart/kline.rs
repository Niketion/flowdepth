use super::{
    Action, Basis, Chart, Interaction, Message, PlotConstants, PlotData, TEXT_SIZE, ViewState,
    indicator, request_fetch, scale::linear::PriceInfoLabel,
};
use crate::chart::indicator::kline::KlineIndicatorImpl;
use crate::connector::fetcher::{
    self, FetchRange, ReqError, RequestHandler, is_trade_fetch_enabled,
};
use crate::{modal::pane::settings::study, style};
use data::aggr::ticks::TickAggr;
use data::aggr::time::TimeSeries;
use data::chart::indicator::{Indicator, KlineIndicator};
use data::chart::kline::{
    BubbleColorMode, BubbleLabelMode, BubblePriceResponse, BubbleVolumeSummary, ClusterKind,
    ClusterScaling, Config, FootprintStudy, FootprintSummary, KlineDataPoint, KlineTrades, NPoc,
    PointOfControl, SessionProfileMode, SessionProfilePlacement, SessionVolumeProfileConfig,
    StabilizedBubbleThreshold, VolumeBubbleCluster, VolumeBubbleConfig, VolumeBubbleSession,
    VwapConfig, adaptive_bubble_threshold_baselines, apply_volume_bubble_budget, bubble_age_factor,
    classify_bubble_price_response, cluster_volume_bubble_trades, percentile,
};
use data::chart::{Autoscale, KlineChartKind, ViewConfig};

use data::config::theme::{composite_color, contrast_ratio, mix_color};
use data::util::abbr_large_numbers;
use exchange::unit::{Price, PriceStep, Qty};
use exchange::{Kline, OpenInterest as OIData, TickerInfo, Trade, UnixMs};

use iced::task::Handle;
use iced::theme::palette::Extended;
use iced::widget::canvas::{self, Event, Geometry, Path, Stroke};
use iced::{Alignment, Color, Element, Point, Rectangle, Renderer, Size, Theme, Vector, mouse};

use chrono::{Datelike, TimeZone, Timelike};
use enum_map::EnumMap;
use rustc_hash::{FxHashMap, FxHashSet};
use std::{cell::RefCell, sync::Arc, time::Instant};

/// Maximum number of raw trades to retain in memory.
/// Older trades are pruned by exchange timestamp when this cap is exceeded.
/// 50k trades ≈ 1.5-3 MB depending on Trade size.
const MAX_RAW_TRADES: usize = 50_000;
const MAX_LIVE_TRADE_BUCKETS: usize = 4_096;

fn retain_latest_raw_trades(raw_trades: &mut Vec<Trade>, previous_len: usize) -> usize {
    if previous_len > 0
        && previous_len < raw_trades.len()
        && raw_trades[previous_len - 1].time > raw_trades[previous_len].time
    {
        raw_trades.sort_by_key(|trade| trade.time);
    }

    let excess = raw_trades.len().saturating_sub(MAX_RAW_TRADES);
    if excess > 0 {
        raw_trades.drain(..excess);
    }
    excess
}

fn deduplicate_incoming_trades(
    existing: &[Trade],
    incoming: &[Trade],
    source: &'static str,
    ticker_info: Option<TickerInfo>,
) -> Vec<Trade> {
    let invalid_prices = incoming
        .iter()
        .filter(|trade| trade.price.units <= 0)
        .collect::<Vec<_>>();
    let invalid_price_count = invalid_prices.len();
    if invalid_price_count > 0 {
        // Keep a small sample in the warning so a venue/parser problem can be traced without
        // dumping the full batch or any account-related data into the log.
        let samples = invalid_prices
            .iter()
            .take(3)
            .map(|trade| {
                format!(
                    "id={} timestamp_ms={} side={} price={} price_units={} qty={}",
                    trade.id.map_or_else(|| "none".into(), |id| id.to_string()),
                    trade.time.as_u64(),
                    if trade.is_sell { "sell" } else { "buy" },
                    trade.price.to_f64(),
                    trade.price.units,
                    trade.qty.to_f64()
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let (ticker, exchange, market) = ticker_info.map_or_else(
            || ("unknown".into(), "unknown".into(), "unknown".into()),
            |info| {
                (
                    info.ticker.to_string(),
                    format!("{:?}", info.exchange()),
                    format!("{:?}", info.market_type()),
                )
            },
        );
        log::warn!(
            "TRADE InvalidPriceDiscarded | source={source} exchange={exchange} ticker={ticker} market={market} count={invalid_price_count} batch_len={} batch_first_ms={} batch_last_ms={} samples=[{samples}] reason=non_positive_execution_price action=discarded_before_chart_aggregation",
            incoming.len(),
            incoming.first().map_or(0, |trade| trade.time.as_u64()),
            incoming.last().map_or(0, |trade| trade.time.as_u64())
        );
    }

    let mut seen_ids = existing
        .iter()
        .filter_map(|trade| trade.id)
        .collect::<FxHashSet<_>>();
    incoming
        .iter()
        .copied()
        // A non-positive execution price is never valid market data. Besides corrupting the
        // footprint bucket, it expands FitToVisible down to zero and compresses the real market
        // into a horizontal line.
        .filter(|trade| trade.price.units > 0)
        .filter(|trade| trade.id.is_none_or(|id| seen_ids.insert(id)))
        .collect()
}

fn exclude_historical_overlap_with_live(
    incoming: Vec<Trade>,
    live_buckets: &FxHashSet<UnixMs>,
    interval: exchange::Timeframe,
) -> (Vec<Trade>, usize) {
    let before = incoming.len();
    let filtered = incoming
        .into_iter()
        .filter(|trade| !live_buckets.contains(&trade.time.floor_to(interval)))
        .collect::<Vec<_>>();
    let discarded = before.saturating_sub(filtered.len());
    (filtered, discarded)
}

impl Chart for KlineChart {
    type IndicatorKind = KlineIndicator;

    fn state(&self) -> &ViewState {
        &self.chart
    }

    fn mut_state(&mut self) -> &mut ViewState {
        &mut self.chart
    }

    fn invalidate_crosshair(&mut self) {
        self.chart.cache.clear_crosshair();
        self.indicators
            .values_mut()
            .filter_map(Option::as_mut)
            .for_each(|indi| indi.clear_crosshair_caches());
    }

    fn invalidate_all(&mut self) {
        self.invalidate(None);
    }

    fn view_indicators(&'_ self, enabled: &[Self::IndicatorKind]) -> Vec<Element<'_, Message>> {
        let chart_state = self.state();
        let visible_region = chart_state.visible_region(chart_state.bounds.size());
        let (earliest, latest) = chart_state.interval_range(&visible_region);
        if earliest > latest {
            return vec![];
        }

        let data_labels_always_visible = self.visual_config.data_labels_always_visible;

        let market = chart_state.ticker_info.market_type();
        let mut elements = vec![];

        for selected_indicator in enabled {
            if !self.kind.allows_indicator(*selected_indicator)
                || !KlineIndicator::for_market(market).contains(selected_indicator)
            {
                continue;
            }
            if selected_indicator.is_overlay() {
                continue;
            }
            if let Some(indi) = self.indicators[*selected_indicator].as_ref() {
                elements.push(indi.element(
                    chart_state,
                    data_labels_always_visible,
                    earliest..=latest,
                ));
            }
        }
        elements
    }

    fn visible_timerange(&self) -> Option<(u64, u64)> {
        let chart = self.state();
        let region = chart.visible_region(chart.bounds.size());

        if region.width == 0.0 {
            return None;
        }

        Some(chart.interval_range(&region))
    }

    fn interval_keys(&self) -> Option<Vec<u64>> {
        match &self.data_source {
            PlotData::TimeBased(_) => None,
            PlotData::TickBased(tick_aggr) => Some(
                tick_aggr
                    .datapoints
                    .iter()
                    .map(|dp| dp.kline.time.as_u64())
                    .collect(),
            ),
        }
    }

    fn autoscaled_coords(&self) -> Vector {
        let chart = self.state();
        let x_translation = match &self.kind {
            KlineChartKind::Footprint { .. } => {
                0.5 * (chart.bounds.width / chart.scaling) - (chart.cell_width / chart.scaling)
            }
            KlineChartKind::Candles => {
                0.5 * (chart.bounds.width / chart.scaling)
                    - (8.0 * chart.cell_width / chart.scaling)
            }
        };
        Vector::new(x_translation, chart.translation.y)
    }

    fn supports_fit_autoscaling(&self) -> bool {
        true
    }

    fn is_empty(&self) -> bool {
        match &self.data_source {
            PlotData::TimeBased(timeseries) => timeseries.datapoints.is_empty(),
            PlotData::TickBased(tick_aggr) => tick_aggr.datapoints.is_empty(),
        }
    }
}

impl PlotConstants for KlineChart {
    fn min_scaling(&self) -> f32 {
        self.kind.min_scaling()
    }

    fn max_scaling(&self) -> f32 {
        self.kind.max_scaling()
    }

    fn max_cell_width(&self) -> f32 {
        self.kind.max_cell_width()
    }

    fn min_cell_width(&self) -> f32 {
        self.kind.min_cell_width()
    }

    fn max_cell_height(&self) -> f32 {
        self.kind.max_cell_height()
    }

    fn min_cell_height(&self) -> f32 {
        self.kind.min_cell_height()
    }

    fn default_cell_width(&self) -> f32 {
        self.kind.default_cell_width()
    }
}

pub struct KlineChart {
    chart: ViewState,
    data_source: PlotData<KlineDataPoint>,
    raw_trades: Vec<Trade>,
    /// True once raw retention has discarded executions. Covered fetch ranges
    /// may then extend further back than the raw data still held in memory.
    raw_trades_pruned: bool,
    /// Time buckets populated from the live raw feed. Historical aggTrades use a different ID
    /// namespace and must never be added to these same buckets.
    live_trade_buckets: FxHashSet<UnixMs>,
    covered_trade_ranges: Vec<(UnixMs, UnixMs)>,
    covered_bubble_summary_ranges: Vec<(UnixMs, UnixMs)>,
    indicators: EnumMap<KlineIndicator, Option<Box<dyn KlineIndicatorImpl>>>,
    fetching_trades: (bool, Option<Handle>),
    pub(crate) kind: KlineChartKind,
    request_handler: RequestHandler,
    study_configurator: study::Configurator<FootprintStudy>,
    last_tick: Instant,
    visual_config: Config,
    gex_snapshot: Option<Arc<data::chart::gex::GexSnapshot>>,
    gex_history: Vec<Arc<data::chart::gex::GexSnapshot>>,
    gex_freshness: data::chart::gex::GexFreshness,
    gex_error: Option<Arc<str>>,
    gex_proxy_history: Vec<Arc<exchange::options::gex_monitor::GexProxyHistoryPoint>>,
    gex_proxy_freshness: data::chart::gex::GexFreshness,
    gex_proxy_error: Option<Arc<str>>,
    derive_flow: Option<Arc<data::chart::gex::DeriveMakerGammaFlow>>,
    derive_freshness: data::chart::gex::GexFreshness,
    gex_render_cache: RefCell<GexRenderCache>,
    rendered_volume_bubbles: RefCell<Vec<RenderedVolumeBubble>>,
    stabilized_bubble_threshold: RefCell<StabilizedBubbleThreshold>,
}

#[derive(Debug, Default)]
struct GexRenderCache {
    key: Option<u64>,
    zone_frames: Arc<[data::chart::gex::GexZoneFrame]>,
    proxy_key: Option<u64>,
    proxy_zone_frames: Arc<[data::chart::gex::GexProxyZoneFrame]>,
}

#[derive(Debug, Clone, Copy)]
pub struct VolumeBubbleQtyScale {
    pub min: f64,
    pub max: f64,
    pub step: f64,
}

impl KlineChart {
    pub fn new(
        layout: ViewConfig,
        basis: Basis,
        step: PriceStep,
        klines_raw: &[Kline],
        raw_trades: Vec<Trade>,
        enabled_indicators: &[KlineIndicator],
        ticker_info: TickerInfo,
        kind: &KlineChartKind,
        visual_config: Option<Config>,
    ) -> Self {
        let mut visual_config = visual_config.unwrap_or_default();
        visual_config.migrate_legacy_indicator_configs();
        let kind = kind.clone();
        let raw_trades =
            deduplicate_incoming_trades(&[], &raw_trades, "initial", Some(ticker_info));

        match basis {
            Basis::Time(interval) => {
                let timeseries = TimeSeries::<KlineDataPoint>::new(interval, step, klines_raw)
                    .with_trades(&raw_trades);

                let base_price_y = timeseries.base_price();
                let latest_x = timeseries
                    .latest_timestamp()
                    .map_or(0, |timestamp| timestamp.as_u64());
                let (scale_high, scale_low) = timeseries.price_scale({
                    match &kind {
                        KlineChartKind::Footprint { .. } => 12,
                        KlineChartKind::Candles => 60,
                    }
                });

                let low_rounded = scale_low.round_to_side_step(true, step);
                let high_rounded = scale_high.round_to_side_step(false, step);

                let y_ticks = Price::steps_between_inclusive(low_rounded, high_rounded, step)
                    .map(|n| n.saturating_sub(1))
                    .unwrap_or(1)
                    .max(1) as f32;

                let cell_width = match &kind {
                    KlineChartKind::Footprint { .. } => 80.0,
                    KlineChartKind::Candles => 4.0,
                };
                let cell_height = match &kind {
                    KlineChartKind::Footprint { .. } => 800.0 / y_ticks,
                    KlineChartKind::Candles => 200.0 / y_ticks,
                };

                let mut chart = ViewState::new(
                    basis,
                    step,
                    step.decimal_places(),
                    ticker_info,
                    ViewConfig {
                        splits: layout.splits.clone(),
                        autoscale: Some(Autoscale::FitToVisible),
                    },
                    cell_width,
                    cell_height,
                );
                chart.base_price_y = base_price_y;
                chart.latest_x = latest_x;

                let x_translation = match &kind {
                    KlineChartKind::Footprint { .. } => {
                        0.5 * (chart.bounds.width / chart.scaling)
                            - (chart.cell_width / chart.scaling)
                    }
                    KlineChartKind::Candles => {
                        0.5 * (chart.bounds.width / chart.scaling)
                            - (8.0 * chart.cell_width / chart.scaling)
                    }
                };
                chart.translation.x = x_translation;

                let data_source = PlotData::TimeBased(timeseries);

                let mut indicators = EnumMap::default();
                for &i in enabled_indicators {
                    if !kind.allows_indicator(i) {
                        continue;
                    }
                    let mut indi = indicator::kline::make_empty(i);
                    indi.on_config_changed(&visual_config);
                    indi.rebuild_from_source(&data_source);
                    indicators[i] = Some(indi);
                }

                KlineChart {
                    chart,
                    visual_config,
                    data_source,
                    raw_trades,
                    raw_trades_pruned: false,
                    live_trade_buckets: FxHashSet::default(),
                    covered_trade_ranges: Vec::new(),
                    covered_bubble_summary_ranges: Vec::new(),
                    indicators,
                    fetching_trades: (false, None),
                    request_handler: RequestHandler::default(),
                    kind: kind.clone(),
                    study_configurator: study::Configurator::new(),
                    last_tick: Instant::now(),
                    gex_snapshot: None,
                    gex_history: Vec::new(),
                    gex_freshness: data::chart::gex::GexFreshness::Loading,
                    gex_error: None,
                    gex_proxy_history: Vec::new(),
                    gex_proxy_freshness: data::chart::gex::GexFreshness::Loading,
                    gex_proxy_error: None,
                    derive_flow: None,
                    derive_freshness: data::chart::gex::GexFreshness::Loading,
                    gex_render_cache: RefCell::new(GexRenderCache::default()),
                    rendered_volume_bubbles: RefCell::new(Vec::new()),
                    stabilized_bubble_threshold: RefCell::new(StabilizedBubbleThreshold::default()),
                }
            }
            Basis::Tick(interval) => {
                let cell_width = match &kind {
                    KlineChartKind::Footprint { .. } => 80.0,
                    KlineChartKind::Candles => 4.0,
                };
                let cell_height = match &kind {
                    KlineChartKind::Footprint { .. } => 90.0,
                    KlineChartKind::Candles => 8.0,
                };

                let mut chart = ViewState::new(
                    basis,
                    step,
                    step.decimal_places(),
                    ticker_info,
                    ViewConfig {
                        splits: layout.splits.clone(),
                        autoscale: Some(Autoscale::FitToVisible),
                    },
                    cell_width,
                    cell_height,
                );

                let x_translation = match &kind {
                    KlineChartKind::Footprint { .. } => {
                        0.5 * (chart.bounds.width / chart.scaling)
                            - (chart.cell_width / chart.scaling)
                    }
                    KlineChartKind::Candles => {
                        0.5 * (chart.bounds.width / chart.scaling)
                            - (8.0 * chart.cell_width / chart.scaling)
                    }
                };
                chart.translation.x = x_translation;

                let data_source = PlotData::TickBased(TickAggr::new(interval, step, &[]));

                let mut indicators = EnumMap::default();
                for &i in enabled_indicators {
                    if !kind.allows_indicator(i) {
                        continue;
                    }
                    let mut indi = indicator::kline::make_empty(i);
                    indi.on_config_changed(&visual_config);
                    indi.rebuild_from_source(&data_source);
                    indicators[i] = Some(indi);
                }

                KlineChart {
                    chart,
                    visual_config,
                    data_source,
                    raw_trades,
                    raw_trades_pruned: false,
                    live_trade_buckets: FxHashSet::default(),
                    covered_trade_ranges: Vec::new(),
                    covered_bubble_summary_ranges: Vec::new(),
                    indicators,
                    fetching_trades: (false, None),
                    request_handler: RequestHandler::default(),
                    kind: kind.clone(),
                    study_configurator: study::Configurator::new(),
                    last_tick: Instant::now(),
                    gex_snapshot: None,
                    gex_history: Vec::new(),
                    gex_freshness: data::chart::gex::GexFreshness::Loading,
                    gex_error: None,
                    gex_proxy_history: Vec::new(),
                    gex_proxy_freshness: data::chart::gex::GexFreshness::Loading,
                    gex_proxy_error: None,
                    derive_flow: None,
                    derive_freshness: data::chart::gex::GexFreshness::Loading,
                    gex_render_cache: RefCell::new(GexRenderCache::default()),
                    rendered_volume_bubbles: RefCell::new(Vec::new()),
                    stabilized_bubble_threshold: RefCell::new(StabilizedBubbleThreshold::default()),
                }
            }
        }
    }

    pub fn update_latest_kline(&mut self, kline: &Kline) {
        match self.data_source {
            PlotData::TimeBased(ref mut timeseries) => {
                let previous_latest_x = self.chart.latest_x;
                let is_new_bucket = !timeseries.datapoints.contains_key(&kline.time);
                timeseries.insert_klines(&[*kline]);
                if is_new_bucket {
                    let bucket_trades = self
                        .raw_trades
                        .iter()
                        .filter(|trade| trade.time.floor_to(timeseries.interval) == kline.time)
                        .copied()
                        .collect::<Vec<_>>();
                    timeseries.insert_trades_existing_buckets(&bucket_trades);
                }

                self.indicators
                    .values_mut()
                    .filter_map(Option::as_mut)
                    .for_each(|indi| indi.on_insert_klines(&[*kline], &self.data_source));

                let chart = self.mut_state();

                let relation = if kline.time.as_u64() > chart.latest_x {
                    chart.latest_x = kline.time.as_u64();
                    "newer"
                } else if kline.time.as_u64() == chart.latest_x {
                    "equal"
                } else {
                    "older"
                };

                chart.last_price = Some(PriceInfoLabel::new(kline.close, kline.open));
                log::trace!(
                    "KLINE UpdateLatest | kline_t={} previous_latest_x={} new_latest_x={} relation={relation}",
                    fetcher::format_time_short(kline.time),
                    previous_latest_x,
                    chart.latest_x
                );
            }
            PlotData::TickBased(_) => {
                log::trace!(
                    "KLINE UpdateLatest | kline_t={} reason=tick_based_ignored",
                    fetcher::format_time_short(kline.time)
                );
            }
        }
    }

    pub fn kind(&self) -> &KlineChartKind {
        &self.kind
    }

    fn fetch_missing_data(&mut self) -> Option<Action> {
        self.request_handler.cleanup_stale();
        if self.fetching_trades.0 && !self.request_handler.has_pending_trade_requests() {
            log::warn!("CHART Footprint | action=clear_fetching reason=no_pending_trade_request");
            self.fetching_trades = (false, None);
        }

        log::debug!(
            "CHART FetchMissingStart | kind={:?} basis={:?} datapoints={} raw_trades={} covered_trade_ranges={} fetching_trades={} bubbles_enabled={} bubbles_session={:?} trade_fetch_enabled={}",
            self.kind,
            self.chart.basis,
            match &self.data_source {
                PlotData::TimeBased(timeseries) => timeseries.datapoints.len(),
                PlotData::TickBased(tick_aggr) => tick_aggr.datapoints.len(),
            },
            self.raw_trades.len(),
            self.covered_trade_ranges.len(),
            self.fetching_trades.0,
            self.indicator_enabled(KlineIndicator::VolumeBubbles),
            self.visual_config.volume_bubbles.session,
            is_trade_fetch_enabled()
        );
        match &self.data_source {
            PlotData::TimeBased(timeseries) => {
                let timeframe_ms = timeseries.interval.to_milliseconds();

                if timeseries.datapoints.is_empty() {
                    let latest = chrono::Utc::now().timestamp_millis() as u64;
                    let earliest = latest.saturating_sub(450 * timeframe_ms);

                    let range = FetchRange::Kline(UnixMs::new(earliest), UnixMs::new(latest));
                    log::info!(
                        "KLINE InitialFetch | reason=empty_data range={}",
                        fetcher::format_time_range(UnixMs::new(earliest), UnixMs::new(latest))
                    );
                    if let Some(action) = request_fetch(
                        &mut self.request_handler,
                        range,
                        Some(&self.chart.ticker_info),
                    ) {
                        log::info!(
                            "KLINE InitialFetchQueued | range={}",
                            fetcher::format_time_range(UnixMs::new(earliest), UnixMs::new(latest))
                        );
                        return Some(action);
                    } else {
                        log::debug!(
                            "KLINE InitialFetchSuppressed | range={} reason=request_handler",
                            fetcher::format_time_range(UnixMs::new(earliest), UnixMs::new(latest))
                        );
                    }
                }

                let Some((visible_earliest, visible_latest)) = self.visible_timerange() else {
                    log::debug!(
                        "CHART FetchMissingSkip | kind={:?} reason=visible_timerange_none bounds={:?}",
                        self.kind,
                        self.chart.bounds
                    );
                    return None;
                };
                let (kline_earliest, kline_latest) = timeseries.timerange();
                let visible_earliest_ms = UnixMs::new(visible_earliest);
                let visible_latest_ms = UnixMs::new(visible_latest);
                let visible_span = visible_latest.saturating_sub(visible_earliest);
                let prefetch_earliest = visible_earliest.saturating_sub(visible_span);
                log::debug!(
                    "CHART FetchMissingRange | visible_range={} kline_range={} visible_span_ms={} prefetch_earliest={}",
                    fetcher::format_time_range(visible_earliest_ms, visible_latest_ms),
                    fetcher::format_time_range(kline_earliest, kline_latest),
                    visible_span,
                    fetcher::format_time_short(UnixMs::new(prefetch_earliest))
                );

                // priority 1, initial klines for visible range
                if visible_earliest_ms < kline_earliest {
                    let range = FetchRange::Kline(UnixMs::new(prefetch_earliest), kline_earliest);
                    log::info!(
                        "KLINE PriorityFetch | reason=visible_before_earliest visible_earliest={} kline_earliest={} fetch={}",
                        fetcher::format_time_short(visible_earliest_ms),
                        fetcher::format_time_short(kline_earliest),
                        fetcher::format_fetch_range(&range)
                    );
                    if let Some(action) = request_fetch(
                        &mut self.request_handler,
                        range,
                        Some(&self.chart.ticker_info),
                    ) {
                        return Some(action);
                    } else {
                        log::debug!(
                            "KLINE PriorityFetchSuppressed | reason=request_handler fetch={}",
                            fetcher::format_fetch_range(&range)
                        );
                    }
                } else {
                    log::trace!(
                        "KLINE PrioritySkip | reason=visible_not_before_earliest visible_earliest={} kline_earliest={}",
                        fetcher::format_time_short(visible_earliest_ms),
                        fetcher::format_time_short(kline_earliest)
                    );
                }

                let now = UnixMs::now();
                let target_to = kline_latest.saturating_add(timeframe_ms).min(now);
                let historical_trade_to =
                    historical_trade_target_to(kline_latest, timeframe_ms, now);
                let vwap_required_from = self.indicator_enabled(KlineIndicator::Vwap).then(|| {
                    let anchor_ms = self.visual_config.vwap.anchor.milliseconds();
                    vwap_required_from(target_to, visible_earliest_ms, anchor_ms)
                });
                let bubble_required_range = (self.indicator_enabled(KlineIndicator::VolumeBubbles)
                    && self.visual_config.volume_bubbles.enabled)
                    .then(|| {
                        volume_bubble_effective_range(
                            kline_latest,
                            timeframe_ms,
                            UnixMs::now(),
                            &self.visual_config.volume_bubbles,
                        )
                    })
                    .flatten();

                // Indicator history must have kline buckets before raw trades
                // or derived summaries can be attached to them.
                let indicator_kline_from = vwap_required_from
                    .into_iter()
                    .chain(bubble_required_range.map(|(from, _)| {
                        UnixMs::new(from.as_u64() - (from.as_u64() % timeframe_ms))
                    }))
                    .min();
                if let Some(required_from) = indicator_kline_from
                    && required_from < kline_earliest
                {
                    let range = FetchRange::Kline(required_from, kline_earliest);
                    if let Some(action) = request_fetch(
                        &mut self.request_handler,
                        range,
                        Some(&self.chart.ticker_info),
                    ) {
                        return Some(action);
                    }
                }

                // priority 2, trades
                if matches!(self.kind, KlineChartKind::Footprint { .. }) {
                    if !self.fetching_trades.0 && is_trade_fetch_enabled() {
                        if let Some((fetch_from, fetch_to)) = timeseries
                            .suggest_trade_fetch_range(visible_earliest_ms, visible_latest_ms)
                        {
                            // The chart intentionally renders whitespace after
                            // the latest candle. It must never turn that visual
                            // future into a historical market-data request.
                            let fetch_to = fetch_to.min(historical_trade_to);
                            log::debug!(
                                "CHART Footprint | action=suggest_missing range={}",
                                fetcher::format_time_range(fetch_from, fetch_to)
                            );
                            if fetch_to <= fetch_from {
                                log::debug!(
                                    "CHART Footprint | action=skip reason=range_after_now range={}",
                                    fetcher::format_time_range(fetch_from, fetch_to)
                                );
                            } else if let Some((fetch_from, fetch_to)) =
                                self.subtract_covered_trade_ranges(fetch_from, fetch_to)
                            {
                                log::info!(
                                    "CHART Footprint | action=fetch_trades reason=missing_range range={}",
                                    fetcher::format_time_range(fetch_from, fetch_to)
                                );
                                let range = FetchRange::Trades(fetch_from, fetch_to);
                                if let Some(action) = request_fetch(
                                    &mut self.request_handler,
                                    range,
                                    Some(&self.chart.ticker_info),
                                ) {
                                    self.fetching_trades = (true, None);
                                    return Some(action);
                                } else {
                                    let reason = self
                                        .request_handler
                                        .last_suppression_reason()
                                        .map_or("throttled", |reason| reason.as_str());
                                    log::info!(
                                        "CHART Footprint | action=suppressed reason={} range={}",
                                        reason,
                                        fetcher::format_fetch_range(&range)
                                    );
                                }
                            } else {
                                log::debug!(
                                    "CHART Footprint | action=skip reason=already_covered range={}",
                                    fetcher::format_time_range(fetch_from, fetch_to)
                                );
                            }
                        } else {
                            log::debug!("CHART Footprint | action=skip reason=no_missing_trades");
                        }
                    } else if !is_trade_fetch_enabled() {
                        log::debug!("CHART Footprint | action=skip reason=trade_fetch_disabled");
                    } else {
                        log::debug!("CHART Footprint | action=skip reason=already_fetching");
                    }
                }

                // Candlestick SVP consumes the same raw trade dataset as
                // footprint and bubbles. Fetch in bounded chronological chunks
                // so daily/weekly profiles never create one unbounded request.
                let svp_enabled = self.indicator_enabled(KlineIndicator::SessionVolumeProfile);
                let vwap_enabled = self.indicator_enabled(KlineIndicator::Vwap);
                if matches!(self.kind, KlineChartKind::Candles)
                    && (svp_enabled || vwap_enabled)
                    && !self.fetching_trades.0
                {
                    let svp_cfg = self.visual_config.session_volume_profile;
                    let vwap_cfg = self.visual_config.vwap;
                    let mut requested_from = visible_earliest_ms;
                    if svp_enabled {
                        requested_from = requested_from.min(UnixMs::new(align_session_start(
                            visible_earliest,
                            svp_cfg.interval.milliseconds(),
                        )));
                    }
                    if vwap_enabled {
                        requested_from =
                            requested_from.min(vwap_required_from.unwrap_or_else(|| {
                                UnixMs::new(align_session_start(
                                    target_to.saturating_sub(1).as_u64(),
                                    vwap_cfg.anchor.milliseconds(),
                                ))
                            }));
                    }
                    let requested_to = if vwap_enabled {
                        historical_trade_to
                    } else {
                        visible_latest_ms.max(kline_latest).min(historical_trade_to)
                    };
                    let requested_from = requested_from.max(kline_earliest);
                    if requested_to > requested_from
                        && let Some((from, to)) =
                            self.latest_uncovered_trade_range(requested_from, requested_to)
                    {
                        // One hour per worker keeps exchange pagination and the
                        // UI responsive. Start from the newest data and move
                        // backwards toward the session boundary.
                        let chunk_from =
                            UnixMs::new(from.as_u64().max(to.as_u64().saturating_sub(60 * 60_000)));
                        let range = FetchRange::Trades(chunk_from, to);
                        log::info!(
                            "OVERLAY Fetch | svp={} vwap={} range={}",
                            svp_enabled,
                            vwap_enabled,
                            fetcher::format_fetch_range(&range)
                        );
                        if let Some(action) = request_fetch(
                            &mut self.request_handler,
                            range,
                            Some(&self.chart.ticker_info),
                        ) {
                            self.fetching_trades = (true, None);
                            return Some(action);
                        }
                    }
                }

                if matches!(self.kind, KlineChartKind::Candles)
                    && self.indicator_enabled(KlineIndicator::VolumeBubbles)
                    && self.visual_config.volume_bubbles.enabled
                    && !self.fetching_trades.0
                {
                    const BUBBLE_FETCH_CHUNK_MS: u64 = 15 * 60_000;
                    if let Some((window_from, window_to)) = bubble_required_range
                        && let Some(window_to) = (window_from < historical_trade_to)
                            .then_some(window_to.min(historical_trade_to))
                        && let Some((gap_from, gap_to)) =
                            self.latest_uncovered_bubble_summary_range(window_from, window_to)
                    {
                        let fetch_to = gap_to;
                        let fetch_from = UnixMs::new(
                            gap_from
                                .as_u64()
                                .max(fetch_to.as_u64().saturating_sub(BUBBLE_FETCH_CHUNK_MS)),
                        );
                        let config = self.visual_config.volume_bubbles;
                        let max_candidates = config
                            .max_candidates_per_candle
                            .max(config.max_bubbles_per_bar);

                        if config.use_raw_trades_when_available
                            && self.is_raw_trade_range_available(fetch_from, fetch_to)
                        {
                            let summaries = self.bubble_summaries_from_raw_trades(
                                fetch_from,
                                fetch_to,
                                timeframe_ms,
                                self.chart.tick_size,
                                &config,
                            );
                            self.insert_bubble_summaries(
                                summaries, fetch_from, fetch_to, 0, 0, None,
                            );
                            return None;
                        }

                        let range = FetchRange::BubbleSummary {
                            from: fetch_from,
                            to: fetch_to,
                            timeframe_ms,
                            price_step: self.chart.tick_size,
                            max_candidates_per_candle: max_candidates,
                            cluster_window_ms: config.cluster_window_ms,
                            cluster_price_ticks: config.cluster_price_ticks,
                        };
                        if let Some(action) = request_fetch(
                            &mut self.request_handler,
                            range,
                            Some(&self.chart.ticker_info),
                        ) {
                            return Some(action);
                        }
                    }
                }

                // priority 3, indicators
                // (e.g. open interest needs external fetch as it's not derived from klines)
                let ctx = indicator::kline::FetchCtx {
                    main_chart: &self.chart,
                    timeframe: timeseries.interval,
                    visible_earliest: visible_earliest_ms,
                    kline_latest,
                    prefetch_earliest: UnixMs::new(prefetch_earliest),
                };
                for (indicator_kind, indi) in self.indicators.iter_mut() {
                    let Some(indi) = indi.as_mut() else {
                        continue;
                    };
                    if let Some(range) = indi.fetch_range(&ctx) {
                        log::debug!(
                            "CHART IndicatorFetch | indicator={:?} range={}",
                            indicator_kind,
                            fetcher::format_fetch_range(&range)
                        );
                        if let Some(action) = request_fetch(
                            &mut self.request_handler,
                            range,
                            Some(&self.chart.ticker_info),
                        ) {
                            log::info!(
                                "CHART IndicatorFetchQueued | indicator={:?} range={}",
                                indicator_kind,
                                fetcher::format_fetch_range(&range)
                            );
                            return Some(action);
                        } else {
                            log::debug!(
                                "CHART IndicatorFetchSuppressed | indicator={:?} range={} reason=request_handler",
                                indicator_kind,
                                fetcher::format_fetch_range(&range)
                            );
                        }
                    }
                }

                // priority 4, missing klines & integrity check
                let check_earliest = UnixMs::new(prefetch_earliest).max(kline_earliest);
                let check_latest = visible_latest_ms.saturating_add(timeframe_ms);
                log::trace!(
                    "KLINE IntegrityCheck | check_earliest={} check_latest={}",
                    fetcher::format_time_short(check_earliest),
                    fetcher::format_time_short(check_latest)
                );

                if let Some(missing_keys) =
                    timeseries.check_kline_integrity(check_earliest, check_latest)
                {
                    let latest = missing_keys
                        .iter()
                        .max()
                        .unwrap_or(&visible_latest_ms)
                        .saturating_add(timeframe_ms);
                    let earliest = missing_keys
                        .iter()
                        .min()
                        .unwrap_or(&visible_earliest_ms)
                        .saturating_sub(timeframe_ms);

                    let range = FetchRange::Kline(earliest, latest);
                    log::warn!(
                        "KLINE IntegrityMissing | missing_count={} min={} max={} fetch={}",
                        missing_keys.len(),
                        missing_keys
                            .iter()
                            .min()
                            .map_or("-".to_string(), |t| fetcher::format_time_short(*t)),
                        missing_keys
                            .iter()
                            .max()
                            .map_or("-".to_string(), |t| fetcher::format_time_short(*t)),
                        fetcher::format_fetch_range(&range)
                    );
                    if let Some(action) = request_fetch(
                        &mut self.request_handler,
                        range,
                        Some(&self.chart.ticker_info),
                    ) {
                        return Some(action);
                    } else {
                        log::debug!(
                            "KLINE IntegrityFetchSuppressed | reason=request_handler fetch={}",
                            fetcher::format_fetch_range(&range)
                        );
                    }
                } else {
                    log::trace!(
                        "KLINE IntegrityPassed | check_range={}",
                        fetcher::format_time_range(check_earliest, check_latest)
                    );
                }
            }
            PlotData::TickBased(_) => {
                // TODO: implement trade fetch
                log::trace!(
                    "CHART TickBased | action=skip reason=trade_fetch_todo kind={:?}",
                    self.kind
                );
            }
        }

        None
    }

    pub fn reset_request_handler(&mut self) {
        let old_generation = self.request_handler.generation_id();
        let superseded_ids = self
            .request_handler
            .supersede_all_pending("settings_changed");

        log::info!(
            "CHART Reset | reason=settings_changed old_generation={} new_generation={} superseded_requests={}",
            old_generation,
            self.request_handler.generation_id(),
            superseded_ids.len()
        );

        // The superseded requests are still in the handler with Superseded status.
        // When their workers complete, they will be detected as stale generation.
        // We keep the handler (don't replace it) so we can track stale results.

        self.fetching_trades = (false, None);
        self.covered_trade_ranges.clear();
        self.covered_bubble_summary_ranges.clear();
    }

    /// Drops all derived historical market data so it is rebuilt from a fresh
    /// persistent-cache/network pass on the next chart tick.
    pub fn invalidate_market_data_cache(&mut self) {
        self.request_handler = RequestHandler::default();
        log::warn!("CHART Reset | reason=cache_invalidated request_history=discarded");
        self.raw_trades.clear();
        self.raw_trades_pruned = false;

        match &mut self.data_source {
            PlotData::TimeBased(timeseries) => {
                for data_point in timeseries.datapoints.values_mut() {
                    data_point.clear_trades();
                    data_point.bubble_summary = BubbleVolumeSummary::default();
                    data_point.trade_coverage = data::chart::kline::TradeCoverage::Unknown;
                }
                timeseries.update_poc_status();
            }
            PlotData::TickBased(_) => {}
        }

        for indicator in self.indicators.values_mut().filter_map(Option::as_mut) {
            indicator.rebuild_from_source(&self.data_source);
            indicator.clear_all_caches();
        }
        self.chart.cache.clear_all();
        self.last_tick = Instant::now() - std::time::Duration::from_secs(1);
    }

    /// Check if a fetch result should be applied or discarded as stale.
    pub fn is_fetch_stale(&self, req_id: uuid::Uuid) -> bool {
        self.request_handler.is_stale_generation(req_id)
    }

    /// Get the generation ID of a request for logging.
    pub fn request_generation(&self, req_id: uuid::Uuid) -> Option<u64> {
        self.request_handler.request_generation(req_id)
    }

    /// Get the current generation ID.
    pub fn current_generation(&self) -> u64 {
        self.request_handler.generation_id()
    }

    pub fn register_backfill_request(&mut self, req_id: uuid::Uuid, fetch: FetchRange) -> bool {
        match self
            .request_handler
            .add_request_with_id(req_id, fetch, Some(&self.chart.ticker_info))
        {
            Ok(Some(_)) => true,
            Ok(None) => false,
            Err(ReqError::Failed(reason)) => {
                log::error!("Failed to request {:?}: {}", fetch, reason);
                false
            }
        }
    }

    pub fn mark_trade_request_completed(&mut self, req_id: uuid::Uuid) {
        self.request_handler.mark_completed(req_id);
    }

    pub fn mark_request_failed(&mut self, req_id: uuid::Uuid, error: String) {
        let failed_trade_request = self.request_handler.is_trade_request(req_id);
        self.request_handler.mark_failed(req_id, error);
        if failed_trade_request {
            self.fetching_trades = (false, None);
        }
    }

    pub fn mark_trade_range_covered(&mut self, from: UnixMs, to: UnixMs) {
        if to <= from {
            log::warn!(
                "DATA Trades CoveredSkip | incoming_range={} reason=invalid_range",
                fetcher::format_time_range(from, to)
            );
            return;
        }

        let before = self.covered_trade_ranges.clone();
        self.covered_trade_ranges.push((from, to));
        self.covered_trade_ranges.sort_by_key(|(from, _)| *from);

        let mut merged: Vec<(UnixMs, UnixMs)> = Vec::new();
        for (range_from, range_to) in self.covered_trade_ranges.drain(..) {
            if let Some((_, last_to)) = merged.last_mut()
                && range_from <= *last_to
            {
                *last_to = (*last_to).max(range_to);
                continue;
            }

            merged.push((range_from, range_to));
        }

        self.covered_trade_ranges = merged;
        log::debug!(
            "DATA Trades Covered | incoming_range={} before={} after={}",
            fetcher::format_time_range(from, to),
            format_trade_ranges(&before),
            format_trade_ranges(&self.covered_trade_ranges)
        );
    }

    pub fn mark_bubble_summary_range_covered(&mut self, from: UnixMs, to: UnixMs) {
        if to <= from {
            log::warn!(
                "BUBBLE Summary Skip | reason=invalid_range range={}",
                fetcher::format_time_range(from, to)
            );
            return;
        }

        let before = self.covered_bubble_summary_ranges.clone();
        self.covered_bubble_summary_ranges.push((from, to));
        self.covered_bubble_summary_ranges
            .sort_by_key(|(from, _)| *from);

        let mut merged: Vec<(UnixMs, UnixMs)> = Vec::new();
        for (range_from, range_to) in self.covered_bubble_summary_ranges.drain(..) {
            if let Some((_, last_to)) = merged.last_mut()
                && range_from <= *last_to
            {
                *last_to = (*last_to).max(range_to);
                continue;
            }

            merged.push((range_from, range_to));
        }

        self.covered_bubble_summary_ranges = merged;
        log::debug!(
            "BUBBLE Summary Covered | incoming_range={} before={} after={}",
            fetcher::format_time_range(from, to),
            format_trade_ranges(&before),
            format_trade_ranges(&self.covered_bubble_summary_ranges)
        );
    }

    pub fn is_trade_range_covered(&self, from: UnixMs, to: UnixMs) -> bool {
        self.covered_trade_ranges
            .iter()
            .any(|(covered_from, covered_to)| from >= *covered_from && to <= *covered_to)
    }

    fn is_raw_trade_range_available(&self, from: UnixMs, to: UnixMs) -> bool {
        self.is_trade_range_covered(from, to)
            && (!self.raw_trades_pruned
                || self
                    .raw_trades
                    .first()
                    .is_some_and(|earliest| earliest.time <= from))
    }

    pub fn subtract_covered_trade_ranges(
        &self,
        from: UnixMs,
        to: UnixMs,
    ) -> Option<(UnixMs, UnixMs)> {
        if to <= from {
            log::debug!(
                "DATA Trades SubtractCovered | input_range={} covered={} returned=- reason=invalid_range",
                fetcher::format_time_range(from, to),
                format_trade_ranges(&self.covered_trade_ranges)
            );
            return None;
        }

        if self.is_trade_range_covered(from, to) {
            log::debug!(
                "DATA Trades SubtractCovered | input_range={} covered={} returned=- reason=fully_covered",
                fetcher::format_time_range(from, to),
                format_trade_ranges(&self.covered_trade_ranges)
            );
            return None;
        }

        let mut cursor = from;
        for (covered_from, covered_to) in &self.covered_trade_ranges {
            if *covered_to <= cursor {
                continue;
            }

            if *covered_from > cursor {
                let result = (cursor, (*covered_from).min(to));
                log::debug!(
                    "DATA Trades SubtractCovered | input_range={} covered={} returned={} reason=gap_before_covered",
                    fetcher::format_time_range(from, to),
                    format_trade_ranges(&self.covered_trade_ranges),
                    fetcher::format_time_range(result.0, result.1)
                );
                return Some(result);
            }

            cursor = cursor.max(*covered_to);
            if cursor >= to {
                log::debug!(
                    "DATA Trades SubtractCovered | input_range={} covered={} returned=- reason=fully_covered_after_merge",
                    fetcher::format_time_range(from, to),
                    format_trade_ranges(&self.covered_trade_ranges)
                );
                return None;
            }
        }

        let result = (cursor, to);
        log::debug!(
            "DATA Trades SubtractCovered | input_range={} covered={} returned={} reason=tail_gap",
            fetcher::format_time_range(from, to),
            format_trade_ranges(&self.covered_trade_ranges),
            fetcher::format_time_range(result.0, result.1)
        );
        Some(result)
    }

    pub fn subtract_covered_bubble_summary_ranges(
        &self,
        from: UnixMs,
        to: UnixMs,
    ) -> Option<(UnixMs, UnixMs)> {
        subtract_covered_ranges(
            &self.covered_bubble_summary_ranges,
            from,
            to,
            "BUBBLE Summary",
        )
    }

    fn latest_uncovered_trade_range(&self, from: UnixMs, to: UnixMs) -> Option<(UnixMs, UnixMs)> {
        select_trade_fetch_gap(&self.covered_trade_ranges, from, to)
    }

    fn latest_uncovered_bubble_summary_range(
        &self,
        from: UnixMs,
        to: UnixMs,
    ) -> Option<(UnixMs, UnixMs)> {
        subtract_covered_ranges_latest(
            &self.covered_bubble_summary_ranges,
            from,
            to,
            "BUBBLE Summary Latest",
        )
    }

    pub fn missing_trade_range(&self, from: UnixMs, to: UnixMs) -> Option<(UnixMs, UnixMs)> {
        self.subtract_covered_trade_ranges(from, to)
    }

    pub fn complete_trade_fetch(
        &mut self,
        req_id: Option<uuid::Uuid>,
        fetch: Option<FetchRange>,
        outcome: fetcher::TradeFetchOutcome,
    ) {
        log::debug!(
            "TRADE CompleteFetch | req={} fetch={} fetching_before={} tail={}",
            fetcher::format_req_id(req_id),
            fetcher::format_fetch_range_compact(fetch),
            self.fetching_trades.0,
            outcome
                .unfilled_tail
                .map(|(f, t)| fetcher::format_time_range(f, t))
                .unwrap_or_else(|| "-".to_string())
        );
        if let Some(id) = req_id {
            self.mark_trade_request_completed(id);
        }

        if let Some(FetchRange::Trades(from, to)) = fetch {
            if let Some((tail_from, tail_to)) = outcome.empty_tail {
                log::info!(
                    "FETCH EmptyCovered | req={} range={}→{} reason=no_progress_near_target",
                    fetcher::format_req_id(req_id),
                    fetcher::format_time_short(tail_from),
                    fetcher::format_time_short(tail_to)
                );
                self.request_handler.mark_empty_trade_range(
                    &self.chart.ticker_info,
                    tail_from,
                    tail_to,
                );
            }
            self.mark_verified_trade_fetch_prefix(from, to, outcome.unfilled_tail);
        }

        self.fetching_trades = (false, None);
        log::debug!(
            "TRADE CompleteFetch | req={} fetching_after=false",
            fetcher::format_req_id(req_id)
        );
    }

    /// Mark a backfill as completed without touching per-pane fetching_trades
    /// state or RequestHandler. Backfill is tracked globally via pending_backfills.
    pub fn complete_backfill(
        &mut self,
        fetch: Option<FetchRange>,
        outcome: fetcher::TradeFetchOutcome,
    ) {
        log::info!(
            "BACKFILL Complete | fetch={} tail={}",
            fetcher::format_fetch_range_compact(fetch),
            outcome
                .unfilled_tail
                .map(|(f, t)| fetcher::format_time_range(f, t))
                .unwrap_or_else(|| "-".to_string())
        );

        if let Some(FetchRange::Trades(from, to)) = fetch {
            if let Some((tail_from, tail_to)) = outcome.empty_tail {
                self.request_handler.mark_empty_trade_range(
                    &self.chart.ticker_info,
                    tail_from,
                    tail_to,
                );
            }
            self.mark_verified_trade_fetch_prefix(from, to, outcome.unfilled_tail);
        }
    }

    /// Marks only the portion a trade worker actually traversed. An empty or
    /// no-progress tail is a retryable gap, not completed order-flow history.
    fn mark_verified_trade_fetch_prefix(
        &mut self,
        from: UnixMs,
        to: UnixMs,
        unfilled_tail: Option<(UnixMs, UnixMs)>,
    ) {
        let verified_to = unfilled_tail
            .map(|(tail_from, _)| tail_from.saturating_sub(1).min(to))
            .unwrap_or(to);

        if verified_to > from {
            self.mark_trade_range_covered(from, verified_to);
            self.mark_trade_buckets_complete(from, verified_to);
        } else {
            log::warn!(
                "DATA Trades CoverageSkipped | requested={} unfilled_tail={} reason=no_verified_prefix",
                fetcher::format_time_range(from, to),
                unfilled_tail
                    .map(|(tail_from, tail_to)| fetcher::format_time_range(tail_from, tail_to))
                    .unwrap_or_else(|| "-".to_string())
            );
        }
    }

    pub fn complete_bubble_summary_fetch(
        &mut self,
        req_id: Option<uuid::Uuid>,
        from: UnixMs,
        to: UnixMs,
    ) {
        log::info!(
            "BUBBLE Summary CompleteFetch | req={} range={}",
            fetcher::format_req_id(req_id),
            fetcher::format_time_range(from, to)
        );
        if let Some(id) = req_id {
            self.mark_trade_request_completed(id);
        }
        self.mark_bubble_summary_range_covered(from, to);
        // A BubbleSummary contains derived price/volume candidates, not the
        // raw executions needed by the footprint. It must not promote raw
        // trade coverage to Complete.
    }

    fn mark_trade_buckets_complete(&mut self, from: UnixMs, to: UnixMs) {
        match &mut self.data_source {
            PlotData::TimeBased(ts) => {
                ts.mark_range_trades_complete(from, to);
            }
            PlotData::TickBased(_) => {}
        }
        if let Some(cvd) = self.indicators[KlineIndicator::CumulativeDelta].as_mut() {
            cvd.rebuild_from_source(&self.data_source);
        }
    }

    /// Mark all fully traversed klines in the visible range as complete.
    /// Called when a trade fetch completes with empty results to prevent
    /// re-requesting the same range.
    pub fn mark_visible_range_trades_complete(&mut self) {
        let (visible_earliest, visible_latest) = match self.visible_timerange() {
            Some(range) => range,
            None => return,
        };
        let earliest_ms = exchange::UnixMs::new(visible_earliest);
        let latest_ms = exchange::UnixMs::new(visible_latest);

        match &mut self.data_source {
            PlotData::TimeBased(ts) => {
                ts.mark_range_trades_complete(earliest_ms, latest_ms);
            }
            PlotData::TickBased(_) => {}
        }
    }

    pub fn raw_trades(&self) -> Vec<Trade> {
        self.raw_trades.clone()
    }

    pub fn set_handle(&mut self, handle: Handle) {
        self.fetching_trades.1 = Some(handle);
    }

    pub fn tick_size(&self) -> PriceStep {
        self.chart.tick_size
    }

    pub fn study_configurator(&self) -> &study::Configurator<FootprintStudy> {
        &self.study_configurator
    }

    pub fn update_study_configurator(&mut self, message: study::Message<FootprintStudy>) {
        let KlineChartKind::Footprint {
            ref mut studies, ..
        } = self.kind
        else {
            return;
        };

        match self.study_configurator.update(message) {
            Some(study::Action::ToggleStudy(study, is_selected)) => {
                if is_selected {
                    let already_exists = studies.iter().any(|s| s.is_same_type(&study));
                    if !already_exists {
                        studies.push(study);
                    }
                } else {
                    studies.retain(|s| !s.is_same_type(&study));
                }
            }
            Some(study::Action::ConfigureStudy(study)) => {
                if let Some(existing_study) = studies.iter_mut().find(|s| s.is_same_type(&study)) {
                    *existing_study = study;
                }
            }
            None => {}
        }

        self.invalidate(None);
    }

    pub fn chart_layout(&self) -> ViewConfig {
        self.chart.layout()
    }

    pub fn visual_config(&self) -> Config {
        self.visual_config
    }

    pub fn indicator_enabled(&self, indicator: KlineIndicator) -> bool {
        self.indicators[indicator].is_some()
    }

    pub fn set_gex_snapshot(&mut self, snapshot: Option<Arc<data::chart::gex::GexSnapshot>>) {
        if self.gex_snapshot.as_ref().map(|value| value.observed_at)
            == snapshot.as_ref().map(|value| value.observed_at)
            && self.gex_snapshot.as_ref().map(|value| value.underlying)
                == snapshot.as_ref().map(|value| value.underlying)
        {
            return;
        }
        self.gex_snapshot = snapshot;
        self.chart.cache.clear_all();
    }

    pub fn set_gex_overlay_data(
        &mut self,
        snapshot: Option<Arc<data::chart::gex::GexSnapshot>>,
        history: Vec<Arc<data::chart::gex::GexSnapshot>>,
        freshness: data::chart::gex::GexFreshness,
        error: Option<Arc<str>>,
        proxy_history: Vec<Arc<exchange::options::gex_monitor::GexProxyHistoryPoint>>,
        proxy_freshness: data::chart::gex::GexFreshness,
        proxy_error: Option<Arc<str>>,
        derive_flow: Option<Arc<data::chart::gex::DeriveMakerGammaFlow>>,
        derive_freshness: data::chart::gex::GexFreshness,
    ) {
        let unchanged = self.gex_snapshot.as_ref().map(|value| value.observed_at)
            == snapshot.as_ref().map(|value| value.observed_at)
            && self.gex_history.len() == history.len()
            && self.gex_history.last().map(|value| value.observed_at)
                == history.last().map(|value| value.observed_at)
            && self.gex_freshness == freshness
            && self.gex_error == error;
        let unchanged = unchanged
            && self.gex_proxy_history.len() == proxy_history.len()
            && self.gex_proxy_history.last().map(|value| value.observed_at)
                == proxy_history.last().map(|value| value.observed_at)
            && self.gex_proxy_freshness == proxy_freshness
            && self.gex_proxy_error == proxy_error;
        let unchanged = unchanged
            && self.derive_flow.as_ref().map(|flow| flow.observed_at)
                == derive_flow.as_ref().map(|flow| flow.observed_at)
            && self.derive_freshness == derive_freshness;
        if unchanged {
            return;
        }
        self.gex_snapshot = snapshot;
        self.gex_history = history;
        self.gex_freshness = freshness;
        self.gex_error = error;
        self.gex_proxy_history = proxy_history;
        self.gex_proxy_freshness = proxy_freshness;
        self.gex_proxy_error = proxy_error;
        self.derive_flow = derive_flow;
        self.derive_freshness = derive_freshness;
        self.chart.cache.clear_all();
    }

    pub fn volume_bubble_qty_scale(&self) -> VolumeBubbleQtyScale {
        let range = match &self.data_source {
            PlotData::TimeBased(timeseries) => timeseries.latest_timestamp().and_then(|latest| {
                volume_bubble_effective_range(
                    latest,
                    timeseries.interval.to_milliseconds(),
                    UnixMs::now(),
                    &self.visual_config.volume_bubbles,
                )
            }),
            PlotData::TickBased(_) => None,
        };

        volume_bubble_qty_scale(max_bubble_qty_in_range(
            &self.data_source,
            range.map_or(1, |(from, _)| from.as_u64()),
            range.map_or(0, |(_, to)| to.as_u64()),
            self.visual_config
                .volume_bubbles
                .use_raw_trades_when_available,
        ))
    }

    pub fn gex_proxy_available(&self) -> bool {
        !self.gex_proxy_history.is_empty()
    }

    pub fn set_visual_config(&mut self, mut visual_config: Config) {
        visual_config.migrate_legacy_indicator_configs();
        let old_bubbles = self.visual_config.volume_bubbles;
        let new_bubbles = visual_config.volume_bubbles;
        let old_svp = self.visual_config.session_volume_profile;
        let new_svp = visual_config.session_volume_profile;
        let old_vwap = self.visual_config.vwap;
        let new_vwap = visual_config.vwap;

        let should_refetch_volume_bubbles = matches!(self.kind, KlineChartKind::Candles)
            && self.indicator_enabled(KlineIndicator::VolumeBubbles)
            && new_bubbles.enabled
            && (old_bubbles.history_window_minutes != new_bubbles.history_window_minutes
                || old_bubbles.session != new_bubbles.session
                || old_bubbles
                    .max_candidates_per_candle
                    .max(old_bubbles.max_bubbles_per_bar)
                    != new_bubbles
                        .max_candidates_per_candle
                        .max(new_bubbles.max_bubbles_per_bar)
                || old_bubbles.use_raw_trades_when_available
                    != new_bubbles.use_raw_trades_when_available);
        let bubble_aggregation_changed = old_bubbles
            .max_candidates_per_candle
            .max(old_bubbles.max_bubbles_per_bar)
            != new_bubbles
                .max_candidates_per_candle
                .max(new_bubbles.max_bubbles_per_bar);
        let should_wake_trade_overlay = matches!(self.kind, KlineChartKind::Candles)
            && ((self.indicator_enabled(KlineIndicator::SessionVolumeProfile)
                && old_svp.interval != new_svp.interval)
                || (self.indicator_enabled(KlineIndicator::Vwap)
                    && old_vwap.anchor != new_vwap.anchor));

        if should_refetch_volume_bubbles {
            log::info!(
                "CHART Settings | bubbles old={:?}→{:?} reason=session_changed",
                old_bubbles.session,
                new_bubbles.session
            );
        }

        self.visual_config = visual_config;
        let config = self.visual_config;
        self.chart.cache.clear_all();
        self.indicators
            .values_mut()
            .filter_map(Option::as_mut)
            .for_each(|indi| {
                indi.on_config_changed(&config);
                indi.clear_all_caches();
            });

        if should_refetch_volume_bubbles {
            if bubble_aggregation_changed {
                self.covered_bubble_summary_ranges.clear();
                if let PlotData::TimeBased(timeseries) = &mut self.data_source {
                    for data_point in timeseries.datapoints.values_mut() {
                        data_point.bubble_summary = BubbleVolumeSummary::default();
                    }
                }
            }
            self.last_tick = Instant::now() - std::time::Duration::from_secs(1);
        } else if should_wake_trade_overlay {
            // Existing raw trades are reusable across session/row settings.
            self.last_tick = Instant::now() - std::time::Duration::from_secs(1);
        }
    }

    pub fn set_cluster_kind(&mut self, new_kind: ClusterKind) {
        if let KlineChartKind::Footprint {
            ref mut clusters, ..
        } = self.kind
        {
            *clusters = new_kind;
        }

        self.invalidate(None);
    }

    pub fn set_cluster_scaling(&mut self, new_scaling: ClusterScaling) {
        if let KlineChartKind::Footprint {
            ref mut scaling, ..
        } = self.kind
        {
            *scaling = new_scaling;
        }

        self.invalidate(None);
    }

    pub fn basis(&self) -> Basis {
        self.chart.basis
    }

    pub fn change_tick_size(&mut self, new_step: PriceStep) {
        let chart = self.mut_state();

        chart.cell_height *= (new_step.units as f32) / (chart.tick_size.units as f32);
        chart.tick_size = new_step;

        match self.data_source {
            PlotData::TickBased(ref mut tick_aggr) => {
                tick_aggr.change_tick_size(new_step, &self.raw_trades);
            }
            PlotData::TimeBased(ref mut timeseries) => {
                timeseries.change_tick_size(new_step);
            }
        }

        self.indicators
            .values_mut()
            .filter_map(Option::as_mut)
            .for_each(|indi| indi.on_ticksize_change(&self.data_source));

        self.invalidate(None);
    }

    pub fn set_basis(&mut self, new_basis: Basis) -> Option<Action> {
        let previous_basis = self.chart.basis;

        self.chart.last_price = None;
        self.chart.basis = new_basis;

        match new_basis {
            Basis::Time(interval) => {
                if matches!(previous_basis, Basis::Tick(_)) {
                    self.raw_trades.clear();
                    self.raw_trades_pruned = false;
                };

                let step = self.chart.tick_size;
                let timeseries = TimeSeries::<KlineDataPoint>::new(interval, step, &[]);
                self.data_source = PlotData::TimeBased(timeseries);
            }
            Basis::Tick(tick_count) => {
                let trades = if matches!(previous_basis, Basis::Tick(_)) {
                    &self.raw_trades
                } else {
                    self.raw_trades.clear();
                    self.raw_trades_pruned = false;
                    &vec![]
                };

                let step = self.chart.tick_size;
                let tick_aggr = TickAggr::new(tick_count, step, trades);
                self.data_source = PlotData::TickBased(tick_aggr);
            }
        }

        self.indicators
            .values_mut()
            .filter_map(Option::as_mut)
            .for_each(|indi| indi.on_basis_change(&self.data_source));

        self.reset_request_handler();
        self.invalidate(Some(Instant::now()))
    }

    pub fn studies(&self) -> Option<Vec<FootprintStudy>> {
        match &self.kind {
            KlineChartKind::Footprint { studies, .. } => Some(studies.clone()),
            _ => None,
        }
    }

    pub fn set_studies(&mut self, new_studies: Vec<FootprintStudy>) {
        if let KlineChartKind::Footprint {
            ref mut studies, ..
        } = self.kind
        {
            *studies = new_studies;
        }

        self.invalidate(None);
    }

    pub fn insert_trades(&mut self, buffer: &[Trade]) {
        let buffer = deduplicate_incoming_trades(
            &self.raw_trades,
            buffer,
            "live_stream",
            Some(self.chart.ticker_info),
        );
        if self.chart.ticker_info.exchange() == exchange::adapter::Exchange::BinanceLinear
            && let PlotData::TimeBased(timeseries) = &self.data_source
        {
            let interval = timeseries.interval;
            let interval_ms = timeseries.interval.to_milliseconds();
            let latest_bucket = buffer
                .iter()
                .map(|trade| trade.time.floor_to(interval))
                .max();
            self.live_trade_buckets
                .extend(buffer.iter().map(|trade| trade.time.floor_to(interval)));
            if self.live_trade_buckets.len() > MAX_LIVE_TRADE_BUCKETS
                && let Some(latest) = latest_bucket
            {
                let cutoff = latest
                    .saturating_sub(interval_ms.saturating_mul(MAX_LIVE_TRADE_BUCKETS as u64));
                self.live_trade_buckets.retain(|bucket| *bucket >= cutoff);
            }
        }
        let raw_before = self.raw_trades.len();
        self.raw_trades.extend_from_slice(&buffer);

        // Historical backfills can arrive after newer live data. Retain by
        // exchange timestamp rather than insertion order so the newest raw
        // window remains available to overlays.
        let excess = retain_latest_raw_trades(&mut self.raw_trades, raw_before);
        if excess > 0 {
            self.raw_trades_pruned = true;
            log::debug!(
                "DATA Trades Prune | reason=cap exceeded={} removed={excess} retained={}",
                self.raw_trades.len() + excess,
                self.raw_trades.len()
            );
        }

        let content_type = match self.data_source {
            PlotData::TickBased(_) => "TickBased",
            PlotData::TimeBased(_) => "TimeBased",
        };
        log::trace!(
            "TRADE InsertLive | content_type={content_type} buffer_len={} first_trade_t={} last_trade_t={} raw_before={} raw_after={}",
            buffer.len(),
            fetcher::format_optional_time(buffer.first().map(|trade| trade.time)),
            fetcher::format_optional_time(buffer.last().map(|trade| trade.time)),
            raw_before,
            self.raw_trades.len()
        );

        match self.data_source {
            PlotData::TickBased(ref mut tick_aggr) => {
                let old_dp_len = tick_aggr.datapoints.len();
                tick_aggr.insert_trades(&buffer);

                if let Some(last_dp) = tick_aggr.datapoints.last() {
                    self.chart.last_price =
                        Some(PriceInfoLabel::new(last_dp.kline.close, last_dp.kline.open));
                } else {
                    self.chart.last_price = None;
                }

                self.indicators
                    .values_mut()
                    .filter_map(Option::as_mut)
                    .for_each(|indi| indi.on_insert_trades(&buffer, old_dp_len, &self.data_source));

                self.invalidate(None);
            }
            PlotData::TimeBased(ref mut timeseries) => {
                timeseries.insert_trades_existing_buckets(&buffer);

                self.indicators
                    .values_mut()
                    .filter_map(Option::as_mut)
                    .for_each(|indi| indi.on_insert_trades(&buffer, 0, &self.data_source));

                self.invalidate(None);
            }
        }
    }

    pub fn insert_raw_trades(&mut self, raw_trades: Vec<Trade>, is_batches_done: bool) {
        let received_size = raw_trades.len();
        let raw_trades = deduplicate_incoming_trades(
            &self.raw_trades,
            &raw_trades,
            "historical_fetch",
            Some(self.chart.ticker_info),
        );
        let (raw_trades, live_overlap_discarded) = match &self.data_source {
            PlotData::TimeBased(timeseries) => exclude_historical_overlap_with_live(
                raw_trades,
                &self.live_trade_buckets,
                timeseries.interval,
            ),
            PlotData::TickBased(_) => (raw_trades, 0),
        };
        let batch_size = raw_trades.len();
        let duplicate_count = received_size
            .saturating_sub(batch_size)
            .saturating_sub(live_overlap_discarded);
        let raw_before = self.raw_trades.len();
        let earliest = raw_trades.first().map(|t| t.time);
        let latest = raw_trades.last().map(|t| t.time);

        log::debug!(
            "DATA Trades | received={received_size} deduplicated={duplicate_count} live_overlap_discarded={live_overlap_discarded} fetched_batch={batch_size} raw_before={raw_before} raw_after={} first={} last={} is_batches_done={is_batches_done}",
            raw_before + batch_size,
            fetcher::format_optional_time(earliest),
            fetcher::format_optional_time(latest)
        );

        if matches!(&self.data_source, PlotData::TickBased(_)) {
            if is_batches_done {
                self.fetching_trades = (false, None);
            }
            return;
        }

        if let PlotData::TimeBased(ref mut timeseries) = self.data_source {
            timeseries.insert_trades_existing_buckets(&raw_trades);
        }

        self.raw_trades.extend_from_slice(&raw_trades);

        // Backfills are requested newest-first, so an older batch may be
        // appended after newer executions already in memory.
        let excess = retain_latest_raw_trades(&mut self.raw_trades, raw_before);
        if excess > 0 {
            self.raw_trades_pruned = true;
            log::debug!(
                "DATA Trades Prune | reason=cap exceeded={} removed={excess} retained={}",
                self.raw_trades.len() + excess,
                self.raw_trades.len()
            );
        }

        self.indicators
            .values_mut()
            .filter_map(Option::as_mut)
            .for_each(|indi| indi.on_insert_trades(&raw_trades, 0, &self.data_source));

        if is_batches_done {
            self.fetching_trades = (false, None);
            log::info!(
                "DATA Trades Done | total_raw={} final_batch={batch_size} is_batches_done={is_batches_done}",
                self.raw_trades.len()
            );
            if batch_size == 0 {
                log::debug!(
                    "DATA Trades Done | final_batch=0 fetching_trades=false reason=terminal_signal_without_new_records"
                );
            }
        }

        self.invalidate(None);
    }

    pub fn insert_bubble_summaries(
        &mut self,
        summaries: Vec<BubbleVolumeSummary>,
        from: UnixMs,
        to: UnixMs,
        trades_seen: usize,
        raw_discarded: usize,
        req_id: Option<uuid::Uuid>,
    ) {
        let candles = summaries.len();
        let candidates = summaries
            .iter()
            .map(|summary| summary.candidates.len())
            .sum::<usize>();

        log::info!(
            "BUBBLE Summary Insert | req={} range={} candles={} candidates={} trades_seen={} raw_discarded={} raw_trades_kept={}",
            fetcher::format_req_id(req_id),
            fetcher::format_time_range(from, to),
            candles,
            candidates,
            trades_seen,
            raw_discarded,
            self.raw_trades.len()
        );

        if let PlotData::TimeBased(ref mut timeseries) = self.data_source {
            timeseries.insert_bubble_summaries(summaries);
        }

        self.complete_bubble_summary_fetch(req_id, from, to);
        self.invalidate(None);
    }

    fn bubble_summaries_from_raw_trades(
        &self,
        from: UnixMs,
        to: UnixMs,
        timeframe_ms: u64,
        price_step: PriceStep,
        config: &VolumeBubbleConfig,
    ) -> Vec<BubbleVolumeSummary> {
        let mut buckets: FxHashMap<UnixMs, Vec<Trade>> = FxHashMap::default();
        for trade in self
            .raw_trades
            .iter()
            .filter(|trade| trade.time >= from && trade.time <= to)
        {
            let candle_time =
                UnixMs::new(trade.time.as_u64() - (trade.time.as_u64() % timeframe_ms));
            buckets.entry(candle_time).or_default().push(*trade);
        }
        let mut summaries = buckets
            .into_iter()
            .map(|(candle_time, trades)| {
                let mut candidates = cluster_volume_bubble_trades(
                    &trades,
                    candle_time,
                    timeframe_ms,
                    price_step,
                    config,
                );
                candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.total_qty));
                candidates.truncate(
                    config
                        .max_candidates_per_candle
                        .max(config.max_bubbles_per_bar),
                );
                BubbleVolumeSummary::new(candle_time, candidates)
            })
            .collect::<Vec<_>>();
        summaries.sort_by_key(|summary| summary.candle_time);
        summaries
    }

    pub fn insert_hist_klines(&mut self, req_id: uuid::Uuid, klines_raw: &[Kline]) {
        let count = klines_raw.len();
        let earliest = klines_raw.first().map(|k| k.time);
        let latest = klines_raw.last().map(|k| k.time);

        log::info!(
            "DATA Klines | req={} records={count} first={} last={}",
            fetcher::short_id(req_id),
            fetcher::format_optional_time(earliest),
            fetcher::format_optional_time(latest)
        );

        match self.data_source {
            PlotData::TimeBased(ref mut timeseries) => {
                let new_buckets = klines_raw
                    .iter()
                    .filter(|kline| !timeseries.datapoints.contains_key(&kline.time))
                    .map(|kline| kline.time)
                    .collect::<FxHashSet<_>>();
                timeseries.insert_klines(klines_raw);
                if !new_buckets.is_empty() {
                    let trades_for_new_buckets = self
                        .raw_trades
                        .iter()
                        .filter(|trade| {
                            new_buckets.contains(&trade.time.floor_to(timeseries.interval))
                        })
                        .copied()
                        .collect::<Vec<_>>();
                    timeseries.insert_trades_existing_buckets(&trades_for_new_buckets);
                    log::debug!(
                        "DATA Klines TradeBackfill | new_buckets={} trades={} reason=new_kline_buckets_only",
                        new_buckets.len(),
                        trades_for_new_buckets.len()
                    );
                }

                self.indicators
                    .values_mut()
                    .filter_map(Option::as_mut)
                    .for_each(|indi| indi.on_insert_klines(klines_raw, &self.data_source));

                if klines_raw.is_empty() {
                    log::warn!(
                        "DATA Klines Complete | req={} records=0 transition=failed reason=no_data",
                        fetcher::short_id(req_id)
                    );
                    self.request_handler
                        .mark_failed(req_id, "No data received".to_string());
                } else {
                    log::debug!(
                        "DATA Klines Complete | req={} records={} transition=completed",
                        fetcher::short_id(req_id),
                        klines_raw.len()
                    );
                    self.request_handler.mark_completed(req_id);
                }
                self.invalidate(None);
            }
            PlotData::TickBased(_) => {}
        }
    }

    pub fn insert_open_interest(&mut self, req_id: Option<uuid::Uuid>, oi_data: &[OIData]) {
        if let Some(req_id) = req_id {
            if oi_data.is_empty() {
                self.request_handler
                    .mark_failed(req_id, "No data received".to_string());
            } else {
                self.request_handler.mark_completed(req_id);
            }
        }

        if let Some(indi) = self.indicators[KlineIndicator::OpenInterest].as_mut() {
            indi.on_open_interest(oi_data);
        }
    }

    fn calc_qty_scales(
        &self,
        earliest: u64,
        latest: u64,
        highest: Price,
        lowest: Price,
        step: PriceStep,
        cluster_kind: ClusterKind,
    ) -> f64 {
        let rounded_highest = highest.round_to_side_step(false, step).add_steps(1, step);
        let rounded_lowest = lowest.round_to_side_step(true, step).add_steps(-1, step);

        match &self.data_source {
            PlotData::TimeBased(timeseries) => timeseries
                .max_qty_ts_range(
                    cluster_kind,
                    UnixMs::new(earliest),
                    UnixMs::new(latest),
                    rounded_highest,
                    rounded_lowest,
                )
                .to_f64(),
            PlotData::TickBased(tick_aggr) => {
                let earliest = earliest as usize;
                let latest = latest as usize;

                tick_aggr
                    .max_qty_idx_range(
                        cluster_kind,
                        earliest,
                        latest,
                        rounded_highest,
                        rounded_lowest,
                    )
                    .to_f64()
            }
        }
    }

    pub fn last_update(&self) -> Instant {
        self.last_tick
    }

    pub fn invalidate(&mut self, now: Option<Instant>) -> Option<Action> {
        let chart = &mut self.chart;

        if let Some(autoscale) = chart.layout.autoscale {
            match autoscale {
                super::Autoscale::CenterLatest => {
                    let x_translation = match &self.kind {
                        KlineChartKind::Footprint { .. } => {
                            0.5 * (chart.bounds.width / chart.scaling)
                                - (chart.cell_width / chart.scaling)
                        }
                        KlineChartKind::Candles => {
                            0.5 * (chart.bounds.width / chart.scaling)
                                - (8.0 * chart.cell_width / chart.scaling)
                        }
                    };
                    chart.translation.x = x_translation;

                    let calculate_target_y = |kline: exchange::Kline| -> f32 {
                        let y_low = chart.price_to_y(kline.low);
                        let y_high = chart.price_to_y(kline.high);
                        let y_close = chart.price_to_y(kline.close);

                        let mut target_y_translation = -(y_low + y_high) / 2.0;

                        if chart.bounds.height > f32::EPSILON && chart.scaling > f32::EPSILON {
                            let visible_half_height = (chart.bounds.height / chart.scaling) / 2.0;

                            let view_center_y_centered = -target_y_translation;

                            let visible_y_top = view_center_y_centered - visible_half_height;
                            let visible_y_bottom = view_center_y_centered + visible_half_height;

                            let padding = chart.cell_height;

                            if y_close < visible_y_top {
                                target_y_translation = -(y_close - padding + visible_half_height);
                            } else if y_close > visible_y_bottom {
                                target_y_translation = -(y_close + padding - visible_half_height);
                            }
                        }
                        target_y_translation
                    };

                    chart.translation.y = self.data_source.latest_y_midpoint(calculate_target_y);
                }
                super::Autoscale::FitToVisible => {
                    let visible_region = chart.visible_region(chart.bounds.size());
                    let (start_interval, end_interval) = chart.interval_range(&visible_region);

                    // Overlay levels are intentionally absent here. In particular, aggregate
                    // GEX Monitor synthetic zones are clipped to this market-price viewport and never
                    // participate in its calculation.
                    if let Some((lowest, highest)) = self
                        .data_source
                        .visible_price_range(start_interval, end_interval)
                    {
                        let chart_height = chart.bounds.height;
                        let tick_size = chart.tick_size.to_f32_lossy();

                        if chart_height > f32::EPSILON && tick_size > 0.0 {
                            let (fit_lowest, fit_highest) =
                                if let KlineChartKind::Footprint { .. } = self.kind {
                                    if let Some((footprint_low, footprint_high)) = self
                                        .data_source
                                        .visible_footprint_price_range(start_interval, end_interval)
                                    {
                                        let half_tick = tick_size * 0.5;
                                        (
                                            footprint_low.to_f32_lossy() - half_tick,
                                            footprint_high.to_f32_lossy() + half_tick,
                                        )
                                    } else {
                                        (lowest, highest)
                                    }
                                } else {
                                    (lowest, highest)
                                };

                            let visible_span = (fit_highest - fit_lowest).max(tick_size);
                            let base_padding = visible_span * 0.05; // 5% padding on top and bottom

                            let mut top_padding = base_padding;
                            let mut bottom_padding = base_padding;

                            if let KlineChartKind::Footprint { .. } = self.kind {
                                let provisional_span = visible_span + top_padding + bottom_padding;
                                if provisional_span > 0.0 {
                                    let provisional_cell_height =
                                        (chart_height * tick_size) / provisional_span;

                                    let outer_padding = price_padding_from_pixels(
                                        provisional_cell_height,
                                        tick_size,
                                    );

                                    top_padding += outer_padding;
                                    bottom_padding += outer_padding;

                                    if self.visual_config.show_footprint_summary {
                                        bottom_padding =
                                            bottom_padding.max(FootprintSummaryLayout::padding(
                                                provisional_cell_height,
                                                chart.scaling,
                                                tick_size,
                                            ));
                                    }
                                }
                            }

                            let padded_span = visible_span + top_padding + bottom_padding;
                            if padded_span > 0.0 {
                                chart.cell_height = (chart_height * tick_size) / padded_span;
                                chart.base_price_y = Price::from_f32(fit_highest + top_padding);
                                chart.translation.y = -chart_height / 2.0;
                            }
                        }
                    }
                }
            }
        }

        chart.cache.clear_all();
        for indi in self.indicators.values_mut().filter_map(Option::as_mut) {
            indi.clear_all_caches();
        }

        if let Some(t) = now {
            self.last_tick = t;
            self.fetch_missing_data()
        } else {
            None
        }
    }

    pub fn toggle_indicator(&mut self, indicator: KlineIndicator) {
        if !self.kind.allows_indicator(indicator) {
            return;
        }

        let prev_indi_count = KlineIndicator::for_market(self.chart.ticker_info.market_type())
            .iter()
            .filter(|indicator| !indicator.is_overlay() && self.indicators[**indicator].is_some())
            .count();

        if self.indicators[indicator].is_some() {
            self.indicators[indicator] = None;
        } else {
            let mut box_indi = indicator::kline::make_empty(indicator);
            box_indi.on_config_changed(&self.visual_config);
            box_indi.rebuild_from_source(&self.data_source);
            self.indicators[indicator] = Some(box_indi);
        }

        if let Some(main_split) = self.chart.layout.splits.first() {
            let current_indi_count =
                KlineIndicator::for_market(self.chart.ticker_info.market_type())
                    .iter()
                    .filter(|indicator| {
                        !indicator.is_overlay() && self.indicators[**indicator].is_some()
                    })
                    .count();
            self.chart.layout.splits = data::util::calc_panel_splits(
                *main_split,
                current_indi_count,
                Some(prev_indi_count),
            );
        }
        self.invalidate(None);
        self.last_tick = Instant::now() - std::time::Duration::from_secs(1);
    }
}

fn format_trade_ranges(ranges: &[(UnixMs, UnixMs)]) -> String {
    if ranges.is_empty() {
        return "-".to_string();
    }

    ranges
        .iter()
        .map(|(from, to)| fetcher::format_time_range(*from, *to))
        .collect::<Vec<_>>()
        .join(",")
}

fn subtract_covered_ranges(
    covered_ranges: &[(UnixMs, UnixMs)],
    from: UnixMs,
    to: UnixMs,
    log_prefix: &str,
) -> Option<(UnixMs, UnixMs)> {
    if to <= from {
        log::debug!(
            "{log_prefix} SubtractCovered | input_range={} covered={} returned=- reason=invalid_range",
            fetcher::format_time_range(from, to),
            format_trade_ranges(covered_ranges)
        );
        return None;
    }

    if covered_ranges
        .iter()
        .any(|(covered_from, covered_to)| from >= *covered_from && to <= *covered_to)
    {
        log::debug!(
            "{log_prefix} SubtractCovered | input_range={} covered={} returned=- reason=fully_covered",
            fetcher::format_time_range(from, to),
            format_trade_ranges(covered_ranges)
        );
        return None;
    }

    let mut cursor = from;
    for (covered_from, covered_to) in covered_ranges {
        if *covered_to <= cursor {
            continue;
        }

        if *covered_from > cursor {
            let result = (cursor, (*covered_from).min(to));
            log::debug!(
                "{log_prefix} SubtractCovered | input_range={} covered={} returned={} reason=gap_before_covered",
                fetcher::format_time_range(from, to),
                format_trade_ranges(covered_ranges),
                fetcher::format_time_range(result.0, result.1)
            );
            return Some(result);
        }

        cursor = cursor.max(*covered_to);
        if cursor >= to {
            log::debug!(
                "{log_prefix} SubtractCovered | input_range={} covered={} returned=- reason=fully_covered_after_merge",
                fetcher::format_time_range(from, to),
                format_trade_ranges(covered_ranges)
            );
            return None;
        }
    }

    let result = (cursor, to);
    log::debug!(
        "{log_prefix} SubtractCovered | input_range={} covered={} returned={} reason=tail_gap",
        fetcher::format_time_range(from, to),
        format_trade_ranges(covered_ranges),
        fetcher::format_time_range(result.0, result.1)
    );
    Some(result)
}

/// Returns the newest uncovered sub-range inside `[from, to)`. Covered ranges
/// are expected to be sorted and merged, as maintained by `KlineChart`.
fn subtract_covered_ranges_latest(
    covered_ranges: &[(UnixMs, UnixMs)],
    from: UnixMs,
    to: UnixMs,
    log_prefix: &str,
) -> Option<(UnixMs, UnixMs)> {
    if to <= from {
        log::debug!(
            "{log_prefix} SubtractCovered | input_range={} covered={} returned=- reason=invalid_range",
            fetcher::format_time_range(from, to),
            format_trade_ranges(covered_ranges)
        );
        return None;
    }

    let mut cursor = to;
    for (covered_from, covered_to) in covered_ranges.iter().rev() {
        if *covered_from >= cursor {
            continue;
        }

        if *covered_to < cursor {
            let result = ((*covered_to).max(from), cursor);
            if result.0 < result.1 {
                log::debug!(
                    "{log_prefix} SubtractCovered | input_range={} covered={} returned={} reason=latest_gap",
                    fetcher::format_time_range(from, to),
                    format_trade_ranges(covered_ranges),
                    fetcher::format_time_range(result.0, result.1)
                );
                return Some(result);
            }
        }

        cursor = cursor.min(*covered_from);
        if cursor <= from {
            log::debug!(
                "{log_prefix} SubtractCovered | input_range={} covered={} returned=- reason=fully_covered",
                fetcher::format_time_range(from, to),
                format_trade_ranges(covered_ranges)
            );
            return None;
        }
    }

    let result = (from, cursor);
    (result.0 < result.1).then_some(result)
}

/// Keep the moving live edge from starving a long historical backfill. A
/// recent tail of at most one minute can wait for the live stream while the
/// next worker advances the older gap. Once that tail grows, it is refreshed
/// before historical traversal resumes.
fn select_trade_fetch_gap(
    covered_ranges: &[(UnixMs, UnixMs)],
    from: UnixMs,
    to: UnixMs,
) -> Option<(UnixMs, UnixMs)> {
    const LIVE_TAIL_DEFER_MS: u64 = 60_000;

    let latest = subtract_covered_ranges_latest(covered_ranges, from, to, "DATA Trades Latest")?;
    let latest_is_short_live_tail =
        latest.1 == to && latest.1.saturating_diff(latest.0) <= LIVE_TAIL_DEFER_MS;

    if latest_is_short_live_tail
        && let Some(oldest) =
            subtract_covered_ranges(covered_ranges, from, to, "DATA Trades Historical")
        && oldest != latest
    {
        return Some(oldest);
    }

    Some(latest)
}

impl canvas::Program<Message> for KlineChart {
    type State = Interaction;

    fn update(
        &self,
        interaction: &mut Interaction,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        super::canvas_interaction(self, interaction, event, bounds, cursor)
    }

    fn draw(
        &self,
        interaction: &Interaction,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let chart = self.state();

        if chart.bounds.width == 0.0 {
            return vec![];
        }

        let bounds_size = bounds.size();
        let palette = theme.extended_palette();

        let klines = chart.cache.main.draw(renderer, bounds_size, |frame| {
            let center = Vector::new(bounds.width / 2.0, bounds.height / 2.0);

            frame.translate(center);
            frame.scale(chart.scaling);
            frame.translate(chart.translation);

            let region = chart.visible_region(frame.size());
            let (earliest, latest) = chart.interval_range(&region);

            let price_to_y = |price| chart.price_to_y(price);
            let interval_to_x = |interval| chart.interval_to_x(interval);

            match &self.kind {
                KlineChartKind::Footprint {
                    clusters,
                    scaling,
                    studies,
                } => {
                    let (highest, lowest) = chart.price_range(&region);

                    let max_cluster_qty = self.calc_qty_scales(
                        earliest,
                        latest,
                        highest,
                        lowest,
                        chart.tick_size,
                        *clusters,
                    );

                    let candle_width = 0.1 * chart.cell_width;
                    let content_spacing = ContentGaps::from_view(candle_width, chart.scaling);

                    let imbalance = studies.iter().find_map(|study| {
                        if let FootprintStudy::Imbalance {
                            threshold,
                            color_scale,
                            ignore_zeros,
                        } = study
                        {
                            Some((*threshold, *color_scale, *ignore_zeros))
                        } else {
                            None
                        }
                    });

                    let cell_layout = FootprintCellLayout {
                        cell_w: chart.cell_width,
                        cell_h: chart.cell_height,
                        candle_w: candle_width,
                        pal: palette,
                        cluster: *clusters,
                        gaps: content_spacing,
                    };

                    let text_size = cell_layout.text_size(chart.scaling);
                    let show_text = cell_layout.should_show_text(chart.scaling);

                    draw_all_npocs(
                        &self.data_source,
                        frame,
                        price_to_y,
                        interval_to_x,
                        &cell_layout,
                        studies,
                        earliest,
                        latest,
                        imbalance.is_some(),
                    );

                    render_data_source(
                        &self.data_source,
                        frame,
                        earliest,
                        latest,
                        interval_to_x,
                        |frame, x_position, kline, trades, _summary| {
                            let individual_max = trades.max_cluster_qty_all(cell_layout.cluster);
                            let cluster_scaling =
                                scaling.effective_qty(max_cluster_qty, individual_max);

                            draw_clusters(
                                frame,
                                price_to_y,
                                x_position,
                                &cell_layout,
                                chart.scaling,
                                cluster_scaling,
                                text_size,
                                self.tick_size(),
                                show_text,
                                self.visual_config.show_footprint_summary,
                                imbalance,
                                kline,
                                trades,
                            );
                        },
                    );
                }
                KlineChartKind::Candles => {
                    let candle_width = chart.cell_width * 0.8;
                    let svp = self.visual_config.session_volume_profile;
                    let chart_interval_ms = match chart.basis {
                        Basis::Time(interval) => interval.to_milliseconds(),
                        Basis::Tick(_) => 60_000,
                    };
                    let proxy_asset =
                        exchange::options::resolve_options_underlying(chart.ticker_info.ticker);
                    let (visible_high, visible_low) = chart.price_range(&region);
                    if self.indicator_enabled(KlineIndicator::GexLevels)
                        && (self.gex_snapshot.is_some() || !self.gex_proxy_history.is_empty())
                    {
                        draw_gex_overlay_background(
                            frame,
                            price_to_y,
                            interval_to_x,
                            &self.gex_history,
                            &self.gex_proxy_history,
                            self.gex_freshness,
                            &self.gex_render_cache,
                            &self.visual_config.gex_levels(),
                            latest,
                            chart.latest_x,
                            proxy_asset,
                            region,
                            chart.scaling,
                            chart_interval_ms,
                            visible_low.to_f64(),
                            visible_high.to_f64(),
                            palette,
                        );
                    }
                    if self.indicator_enabled(KlineIndicator::SessionVolumeProfile) {
                        draw_session_volume_profiles(
                            &self.data_source,
                            frame,
                            earliest,
                            latest,
                            interval_to_x,
                            price_to_y,
                            chart.cell_height,
                            chart.tick_size,
                            &svp,
                            palette,
                        );
                    }
                    if self.indicator_enabled(KlineIndicator::Vwap) {
                        draw_vwap_overlay(
                            &self.data_source,
                            frame,
                            earliest,
                            latest,
                            interval_to_x,
                            price_to_y,
                            &self.visual_config.vwap,
                            palette,
                        );
                    }
                    let volume_bubbles = self.visual_config.volume_bubbles;
                    let bubbles_enabled = volume_bubbles.enabled
                        && self.indicator_enabled(KlineIndicator::VolumeBubbles);
                    if !bubbles_enabled {
                        self.rendered_volume_bubbles.borrow_mut().clear();
                    }
                    let volume_bubble_range = bubbles_enabled
                        .then(|| match &self.data_source {
                            PlotData::TimeBased(timeseries) => {
                                timeseries.latest_timestamp().and_then(|latest| {
                                    volume_bubble_effective_range(
                                        latest,
                                        timeseries.interval.to_milliseconds(),
                                        UnixMs::now(),
                                        &volume_bubbles,
                                    )
                                })
                            }
                            PlotData::TickBased(_) => None,
                        })
                        .flatten();
                    render_data_source(
                        &self.data_source,
                        frame,
                        earliest,
                        latest,
                        interval_to_x,
                        |frame, x_position, kline, _trades, _summary| {
                            draw_candle_dp(
                                frame,
                                price_to_y,
                                candle_width,
                                palette,
                                x_position,
                                kline,
                            );
                        },
                    );
                    if self.indicator_enabled(KlineIndicator::GexLevels)
                        && !self.gex_proxy_history.is_empty()
                    {
                        draw_gex_proxy_zone_cores(
                            frame,
                            price_to_y,
                            interval_to_x,
                            &self.gex_proxy_history,
                            &self.gex_history,
                            &self.gex_render_cache,
                            &self.visual_config.gex_levels(),
                            region,
                            chart.scaling,
                            chart_interval_ms,
                            chart.latest_x,
                            proxy_asset,
                        );
                    }
                    if bubbles_enabled && let Some((bubble_from, bubble_to)) = volume_bubble_range {
                        let bubbles = build_rendered_volume_bubbles(
                            &self.data_source,
                            earliest.max(bubble_from.as_u64()),
                            latest.min(bubble_to.as_u64()),
                            price_to_y,
                            interval_to_x,
                            chart.tick_size,
                            chart.cell_width,
                            chart.scaling,
                            &volume_bubbles,
                            palette,
                            UnixMs::now(),
                            &self.stabilized_bubble_threshold,
                        );
                        draw_rendered_volume_bubbles(
                            frame,
                            &bubbles,
                            chart.scaling,
                            palette,
                            &volume_bubbles,
                        );
                        *self.rendered_volume_bubbles.borrow_mut() = bubbles;
                    }
                    if self.indicator_enabled(KlineIndicator::GexLevels)
                        && let Some(snapshot) = &self.gex_snapshot
                    {
                        draw_gex_overlay_foreground(
                            frame,
                            price_to_y,
                            interval_to_x,
                            snapshot,
                            &self.gex_history,
                            &self.gex_render_cache,
                            &self.visual_config.gex_levels(),
                            chart_interval_ms,
                            region,
                            chart.scaling,
                            palette,
                        );
                    }
                }
            }

            chart.draw_last_price_line(frame, palette, region);
        });

        let crosshair = chart.cache.crosshair.draw(renderer, bounds_size, |frame| {
            let visible_region = chart.visible_region(bounds_size);
            let visible_range = chart.interval_range(&visible_region);

            if let Some(cursor_position) = cursor.position_in(bounds) {
                let (_, rounded_aggregation) =
                    chart.draw_crosshair(frame, theme, bounds_size, cursor_position, interaction);
                let center = Vector::new(bounds.width / 2.0, bounds.height / 2.0);
                let bubbles = self.rendered_volume_bubbles.borrow();
                if let Some(bubble) = hit_test_volume_bubbles(
                    &bubbles,
                    cursor_position,
                    center,
                    chart.translation,
                    chart.scaling,
                ) {
                    draw_hovered_volume_bubble(
                        frame,
                        bubble,
                        center,
                        chart.translation,
                        chart.scaling,
                        &self.visual_config.volume_bubbles,
                    );
                    draw_volume_bubble_tooltip(
                        frame,
                        bubble,
                        palette,
                        &chart.ticker_info,
                        &self.visual_config.volume_bubbles,
                        cursor_position,
                        bounds_size,
                    );
                } else if self.visual_config.gex_levels().show_hover_tooltip
                    && self.indicator_enabled(KlineIndicator::GexLevels)
                    && draw_gex_hover_tooltip(
                        frame,
                        chart,
                        &self.gex_history,
                        &self.gex_proxy_history,
                        &self.gex_render_cache,
                        &self.visual_config.gex_levels(),
                        cursor_position,
                        bounds_size,
                        palette,
                    )
                {
                } else {
                    draw_crosshair_tooltip(
                        &self.data_source,
                        &chart.ticker_info,
                        frame,
                        palette,
                        chart.basis,
                        Some(rounded_aggregation),
                        visible_range,
                    );
                }
            } else if self.visual_config.data_labels_always_visible {
                draw_crosshair_tooltip(
                    &self.data_source,
                    &chart.ticker_info,
                    frame,
                    palette,
                    chart.basis,
                    None,
                    visible_range,
                );
            }
        });

        vec![klines, crosshair]
    }

    fn mouse_interaction(
        &self,
        interaction: &Interaction,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        match interaction {
            Interaction::Panning { .. } => mouse::Interaction::Grabbing,
            Interaction::Zoomin { .. } => mouse::Interaction::ZoomIn,
            Interaction::None | Interaction::Ruler { .. } => {
                if cursor.is_over(bounds) {
                    mouse::Interaction::Crosshair
                } else {
                    mouse::Interaction::default()
                }
            }
        }
    }
}

fn draw_gex_hover_tooltip(
    frame: &mut canvas::Frame,
    chart: &ViewState,
    history: &[Arc<data::chart::gex::GexSnapshot>],
    proxy_history: &[Arc<exchange::options::gex_monitor::GexProxyHistoryPoint>],
    render_cache: &RefCell<GexRenderCache>,
    config: &data::chart::gex::GexLevelsConfig,
    cursor: Point,
    bounds: Size,
    palette: &Extended,
) -> bool {
    if draw_gex_proxy_tooltip(
        frame,
        chart,
        proxy_history,
        history,
        render_cache,
        cursor,
        bounds,
        palette,
    ) {
        return true;
    }
    draw_gex_zone_tooltip(
        frame,
        chart,
        history,
        render_cache,
        config,
        cursor,
        bounds,
        palette,
    )
}

#[allow(clippy::too_many_arguments)]
fn draw_gex_proxy_tooltip(
    frame: &mut canvas::Frame,
    chart: &ViewState,
    proxy_history: &[Arc<exchange::options::gex_monitor::GexProxyHistoryPoint>],
    deribit_history: &[Arc<data::chart::gex::GexSnapshot>],
    render_cache: &RefCell<GexRenderCache>,
    cursor: Point,
    bounds: Size,
    palette: &Extended,
) -> bool {
    let center = Vector::new(bounds.width / 2.0, bounds.height / 2.0);
    let world = Point::new(
        (cursor.x - center.x) / chart.scaling - chart.translation.x,
        (cursor.y - center.y) / chart.scaling - chart.translation.y,
    );
    let interval_ms = match chart.basis {
        Basis::Time(interval) => interval.to_milliseconds(),
        Basis::Tick(_) => 60_000,
    };
    let frames = cached_gex_proxy_zone_frames(
        proxy_history,
        deribit_history,
        interval_ms,
        chart.latest_x,
        exchange::options::resolve_options_underlying(chart.ticker_info.ticker),
        render_cache,
    );
    let timestamp = UnixMs::new(chart.x_to_interval(world.x));
    let price = chart.y_to_price(world.y).to_f64();
    let Some((zone_frame, zone)) = proxy_zone_hit_test(&frames, timestamp, price) else {
        return false;
    };
    let observed = zone_frame
        .observed_at
        .format_utc("%Y-%m-%d %H:%M:%S")
        .unwrap_or_else(|| zone_frame.observed_at.as_u64().to_string());
    let lines = [
        proxy_zone_label(zone.role).to_owned(),
        format!("Range: ${:.2} – ${:.2}", zone.lower_price, zone.upper_price),
        format!("Strength: {:.0}%", zone.strength * 100.0),
        format!("Provider Total GEX: {:+.2}", zone_frame.total_gex),
        format!("Observed: {observed} UTC"),
        "GEX Monitor synthetic zone".to_owned(),
    ];
    let width = 330.0;
    let height = 12.0 + lines.len() as f32 * 16.0;
    let x = (cursor.x + 14.0).min((bounds.width - width - 4.0).max(4.0));
    let y = (cursor.y + 14.0).min((bounds.height - height - 4.0).max(4.0));
    frame.fill(
        &Path::rectangle(Point::new(x, y), Size::new(width, height)),
        palette.background.base.color.scale_alpha(0.94),
    );
    for (line, text) in lines.iter().enumerate() {
        draw_cluster_text(
            frame,
            text,
            Point::new(x + 8.0, y + 10.0 + line as f32 * 16.0),
            11.0,
            palette.background.base.text,
            Alignment::Start,
            Alignment::Start,
        );
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn draw_gex_zone_tooltip(
    frame: &mut canvas::Frame,
    chart: &ViewState,
    history: &[Arc<data::chart::gex::GexSnapshot>],
    render_cache: &RefCell<GexRenderCache>,
    config: &data::chart::gex::GexLevelsConfig,
    cursor: Point,
    bounds: Size,
    palette: &Extended,
) -> bool {
    if history.is_empty() {
        return false;
    }
    let center = Vector::new(bounds.width / 2.0, bounds.height / 2.0);
    let world = Point::new(
        (cursor.x - center.x) / chart.scaling - chart.translation.x,
        (cursor.y - center.y) / chart.scaling - chart.translation.y,
    );
    let bucket_ms = match chart.basis {
        Basis::Time(interval) => interval.to_milliseconds(),
        Basis::Tick(_) => 60_000,
    };
    let frames = cached_gex_zone_frames(history, bucket_ms, config, render_cache);
    let bucket =
        data::chart::gex::gex_bucket_start(UnixMs::new(chart.x_to_interval(world.x)), bucket_ms);
    let frame_index = frames.partition_point(|value| value.bucket_start < bucket);
    let zone_frame = if frame_index < frames.len() && frames[frame_index].bucket_start == bucket {
        &frames[frame_index]
    } else {
        return false;
    };
    let hovered_price = chart.y_to_price(world.y).to_f64();
    let profile_hover = cursor.x
        >= bounds.width
            * (1.0 - gex_profile_width_percent(config.current_profile_width_percent) / 100.0);
    let mut lines = Vec::new();
    if profile_hover && config.show_current_profile {
        let Some(snapshot) = history.iter().rev().find(|snapshot| {
            snapshot.observed_at <= zone_frame.bucket_start.saturating_add(bucket_ms)
        }) else {
            return false;
        };
        let Some(strike) = snapshot.strikes.iter().min_by(|a, b| {
            (a.strike - hovered_price)
                .abs()
                .total_cmp(&(b.strike - hovered_price).abs())
        }) else {
            return false;
        };
        if (chart.price_to_y(Price::from_f64(strike.strike)) - world.y).abs() * chart.scaling > 4.0
        {
            return false;
        }
        lines.extend([
            format!("Strike ${:.2}", strike.strike),
            format!("Net GEX {:+.2}", strike.net_gex_1pct),
            format!(
                "Call / Put GEX {:+.2} / {:+.2}",
                strike.call_gex_1pct, strike.put_gex_1pct
            ),
            format!(
                "Call / Put OI {:.2} / {:.2}",
                strike.call_open_interest, strike.put_open_interest
            ),
            format!("Gamma: {}", strike.gamma_provenance),
        ]);
    } else {
        let Some((zone, band)) = gex_zone_band_hit_test(zone_frame, hovered_price) else {
            return false;
        };
        let label = match zone.sign {
            data::chart::gex::GexZoneSign::Positive => "Positive Gamma Zone",
            data::chart::gex::GexZoneSign::Negative => "Negative Gamma Zone",
        };
        lines.extend([
            label.to_owned(),
            format!("Range: ${:.0} – ${:.0}", band.lower_price, band.upper_price),
            format!("Net GEX: {:+.2}", band.net_gex_1pct),
            format!("Strength: {:.0}%", band.normalized_strength * 100.0),
            format!(
                "Peak: ${:.0} ({:.0}%)",
                zone.peak_price,
                zone.normalized_strength * 100.0
            ),
            format!("Persistence: {:.0}%", zone.persistence_score * 100.0),
            format!(
                "Expiry: {}",
                zone.dominant_expiry
                    .map_or_else(|| "n/a".to_owned(), |value| value.as_u64().to_string())
            ),
            format!("Deribit OI Proxy / {}", zone.gamma_provenance),
        ]);
    }
    let width = 300.0;
    let height = 12.0 + lines.len() as f32 * 16.0;
    let x = (cursor.x + 14.0).min((bounds.width - width - 4.0).max(4.0));
    let y = (cursor.y + 14.0).min((bounds.height - height - 4.0).max(4.0));
    frame.fill(
        &Path::rectangle(Point::new(x, y), Size::new(width, height)),
        palette.background.base.color.scale_alpha(0.94),
    );
    for (line, text) in lines.iter().enumerate() {
        draw_cluster_text(
            frame,
            text,
            Point::new(x + 8.0, y + 10.0 + line as f32 * 16.0),
            11.0,
            palette.background.base.text,
            Alignment::Start,
            Alignment::Start,
        );
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn draw_gex_overlay_background(
    frame: &mut canvas::Frame,
    price_to_y: impl Fn(Price) -> f32 + Copy,
    time_to_x: impl Fn(u64) -> f32 + Copy,
    history: &[Arc<data::chart::gex::GexSnapshot>],
    proxy_history: &[Arc<exchange::options::gex_monitor::GexProxyHistoryPoint>],
    freshness: data::chart::gex::GexFreshness,
    render_cache: &RefCell<GexRenderCache>,
    config: &data::chart::gex::GexLevelsConfig,
    latest_candle_time: u64,
    actual_latest_candle_time: u64,
    proxy_asset: Option<exchange::options::OptionsUnderlying>,
    visible_region: Rectangle,
    chart_scaling: f32,
    chart_interval_ms: u64,
    visible_low: f64,
    visible_high: f64,
    palette: &Extended,
) {
    draw_gex_proxy_zone_background(
        frame,
        price_to_y,
        time_to_x,
        proxy_history,
        history,
        render_cache,
        config,
        visible_region,
        chart_scaling,
        chart_interval_ms,
        actual_latest_candle_time,
        proxy_asset,
    );
    draw_gex_zone_background(
        frame,
        price_to_y,
        time_to_x,
        history,
        freshness,
        render_cache,
        config,
        latest_candle_time,
        visible_region,
        chart_scaling,
        chart_interval_ms,
        visible_low,
        visible_high,
        palette,
    );
}

#[derive(Debug, Clone)]
struct GexProxyZoneRun {
    role: data::chart::gex::GexProxyZoneRole,
    start: UnixMs,
    end: UnixMs,
    center_price: f64,
    lower_price: f64,
    upper_price: f64,
    strength: f32,
    observed_at: UnixMs,
}

fn proxy_zone_color(role: data::chart::gex::GexProxyZoneRole) -> Color {
    use data::chart::gex::GexProxyZoneRole;
    match role {
        GexProxyZoneRole::PositivePrimary | GexProxyZoneRole::PositiveSecondary => {
            Color::from_rgb8(0x16, 0xd8, 0xc5)
        }
        GexProxyZoneRole::NegativePrimary | GexProxyZoneRole::NegativeSecondary => {
            Color::from_rgb8(0xff, 0x31, 0x5d)
        }
    }
}

fn proxy_zone_label(role: data::chart::gex::GexProxyZoneRole) -> &'static str {
    use data::chart::gex::GexProxyZoneRole;
    match role {
        GexProxyZoneRole::PositivePrimary => "Positive Primary Proxy Zone",
        GexProxyZoneRole::PositiveSecondary => "Positive Secondary Proxy Zone",
        GexProxyZoneRole::NegativePrimary => "Negative Primary Proxy Zone",
        GexProxyZoneRole::NegativeSecondary => "Negative Secondary Proxy Zone",
    }
}

fn cached_gex_proxy_zone_frames(
    history: &[Arc<exchange::options::gex_monitor::GexProxyHistoryPoint>],
    deribit_history: &[Arc<data::chart::gex::GexSnapshot>],
    bucket_ms: u64,
    latest_candle_time: u64,
    asset: Option<exchange::options::OptionsUnderlying>,
    render_cache: &RefCell<GexRenderCache>,
) -> Arc<[data::chart::gex::GexProxyZoneFrame]> {
    let deribit_coverage_key = deribit_history.iter().fold(0u64, |key, snapshot| {
        key.rotate_left(5) ^ snapshot.observed_at.as_u64()
    });
    let key = history
        .last()
        .and_then(|point| u64::try_from(point.observed_at).ok())
        .unwrap_or(0)
        ^ (history.len() as u64).rotate_left(7)
        ^ bucket_ms.rotate_left(17)
        ^ latest_candle_time.rotate_left(29)
        ^ asset.map_or(0, |value| (value as u64 + 1).rotate_left(41))
        ^ (deribit_history.len() as u64).rotate_left(47)
        ^ deribit_coverage_key.rotate_left(53)
        ^ 0x4750_5a4f_4e45_0001;
    let mut cache = render_cache.borrow_mut();
    if cache.proxy_key != Some(key) {
        cache.proxy_zone_frames = data::chart::gex::build_gex_proxy_zone_frames(
            history,
            deribit_history,
            bucket_ms,
            UnixMs::new(latest_candle_time),
        )
        .into();
        cache.proxy_key = Some(key);
    }
    cache.proxy_zone_frames.clone()
}

fn proxy_zones_nearly_equal(a: &GexProxyZoneRun, b: &data::chart::gex::GexProxyZone) -> bool {
    let half_width_a = (a.upper_price - a.lower_price) * 0.5;
    let half_width_b = (b.upper_price - b.lower_price) * 0.5;
    let tolerance = half_width_a.max(half_width_b) * 0.05;
    (a.center_price - b.center_price).abs() <= tolerance
        && (half_width_a - half_width_b).abs() <= tolerance
}

fn build_gex_proxy_zone_runs(
    frames: &[data::chart::gex::GexProxyZoneFrame],
) -> Vec<GexProxyZoneRun> {
    let mut runs: Vec<GexProxyZoneRun> = Vec::new();
    for role in data::chart::gex::GexProxyZoneRole::ALL {
        for frame in frames {
            let Some(zone) = frame.zones.iter().find(|zone| zone.role == role) else {
                continue;
            };
            if let Some(last) = runs.last_mut()
                && last.role == role
                && last.end == frame.bucket_start
                && frame.observed_at.saturating_diff(last.observed_at)
                    <= 7 * 60 * 1_000 + 30 * 1_000
                && proxy_zones_nearly_equal(last, zone)
            {
                last.end = frame.bucket_end;
                last.strength = last.strength.max(zone.strength);
                last.observed_at = frame.observed_at;
                continue;
            }
            runs.push(GexProxyZoneRun {
                role,
                start: frame.bucket_start,
                end: frame.bucket_end,
                center_price: zone.center_price,
                lower_price: zone.lower_price,
                upper_price: zone.upper_price,
                strength: zone.strength,
                observed_at: frame.observed_at,
            });
        }
    }
    runs
}

fn gex_proxy_zone_transitions(
    frames: &[data::chart::gex::GexProxyZoneFrame],
) -> Vec<(
    UnixMs,
    data::chart::gex::GexProxyZoneRole,
    data::chart::gex::GexProxyZone,
    data::chart::gex::GexProxyZone,
)> {
    const MAX_GAP_MS: u64 = 7 * 60 * 1_000 + 30 * 1_000;
    let mut transitions = Vec::new();
    for role in data::chart::gex::GexProxyZoneRole::ALL {
        let mut previous: Option<(
            &data::chart::gex::GexProxyZoneFrame,
            &data::chart::gex::GexProxyZone,
        )> = None;
        for frame in frames {
            let Some(zone) = frame.zones.iter().find(|zone| zone.role == role) else {
                previous = None;
                continue;
            };
            if let Some((old_frame, old_zone)) = previous
                && old_frame.bucket_end == frame.bucket_start
                && frame.observed_at.saturating_diff(old_frame.observed_at) <= MAX_GAP_MS
                && (old_zone.lower_price != zone.lower_price
                    || old_zone.upper_price != zone.upper_price)
            {
                transitions.push((frame.bucket_start, role, old_zone.clone(), zone.clone()));
            }
            previous = Some((frame, zone));
        }
    }
    transitions
}

fn proxy_zone_hit_test(
    frames: &[data::chart::gex::GexProxyZoneFrame],
    timestamp: UnixMs,
    price: f64,
) -> Option<(
    &data::chart::gex::GexProxyZoneFrame,
    &data::chart::gex::GexProxyZone,
)> {
    frames
        .iter()
        .filter(|frame| timestamp >= frame.bucket_start && timestamp < frame.bucket_end)
        .flat_map(|frame| frame.zones.iter().map(move |zone| (frame, zone)))
        .filter(|(_, zone)| price >= zone.lower_price && price <= zone.upper_price)
        .max_by(|(_, a), (_, b)| a.strength.total_cmp(&b.strength))
}

#[allow(clippy::too_many_arguments)]
fn draw_gex_proxy_zone_background(
    frame: &mut canvas::Frame,
    price_to_y: impl Fn(Price) -> f32 + Copy,
    time_to_x: impl Fn(u64) -> f32 + Copy,
    proxy_history: &[Arc<exchange::options::gex_monitor::GexProxyHistoryPoint>],
    deribit_history: &[Arc<data::chart::gex::GexSnapshot>],
    render_cache: &RefCell<GexRenderCache>,
    config: &data::chart::gex::GexLevelsConfig,
    region: Rectangle,
    scaling: f32,
    bucket_ms: u64,
    latest_candle_time: u64,
    asset: Option<exchange::options::OptionsUnderlying>,
) {
    if !config.show_historical_zones || proxy_history.is_empty() {
        return;
    }
    let frames = cached_gex_proxy_zone_frames(
        proxy_history,
        deribit_history,
        bucket_ms,
        latest_candle_time,
        asset,
        render_cache,
    );
    let border_width = gex_screen_width_to_world(0.75, scaling);
    for run in build_gex_proxy_zone_runs(&frames) {
        let x0 = time_to_x(run.start.as_u64()).max(region.x);
        let x1 = time_to_x(run.end.as_u64()).min(region.x + region.width);
        let top = price_to_y(Price::from_f64(run.upper_price));
        let bottom = price_to_y(Price::from_f64(run.lower_price));
        let y0 = top.min(bottom).max(region.y);
        let y1 = top.max(bottom).min(region.y + region.height);
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let color = proxy_zone_color(run.role);
        frame.fill(
            &Path::rectangle(Point::new(x0, y0), Size::new(x1 - x0, y1 - y0)),
            color.scale_alpha(0.06 + 0.16 * run.strength),
        );
        let border = color.scale_alpha(0.12 + 0.18 * run.strength);
        frame.stroke(
            &Path::line(Point::new(x0, y0), Point::new(x1, y0)),
            Stroke::default()
                .with_color(border)
                .with_width(border_width),
        );
        frame.stroke(
            &Path::line(Point::new(x0, y1), Point::new(x1, y1)),
            Stroke::default()
                .with_color(border)
                .with_width(border_width),
        );
    }
    for (at, role, old, new) in gex_proxy_zone_transitions(&frames) {
        let x = time_to_x(at.as_u64());
        if x < region.x || x > region.x + region.width {
            continue;
        }
        let color = proxy_zone_color(role).scale_alpha(0.12 + 0.18 * new.strength);
        for (from, to) in [
            (old.upper_price, new.upper_price),
            (old.lower_price, new.lower_price),
        ] {
            let y0 = price_to_y(Price::from_f64(from));
            let y1 = price_to_y(Price::from_f64(to));
            if (y0 < region.y && y1 < region.y)
                || (y0 > region.y + region.height && y1 > region.y + region.height)
            {
                continue;
            }
            frame.stroke(
                &Path::line(
                    Point::new(x, y0.clamp(region.y, region.y + region.height)),
                    Point::new(x, y1.clamp(region.y, region.y + region.height)),
                ),
                Stroke::default().with_color(color).with_width(border_width),
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_gex_proxy_zone_cores(
    frame: &mut canvas::Frame,
    price_to_y: impl Fn(Price) -> f32 + Copy,
    time_to_x: impl Fn(u64) -> f32 + Copy,
    proxy_history: &[Arc<exchange::options::gex_monitor::GexProxyHistoryPoint>],
    deribit_history: &[Arc<data::chart::gex::GexSnapshot>],
    render_cache: &RefCell<GexRenderCache>,
    config: &data::chart::gex::GexLevelsConfig,
    region: Rectangle,
    scaling: f32,
    bucket_ms: u64,
    latest_candle_time: u64,
    asset: Option<exchange::options::OptionsUnderlying>,
) {
    if !config.show_historical_zones {
        return;
    }
    let frames = cached_gex_proxy_zone_frames(
        proxy_history,
        deribit_history,
        bucket_ms,
        latest_candle_time,
        asset,
        render_cache,
    );
    for run in build_gex_proxy_zone_runs(&frames) {
        let x0 = time_to_x(run.start.as_u64()).max(region.x);
        let x1 = time_to_x(run.end.as_u64()).min(region.x + region.width);
        let y = price_to_y(Price::from_f64(run.center_price));
        if x1 <= x0 || !gex_level_is_visible(y, region) {
            continue;
        }
        frame.stroke(
            &Path::line(Point::new(x0, y), Point::new(x1, y)),
            Stroke::default()
                .with_color(proxy_zone_color(run.role).scale_alpha(0.18 + 0.28 * run.strength))
                .with_width(gex_screen_width_to_world(1.5 + 1.5 * run.strength, scaling)),
        );
    }
    if config.show_gamma_flip_marker {
        draw_gex_proxy_flip(frame, price_to_y, time_to_x, &frames, region, scaling);
    }
}

fn draw_gex_proxy_flip(
    frame: &mut canvas::Frame,
    price_to_y: impl Fn(Price) -> f32 + Copy,
    time_to_x: impl Fn(u64) -> f32 + Copy,
    frames: &[data::chart::gex::GexProxyZoneFrame],
    region: Rectangle,
    scaling: f32,
) {
    const MAX_GAP_MS: u64 = 7 * 60 * 1_000 + 30 * 1_000;
    let color = Color::from_rgb8(0xb7, 0xbf, 0xcc).scale_alpha(0.22);
    let width = gex_screen_width_to_world(1.0, scaling);
    let mut previous: Option<&data::chart::gex::GexProxyZoneFrame> = None;
    for zone_frame in frames {
        let Some(level) = zone_frame.flip_level else {
            previous = None;
            continue;
        };
        let x0 = time_to_x(zone_frame.bucket_start.as_u64()).max(region.x);
        let x1 = time_to_x(zone_frame.bucket_end.as_u64()).min(region.x + region.width);
        let y = price_to_y(Price::from_f64(level));
        if x1 > x0 && gex_level_is_visible(y, region) {
            frame.stroke(
                &Path::line(Point::new(x0, y), Point::new(x1, y)),
                Stroke::default().with_color(color).with_width(width),
            );
        }
        if let Some(old) = previous
            && old.bucket_end == zone_frame.bucket_start
            && zone_frame.observed_at.saturating_diff(old.observed_at) <= MAX_GAP_MS
            && let Some(old_level) = old.flip_level
            && old_level != level
        {
            let x = time_to_x(zone_frame.bucket_start.as_u64());
            let old_y = price_to_y(Price::from_f64(old_level));
            if x >= region.x
                && x <= region.x + region.width
                && (gex_level_is_visible(old_y, region) || gex_level_is_visible(y, region))
            {
                frame.stroke(
                    &Path::line(
                        Point::new(x, old_y.clamp(region.y, region.y + region.height)),
                        Point::new(x, y.clamp(region.y, region.y + region.height)),
                    ),
                    Stroke::default().with_color(color).with_width(width),
                );
            }
        }
        previous = Some(zone_frame);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_gex_overlay_foreground(
    frame: &mut canvas::Frame,
    price_to_y: impl Fn(Price) -> f32 + Copy,
    time_to_x: impl Fn(u64) -> f32 + Copy,
    snapshot: &data::chart::gex::GexSnapshot,
    history: &[Arc<data::chart::gex::GexSnapshot>],
    render_cache: &RefCell<GexRenderCache>,
    config: &data::chart::gex::GexLevelsConfig,
    chart_interval_ms: u64,
    visible_region: Rectangle,
    chart_scaling: f32,
    palette: &Extended,
) {
    draw_gex_zone_cores(
        frame,
        price_to_y,
        time_to_x,
        history,
        render_cache,
        config,
        visible_region,
        chart_scaling,
        chart_interval_ms,
    );
    if config.show_current_profile {
        draw_gex_zone_profile(
            frame,
            price_to_y,
            snapshot,
            config,
            visible_region,
            chart_scaling,
            palette,
        );
    }
    draw_gex_zone_markers(
        frame,
        price_to_y,
        snapshot,
        config,
        visible_region,
        chart_scaling,
        palette,
    );
}

fn cached_gex_zone_frames(
    history: &[Arc<data::chart::gex::GexSnapshot>],
    bucket_ms: u64,
    config: &data::chart::gex::GexLevelsConfig,
    render_cache: &RefCell<GexRenderCache>,
) -> Arc<[data::chart::gex::GexZoneFrame]> {
    let key = history
        .last()
        .map_or(0, |snapshot| snapshot.observed_at.as_u64())
        ^ (history.len() as u64).rotate_left(7)
        ^ bucket_ms.rotate_left(17)
        ^ u64::from(config.minimum_zone_strength.to_bits()).rotate_left(49)
        ^ u64::from(config.max_positive_zones).rotate_left(53)
        ^ u64::from(config.max_negative_zones).rotate_left(57)
        ^ u64::from(config.fade_buckets).rotate_left(61)
        ^ u64::from(config.persistent_lookback_minutes)
        ^ (config.expiry_filter as u64).rotate_left(11)
        ^ (config.gamma_source as u64).rotate_left(31)
        ^ config.minimum_open_interest.to_bits().rotate_left(41)
        ^ config.minimum_absolute_gex.to_bits().rotate_left(47);
    let mut cache = render_cache.borrow_mut();
    if cache.key != Some(key) {
        cache.zone_frames =
            data::chart::gex::build_gex_zone_frames(history, bucket_ms, config).into();
        cache.key = Some(key);
    }
    cache.zone_frames.clone()
}

fn gex_zone_fade(zone: &data::chart::gex::GexZone) -> f32 {
    if zone.state == data::chart::gex::GexZoneState::Fading {
        0.65f32.powi(i32::from(zone.missing_buckets))
    } else {
        1.0
    }
}

fn gex_band_effective_strength(
    zone: &data::chart::gex::GexZone,
    band: &data::chart::gex::GexZoneBand,
) -> f32 {
    band.normalized_strength.clamp(0.0, 1.0).powf(1.35)
        * (0.70 + 0.30 * zone.persistence_score.clamp(0.0, 1.0))
        * gex_zone_fade(zone)
}

fn gex_zone_band_hit_test(
    frame: &data::chart::gex::GexZoneFrame,
    price: f64,
) -> Option<(&data::chart::gex::GexZone, &data::chart::gex::GexZoneBand)> {
    frame
        .zones
        .iter()
        .flat_map(|zone| zone.bands.iter().map(move |band| (zone, band)))
        .filter(|(_, band)| price >= band.lower_price && price <= band.upper_price)
        .max_by(|(zone_a, band_a), (zone_b, band_b)| {
            gex_band_effective_strength(zone_a, band_a)
                .total_cmp(&gex_band_effective_strength(zone_b, band_b))
        })
}

fn zone_color(
    zone: &data::chart::gex::GexZone,
    config: &data::chart::gex::GexLevelsConfig,
    palette: &Extended,
) -> Color {
    resolve_gex_color(
        match zone.sign {
            data::chart::gex::GexZoneSign::Positive => config.positive_color,
            data::chart::gex::GexZoneSign::Negative => config.negative_color,
        },
        palette,
    )
}

fn gex_screen_width_to_world(screen_width: f32, scaling: f32) -> f32 {
    screen_width / scaling.max(f32::EPSILON)
}

fn gex_level_is_visible(y: f32, region: Rectangle) -> bool {
    y.is_finite() && y >= region.y && y <= region.y + region.height
}

fn gex_profile_width_percent(value: f32) -> f32 {
    value.clamp(4.0, 7.0)
}

fn gex_projection_bounds(
    last_candle_x: f32,
    plot_right: f32,
    visible_width: f32,
) -> Option<(f32, f32)> {
    let start = last_candle_x.min(plot_right);
    let end = (start + visible_width * 0.20).min(plot_right);
    (end > start).then_some((start, end))
}

fn resolve_gex_color(role: data::chart::gex::GexLevelColor, palette: &Extended) -> Color {
    use data::chart::gex::GexLevelColor;
    match role {
        GexLevelColor::Cyan => Color::from_rgb8(0x16, 0xd8, 0xc5),
        GexLevelColor::Magenta => Color::from_rgb8(0xff, 0x31, 0x5d),
        GexLevelColor::Primary => palette.primary.strong.color,
        GexLevelColor::Success => palette.success.strong.color,
        GexLevelColor::Danger => palette.danger.strong.color,
        GexLevelColor::Warning => palette.warning.strong.color,
        GexLevelColor::Secondary => palette.secondary.strong.color,
    }
}

fn draw_gex_level_label(
    frame: &mut canvas::Frame,
    label: &str,
    position: Point,
    color: Color,
    scaling: f32,
    palette: &Extended,
) {
    let width = gex_screen_width_to_world(10.0 + label.chars().count() as f32 * 6.2, scaling);
    let height = gex_screen_width_to_world(16.0, scaling);
    frame.fill(
        &Path::rectangle(
            Point::new(position.x, position.y - height * 0.5),
            Size::new(width, height),
        ),
        palette.background.base.color.scale_alpha(0.86),
    );
    draw_cluster_text(
        frame,
        label,
        Point::new(
            position.x + gex_screen_width_to_world(5.0, scaling),
            position.y,
        ),
        gex_screen_width_to_world(10.0, scaling),
        color,
        Alignment::Start,
        Alignment::Center,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_gex_zone_background(
    frame: &mut canvas::Frame,
    price_to_y: impl Fn(Price) -> f32 + Copy,
    time_to_x: impl Fn(u64) -> f32 + Copy,
    history: &[Arc<data::chart::gex::GexSnapshot>],
    freshness: data::chart::gex::GexFreshness,
    render_cache: &RefCell<GexRenderCache>,
    config: &data::chart::gex::GexLevelsConfig,
    latest_candle_time: u64,
    region: Rectangle,
    scaling: f32,
    bucket_ms: u64,
    visible_low: f64,
    visible_high: f64,
    palette: &Extended,
) {
    if history.is_empty() {
        return;
    }
    let frames = cached_gex_zone_frames(history, bucket_ms, config, render_cache);
    let profile_gutter = if config.show_current_profile && region.width * scaling >= 320.0 {
        region.width * gex_profile_width_percent(config.current_profile_width_percent) / 100.0
    } else {
        0.0
    };
    let plot_right = region.x + region.width - profile_gutter;
    let border_width = gex_screen_width_to_world(1.0, scaling);

    for zone_frame in frames.iter() {
        if !config.show_historical_zones {
            break;
        }
        let x0 = time_to_x(zone_frame.bucket_start.as_u64()).max(region.x);
        let x1 =
            time_to_x(zone_frame.bucket_start.saturating_add(bucket_ms).as_u64()).min(plot_right);
        if x1 <= x0 {
            continue;
        }
        for zone in zone_frame
            .zones
            .iter()
            .filter(|zone| zone.upper_price >= visible_low && zone.lower_price <= visible_high)
        {
            let color = zone_color(zone, config, palette);
            for band in zone
                .bands
                .iter()
                .filter(|band| band.upper_price >= visible_low && band.lower_price <= visible_high)
            {
                let top = price_to_y(Price::from_f64(band.upper_price)).max(region.y);
                let bottom =
                    price_to_y(Price::from_f64(band.lower_price)).min(region.y + region.height);
                if bottom <= top {
                    continue;
                }
                let effective_strength = gex_band_effective_strength(zone, band);
                let body_alpha = 0.02 + 0.23 * effective_strength;
                frame.fill(
                    &Path::rectangle(Point::new(x0, top), Size::new(x1 - x0, bottom - top)),
                    color.scale_alpha(body_alpha),
                );
                let is_peak = band.strike == zone.peak_price;
                let center_y = price_to_y(Price::from_f64(band.strike));
                frame.stroke(
                    &Path::line(Point::new(x0, center_y), Point::new(x1, center_y)),
                    Stroke::default()
                        .with_color(
                            color.scale_alpha(
                                (0.16
                                    + 0.54 * effective_strength
                                    + if is_peak { 0.18 } else { 0.0 })
                                .clamp(0.0, 1.0),
                            ),
                        )
                        .with_width(border_width * if is_peak { 1.6 } else { 0.85 }),
                );
            }
        }
    }

    if config.show_active_projection
        && freshness == data::chart::gex::GexFreshness::Fresh
        && let Some(last) = frames.last()
        && let Some((projection_start, projection_end)) = gex_projection_bounds(
            time_to_x(latest_candle_time).max(region.x),
            plot_right,
            region.width,
        )
    {
        for zone in last.zones.iter() {
            let color = zone_color(zone, config, palette);
            for band in zone.bands.iter() {
                let top = price_to_y(Price::from_f64(band.upper_price)).max(region.y);
                let bottom =
                    price_to_y(Price::from_f64(band.lower_price)).min(region.y + region.height);
                if bottom <= top {
                    continue;
                }
                let effective_strength = gex_band_effective_strength(zone, band);
                frame.fill(
                    &Path::rectangle(
                        Point::new(projection_start, top),
                        Size::new(projection_end - projection_start, bottom - top),
                    ),
                    color.scale_alpha(0.02 + 0.23 * effective_strength),
                );
                let is_peak = band.strike == zone.peak_price;
                let y = price_to_y(Price::from_f64(band.strike));
                frame.stroke(
                    &Path::line(
                        Point::new(projection_start, y),
                        Point::new(projection_end, y),
                    ),
                    Stroke::default()
                        .with_color(
                            color.scale_alpha(
                                (0.16
                                    + 0.54 * effective_strength
                                    + if is_peak { 0.18 } else { 0.0 })
                                .clamp(0.0, 1.0),
                            ),
                        )
                        .with_width(border_width * if is_peak { 1.6 } else { 0.85 }),
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_gex_zone_cores(
    frame: &mut canvas::Frame,
    price_to_y: impl Fn(Price) -> f32 + Copy,
    time_to_x: impl Fn(u64) -> f32 + Copy,
    history: &[Arc<data::chart::gex::GexSnapshot>],
    render_cache: &RefCell<GexRenderCache>,
    config: &data::chart::gex::GexLevelsConfig,
    region: Rectangle,
    scaling: f32,
    bucket_ms: u64,
) {
    let bucket_ms = bucket_ms.max(1);
    let frames = cached_gex_zone_frames(history, bucket_ms, config, render_cache);
    for (frame_index, zone_frame) in frames.iter().enumerate() {
        if !config.show_historical_zones && frame_index + 1 != frames.len() {
            continue;
        }
        let x0 = time_to_x(zone_frame.bucket_start.as_u64()).max(region.x);
        let x1 = time_to_x(zone_frame.bucket_start.saturating_add(bucket_ms).as_u64())
            .min(region.x + region.width);
        if x1 <= x0 {
            continue;
        }
        for zone in zone_frame.zones.iter() {
            let color = match zone.sign {
                data::chart::gex::GexZoneSign::Positive => Color::from_rgb8(0x16, 0xd8, 0xc5),
                data::chart::gex::GexZoneSign::Negative => Color::from_rgb8(0xff, 0x31, 0x5d),
            };
            for band in zone.bands.iter() {
                let y = price_to_y(Price::from_f64(band.strike));
                if y < region.y || y > region.y + region.height {
                    continue;
                }
                let effective_strength = gex_band_effective_strength(zone, band);
                let is_peak = band.strike == zone.peak_price;
                let height = gex_screen_width_to_world(
                    if is_peak {
                        2.0 + 2.0 * band.normalized_strength.clamp(0.0, 1.0)
                    } else {
                        0.8 + 1.2 * band.normalized_strength.clamp(0.0, 1.0)
                    },
                    scaling,
                );
                frame.fill(
                    &Path::rectangle(Point::new(x0, y - height * 0.5), Size::new(x1 - x0, height)),
                    color.scale_alpha(
                        (0.20 + 0.65 * effective_strength + if is_peak { 0.10 } else { 0.0 })
                            .clamp(0.0, 1.0),
                    ),
                );
            }
        }
    }
}

fn draw_gex_zone_profile(
    frame: &mut canvas::Frame,
    price_to_y: impl Fn(Price) -> f32,
    snapshot: &data::chart::gex::GexSnapshot,
    config: &data::chart::gex::GexLevelsConfig,
    region: Rectangle,
    scaling: f32,
    palette: &Extended,
) {
    if region.width * scaling < 320.0 {
        return;
    }
    let width =
        region.width * gex_profile_width_percent(config.current_profile_width_percent) / 100.0;
    let right = region.x + region.width;
    let left = right - width;
    let zero = left + width * 0.5;
    frame.fill(
        &Path::rectangle(Point::new(left, region.y), Size::new(width, region.height)),
        palette.background.base.color.scale_alpha(0.76),
    );
    frame.stroke(
        &Path::line(
            Point::new(zero, region.y),
            Point::new(zero, region.y + region.height),
        ),
        Stroke::default()
            .with_color(palette.background.strong.color.scale_alpha(0.40))
            .with_width(gex_screen_width_to_world(1.0, scaling)),
    );
    let positive_scale = data::chart::gex::gex_percentile_95(
        snapshot
            .strikes
            .iter()
            .map(|strike| strike.net_gex_1pct)
            .filter(|value| *value > 0.0),
    );
    let negative_scale = data::chart::gex::gex_percentile_95(
        snapshot
            .strikes
            .iter()
            .map(|strike| strike.net_gex_1pct)
            .filter(|value| *value < 0.0),
    );
    for strike in snapshot.strikes.iter() {
        let (scale, role) = if strike.net_gex_1pct > 0.0 {
            (positive_scale, config.positive_color)
        } else {
            (negative_scale, config.negative_color)
        };
        let Some(scale) = scale else { continue };
        let strength =
            ((strike.net_gex_1pct.abs() / scale).asinh() / 1.0f64.asinh()).clamp(0.0, 1.0) as f32;
        if strength < config.minimum_zone_strength {
            continue;
        }
        let length = width * 0.5 * strength;
        let x = if strike.net_gex_1pct >= 0.0 {
            zero
        } else {
            zero - length
        };
        let y = price_to_y(Price::from_f64(strike.strike));
        if y < region.y || y > region.y + region.height {
            continue;
        }
        let height = gex_screen_width_to_world(2.0, scaling);
        frame.fill(
            &Path::rectangle(Point::new(x, y - height * 0.5), Size::new(length, height)),
            resolve_gex_color(role, palette).scale_alpha(0.85),
        );
    }
}

fn draw_gex_zone_markers(
    frame: &mut canvas::Frame,
    price_to_y: impl Fn(Price) -> f32,
    snapshot: &data::chart::gex::GexSnapshot,
    config: &data::chart::gex::GexLevelsConfig,
    region: Rectangle,
    scaling: f32,
    palette: &Extended,
) {
    let markers = gex_deribit_markers(snapshot, config);
    let gutter = if config.show_current_profile && region.width * scaling >= 320.0 {
        region.width * gex_profile_width_percent(config.current_profile_width_percent) / 100.0
    } else {
        0.0
    };
    let x = region.x + region.width - gutter - gex_screen_width_to_world(30.0, scaling);
    let spacing = gex_screen_width_to_world(16.0, scaling);
    let mut previous_y: Option<f32> = None;
    let mut values = markers
        .into_iter()
        .filter_map(|(show, label, price, role)| show.then_some((label, price?, role)))
        .map(|(label, price, role)| (price_to_y(Price::from_f64(price)), label, role))
        .filter(|(y, _, _)| gex_level_is_visible(*y, region))
        .collect::<Vec<_>>();
    values.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (exact_y, label, role) in values {
        let label_y = previous_y.map_or(exact_y, |previous| exact_y.max(previous + spacing));
        frame.stroke(
            &Path::line(
                Point::new(x - gex_screen_width_to_world(30.0, scaling), exact_y),
                Point::new(x, exact_y),
            ),
            Stroke::default()
                .with_color(resolve_gex_color(role, palette).scale_alpha(0.72))
                .with_width(gex_screen_width_to_world(1.0, scaling)),
        );
        draw_gex_level_label(
            frame,
            label,
            Point::new(x, label_y),
            resolve_gex_color(role, palette),
            scaling,
            palette,
        );
        previous_y = Some(label_y);
    }
}

fn gex_deribit_markers(
    snapshot: &data::chart::gex::GexSnapshot,
    config: &data::chart::gex::GexLevelsConfig,
) -> [(
    bool,
    &'static str,
    Option<f64>,
    data::chart::gex::GexLevelColor,
); 3] {
    [
        (
            config.show_call_wall_marker,
            "CW",
            snapshot.call_wall,
            config.positive_color,
        ),
        (
            config.show_put_wall_marker,
            "PW",
            snapshot.put_wall,
            config.negative_color,
        ),
        (
            config.show_gamma_flip_marker,
            "GF",
            snapshot.gamma_flip,
            data::chart::gex::GexLevelColor::Primary,
        ),
    ]
}

#[derive(Debug, Clone, Copy)]
struct VwapPoint {
    time: u64,
    value: Price,
    upper: Price,
    lower: Price,
}

fn build_vwap_sessions(
    data_source: &PlotData<KlineDataPoint>,
    earliest: u64,
    latest: u64,
    config: &VwapConfig,
) -> Vec<Vec<VwapPoint>> {
    let PlotData::TimeBased(timeseries) = data_source else {
        return Vec::new();
    };
    let anchor_ms = config.anchor.milliseconds();
    let from = align_session_start(earliest, anchor_ms);
    let mut sessions = Vec::<Vec<VwapPoint>>::new();
    let mut active_session = None;
    let mut sum_volume = 0.0;
    let mut sum_price_volume = 0.0;
    let mut sum_price_squared_volume = 0.0;

    for (_, dp) in timeseries
        .datapoints
        .range(UnixMs::new(from)..=UnixMs::new(latest))
    {
        let session = align_session_start(dp.kline.time.as_u64(), anchor_ms);
        if active_session != Some(session) {
            active_session = Some(session);
            sum_volume = 0.0;
            sum_price_volume = 0.0;
            sum_price_squared_volume = 0.0;
            sessions.push(Vec::new());
        }
        for (price, trades) in &dp.footprint.trades {
            let volume = trades.total_qty().to_f64();
            let price = price.to_f64();
            sum_volume += volume;
            sum_price_volume += price * volume;
            sum_price_squared_volume += price * price * volume;
        }
        if sum_volume <= 0.0 {
            continue;
        }
        let vwap = sum_price_volume / sum_volume;
        let variance = (sum_price_squared_volume / sum_volume - vwap * vwap).max(0.0);
        let band = variance.sqrt() * f64::from(config.band_multiplier.max(0.0));
        if let Some(points) = sessions.last_mut() {
            points.push(VwapPoint {
                time: dp.kline.time.as_u64(),
                value: Price::from_f64(vwap),
                upper: Price::from_f64(vwap + band),
                lower: Price::from_f64(vwap - band),
            });
        }
    }
    sessions.retain(|points| !points.is_empty());
    sessions
}

fn draw_vwap_overlay(
    data_source: &PlotData<KlineDataPoint>,
    frame: &mut canvas::Frame,
    earliest: u64,
    latest: u64,
    interval_to_x: impl Fn(u64) -> f32,
    price_to_y: impl Fn(Price) -> f32,
    config: &VwapConfig,
    palette: &Extended,
) {
    let sessions = build_vwap_sessions(data_source, earliest, latest, config);
    let vwap_color = palette.warning.strong.color.scale_alpha(0.96);
    let band_color = palette.secondary.strong.color.scale_alpha(0.62);
    for points in sessions {
        let draw_series = |frame: &mut canvas::Frame,
                           select: fn(&VwapPoint) -> Price,
                           color: Color,
                           width: f32| {
            let mut builder = canvas::path::Builder::new();
            if let Some(first) = points.first() {
                builder.move_to(Point::new(
                    interval_to_x(first.time),
                    price_to_y(select(first)),
                ));
                for point in points.iter().skip(1) {
                    builder.line_to(Point::new(
                        interval_to_x(point.time),
                        price_to_y(select(point)),
                    ));
                }
                frame.stroke(
                    &builder.build(),
                    Stroke::default().with_color(color).with_width(width),
                );
            }
        };
        draw_series(
            frame,
            |point| point.value,
            vwap_color,
            config.line_width.clamp(0.5, 5.0),
        );
        if config.show_bands {
            draw_series(frame, |point| point.upper, band_color, 0.8);
            draw_series(frame, |point| point.lower, band_color, 0.8);
        }
        if config.show_labels
            && let Some(last) = points.last()
        {
            let x = interval_to_x(last.time) + 2.0;
            draw_cluster_text(
                frame,
                "VWAP",
                Point::new(x, price_to_y(last.value)),
                7.0,
                vwap_color,
                Alignment::Start,
                Alignment::Center,
            );
            if config.show_bands {
                draw_cluster_text(
                    frame,
                    "+σ",
                    Point::new(x, price_to_y(last.upper)),
                    7.0,
                    band_color,
                    Alignment::Start,
                    Alignment::Center,
                );
                draw_cluster_text(
                    frame,
                    "-σ",
                    Point::new(x, price_to_y(last.lower)),
                    7.0,
                    band_color,
                    Alignment::Start,
                    Alignment::Center,
                );
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ProfileBin {
    buy: f64,
    sell: f64,
}

impl ProfileBin {
    fn volume(self) -> f64 {
        self.buy + self.sell
    }
    fn delta(self) -> f64 {
        self.buy - self.sell
    }
}

#[derive(Debug)]
struct SessionProfile {
    start: u64,
    end: u64,
    rows: Vec<(Price, ProfileBin)>,
    poc: Price,
    vah: Price,
    val: Price,
    vwap: Price,
    high: Price,
    low: Price,
}

fn align_session_start(timestamp: u64, interval_ms: u64) -> u64 {
    if interval_ms == 7 * 24 * 60 * 60_000 {
        // Unix epoch was a Thursday; shift by three days so weekly profiles
        // open on Monday 00:00 UTC.
        const MONDAY_SHIFT: u64 = 3 * 24 * 60 * 60_000;
        return (timestamp.saturating_add(MONDAY_SHIFT) / interval_ms * interval_ms)
            .saturating_sub(MONDAY_SHIFT);
    }
    timestamp / interval_ms * interval_ms
}

fn vwap_required_from(target_to: UnixMs, visible_earliest: UnixMs, anchor_ms: u64) -> UnixMs {
    let last_real_ms = target_to.saturating_sub(1).as_u64();
    let current_session = align_session_start(last_real_ms, anchor_ms);
    let visible_session =
        align_session_start(visible_earliest.as_u64().min(last_real_ms), anchor_ms);
    UnixMs::new(current_session.min(visible_session))
}

/// REST backfills stop at the last fully closed candle. Trades for the open
/// candle arrive through the live stream; chasing `now()` here would otherwise
/// create one historical request per chart tick and starve older gaps.
fn historical_trade_target_to(kline_latest: UnixMs, timeframe_ms: u64, now: UnixMs) -> UnixMs {
    let latest_candle_end = kline_latest.saturating_add(timeframe_ms);
    if now < latest_candle_end {
        kline_latest
    } else {
        latest_candle_end
    }
}

fn build_session_profiles(
    data_source: &PlotData<KlineDataPoint>,
    earliest: u64,
    latest: u64,
    tick_size: PriceStep,
    config: &SessionVolumeProfileConfig,
) -> Vec<SessionProfile> {
    let PlotData::TimeBased(timeseries) = data_source else {
        return Vec::new();
    };
    let session_ms = config.interval.milliseconds();
    let row_units = tick_size
        .units
        .saturating_mul(i64::from(config.row_size_ticks.max(1)))
        .max(1);
    let from = align_session_start(earliest, session_ms);
    let mut grouped: FxHashMap<u64, (FxHashMap<i64, ProfileBin>, Price, Price)> =
        FxHashMap::default();

    for (_, dp) in timeseries
        .datapoints
        .range(UnixMs::new(from)..=UnixMs::new(latest))
    {
        if dp.footprint.trades.is_empty() {
            continue;
        }
        let session_start = align_session_start(dp.kline.time.as_u64(), session_ms);
        let entry = grouped
            .entry(session_start)
            .or_insert_with(|| (FxHashMap::default(), dp.kline.high, dp.kline.low));
        entry.1 = entry.1.max(dp.kline.high);
        entry.2 = entry.2.min(dp.kline.low);
        for (price, trades) in &dp.footprint.trades {
            let bin_units = price.units.div_euclid(row_units).saturating_mul(row_units);
            let bin = entry.0.entry(bin_units).or_default();
            bin.buy += trades.buy_qty.to_f64();
            bin.sell += trades.sell_qty.to_f64();
        }
    }

    let mut result = grouped
        .into_iter()
        .filter_map(|(start, (bins, high, low))| {
            let mut rows: Vec<_> = bins
                .into_iter()
                .map(|(units, bin)| (Price::from_units(units.saturating_add(row_units / 2)), bin))
                .filter(|(_, bin)| bin.volume() > 0.0)
                .collect();
            rows.sort_by_key(|(price, _)| *price);
            if rows.is_empty() {
                return None;
            }

            let poc_index = rows
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| a.1.volume().total_cmp(&b.1.volume()))
                .map(|(index, _)| index)?;
            let total: f64 = rows.iter().map(|(_, bin)| bin.volume()).sum();
            let target = total * (f64::from(config.value_area_percent.clamp(1.0, 100.0)) / 100.0);
            let mut included = rows[poc_index].1.volume();
            let mut low_index = poc_index;
            let mut high_index = poc_index;
            while included < target && (low_index > 0 || high_index + 1 < rows.len()) {
                let below = if low_index > 0 {
                    rows[low_index - 1].1.volume()
                } else {
                    -1.0
                };
                let above = if high_index + 1 < rows.len() {
                    rows[high_index + 1].1.volume()
                } else {
                    -1.0
                };
                if above >= below {
                    high_index += 1;
                    included += rows[high_index].1.volume();
                } else {
                    low_index -= 1;
                    included += rows[low_index].1.volume();
                }
            }
            let weighted: f64 = rows
                .iter()
                .map(|(price, bin)| price.to_f64() * bin.volume())
                .sum();
            Some(SessionProfile {
                start,
                end: start.saturating_add(session_ms),
                poc: rows[poc_index].0,
                vah: rows[high_index].0,
                val: rows[low_index].0,
                vwap: Price::from_f64(weighted / total.max(f64::EPSILON)),
                high,
                low,
                rows,
            })
        })
        .collect::<Vec<_>>();
    result.sort_by_key(|profile| profile.start);
    result
}

#[allow(clippy::too_many_arguments)]
fn draw_session_volume_profiles(
    data_source: &PlotData<KlineDataPoint>,
    frame: &mut canvas::Frame,
    earliest: u64,
    latest: u64,
    interval_to_x: impl Fn(u64) -> f32,
    price_to_y: impl Fn(Price) -> f32,
    cell_height: f32,
    tick_size: PriceStep,
    config: &SessionVolumeProfileConfig,
    palette: &Extended,
) {
    let profiles = build_session_profiles(data_source, earliest, latest, tick_size, config);
    let row_height = cell_height * f32::from(config.row_size_ticks.max(1)) * 0.86;
    for profile in profiles {
        let session_left = interval_to_x(profile.start);
        let session_right = interval_to_x(profile.end);
        let full_width = (session_right - session_left).abs();
        let max_width = full_width * (config.width_percent.clamp(1.0, 100.0) / 100.0);
        let max_value = profile
            .rows
            .iter()
            .map(|(_, bin)| match config.mode {
                SessionProfileMode::Volume => bin.volume(),
                SessionProfileMode::Delta => bin.delta().abs(),
            })
            .fold(0.0f64, f64::max);
        if max_value <= 0.0 {
            continue;
        }

        for (price, bin) in &profile.rows {
            let value = match config.mode {
                SessionProfileMode::Volume => bin.volume(),
                SessionProfileMode::Delta => bin.delta().abs(),
            };
            let width = max_width * (value / max_value) as f32;
            let x = match config.placement {
                SessionProfilePlacement::Left => session_left,
                SessionProfilePlacement::Right => session_right - width,
            };
            let in_value_area = *price >= profile.val && *price <= profile.vah;
            let base = match config.mode {
                SessionProfileMode::Volume => palette.primary.strong.color,
                SessionProfileMode::Delta if bin.delta() >= 0.0 => palette.success.strong.color,
                SessionProfileMode::Delta => palette.danger.strong.color,
            };
            frame.fill_rectangle(
                Point::new(x, price_to_y(*price) - row_height / 2.0),
                Size::new(width.max(0.1), row_height.max(0.1)),
                base.scale_alpha(if in_value_area { 0.38 } else { 0.18 }),
            );
        }

        let draw_level = |frame: &mut canvas::Frame, price: Price, color: Color, width: f32| {
            frame.stroke(
                &Path::line(
                    Point::new(session_left, price_to_y(price)),
                    Point::new(session_right, price_to_y(price)),
                ),
                Stroke::default().with_color(color).with_width(width),
            );
        };
        let draw_label = |frame: &mut canvas::Frame, text: &str, price: Price, color: Color| {
            let (x, alignment) = match config.placement {
                SessionProfilePlacement::Left => (session_left + 2.0, Alignment::Start),
                SessionProfilePlacement::Right => (session_right - 2.0, Alignment::End),
            };
            draw_cluster_text(
                frame,
                text,
                Point::new(x, price_to_y(price) - 1.0),
                7.0,
                color,
                alignment,
                Alignment::End,
            );
        };
        if config.show_poc {
            let color = palette.warning.strong.color.scale_alpha(0.95);
            draw_level(frame, profile.poc, color, 1.6);
            draw_label(frame, "POC", profile.poc, color);
        }
        if config.show_value_area {
            let color = palette.primary.strong.color.scale_alpha(0.82);
            draw_level(frame, profile.vah, color, 1.0);
            draw_level(frame, profile.val, color, 1.0);
            draw_label(frame, "VAH", profile.vah, color);
            draw_label(frame, "VAL", profile.val, color);
        }
        if config.show_vwap {
            let color = palette.success.base.color.scale_alpha(0.85);
            draw_level(frame, profile.vwap, color, 1.0);
            draw_label(frame, "VWAP", profile.vwap, color);
        }
        if config.show_session_high_low {
            let color = palette.background.strong.text.scale_alpha(0.42);
            draw_level(frame, profile.high, color, 0.7);
            draw_level(frame, profile.low, color, 0.7);
        }
    }
}

fn draw_footprint_kline(
    frame: &mut canvas::Frame,
    price_to_y: impl Fn(Price) -> f32,
    x_position: f32,
    candle_width: f32,
    kline: &Kline,
    palette: &Extended,
) {
    let y_open = price_to_y(kline.open);
    let y_high = price_to_y(kline.high);
    let y_low = price_to_y(kline.low);
    let y_close = price_to_y(kline.close);

    let body_color = if kline.close >= kline.open {
        palette.success.weak.color
    } else {
        palette.danger.weak.color
    };
    frame.fill_rectangle(
        Point::new(x_position - (candle_width / 8.0), y_open.min(y_close)),
        Size::new(candle_width / 4.0, (y_open - y_close).abs()),
        body_color,
    );

    let wick_color = if kline.close >= kline.open {
        palette.success.weak.color
    } else {
        palette.danger.weak.color
    };
    let marker_line = Stroke::with_color(
        Stroke {
            width: 1.0,
            ..Default::default()
        },
        wick_color.scale_alpha(0.6),
    );
    frame.stroke(
        &Path::line(
            Point::new(x_position, y_high),
            Point::new(x_position, y_low),
        ),
        marker_line,
    );
}

fn draw_candle_dp(
    frame: &mut canvas::Frame,
    price_to_y: impl Fn(Price) -> f32,
    candle_width: f32,
    palette: &Extended,
    x_position: f32,
    kline: &Kline,
) {
    let y_open = price_to_y(kline.open);
    let y_high = price_to_y(kline.high);
    let y_low = price_to_y(kline.low);
    let y_close = price_to_y(kline.close);

    let body_color = if kline.close >= kline.open {
        palette.success.base.color
    } else {
        palette.danger.base.color
    };
    frame.fill_rectangle(
        Point::new(x_position - (candle_width / 2.0), y_open.min(y_close)),
        Size::new(candle_width, (y_open - y_close).abs()),
        body_color,
    );

    let wick_color = if kline.close >= kline.open {
        palette.success.base.color
    } else {
        palette.danger.base.color
    };
    frame.fill_rectangle(
        Point::new(x_position - (candle_width / 8.0), y_high),
        Size::new(candle_width / 4.0, (y_high - y_low).abs()),
        wick_color,
    );
}

#[derive(Debug, Clone)]
pub struct RenderedVolumeBubble {
    pub cluster: VolumeBubbleCluster,
    pub center: Point,
    pub original_center: Point,
    pub radius_px: f32,
    pub fill_color: Color,
    pub border_color: Color,
    pub fill_alpha: f32,
    pub border_alpha: f32,
    pub label: Option<String>,
    pub age_factor: f32,
    pub price_response: BubblePriceResponse,
}

fn volume_bubble_color(
    bubble: &VolumeBubbleCluster,
    color_mode: BubbleColorMode,
    palette: &Extended,
) -> Color {
    let total_qty = bubble.total_qty.to_f64().max(f64::EPSILON);
    let delta = bubble.delta_qty.to_f64();
    let dominance = (delta.abs() / total_qty).clamp(0.0, 1.0) as f32;

    if dominance < 0.10 {
        return palette.background.strong.text;
    }

    match color_mode {
        BubbleColorMode::Delta => {
            let base = if delta > 0.0 {
                palette.success.strong.color
            } else {
                palette.danger.strong.color
            };
            mix_color(
                base,
                palette.background.base.color,
                0.55 + (dominance * 0.35),
            )
        }
        BubbleColorMode::DominantSide => {
            let base = if bubble.buy_qty >= bubble.sell_qty {
                palette.success.strong.color
            } else {
                palette.danger.strong.color
            };
            mix_color(base, palette.background.base.color, 0.55 + dominance * 0.35)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn build_rendered_volume_bubbles(
    data_source: &PlotData<KlineDataPoint>,
    earliest: u64,
    latest: u64,
    price_to_y: impl Fn(Price) -> f32,
    interval_to_x: impl Fn(u64) -> f32,
    price_step: PriceStep,
    candle_width: f32,
    scaling: f32,
    config: &VolumeBubbleConfig,
    palette: &Extended,
    now: UnixMs,
    stabilized_threshold: &RefCell<StabilizedBubbleThreshold>,
) -> Vec<RenderedVolumeBubble> {
    if scaling <= f32::EPSILON || config.max_bubbles_per_bar == 0 || latest < earliest {
        return Vec::new();
    }
    let mut clusters = Vec::new();
    match data_source {
        PlotData::TimeBased(timeseries) => {
            let timeframe_ms = timeseries.interval.to_milliseconds();
            let baseline_from =
                latest.saturating_sub(config.adaptive_window_minutes.max(1).saturating_mul(60_000));
            for (&candle_time, dp) in timeseries
                .datapoints
                .range(UnixMs::new(earliest.min(baseline_from))..=UnixMs::new(latest))
            {
                if config.use_raw_trades_when_available
                    && dp.trade_coverage == data::chart::kline::TradeCoverage::Complete
                    && !dp.trade_sequence.is_empty()
                {
                    clusters.extend(cluster_volume_bubble_trades(
                        &dp.trade_sequence,
                        candle_time,
                        timeframe_ms,
                        price_step,
                        config,
                    ));
                } else if dp.bubble_summary.algorithm_version
                    == data::chart::kline::BUBBLE_SUMMARY_ALGORITHM_VERSION
                {
                    clusters.extend(dp.bubble_summary.candidates.iter().copied());
                }
            }
        }
        PlotData::TickBased(_) => return Vec::new(),
    }
    let distribution = clusters
        .iter()
        .filter(|cluster| {
            cluster.last_time.as_u64()
                >= latest
                    .saturating_sub(config.adaptive_window_minutes.max(1).saturating_mul(60_000))
        })
        .map(|cluster| cluster.total_qty.to_f64())
        .collect::<Vec<_>>();
    let baseline_clusters = clusters
        .iter()
        .copied()
        .filter(|cluster| {
            cluster.last_time.as_u64()
                >= latest
                    .saturating_sub(config.adaptive_window_minutes.max(1).saturating_mul(60_000))
        })
        .collect::<Vec<_>>();
    let baselines = adaptive_bubble_threshold_baselines(&baseline_clusters, config, 20);
    let threshold = stabilized_threshold
        .borrow_mut()
        .update(baselines.combined.effective, now);
    let scale = threshold / baselines.combined.effective.max(f64::EPSILON);
    let buy_threshold = baselines.buy_dominant.effective * scale;
    let sell_threshold = baselines.sell_dominant.effective * scale;
    clusters.retain(|cluster| {
        cluster.candle_time.as_u64() >= earliest
            && cluster.candle_time.as_u64() <= latest
            && cluster.total_qty.to_f64()
                >= if cluster.buy_qty >= cluster.sell_qty {
                    buy_threshold
                } else {
                    sell_threshold
                }
    });
    let reference_p99 = percentile(&distribution, 99.0).unwrap_or(threshold.max(1.0));
    let mut clusters = apply_volume_bubble_budget(
        clusters,
        config,
        buy_threshold.min(sell_threshold).min(threshold),
    );
    clusters.sort_by_key(|cluster| (cluster.candle_time, cluster.weighted_time, cluster.id));
    let response_for = |cluster: &VolumeBubbleCluster| {
        let target = cluster
            .last_time
            .saturating_add(u64::from(config.price_response_horizon_seconds).saturating_mul(1_000));
        let horizon_elapsed = now >= target;
        let future_price = if horizon_elapsed {
            match data_source {
                PlotData::TimeBased(timeseries) => timeseries
                    .datapoints
                    .range(target..)
                    .next()
                    .map(|(_, point)| point.kline.close),
                PlotData::TickBased(_) => None,
            }
        } else {
            None
        };
        classify_bubble_price_response(cluster, future_price, horizon_elapsed, config)
    };

    let mut rendered = clusters
        .into_iter()
        .map(|cluster| {
            let radius_px = data::chart::kline::volume_bubble_radius(
                cluster.total_qty.to_f64(),
                threshold,
                reference_p99,
                config.min_radius_px,
                config.max_radius_px,
            );
            let original_center = Point::new(
                interval_to_x(cluster.weighted_time.as_u64()),
                price_to_y(cluster.vwap_price),
            );
            let color = volume_bubble_color(&cluster, config.color_mode, palette);
            let age_factor = bubble_age_factor(
                now.as_u64().saturating_sub(cluster.last_time.as_u64()),
                config.age_fading,
            );
            let importance = (cluster.percentile_rank / 100.0).clamp(0.0, 1.0);
            let fill_alpha = if config.fill_enabled {
                (0.08 + 0.12 * importance) * config.fill_intensity * age_factor
            } else {
                0.0
            };
            let border_alpha = config.border_opacity * age_factor;
            let price_response = response_for(&cluster);
            let label = match config.label_mode {
                BubbleLabelMode::None => None,
                BubbleLabelMode::ExtremeOnly
                    if cluster.percentile_rank < config.label_percentile =>
                {
                    None
                }
                BubbleLabelMode::ExtremeOnly | BubbleLabelMode::All if radius_px >= 9.0 => {
                    Some(abbr_large_numbers(cluster.total_qty.to_f64()))
                }
                BubbleLabelMode::ExtremeOnly | BubbleLabelMode::All => None,
            };
            RenderedVolumeBubble {
                cluster,
                center: original_center,
                original_center,
                radius_px,
                fill_color: color,
                border_color: color,
                fill_alpha: fill_alpha.clamp(0.0, 1.0),
                border_alpha: border_alpha.clamp(0.0, 1.0),
                label,
                age_factor,
                price_response,
            }
        })
        .collect::<Vec<_>>();
    collision_layout(&mut rendered, candle_width, scaling, config);
    apply_label_budget(&mut rendered, config.max_labels_in_view);
    rendered
}

fn collision_layout(
    bubbles: &mut Vec<RenderedVolumeBubble>,
    candle_width: f32,
    scaling: f32,
    config: &VolumeBubbleConfig,
) {
    bubbles.sort_by(|left, right| {
        right
            .cluster
            .importance_score
            .total_cmp(&left.cluster.importance_score)
            .then_with(|| left.cluster.id.cmp(&right.cluster.id))
    });
    let max_offset = (8.0 / scaling).min(candle_width * 0.45);
    let offsets = [0.0, -0.5, 0.5, -1.0, 1.0];
    let mut accepted: Vec<RenderedVolumeBubble> = Vec::with_capacity(bubbles.len());
    for mut bubble in bubbles.drain(..) {
        let side_sign = if bubble.cluster.buy_qty >= bubble.cluster.sell_qty {
            1.0
        } else {
            -1.0
        };
        let position = offsets
            .into_iter()
            .map(|factor| factor * max_offset * side_sign)
            .find(|offset| {
                let center =
                    Point::new(bubble.original_center.x + offset, bubble.original_center.y);
                accepted.iter().all(|other| {
                    let dx = (center.x - other.center.x) * scaling;
                    let dy = (center.y - other.center.y) * scaling;
                    let required = config
                        .min_center_distance_px
                        .max((bubble.radius_px + other.radius_px) * 0.65);
                    dx.mul_add(dx, dy * dy).sqrt() >= required
                })
            });
        if let Some(offset) = position {
            bubble.center.x += offset;
            accepted.push(bubble);
        }
    }
    accepted.sort_by_key(|bubble| {
        (
            bubble.cluster.candle_time,
            bubble.cluster.weighted_time,
            bubble.cluster.id,
        )
    });
    *bubbles = accepted;
}

fn apply_label_budget(bubbles: &mut [RenderedVolumeBubble], budget: usize) {
    let mut indices = bubbles
        .iter()
        .enumerate()
        .filter(|(_, bubble)| bubble.label.is_some())
        .map(|(index, bubble)| (index, bubble.cluster.importance_score))
        .collect::<Vec<_>>();
    indices.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    for (index, _) in indices.into_iter().skip(budget) {
        bubbles[index].label = None;
    }
}

fn draw_rendered_volume_bubbles(
    frame: &mut canvas::Frame,
    bubbles: &[RenderedVolumeBubble],
    scaling: f32,
    palette: &Extended,
    config: &VolumeBubbleConfig,
) {
    for bubble in bubbles {
        let radius = bubble.radius_px / scaling;
        if (bubble.center.x - bubble.original_center.x).abs() * scaling > 4.0 {
            frame.stroke(
                &Path::line(bubble.original_center, bubble.center),
                Stroke::default()
                    .with_color(bubble.border_color.scale_alpha(bubble.border_alpha * 0.45))
                    .with_width(0.7 / scaling),
            );
        }
        let circle = Path::circle(bubble.center, radius);
        if bubble.price_response == BubblePriceResponse::FollowThrough {
            frame.stroke(
                &Path::circle(bubble.center, radius + 2.5 / scaling),
                Stroke::default()
                    .with_color(bubble.border_color.scale_alpha(bubble.border_alpha * 0.28))
                    .with_width(1.0 / scaling),
            );
        }
        if config.three_dimensional {
            draw_three_dimensional_bubble(frame, bubble, radius, scaling);
        } else {
            frame.fill(&circle, bubble.fill_color.scale_alpha(bubble.fill_alpha));
        }
        frame.stroke(
            &circle,
            Stroke::default()
                .with_color(bubble.border_color.scale_alpha(bubble.border_alpha))
                .with_width(1.6 / scaling),
        );
        if bubble.price_response == BubblePriceResponse::Reversed {
            frame.fill(
                &Path::circle(
                    Point::new(
                        bubble.center.x + radius * 0.75,
                        bubble.center.y - radius * 0.75,
                    ),
                    1.8 / scaling,
                ),
                palette
                    .warning
                    .strong
                    .color
                    .scale_alpha(bubble.border_alpha),
            );
        }
        if let Some(label) = &bubble.label {
            draw_cluster_text(
                frame,
                label,
                bubble.center,
                (bubble.radius_px * 0.72).clamp(7.0, 10.0) / scaling,
                palette.background.base.text,
                Alignment::Center,
                Alignment::Center,
            );
        }
    }
}

fn draw_three_dimensional_bubble(
    frame: &mut canvas::Frame,
    bubble: &RenderedVolumeBubble,
    radius: f32,
    scaling: f32,
) {
    let shadow_offset = 2.5 / scaling;
    frame.fill(
        &Path::circle(
            Point::new(
                bubble.center.x + shadow_offset,
                bubble.center.y + shadow_offset * 1.25,
            ),
            radius * 1.02,
        ),
        Color::BLACK.scale_alpha(0.24 * bubble.age_factor),
    );

    let base_alpha = (bubble.fill_alpha * 2.4)
        .max(0.34 * bubble.age_factor)
        .clamp(0.0, 0.88);
    frame.fill(
        &Path::circle(bubble.center, radius),
        bubble.fill_color.scale_alpha(base_alpha),
    );

    // Layered highlights approximate a radial gradient while keeping the
    // bubbles in the existing canvas renderer.
    for (radius_factor, offset_factor, alpha) in
        [(0.72, 0.10, 0.16), (0.48, 0.22, 0.19), (0.22, 0.38, 0.28)]
    {
        let highlight_center = Point::new(
            bubble.center.x - radius * offset_factor,
            bubble.center.y - radius * offset_factor,
        );
        frame.fill(
            &Path::circle(highlight_center, radius * radius_factor),
            Color::WHITE.scale_alpha(alpha * bubble.age_factor),
        );
    }

    frame.stroke(
        &Path::circle(
            Point::new(
                bubble.center.x + radius * 0.08,
                bubble.center.y + radius * 0.10,
            ),
            radius * 0.84,
        ),
        Stroke::default()
            .with_color(Color::BLACK.scale_alpha(0.16 * bubble.age_factor))
            .with_width(0.8 / scaling),
    );
}

fn bubble_screen_center(
    bubble: &RenderedVolumeBubble,
    viewport_center: Vector,
    translation: Vector,
    scaling: f32,
) -> Point {
    Point::new(
        viewport_center.x + (translation.x + bubble.center.x) * scaling,
        viewport_center.y + (translation.y + bubble.center.y) * scaling,
    )
}

fn hit_test_volume_bubbles(
    bubbles: &[RenderedVolumeBubble],
    cursor: Point,
    viewport_center: Vector,
    translation: Vector,
    scaling: f32,
) -> Option<&RenderedVolumeBubble> {
    bubbles
        .iter()
        .filter(|bubble| {
            let center = bubble_screen_center(bubble, viewport_center, translation, scaling);
            let dx = cursor.x - center.x;
            let dy = cursor.y - center.y;
            dx.mul_add(dx, dy * dy) <= bubble.radius_px * bubble.radius_px
        })
        .max_by(|left, right| {
            left.cluster
                .importance_score
                .total_cmp(&right.cluster.importance_score)
                .then_with(|| right.cluster.id.cmp(&left.cluster.id))
        })
}

fn draw_hovered_volume_bubble(
    frame: &mut canvas::Frame,
    bubble: &RenderedVolumeBubble,
    viewport_center: Vector,
    translation: Vector,
    scaling: f32,
    config: &VolumeBubbleConfig,
) {
    let center = bubble_screen_center(bubble, viewport_center, translation, scaling);
    let circle = Path::circle(center, bubble.radius_px + 2.0);
    frame.fill(
        &circle,
        bubble
            .fill_color
            .scale_alpha((config.hover_opacity + (1.0 - bubble.age_factor) * 0.02).clamp(0.0, 1.0)),
    );
    frame.stroke(
        &circle,
        Stroke::default()
            .with_color(bubble.border_color.scale_alpha(0.98))
            .with_width(2.0),
    );
}

fn bubble_tooltip_lines(
    bubble: &RenderedVolumeBubble,
    unit: &str,
    threshold_mode: data::chart::kline::BubbleThresholdMode,
) -> Vec<String> {
    let cluster = &bubble.cluster;
    let total = cluster.total_qty.to_f64();
    let dominance = cluster.buy_qty.abs_diff(cluster.sell_qty).to_f64() / total.max(f64::EPSILON);
    let heading = if dominance < 0.10 {
        "Mixed flow"
    } else if cluster.buy_qty > cluster.sell_qty {
        "Aggressive buys"
    } else {
        "Aggressive sells"
    };
    let duration = cluster
        .last_time
        .as_u64()
        .saturating_sub(cluster.first_time.as_u64());
    let timestamp = cluster
        .weighted_time
        .format_utc("%Y-%m-%d %H:%M:%S%.3f UTC")
        .unwrap_or_else(|| cluster.weighted_time.as_u64().to_string());
    let buy_share = cluster.buy_qty.to_f64() / total.max(f64::EPSILON) * 100.0;
    let sell_share = cluster.sell_qty.to_f64() / total.max(f64::EPSILON) * 100.0;
    vec![
        heading.to_string(),
        timestamp,
        format!("Total volume      {:.4} {unit}", total),
        format!("Buy volume        {:.4} {unit}", cluster.buy_qty.to_f64()),
        format!("Sell volume       {:.4} {unit}", cluster.sell_qty.to_f64()),
        format!("Flow split        {buy_share:.1}% buy / {sell_share:.1}% sell"),
        format!("Delta            {:+.4} {unit}", cluster.delta_qty.to_f64()),
        format!("Trades            {}", cluster.trade_count),
        format!(
            "Largest trade     {:.4} {unit}",
            cluster.largest_trade_qty.to_f64()
        ),
        format!("VWAP              {:.4}", cluster.vwap_price.to_f64()),
        format!("Duration          {duration} ms"),
        format!(
            "Relative size     {:.1} percentile",
            cluster.percentile_rank
        ),
        format!("Threshold mode    {threshold_mode}"),
        format!("Price response    {:?}", bubble.price_response),
    ]
}

fn draw_volume_bubble_tooltip(
    frame: &mut canvas::Frame,
    bubble: &RenderedVolumeBubble,
    palette: &Extended,
    ticker_info: &TickerInfo,
    config: &VolumeBubbleConfig,
    cursor: Point,
    bounds: Size,
) {
    let symbol = ticker_info.ticker.display_symbol().unwrap_or("units");
    let unit = ["USDT", "USDC", "USD", "BTC", "ETH"]
        .into_iter()
        .find_map(|quote| symbol.strip_suffix(quote))
        .filter(|base| !base.is_empty())
        .unwrap_or("units");
    let lines = bubble_tooltip_lines(bubble, unit, config.threshold_mode);
    let width = 330.0;
    let line_height = 17.0;
    let height = line_height * lines.len() as f32 + 12.0;
    let x = if cursor.x + width + 14.0 <= bounds.width {
        cursor.x + 14.0
    } else {
        cursor.x - width - 14.0
    }
    .clamp(8.0, (bounds.width - width - 8.0).max(8.0));
    let y = (cursor.y - height * 0.5).clamp(8.0, (bounds.height - height - 8.0).max(8.0));
    frame.fill_rectangle(
        Point::new(x, y),
        Size::new(width, height),
        palette.background.weakest.color.scale_alpha(0.96),
    );
    for (index, line) in lines.into_iter().enumerate() {
        frame.fill_text(canvas::Text {
            content: line,
            position: Point::new(x + 8.0, y + 6.0 + index as f32 * line_height),
            size: iced::Pixels(if index == 0 { 13.0 } else { 12.0 }),
            color: if index == 0 {
                bubble.border_color
            } else {
                palette.background.base.text
            },
            font: style::AZERET_MONO,
            ..canvas::Text::default()
        });
    }
}

fn current_volume_bubble_session_start_ms(
    now: chrono::DateTime<chrono::Utc>,
    session: VolumeBubbleSession,
) -> UnixMs {
    let rome_now = now.with_timezone(&chrono_tz::Europe::Rome);
    let active_session = match session {
        VolumeBubbleSession::Auto => {
            let minutes_since_midnight = rome_now.hour() * 60 + rome_now.minute();
            match minutes_since_midnight {
                0..540 => VolumeBubbleSession::Asian,
                540..930 => VolumeBubbleSession::London,
                _ => VolumeBubbleSession::NewYork,
            }
        }
        selected => selected,
    };

    let (hour, minute) = match active_session {
        VolumeBubbleSession::Auto => unreachable!("auto session is resolved above"),
        VolumeBubbleSession::Asian => (0, 0),
        VolumeBubbleSession::London => (9, 0),
        VolumeBubbleSession::NewYork => (15, 30),
    };

    let session_start = chrono_tz::Europe::Rome
        .with_ymd_and_hms(
            rome_now.year(),
            rome_now.month(),
            rome_now.day(),
            hour,
            minute,
            0,
        )
        .earliest()
        .unwrap_or(rome_now)
        .with_timezone(&chrono::Utc);

    UnixMs::new(session_start.timestamp_millis().max(0) as u64)
}

fn volume_bubble_effective_range(
    kline_latest: UnixMs,
    timeframe_ms: u64,
    now: UnixMs,
    config: &VolumeBubbleConfig,
) -> Option<(UnixMs, UnixMs)> {
    let window_to = kline_latest.saturating_add(timeframe_ms).min(now);
    let window_from =
        window_to.saturating_sub(config.history_window_minutes.max(1).saturating_mul(60_000));
    let at_window_end =
        chrono::DateTime::from_timestamp_millis(window_to.saturating_sub(1).as_u64() as i64)?;
    let session_start = current_volume_bubble_session_start_ms(at_window_end, config.session);
    let effective_from = window_from.max(session_start);
    (effective_from < window_to).then_some((effective_from, window_to))
}

fn max_bubble_qty_in_range(
    data_source: &PlotData<KlineDataPoint>,
    earliest: u64,
    latest: u64,
    prefer_raw_trades: bool,
) -> Option<f64> {
    if latest < earliest {
        return None;
    }

    let max_from_sources = |trades: &KlineTrades, summary: &BubbleVolumeSummary| {
        if !prefer_raw_trades && !summary.is_empty() {
            return summary
                .candidates
                .iter()
                .map(|candidate| candidate.total_qty.to_f64())
                .filter(|qty| *qty > 0.0)
                .max_by(f64::total_cmp);
        }

        let raw_max = trades
            .trades
            .values()
            .map(|group| group.total_qty().to_f64())
            .filter(|qty| *qty > 0.0)
            .max_by(f64::total_cmp);
        if raw_max.is_some() || summary.is_empty() {
            raw_max
        } else {
            summary
                .candidates
                .iter()
                .map(|candidate| candidate.total_qty.to_f64())
                .filter(|qty| *qty > 0.0)
                .max_by(f64::total_cmp)
        }
    };

    match data_source {
        PlotData::TickBased(tick_aggr) => {
            let earliest = earliest as usize;
            let latest = latest as usize;

            tick_aggr
                .datapoints
                .iter()
                .enumerate()
                .filter(|(index, _)| *index >= earliest && *index <= latest)
                .filter_map(|(_, dp)| {
                    max_from_sources(&dp.footprint, &BubbleVolumeSummary::default())
                })
                .max_by(f64::total_cmp)
        }
        PlotData::TimeBased(timeseries) => timeseries
            .datapoints
            .range(UnixMs::new(earliest)..=UnixMs::new(latest))
            .filter_map(|(_, dp)| max_from_sources(&dp.footprint, &dp.bubble_summary))
            .max_by(f64::total_cmp),
    }
}

fn volume_bubble_qty_scale(max_qty: Option<f64>) -> VolumeBubbleQtyScale {
    let max = max_qty
        .filter(|value| value.is_finite() && *value > 0.0)
        .map(|value| nice_ceiling(value * 1.1))
        .unwrap_or(100.0)
        .max(1.0);
    let step = nice_step(max / 100.0);

    VolumeBubbleQtyScale {
        min: 0.0,
        max,
        step,
    }
}

fn nice_ceiling(value: f64) -> f64 {
    if !value.is_finite() || value <= 0.0 {
        return 1.0;
    }

    let magnitude = 10.0f64.powf(value.log10().floor());
    let normalized = value / magnitude;
    let nice = if normalized <= 1.0 {
        1.0
    } else if normalized <= 2.0 {
        2.0
    } else if normalized <= 2.5 {
        2.5
    } else if normalized <= 5.0 {
        5.0
    } else {
        10.0
    };

    nice * magnitude
}

fn nice_step(value: f64) -> f64 {
    if !value.is_finite() || value <= 0.0 {
        return 1.0;
    }

    let magnitude = 10.0f64.powf(value.log10().floor());
    let normalized = value / magnitude;
    let nice = if normalized <= 1.0 {
        1.0
    } else if normalized <= 2.0 {
        2.0
    } else if normalized <= 5.0 {
        5.0
    } else {
        10.0
    };

    nice * magnitude
}

fn render_data_source<F>(
    data_source: &PlotData<KlineDataPoint>,
    frame: &mut canvas::Frame,
    earliest: u64,
    latest: u64,
    interval_to_x: impl Fn(u64) -> f32,
    draw_fn: F,
) where
    F: Fn(&mut canvas::Frame, f32, &Kline, &KlineTrades, &BubbleVolumeSummary),
{
    match data_source {
        PlotData::TickBased(tick_aggr) => {
            let earliest = earliest as usize;
            let latest = latest as usize;

            tick_aggr
                .datapoints
                .iter()
                .rev()
                .enumerate()
                .filter(|(index, _)| *index <= latest && *index >= earliest)
                .for_each(|(index, tick_aggr)| {
                    let x_position = interval_to_x(index as u64);

                    draw_fn(
                        frame,
                        x_position,
                        &tick_aggr.kline,
                        &tick_aggr.footprint,
                        &BubbleVolumeSummary::default(),
                    );
                });
        }
        PlotData::TimeBased(timeseries) => {
            if latest < earliest {
                return;
            }

            timeseries
                .datapoints
                .range(UnixMs::new(earliest)..=UnixMs::new(latest))
                .for_each(|(timestamp, dp)| {
                    let x_position = interval_to_x(timestamp.as_u64());

                    draw_fn(
                        frame,
                        x_position,
                        &dp.kline,
                        &dp.footprint,
                        &dp.bubble_summary,
                    );
                });
        }
    }
}

fn draw_all_npocs(
    data_source: &PlotData<KlineDataPoint>,
    frame: &mut canvas::Frame,
    price_to_y: impl Fn(Price) -> f32,
    interval_to_x: impl Fn(u64) -> f32,
    layout: &FootprintCellLayout<'_>,
    studies: &[FootprintStudy],
    visible_earliest: u64,
    visible_latest: u64,
    imb_study_on: bool,
) {
    let Some(lookback) = studies.iter().find_map(|study| {
        if let FootprintStudy::NPoC { lookback } = study {
            Some(*lookback)
        } else {
            None
        }
    }) else {
        return;
    };

    let (filled_color, naked_color) = (
        layout.pal.background.strong.color,
        if layout.pal.is_dark {
            layout.pal.warning.weak.color.scale_alpha(0.5)
        } else {
            layout.pal.warning.strong.color
        },
    );

    let line_height = layout.cell_h.min(1.0);

    let bar_width_factor: f32 = 0.9;
    let inset = (layout.cell_w * (1.0 - bar_width_factor)) / 2.0;

    let candle_lane_factor: f32 = match layout.cluster {
        ClusterKind::VolumeProfile | ClusterKind::DeltaProfile => 0.25,
        ClusterKind::BidAsk | ClusterKind::Table => 1.0,
    };

    let start_x_for = |cell_center_x: f32| -> f32 {
        match layout.cluster {
            ClusterKind::Table => cell_center_x + (layout.cell_w / 2.0) - inset,
            ClusterKind::BidAsk => {
                cell_center_x + (layout.candle_w / 2.0) + layout.gaps.candle_to_cluster
            }
            ClusterKind::VolumeProfile | ClusterKind::DeltaProfile => {
                let content_left = (cell_center_x - (layout.cell_w / 2.0)) + inset;
                let candle_lane_left = content_left
                    + if imb_study_on {
                        layout.candle_w + layout.gaps.marker_to_candle
                    } else {
                        0.0
                    };
                candle_lane_left
                    + layout.candle_w * candle_lane_factor
                    + layout.gaps.candle_to_cluster
            }
        }
    };

    let wick_x_for = |cell_center_x: f32| -> f32 {
        match layout.cluster {
            ClusterKind::BidAsk | ClusterKind::Table => cell_center_x,
            ClusterKind::VolumeProfile | ClusterKind::DeltaProfile => {
                let content_left = (cell_center_x - (layout.cell_w / 2.0)) + inset;
                let candle_lane_left = content_left
                    + if imb_study_on {
                        layout.candle_w + layout.gaps.marker_to_candle
                    } else {
                        0.0
                    };
                candle_lane_left + (layout.candle_w * candle_lane_factor) / 2.0
                    - (layout.gaps.candle_to_cluster * 0.5)
            }
        }
    };

    let end_x_for = |cell_center_x: f32| -> f32 {
        match layout.cluster {
            ClusterKind::Table => {
                let content_left = cell_center_x - (layout.cell_w / 2.0) + inset;
                let content_right = cell_center_x + (layout.cell_w / 2.0) - inset;
                let table_layout = TableLayout::new(
                    content_left,
                    content_right,
                    layout.candle_w,
                    layout.gaps,
                    imb_study_on,
                );
                table_layout.table_left
            }
            ClusterKind::BidAsk => {
                cell_center_x - (layout.candle_w / 2.0) - layout.gaps.candle_to_cluster
            }
            ClusterKind::VolumeProfile | ClusterKind::DeltaProfile => wick_x_for(cell_center_x),
        }
    };

    let rightmost_cell_center_x = {
        let earliest_x = interval_to_x(visible_earliest);
        let latest_x = interval_to_x(visible_latest);
        if earliest_x > latest_x {
            earliest_x
        } else {
            latest_x
        }
    };

    let mut draw_the_line = |interval: u64, poc: &PointOfControl| {
        let start_x = start_x_for(interval_to_x(interval));

        let (line_width, color) = match poc.status {
            NPoc::Naked => {
                let end_x = end_x_for(rightmost_cell_center_x);
                let line_width = end_x - start_x;
                if line_width.abs() <= layout.cell_w {
                    return;
                }
                (line_width, naked_color)
            }
            NPoc::Filled { at } => {
                let end_x = end_x_for(interval_to_x(at));
                let line_width = end_x - start_x;
                if line_width.abs() <= layout.cell_w {
                    return;
                }
                (line_width, filled_color)
            }
            _ => return,
        };

        frame.fill_rectangle(
            Point::new(start_x, price_to_y(poc.price) - line_height / 2.0),
            Size::new(line_width, line_height),
            color,
        );
    };

    match data_source {
        PlotData::TickBased(tick_aggr) => {
            tick_aggr
                .datapoints
                .iter()
                .rev()
                .enumerate()
                .take(lookback)
                .filter_map(|(index, dp)| dp.footprint.poc.as_ref().map(|poc| (index as u64, poc)))
                .for_each(|(interval, poc)| draw_the_line(interval, poc));
        }
        PlotData::TimeBased(timeseries) => {
            timeseries
                .datapoints
                .iter()
                .rev()
                .take(lookback)
                .filter_map(|(timestamp, dp)| {
                    dp.footprint
                        .poc
                        .as_ref()
                        .map(|poc| (timestamp.as_u64(), poc))
                })
                .for_each(|(interval, poc)| draw_the_line(interval, poc));
        }
    }
}

fn draw_clusters(
    frame: &mut canvas::Frame,
    price_to_y: impl Fn(Price) -> f32,
    x_position: f32,
    layout: &FootprintCellLayout<'_>,
    scaling: f32,
    max_cluster_qty: f64,
    text_size: f32,
    step: PriceStep,
    show_text: bool,
    show_summary: bool,
    imbalance: Option<(usize, Option<usize>, bool)>,
    kline: &Kline,
    footprint: &KlineTrades,
) {
    let text_color = layout.pal.background.weakest.text;

    let bar_width_factor: f32 = 0.9;
    let inset = (layout.cell_w * (1.0 - bar_width_factor)) / 2.0;

    let cell_left = x_position - (layout.cell_w / 2.0);
    let content_left = cell_left + inset;
    let content_right = x_position + (layout.cell_w / 2.0) - inset;

    let mut table_layout: Option<TableLayout> = None;

    match layout.cluster {
        ClusterKind::VolumeProfile | ClusterKind::DeltaProfile => {
            let area = ProfileArea::new(
                content_left,
                content_right,
                layout.candle_w,
                layout.gaps,
                imbalance.is_some(),
            );
            let bar_alpha = if show_text { 0.25 } else { 1.0 };

            for (price, group) in &footprint.trades {
                let buy_qty = group.buy_qty.to_f64();
                let sell_qty = group.sell_qty.to_f64();
                let y = price_to_y(*price);

                match layout.cluster {
                    ClusterKind::VolumeProfile => {
                        super::draw_volume_bar(
                            frame,
                            area.bars_left,
                            y,
                            buy_qty,
                            sell_qty,
                            max_cluster_qty,
                            area.bars_width,
                            layout.cell_h,
                            layout.pal.success.base.color,
                            layout.pal.danger.base.color,
                            bar_alpha,
                            true,
                        );

                        if show_text {
                            draw_cluster_text(
                                frame,
                                &abbr_large_numbers(f64::from(group.total_qty())),
                                Point::new(area.bars_left, y),
                                text_size,
                                text_color,
                                Alignment::Start,
                                Alignment::Center,
                            );
                        }
                    }
                    ClusterKind::DeltaProfile => {
                        let delta = group.delta_qty().to_f64();
                        if show_text {
                            draw_cluster_text(
                                frame,
                                &abbr_large_numbers(delta),
                                Point::new(area.bars_left, y),
                                text_size,
                                text_color,
                                Alignment::Start,
                                Alignment::Center,
                            );
                        }

                        let bar_width = (delta.abs() / max_cluster_qty) as f32 * area.bars_width;
                        if bar_width > 0.0 {
                            let color = if delta >= 0.0 {
                                layout.pal.success.base.color.scale_alpha(bar_alpha)
                            } else {
                                layout.pal.danger.base.color.scale_alpha(bar_alpha)
                            };
                            frame.fill_rectangle(
                                Point::new(area.bars_left, y - (layout.cell_h / 2.0)),
                                Size::new(bar_width, layout.cell_h),
                                color,
                            );
                        }
                    }
                    _ => {}
                }

                if let Some((threshold, color_scale, ignore_zeros)) = imbalance {
                    let higher_price = price.add_steps(1, step);

                    let rect_w = ((area.imb_marker_width - 1.0) / 2.0).max(1.0);
                    let buyside_x = area.imb_marker_left + area.imb_marker_width - rect_w;
                    let sellside_x =
                        area.imb_marker_left + area.imb_marker_width - (2.0 * rect_w) - 1.0;

                    draw_imbalance_markers(
                        frame,
                        &price_to_y,
                        footprint,
                        *price,
                        sell_qty,
                        higher_price,
                        threshold,
                        color_scale,
                        ignore_zeros,
                        layout.cell_h,
                        layout.pal,
                        buyside_x,
                        sellside_x,
                        rect_w,
                    );
                }
            }

            draw_footprint_kline(
                frame,
                &price_to_y,
                area.candle_center_x,
                layout.candle_w,
                kline,
                layout.pal,
            );
        }
        ClusterKind::Table => {
            let tl = TableLayout::new(
                content_left,
                content_right,
                layout.candle_w,
                layout.gaps,
                imbalance.is_some(),
            );
            let area = TableArea::new(frame, &price_to_y, &tl, layout.candle_w, kline, layout.pal);
            table_layout = Some(tl);
            let table_width = area.width();
            let half_width = table_width / 2.0;
            let cell_border = 1.0;
            let grid_color = layout.pal.background.weakest.text.scale_alpha(0.32);
            for (price, group) in &footprint.trades {
                let buy_qty = group.buy_qty.to_f64();
                let sell_qty = group.sell_qty.to_f64();
                let y = price_to_y(*price);
                let row_top = y - (layout.cell_h / 2.0);

                frame.fill_rectangle(
                    Point::new(area.table_left, row_top),
                    Size::new(half_width, layout.cell_h),
                    ImbalanceSide::Sell.volume_bg_color(sell_qty, max_cluster_qty, layout.pal),
                );
                frame.fill_rectangle(
                    Point::new(area.table_left + half_width, row_top),
                    Size::new(half_width, layout.cell_h),
                    ImbalanceSide::Buy.volume_bg_color(buy_qty, max_cluster_qty, layout.pal),
                );
                let sell_text_color = ImbalanceSide::Sell.volume_text_color(
                    sell_qty,
                    max_cluster_qty,
                    text_color,
                    layout.pal,
                );
                let buy_text_color = ImbalanceSide::Buy.volume_text_color(
                    buy_qty,
                    max_cluster_qty,
                    text_color,
                    layout.pal,
                );

                if let Some((threshold, color_scale, ignore_zeros)) = imbalance {
                    if let Some(alpha) = ImbalanceSide::Sell.color_alpha(
                        footprint,
                        *price,
                        sell_qty,
                        step,
                        threshold,
                        color_scale,
                        ignore_zeros,
                    ) {
                        ImbalanceSide::Sell.draw_table_marker(
                            frame,
                            layout.pal,
                            alpha,
                            sell_qty,
                            max_cluster_qty,
                            Rectangle::new(
                                Point::new(area.table_left, row_top),
                                Size::new(half_width, layout.cell_h),
                            ),
                        );
                    }

                    if let Some(alpha) = ImbalanceSide::Buy.color_alpha(
                        footprint,
                        *price,
                        buy_qty,
                        step,
                        threshold,
                        color_scale,
                        ignore_zeros,
                    ) {
                        ImbalanceSide::Buy.draw_table_marker(
                            frame,
                            layout.pal,
                            alpha,
                            buy_qty,
                            max_cluster_qty,
                            Rectangle::new(
                                Point::new(area.table_left + half_width, row_top),
                                Size::new(half_width, layout.cell_h),
                            ),
                        );
                    }
                }

                frame.fill_rectangle(
                    Point::new(area.table_left, row_top),
                    Size::new(table_width, cell_border),
                    grid_color,
                );
                frame.fill_rectangle(
                    Point::new(area.table_left, row_top + layout.cell_h - cell_border),
                    Size::new(table_width, cell_border),
                    grid_color,
                );
                frame.fill_rectangle(
                    Point::new(area.table_left, row_top),
                    Size::new(cell_border, layout.cell_h),
                    grid_color,
                );
                frame.fill_rectangle(
                    Point::new(area.table_left + half_width, row_top),
                    Size::new(cell_border, layout.cell_h),
                    grid_color,
                );
                frame.fill_rectangle(
                    Point::new(area.table_right - cell_border, row_top),
                    Size::new(cell_border, layout.cell_h),
                    grid_color,
                );

                if show_text {
                    draw_cluster_text(
                        frame,
                        &abbr_large_numbers(sell_qty),
                        Point::new(area.table_left + half_width - 3.0, y),
                        text_size,
                        sell_text_color,
                        Alignment::End,
                        Alignment::Center,
                    );
                    draw_cluster_text(
                        frame,
                        &abbr_large_numbers(buy_qty),
                        Point::new(area.table_left + half_width + 3.0, y),
                        text_size,
                        buy_text_color,
                        Alignment::Start,
                        Alignment::Center,
                    );
                }
            }
        }
        ClusterKind::BidAsk => {
            let area = BidAskArea::new(
                x_position,
                content_left,
                content_right,
                layout.candle_w,
                layout.gaps,
            );

            let bar_alpha = if show_text { 0.25 } else { 1.0 };

            let imb_marker_reserve = if imbalance.is_some() {
                ((area.imb_marker_width - 1.0) / 2.0).max(1.0)
            } else {
                0.0
            };

            let right_max_x =
                area.bid_area_right - imb_marker_reserve - (2.0 * layout.gaps.marker_to_bars);
            let right_area_width = (right_max_x - area.bid_area_left).max(0.0);

            let left_min_x =
                area.ask_area_left + imb_marker_reserve + (2.0 * layout.gaps.marker_to_bars);
            let left_area_width = (area.ask_area_right - left_min_x).max(0.0);

            for (price, group) in &footprint.trades {
                let buy_qty = group.buy_qty.to_f64();
                let sell_qty = group.sell_qty.to_f64();
                let y = price_to_y(*price);

                if buy_qty > 0.0 && right_area_width > 0.0 {
                    if show_text {
                        draw_cluster_text(
                            frame,
                            &abbr_large_numbers(buy_qty),
                            Point::new(area.bid_area_left, y),
                            text_size,
                            text_color,
                            Alignment::Start,
                            Alignment::Center,
                        );
                    }

                    let bar_width = (buy_qty / max_cluster_qty) as f32 * right_area_width;
                    if bar_width > 0.0 {
                        frame.fill_rectangle(
                            Point::new(area.bid_area_left, y - (layout.cell_h / 2.0)),
                            Size::new(bar_width, layout.cell_h),
                            layout.pal.success.base.color.scale_alpha(bar_alpha),
                        );
                    }
                }
                if sell_qty > 0.0 && left_area_width > 0.0 {
                    if show_text {
                        draw_cluster_text(
                            frame,
                            &abbr_large_numbers(sell_qty),
                            Point::new(area.ask_area_right, y),
                            text_size,
                            text_color,
                            Alignment::End,
                            Alignment::Center,
                        );
                    }

                    let bar_width = (sell_qty / max_cluster_qty) as f32 * left_area_width;
                    if bar_width > 0.0 {
                        frame.fill_rectangle(
                            Point::new(area.ask_area_right, y - (layout.cell_h / 2.0)),
                            Size::new(-bar_width, layout.cell_h),
                            layout.pal.danger.base.color.scale_alpha(bar_alpha),
                        );
                    }
                }

                if let Some((threshold, color_scale, ignore_zeros)) = imbalance
                    && area.imb_marker_width > 0.0
                {
                    let higher_price = price.add_steps(1, step);

                    let rect_width = ((area.imb_marker_width - 1.0) / 2.0).max(1.0);

                    let buyside_x = area.bid_area_right - rect_width - layout.gaps.marker_to_bars;
                    let sellside_x = area.ask_area_left + layout.gaps.marker_to_bars;

                    draw_imbalance_markers(
                        frame,
                        &price_to_y,
                        footprint,
                        *price,
                        sell_qty,
                        higher_price,
                        threshold,
                        color_scale,
                        ignore_zeros,
                        layout.cell_h,
                        layout.pal,
                        buyside_x,
                        sellside_x,
                        rect_width,
                    );
                }
            }

            draw_footprint_kline(
                frame,
                &price_to_y,
                area.candle_center_x,
                layout.candle_w,
                kline,
                layout.pal,
            );
        }
    }

    if show_summary {
        let Some(summary) = FootprintSummary::from_trades(footprint) else {
            return;
        };

        let summary_layout = FootprintSummaryLayout::new(layout.cell_h, scaling);

        let summary_x = match layout.cluster {
            ClusterKind::Table => {
                let tl = table_layout
                    .as_ref()
                    .expect("TableLayout must be set for Table cluster");
                (tl.table_left + tl.table_right) / 2.0
            }
            _ => x_position,
        };

        let lowest_trade_price = footprint.trades.keys().min();

        let summary_y = match lowest_trade_price {
            Some(p) => price_to_y(*p) + layout.cell_h / 2.0 + summary_layout.gap,
            None => price_to_y(kline.low) + layout.cell_h / 2.0 + summary_layout.gap,
        };

        draw_cluster_text(
            frame,
            &format!("V: {}", abbr_large_numbers(summary.total.to_f64())),
            Point::new(summary_x, summary_y),
            summary_layout.text_size,
            layout.pal.background.weakest.text,
            Alignment::Center,
            Alignment::Start,
        );

        let delta_color = if summary.delta >= Qty::ZERO {
            layout.pal.success.base.color
        } else {
            layout.pal.danger.base.color
        };

        draw_cluster_text(
            frame,
            &format!("Δ: {}", abbr_large_numbers(summary.delta.to_f64())),
            Point::new(
                summary_x,
                summary_y + summary_layout.text_size + summary_layout.line_gap,
            ),
            summary_layout.text_size,
            delta_color,
            Alignment::Center,
            Alignment::Start,
        );
    }
}

fn draw_imbalance_markers(
    frame: &mut canvas::Frame,
    price_to_y: &impl Fn(Price) -> f32,
    footprint: &KlineTrades,
    price: Price,
    sell_qty: f64,
    higher_price: Price,
    threshold: usize,
    color_scale: Option<usize>,
    ignore_zeros: bool,
    cell_height: f32,
    palette: &Extended,
    buyside_x: f32,
    sellside_x: f32,
    rect_width: f32,
) {
    if ignore_zeros && sell_qty <= 0.0 {
        return;
    }

    if let Some(group) = footprint.trades.get(&higher_price) {
        let diagonal_buy_qty = group.buy_qty.to_f64();

        if ignore_zeros && diagonal_buy_qty <= 0.0 {
            return;
        }

        let rect_height = cell_height / 2.0;

        let alpha_from_ratio = |ratio: f64| -> f32 {
            if let Some(scale) = color_scale {
                let divisor = (scale as f64 / 10.0) - 1.0;
                (0.2 + 0.8 * ((ratio - 1.0) / divisor).min(1.0)).min(1.0) as f32
            } else {
                1.0
            }
        };

        if diagonal_buy_qty >= sell_qty {
            let required_qty = sell_qty * (100 + threshold) as f64 / 100.0;
            if diagonal_buy_qty > required_qty {
                let ratio = diagonal_buy_qty / required_qty;
                let alpha = alpha_from_ratio(ratio);

                let y = price_to_y(higher_price);
                frame.fill_rectangle(
                    Point::new(buyside_x, y - (rect_height / 2.0)),
                    Size::new(rect_width, rect_height),
                    ImbalanceSide::Buy.marker_bg_color(palette, alpha),
                );
            }
        } else {
            let required_qty = diagonal_buy_qty * (100 + threshold) as f64 / 100.0;
            if sell_qty > required_qty {
                let ratio = sell_qty / required_qty;
                let alpha = alpha_from_ratio(ratio);

                let y = price_to_y(price);
                frame.fill_rectangle(
                    Point::new(sellside_x, y - (rect_height / 2.0)),
                    Size::new(rect_width, rect_height),
                    ImbalanceSide::Sell.marker_bg_color(palette, alpha),
                );
            }
        }
    }
}

fn draw_cluster_text(
    frame: &mut canvas::Frame,
    text: &str,
    position: Point,
    text_size: f32,
    color: iced::Color,
    align_x: Alignment,
    align_y: Alignment,
) {
    frame.fill_text(canvas::Text {
        content: text.to_string(),
        position,
        size: iced::Pixels(text_size),
        color,
        align_x: align_x.into(),
        align_y: align_y.into(),
        font: style::AZERET_MONO,
        ..canvas::Text::default()
    });
}

fn draw_crosshair_tooltip(
    data: &PlotData<KlineDataPoint>,
    ticker_info: &TickerInfo,
    frame: &mut canvas::Frame,
    palette: &Extended,
    basis: Basis,
    at_interval: Option<u64>,
    visible_range: (u64, u64),
) {
    let (visible_earliest, visible_latest) = visible_range;

    let kline_opt = match (data, at_interval) {
        (PlotData::TimeBased(timeseries), Some(at_interval)) => {
            let in_visible = at_interval >= visible_earliest && at_interval <= visible_latest;

            timeseries
                .datapoints
                .get(&UnixMs::new(at_interval))
                .map(|dp| &dp.kline)
                .or_else(|| {
                    if in_visible {
                        let search_end = at_interval.min(visible_latest);
                        timeseries
                            .datapoints
                            .range(UnixMs::new(visible_earliest)..=UnixMs::new(search_end))
                            .next_back()
                            .map(|(_, dp)| &dp.kline)
                    } else {
                        None
                    }
                })
                .or_else(|| {
                    let right_of_latest = match basis {
                        Basis::Time(_) => at_interval > visible_latest,
                        Basis::Tick(_) => at_interval < visible_earliest,
                    };

                    if right_of_latest {
                        timeseries
                            .datapoints
                            .range(UnixMs::new(visible_earliest)..=UnixMs::new(visible_latest))
                            .next_back()
                            .map(|(_, dp)| &dp.kline)
                    } else {
                        None
                    }
                })
                .or_else(|| {
                    let (last_time, dp) = timeseries.datapoints.last_key_value()?;
                    (at_interval > last_time.as_u64()).then_some(&dp.kline)
                })
        }
        (PlotData::TickBased(tick_aggr), Some(at_interval)) => {
            let kline_at = |interval: u64| {
                let index = (interval / u64::from(tick_aggr.interval.0)) as usize;
                (index < tick_aggr.datapoints.len())
                    .then(|| &tick_aggr.datapoints[tick_aggr.datapoints.len() - 1 - index].kline)
            };

            let in_visible = at_interval >= visible_earliest && at_interval <= visible_latest;

            kline_at(at_interval).or_else(|| {
                let right_of_latest = match basis {
                    Basis::Time(_) => at_interval > visible_latest,
                    Basis::Tick(_) => at_interval < visible_earliest,
                };

                if in_visible || right_of_latest {
                    kline_at(visible_earliest)
                } else {
                    None
                }
            })
        }
        (PlotData::TimeBased(timeseries), None) => timeseries
            .datapoints
            .last_key_value()
            .map(|(_, dp)| &dp.kline),
        (PlotData::TickBased(tick_aggr), None) => tick_aggr.datapoints.last().map(|dp| &dp.kline),
    };

    if let Some(kline) = kline_opt {
        let change_pct = ((kline.close - kline.open) / kline.open * 100.0) as f32;
        let change_color = if change_pct >= 0.0 {
            palette.success.base.color
        } else {
            palette.danger.base.color
        };

        let base_color = palette.background.base.text;
        let precision = ticker_info.min_ticksize;

        let segments = [
            ("O", base_color, false),
            (&kline.open.to_string(precision), change_color, true),
            ("H", base_color, false),
            (&kline.high.to_string(precision), change_color, true),
            ("L", base_color, false),
            (&kline.low.to_string(precision), change_color, true),
            ("C", base_color, false),
            (&kline.close.to_string(precision), change_color, true),
            (&format!("{change_pct:+.2}%"), change_color, true),
        ];

        let total_width: f32 = segments
            .iter()
            .map(|(s, _, _)| s.len() as f32 * (TEXT_SIZE * 0.8))
            .sum();

        let position = Point::new(8.0, 8.0);

        let tooltip_rect = Rectangle {
            x: position.x,
            y: position.y,
            width: total_width,
            height: 16.0,
        };

        frame.fill_rectangle(
            tooltip_rect.position(),
            tooltip_rect.size(),
            palette.background.weakest.color.scale_alpha(0.9),
        );

        let mut x = position.x;
        for (text, seg_color, is_value) in segments {
            frame.fill_text(canvas::Text {
                content: text.to_string(),
                position: Point::new(x, position.y),
                size: iced::Pixels(crate::style::text_size::BODY),
                color: seg_color,
                font: style::AZERET_MONO,
                ..canvas::Text::default()
            });
            x += text.len() as f32 * 8.0;
            x += if is_value { 6.0 } else { 2.0 };
        }
    }
}

#[derive(Clone, Copy)]
enum ImbalanceSide {
    Buy,
    Sell,
}

impl ImbalanceSide {
    fn volume_bg_color(self, qty: f64, max_qty: f64, palette: &Extended) -> Color {
        const MIN_ALPHA: f32 = 0.04;

        let intensity = if max_qty > 0.0 {
            (qty / max_qty).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        let alpha = MIN_ALPHA + intensity * (1.0 - MIN_ALPHA);

        match self {
            ImbalanceSide::Buy => palette.success.base.color.scale_alpha(alpha),
            ImbalanceSide::Sell => palette.danger.base.color.scale_alpha(alpha),
        }
    }

    fn volume_text_color(
        self,
        qty: f64,
        max_qty: f64,
        default_color: Color,
        palette: &Extended,
    ) -> Color {
        let cell_color = self.volume_bg_color(qty, max_qty, palette);
        let cell_background = composite_color(cell_color, palette.background.base.color);
        let inverted_color = palette.background.base.color;

        if contrast_ratio(cell_background, inverted_color)
            > contrast_ratio(cell_background, default_color)
        {
            inverted_color
        } else {
            default_color
        }
    }

    fn marker_bg_color(self, palette: &Extended, alpha: f32) -> Color {
        let accent = match self {
            ImbalanceSide::Buy => palette.success.strong.color,
            ImbalanceSide::Sell => palette.danger.strong.color,
        };
        let alpha = alpha.clamp(0.0, 1.0);

        if palette.is_dark {
            let tint = 0.28 + (alpha * 0.32);
            mix_color(accent, palette.background.strongest.color, tint)
        } else {
            let tint = 0.18 + (alpha * 0.24);
            mix_color(accent, palette.background.weak.color, tint)
        }
    }

    fn color_alpha(
        self,
        footprint: &KlineTrades,
        price: Price,
        qty: f64,
        step: PriceStep,
        threshold: usize,
        color_scale: Option<usize>,
        ignore_zeros: bool,
    ) -> Option<f32> {
        let diagonal_price = match self {
            ImbalanceSide::Buy => price.add_steps(-1, step),
            ImbalanceSide::Sell => price.add_steps(1, step),
        };
        let diagonal_qty = footprint
            .trades
            .get(&diagonal_price)
            .map(|group| match self {
                ImbalanceSide::Buy => group.sell_qty.to_f64(),
                ImbalanceSide::Sell => group.buy_qty.to_f64(),
            })
            .unwrap_or_default();

        if ignore_zeros && (qty <= 0.0 || diagonal_qty <= 0.0) {
            return None;
        }

        let required_qty = diagonal_qty * (100 + threshold) as f64 / 100.0;

        if required_qty <= 0.0 {
            return (qty > 0.0).then_some(1.0);
        }

        if qty <= required_qty {
            return None;
        }

        let ratio = qty / required_qty;
        Some(if let Some(scale) = color_scale {
            let divisor = (scale as f64 / 10.0) - 1.0;
            (0.2 + 0.8 * ((ratio - 1.0) / divisor).min(1.0)).min(1.0) as f32
        } else {
            1.0
        })
    }

    fn draw_table_marker(
        self,
        frame: &mut canvas::Frame,
        palette: &Extended,
        alpha: f32,
        qty: f64,
        max_qty: f64,
        cell: Rectangle,
    ) {
        if cell.height <= 0.0 {
            return;
        }

        let bar_width = 2.5;
        let gap = 1.5;

        let volume_intensity = if max_qty > 0.0 {
            (qty / max_qty).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        let imbalance_strength = alpha.clamp(0.0, 1.0);
        let marker_alpha = 0.38 + (volume_intensity * 0.24) + (imbalance_strength * 0.38);
        let marker_alpha = marker_alpha.clamp(0.38, 1.0);

        let color = palette.warning.strong.color.scale_alpha(marker_alpha);

        let (x, bar_w) = match self {
            ImbalanceSide::Sell => (cell.x - bar_width - gap, bar_width),
            ImbalanceSide::Buy => (cell.x + cell.width + gap, bar_width),
        };

        frame.fill_rectangle(Point::new(x, cell.y), Size::new(bar_w, cell.height), color);
    }
}

#[derive(Clone, Copy, Debug)]
struct ContentGaps {
    /// Space between imb. markers candle body
    marker_to_candle: f32,
    /// Space between candle body and clusters
    candle_to_cluster: f32,
    /// Inner space reserved between imb. markers and clusters (used for BidAsk)
    marker_to_bars: f32,
}

impl ContentGaps {
    fn from_view(candle_width: f32, scaling: f32) -> Self {
        let px = |p: f32| p / scaling;
        let base = (candle_width * 0.2).max(px(2.0));
        Self {
            marker_to_candle: base,
            candle_to_cluster: base,
            marker_to_bars: px(2.0),
        }
    }
}

/// Layout and style parameters shared across footprint cell draw functions.
struct FootprintCellLayout<'a> {
    cell_w: f32,
    cell_h: f32,
    candle_w: f32,
    pal: &'a Extended,
    cluster: ClusterKind,
    gaps: ContentGaps,
}

impl FootprintCellLayout<'_> {
    /// Compute the text size for cluster labels based on on-screen cell dimensions.
    fn text_size(&self, scaling: f32) -> f32 {
        let cell_height_unscaled = self.cell_h * scaling;
        let cell_width_unscaled = self.cell_w * scaling;
        let from_height = cell_height_unscaled.round().min(16.0) - 3.0;
        let from_width = (cell_width_unscaled * 0.1).round().min(16.0) - 3.0;
        from_height.min(from_width)
    }

    /// Whether cluster text labels should be drawn given current zoom level.
    fn should_show_text(&self, scaling: f32) -> bool {
        const THRESHOLD: f32 = 8.0;
        self.cell_h * scaling > THRESHOLD
            && self.cell_w * scaling > self.cluster.min_footprint_width()
    }
}

struct ProfileArea {
    imb_marker_left: f32,
    imb_marker_width: f32,
    bars_left: f32,
    bars_width: f32,
    candle_center_x: f32,
}

impl ProfileArea {
    fn new(
        content_left: f32,
        content_right: f32,
        candle_width: f32,
        gaps: ContentGaps,
        has_imbalance: bool,
    ) -> Self {
        let candle_lane_left = if has_imbalance {
            content_left + candle_width + gaps.marker_to_candle
        } else {
            content_left
        };
        let candle_lane_width = candle_width * 0.25;

        let bars_left = candle_lane_left + candle_lane_width + gaps.candle_to_cluster;
        let bars_width = (content_right - bars_left).max(0.0);

        let candle_center_x = candle_lane_left + (candle_lane_width / 2.0);

        Self {
            imb_marker_left: content_left,
            imb_marker_width: if has_imbalance { candle_width } else { 0.0 },
            bars_left,
            bars_width,
            candle_center_x,
        }
    }
}

struct BidAskArea {
    bid_area_left: f32,
    bid_area_right: f32,
    ask_area_left: f32,
    ask_area_right: f32,
    candle_center_x: f32,
    imb_marker_width: f32,
}

impl BidAskArea {
    fn new(
        x_position: f32,
        content_left: f32,
        content_right: f32,
        candle_width: f32,
        spacing: ContentGaps,
    ) -> Self {
        let candle_body_width = candle_width * 0.25;

        let candle_left = x_position - (candle_body_width / 2.0);
        let candle_right = x_position + (candle_body_width / 2.0);

        let ask_area_right = candle_left - spacing.candle_to_cluster;
        let bid_area_left = candle_right + spacing.candle_to_cluster;

        Self {
            bid_area_left,
            bid_area_right: content_right,
            ask_area_left: content_left,
            ask_area_right,
            candle_center_x: x_position,
            imb_marker_width: candle_width,
        }
    }
}

struct TableLayout {
    table_left: f32,
    table_right: f32,
    candle_center_x: f32,
}

impl TableLayout {
    fn new(
        content_left: f32,
        content_right: f32,
        candle_width: f32,
        spacing: ContentGaps,
        has_imbalance: bool,
    ) -> Self {
        let (candle_center_x, table_left) = if has_imbalance {
            let ccx = content_left + candle_width / 2.0;
            let tl = (content_left + candle_width + spacing.candle_to_cluster).min(content_right);
            (ccx, tl)
        } else {
            let thin_candle = candle_width * 0.25;
            let ccx = content_left + thin_candle / 2.0;
            let tl = (content_left + thin_candle + spacing.candle_to_cluster).min(content_right);
            (ccx, tl)
        };

        Self {
            table_left,
            table_right: content_right,
            candle_center_x,
        }
    }
}

struct TableArea {
    table_left: f32,
    table_right: f32,
}

impl TableArea {
    fn new(
        frame: &mut canvas::Frame,
        price_to_y: &impl Fn(Price) -> f32,
        table_layout: &TableLayout,
        candle_width: f32,
        kline: &Kline,
        palette: &Extended,
    ) -> Self {
        draw_footprint_kline(
            frame,
            price_to_y,
            table_layout.candle_center_x,
            candle_width,
            kline,
            palette,
        );

        Self {
            table_left: table_layout.table_left,
            table_right: table_layout.table_right,
        }
    }

    fn width(&self) -> f32 {
        (self.table_right - self.table_left).max(0.0)
    }
}

struct FootprintSummaryLayout {
    text_size: f32,
    gap: f32,
    line_gap: f32,
}

impl FootprintSummaryLayout {
    /// Computes the text size, gap, and line gap for footprint summary text.
    /// Scales the font down when the on-screen cell height is too small.
    fn new(cell_height: f32, scaling: f32) -> FootprintSummaryLayout {
        const MIN_SCREEN_CELL_H_PX: f32 = 6.0;
        const MIN_TEXT_SIZE_PX: f32 = 3.0;
        const SUMMARY_GAP_PX: f32 = 8.0;
        const SUMMARY_LINE_GAP_PX: f32 = 2.0;

        let max_text_size = style::text_size::TINY;
        let screen_cell_h = cell_height * scaling;
        let text_size = if screen_cell_h < MIN_SCREEN_CELL_H_PX {
            (max_text_size * (screen_cell_h / MIN_SCREEN_CELL_H_PX)).max(MIN_TEXT_SIZE_PX)
        } else {
            max_text_size
        };

        let gap = SUMMARY_GAP_PX / scaling;
        let line_gap = SUMMARY_LINE_GAP_PX / scaling;

        FootprintSummaryLayout {
            text_size,
            gap,
            line_gap,
        }
    }

    fn padding(cell_height: f32, scaling: f32, tick_size: f32) -> f32 {
        if cell_height <= f32::EPSILON {
            return 0.0;
        }

        let layout = Self::new(cell_height, scaling);

        let first_line_bottom = layout.gap + layout.text_size;
        let second_line_bottom = first_line_bottom + layout.line_gap + layout.text_size;

        let summary_ticks = second_line_bottom / cell_height;
        summary_ticks * tick_size
    }
}

#[inline]
fn price_padding_from_pixels(cell_height: f32, tick_size: f32) -> f32 {
    const OUTER_BOUND_PADDING_PX: f32 = 4.0;

    if cell_height <= f32::EPSILON {
        return 0.0;
    }

    (OUTER_BOUND_PADDING_PX / cell_height) * tick_size
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_trade(id: u64, time: u64, qty: f64) -> Trade {
        Trade {
            id: Some(id),
            time: UnixMs::new(time),
            is_sell: false,
            price: Price::from_f64(100.0),
            qty: Qty::from_f64(qty),
        }
    }

    #[test]
    fn retained_trade_ids_prevent_bubble_reaggregation() {
        let trade = test_trade(42, 61_000, 10.0);
        assert!(deduplicate_incoming_trades(&[trade], &[trade], "test", None).is_empty());
    }

    #[test]
    fn non_positive_trade_price_cannot_poison_footprint_autoscale() {
        let valid = test_trade(1, 61_000, 1.0);
        let mut zero = test_trade(2, 61_001, 1.0);
        zero.price = Price::from_f64(0.0);

        let filtered = deduplicate_incoming_trades(&[], &[zero, valid], "test", None);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, valid.id);
        assert_eq!(filtered[0].price, valid.price);
    }

    #[test]
    fn historical_aggtrades_do_not_double_a_live_raw_bucket() {
        let live_bucket = UnixMs::new(60_000);
        let live_buckets = FxHashSet::from_iter([live_bucket]);
        let overlapping = test_trade(10, 61_000, 5.0);
        let historical_only = test_trade(11, 121_000, 7.0);

        let (filtered, discarded) = exclude_historical_overlap_with_live(
            vec![overlapping, historical_only],
            &live_buckets,
            exchange::Timeframe::M1,
        );

        assert_eq!(discarded, 1);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, historical_only.id);
    }

    #[test]
    fn older_backfill_cannot_evict_the_latest_raw_trades() {
        let mut retained = (0..MAX_RAW_TRADES)
            .map(|index| test_trade(index as u64, 100_000 + index as u64, 1.0))
            .collect::<Vec<_>>();
        let previous_len = retained.len();
        retained.push(test_trade(999_999, 1_000, 1.0));

        let removed = retain_latest_raw_trades(&mut retained, previous_len);

        assert_eq!(removed, 1);
        assert_eq!(retained.len(), MAX_RAW_TRADES);
        assert_eq!(
            retained.first().map(|trade| trade.time),
            Some(UnixMs::new(100_000))
        );
        assert_eq!(
            retained.last().map(|trade| trade.time),
            Some(UnixMs::new(100_000 + MAX_RAW_TRADES as u64 - 1))
        );
    }

    #[test]
    fn evicted_trade_ids_must_still_prevent_bubble_reaggregation() {
        let previously_aggregated = test_trade(42, 61_000, 10.0);
        let retained_newer_trade = test_trade(99, 120_000, 1.0);
        let price_step = PriceStep {
            units: Price::from_f64(0.1).units,
        };
        let mut candle = KlineDataPoint {
            kline: Kline {
                time: UnixMs::new(60_000),
                open: previously_aggregated.price,
                high: previously_aggregated.price,
                low: previously_aggregated.price,
                close: previously_aggregated.price,
                volume: exchange::Volume::empty_buy_sell(),
            },
            footprint: KlineTrades::new(),
            bubble_summary: BubbleVolumeSummary::default(),
            trade_coverage: data::chart::kline::TradeCoverage::Unknown,
            trade_sequence: Vec::new(),
            trade_ids: Default::default(),
        };
        candle.add_trade(&previously_aggregated, price_step);

        // The candle bucket still contains trade 42, but raw_trades no longer
        // does after retention pruning. Re-fetching an overlapping range must
        // not let trade 42 be aggregated into the bucket a second time.
        let redelivered = deduplicate_incoming_trades(
            &[retained_newer_trade],
            &[previously_aggregated],
            "test",
            None,
        );
        for trade in redelivered {
            candle.add_trade(&trade, price_step);
        }

        let total_qty = candle
            .footprint
            .trades
            .values()
            .map(|group| group.total_qty())
            .fold(Qty::ZERO, |total, qty| total + qty);
        assert_eq!(total_qty.to_f64(), 10.0);
    }

    #[test]
    fn latest_uncovered_range_starts_from_the_newest_gap() {
        let covered = [
            (UnixMs::new(120), UnixMs::new(140)),
            (UnixMs::new(160), UnixMs::new(180)),
        ];

        assert_eq!(
            subtract_covered_ranges_latest(&covered, UnixMs::new(100), UnixMs::new(200), "TEST",),
            Some((UnixMs::new(180), UnixMs::new(200)))
        );
    }

    #[test]
    fn latest_uncovered_range_moves_back_after_the_tail_is_covered() {
        let covered = [
            (UnixMs::new(120), UnixMs::new(140)),
            (UnixMs::new(160), UnixMs::new(200)),
        ];

        assert_eq!(
            subtract_covered_ranges_latest(&covered, UnixMs::new(100), UnixMs::new(200), "TEST",),
            Some((UnixMs::new(140), UnixMs::new(160)))
        );
    }

    #[test]
    fn latest_uncovered_range_returns_none_when_fully_covered() {
        assert_eq!(
            subtract_covered_ranges_latest(
                &[(UnixMs::new(90), UnixMs::new(210))],
                UnixMs::new(100),
                UnixMs::new(200),
                "TEST",
            ),
            None
        );
    }

    #[test]
    fn vwap_daily_always_starts_at_current_session_open() {
        let day = 86_400_000;
        let target_to = UnixMs::new(2 * day + 12 * 60 * 60_000);
        assert_eq!(
            vwap_required_from(target_to, UnixMs::new(2 * day + 10 * 60 * 60_000), day),
            UnixMs::new(2 * day)
        );
    }

    #[test]
    fn vwap_covers_visible_previous_session_from_its_open() {
        let day = 86_400_000;
        assert_eq!(
            vwap_required_from(
                UnixMs::new(2 * day + 12 * 60 * 60_000),
                UnixMs::new(day + 18 * 60 * 60_000),
                day,
            ),
            UnixMs::new(day)
        );
    }

    #[test]
    fn bubble_window_uses_latest_candle_end_and_fixed_history() {
        let now = chrono::Utc
            .with_ymd_and_hms(2026, 1, 15, 12, 0, 0)
            .single()
            .unwrap();
        let now = UnixMs::new(now.timestamp_millis() as u64);
        let config = VolumeBubbleConfig::default();
        let range = volume_bubble_effective_range(now.saturating_sub(30_000), 60_000, now, &config)
            .unwrap();
        assert_eq!(range.1, now);
        assert_eq!(range.0, now.saturating_sub(30 * 60_000));
    }

    #[test]
    fn short_live_trade_tail_does_not_starve_historical_gap() {
        let covered = [(UnixMs::new(100), UnixMs::new(900))];
        assert_eq!(
            select_trade_fetch_gap(&covered, UnixMs::new(0), UnixMs::new(1_000)),
            Some((UnixMs::new(0), UnixMs::new(100)))
        );
    }

    #[test]
    fn grown_live_trade_tail_is_refreshed_before_history() {
        let covered = [(UnixMs::new(100), UnixMs::new(61_000))];
        assert_eq!(
            select_trade_fetch_gap(&covered, UnixMs::new(0), UnixMs::new(122_000)),
            Some((UnixMs::new(61_000), UnixMs::new(122_000)))
        );
    }

    #[test]
    fn historical_trade_fetch_does_not_chase_open_candle() {
        let candle_open = UnixMs::new(600_000);
        assert_eq!(
            historical_trade_target_to(candle_open, 60_000, UnixMs::new(630_000)),
            candle_open
        );
        assert_eq!(
            historical_trade_target_to(candle_open, 60_000, UnixMs::new(660_000)),
            UnixMs::new(660_000)
        );
    }

    fn rendered_test_bubble(id: u64, x: f32, y: f32, importance: f32) -> RenderedVolumeBubble {
        let qty = Qty::from_f64(10.0 + f64::from(importance));
        RenderedVolumeBubble {
            cluster: VolumeBubbleCluster {
                id,
                candle_time: UnixMs::new(60_000),
                first_time: UnixMs::new(61_000 + id),
                last_time: UnixMs::new(61_100 + id),
                weighted_time: UnixMs::new(61_050 + id),
                vwap_price: Price::from_f64(100.0),
                total_qty: qty,
                buy_qty: qty,
                sell_qty: Qty::ZERO,
                delta_qty: qty,
                trade_count: 2,
                largest_trade_qty: Qty::from_f64(7.0),
                percentile_rank: 98.7,
                importance_score: importance,
            },
            center: Point::new(x, y),
            original_center: Point::new(x, y),
            radius_px: 8.0,
            fill_color: Color::from_rgb(0.2, 0.8, 0.3),
            border_color: Color::from_rgb(0.2, 0.8, 0.3),
            fill_alpha: 0.12,
            border_alpha: 0.9,
            label: Some("10".into()),
            age_factor: 1.0,
            price_response: BubblePriceResponse::Neutral,
        }
    }

    #[test]
    fn collision_layout_is_horizontal_bounded_and_deterministic() {
        let config = VolumeBubbleConfig::default();
        let original = vec![
            rendered_test_bubble(1, 10.0, 20.0, 2.0),
            rendered_test_bubble(2, 10.0, 20.0, 1.0),
        ];
        let mut first = original.clone();
        let mut second = original;
        collision_layout(&mut first, 30.0, 1.0, &config);
        collision_layout(&mut second, 30.0, 1.0, &config);
        assert_eq!(first.len(), second.len());
        assert_eq!(
            first
                .iter()
                .map(|b| (b.cluster.id, b.center.x, b.center.y))
                .collect::<Vec<_>>(),
            second
                .iter()
                .map(|b| (b.cluster.id, b.center.x, b.center.y))
                .collect::<Vec<_>>()
        );
        assert!(
            first
                .iter()
                .all(|bubble| bubble.center.y == bubble.original_center.y)
        );
        assert!(
            first
                .iter()
                .all(|bubble| (bubble.center.x - bubble.original_center.x).abs() <= 8.0)
        );
    }

    #[test]
    fn collision_excludes_less_important_when_no_space() {
        let config = VolumeBubbleConfig {
            min_center_distance_px: 100.0,
            ..VolumeBubbleConfig::default()
        };
        let mut bubbles = vec![
            rendered_test_bubble(1, 10.0, 20.0, 2.0),
            rendered_test_bubble(2, 10.0, 20.0, 1.0),
        ];
        collision_layout(&mut bubbles, 2.0, 1.0, &config);
        assert_eq!(bubbles.len(), 1);
        assert_eq!(bubbles[0].cluster.id, 1);
    }

    #[test]
    fn hit_test_prefers_visually_dominant_bubble_and_tooltip_has_core_values() {
        let bubbles = vec![
            rendered_test_bubble(1, 0.0, 0.0, 1.0),
            rendered_test_bubble(2, 0.0, 0.0, 2.0),
        ];
        let hit = hit_test_volume_bubbles(
            &bubbles,
            Point::new(50.0, 50.0),
            Vector::new(50.0, 50.0),
            Vector::new(0.0, 0.0),
            1.0,
        )
        .unwrap();
        assert_eq!(hit.cluster.id, 2);
        let lines =
            bubble_tooltip_lines(hit, "BTC", data::chart::kline::BubbleThresholdMode::Hybrid);
        let text = lines.join("\n");
        assert!(text.contains("Total volume"));
        assert!(text.contains("Delta"));
        assert!(text.contains("Trades"));
        assert!(text.contains("98.7 percentile"));
    }

    #[test]
    fn label_budget_keeps_only_most_important() {
        let mut bubbles = vec![
            rendered_test_bubble(1, 0.0, 0.0, 1.0),
            rendered_test_bubble(2, 20.0, 0.0, 3.0),
            rendered_test_bubble(3, 40.0, 0.0, 2.0),
        ];
        apply_label_budget(&mut bubbles, 1);
        assert_eq!(
            bubbles
                .iter()
                .filter(|bubble| bubble.label.is_some())
                .count(),
            1
        );
        assert!(
            bubbles
                .iter()
                .find(|bubble| bubble.cluster.id == 2)
                .unwrap()
                .label
                .is_some()
        );
    }

    #[test]
    fn gex_zone_hit_test_requires_a_specific_band() {
        use data::chart::gex::{
            GexGammaProvenance, GexZone, GexZoneBand, GexZoneFrame, GexZoneSign, GexZoneState,
        };

        let bands: Arc<[GexZoneBand]> = vec![
            GexZoneBand {
                strike: 65_000.0,
                lower_price: 64_900.0,
                upper_price: 65_100.0,
                normalized_strength: 0.25,
                net_gex_1pct: 3.0,
            },
            GexZoneBand {
                strike: 67_000.0,
                lower_price: 66_900.0,
                upper_price: 67_100.0,
                normalized_strength: 1.0,
                net_gex_1pct: 10.0,
            },
        ]
        .into();
        let zone = GexZone {
            id: 1,
            observed_at: UnixMs::new(1),
            lower_price: 64_900.0,
            upper_price: 67_100.0,
            peak_price: 67_000.0,
            net_gex_1pct: 13.0,
            absolute_gex_1pct: 13.0,
            normalized_strength: 1.0,
            persistence_score: 0.5,
            sign: GexZoneSign::Positive,
            dominant_expiry: None,
            gamma_provenance: GexGammaProvenance::Derived,
            state: GexZoneState::Active,
            missing_buckets: 0,
            bands,
        };
        let frame = GexZoneFrame {
            bucket_start: UnixMs::new(1),
            source_spot: 66_000.0,
            zones: vec![zone].into(),
        };

        let (_, band) = gex_zone_band_hit_test(&frame, 65_000.0).expect("65k band hit");
        assert_eq!(band.strike, 65_000.0);
        assert!(gex_zone_band_hit_test(&frame, 66_000.0).is_none());
    }

    fn proxy_test_point(
        observed_at: i64,
    ) -> Arc<exchange::options::gex_monitor::GexProxyHistoryPoint> {
        proxy_test_point_with_levels(
            observed_at,
            Some(105.0),
            None,
            Some(95.0),
            None,
            Some(100.0),
        )
    }

    fn proxy_test_point_with_levels(
        observed_at: i64,
        positive_level_1: Option<f64>,
        positive_level_2: Option<f64>,
        negative_level_1: Option<f64>,
        negative_level_2: Option<f64>,
        flip_level: Option<f64>,
    ) -> Arc<exchange::options::gex_monitor::GexProxyHistoryPoint> {
        Arc::new(exchange::options::gex_monitor::GexProxyHistoryPoint {
            observed_at,
            source_spot: 100.0,
            total_gex: 1.0,
            flip_level,
            call_wall: Some(110.0),
            put_wall: Some(90.0),
            positive_level_1,
            positive_level_2,
            negative_level_1,
            negative_level_2,
        })
    }

    fn deribit_test_snapshot(observed_at: u64) -> Arc<data::chart::gex::GexSnapshot> {
        use data::chart::gex::*;
        Arc::new(GexSnapshot {
            provider: exchange::options::OptionsProvider::Deribit,
            underlying: exchange::options::OptionsUnderlying::Btc,
            model: GexSignModel::CallPutOiProxy,
            expiry_filter: GexExpiryFilter::SevenDays,
            gamma_source: GexGammaSource::BlackScholesDerived,
            gamma_provenance: GexGammaProvenance::Derived,
            source_spot: 100.0,
            observed_at: UnixMs::new(observed_at),
            calculated_at: UnixMs::new(observed_at),
            net_gex_1pct: Some(1.0),
            absolute_gex_1pct: 1.0,
            call_wall: Some(110.0),
            put_wall: Some(90.0),
            gamma_flip: Some(100.0),
            intrinsic_stress: IntrinsicStressMetrics::default(),
            gamma_vega: GammaVegaMetrics::default(),
            strikes: Arc::from([]),
            expiry_strikes: Arc::from([]),
            scenario_curve: Arc::from([GexScenarioPoint {
                price: 100.0,
                net_gex_1pct: 1.0,
                absolute_gex_1pct: 1.0,
            }]),
            scale_p95: 1.0,
        })
    }

    fn proxy_frames(
        points: &[Arc<exchange::options::gex_monitor::GexProxyHistoryPoint>],
        deribit: &[Arc<data::chart::gex::GexSnapshot>],
        interval_ms: u64,
        latest: u64,
    ) -> Vec<data::chart::gex::GexProxyZoneFrame> {
        data::chart::gex::build_gex_proxy_zone_frames(
            points,
            deribit,
            interval_ms,
            UnixMs::new(latest),
        )
    }

    #[test]
    fn proxy_transitions_stop_at_gaps_and_never_connect_roles() {
        let points = vec![
            proxy_test_point_with_levels(0, Some(105.0), None, Some(95.0), None, None),
            proxy_test_point_with_levels(8 * 60_000, Some(106.0), None, Some(94.0), None, None),
        ];
        let frames = proxy_frames(&points, &[], 60_000, 15 * 60_000);
        assert!(gex_proxy_zone_transitions(&frames).is_empty());

        let consecutive = vec![
            proxy_test_point_with_levels(0, Some(105.0), None, Some(95.0), None, None),
            proxy_test_point_with_levels(5 * 60_000, Some(106.0), None, Some(94.0), None, None),
        ];
        let transitions =
            gex_proxy_zone_transitions(&proxy_frames(&consecutive, &[], 5 * 60_000, 10 * 60_000));
        assert_eq!(transitions.len(), 2);
        assert!(
            transitions
                .iter()
                .all(|(_, role, old, new)| { old.role == *role && new.role == *role })
        );
    }

    #[test]
    fn proxy_tooltip_hit_test_is_limited_to_real_band_area() {
        let frames = proxy_frames(&[proxy_test_point(0)], &[], 60_000, 5 * 60_000);
        let zone = &frames[0].zones[0];
        assert!(proxy_zone_hit_test(&frames, UnixMs::new(30_000), zone.center_price,).is_some());
        assert!(
            proxy_zone_hit_test(&frames, UnixMs::new(30_000), zone.upper_price + 0.01,).is_none()
        );
        assert!(
            proxy_zone_hit_test(&frames, UnixMs::new(6 * 60_000), zone.center_price,).is_none()
        );
    }

    #[test]
    fn btc_one_minute_proxy_zones_remain_continuous_at_different_clip_ranges() {
        let frames = proxy_frames(
            &[proxy_test_point(0), proxy_test_point(5 * 60_000)],
            &[],
            60_000,
            10 * 60_000,
        );
        let p1 = frames
            .iter()
            .filter_map(|frame| {
                frame
                    .zones
                    .iter()
                    .find(|zone| zone.role == data::chart::gex::GexProxyZoneRole::PositivePrimary)
                    .map(|_| (frame.bucket_start, frame.bucket_end))
            })
            .collect::<Vec<_>>();
        assert_eq!(p1.len(), 10);
        assert!(p1.windows(2).all(|pair| pair[0].1 == pair[1].0));
        let runs = build_gex_proxy_zone_runs(&frames);
        let p1_run = runs
            .iter()
            .find(|run| run.role == data::chart::gex::GexProxyZoneRole::PositivePrimary)
            .unwrap();
        assert_eq!(
            (p1_run.start.as_u64(), p1_run.end.as_u64()),
            (0, 10 * 60_000)
        );
    }

    #[test]
    fn deribit_current_markers_keep_call_put_and_flip_values() {
        let snapshot = deribit_test_snapshot(0);
        let config = data::chart::gex::GexLevelsConfig::default();
        let markers = gex_deribit_markers(&snapshot, &config);
        assert_eq!(markers[0].1, "CW");
        assert_eq!(markers[0].2, snapshot.call_wall);
        assert_eq!(markers[1].1, "PW");
        assert_eq!(markers[1].2, snapshot.put_wall);
        assert_eq!(markers[2].1, "GF");
        assert_eq!(markers[2].2, snapshot.gamma_flip);
    }

    #[test]
    fn proxy_zones_do_not_modify_kline_fit_to_visible_autoscale() {
        use exchange::adapter::Exchange;

        let ticker_info = TickerInfo::new(
            exchange::Ticker::new("BTCUSDT", Exchange::BinanceLinear),
            1.0,
            0.001,
            None,
        );
        let klines = [
            Kline::new(
                0,
                100.0,
                102.0,
                99.0,
                101.0,
                exchange::Volume::TotalOnly(Qty::from_f64(1.0)),
                ticker_info.min_ticksize,
            ),
            Kline::new(
                60_000,
                101.0,
                104.0,
                100.0,
                103.0,
                exchange::Volume::TotalOnly(Qty::from_f64(1.0)),
                ticker_info.min_ticksize,
            ),
        ];
        let mut chart = KlineChart::new(
            ViewConfig {
                splits: vec![],
                autoscale: Some(Autoscale::FitToVisible),
            },
            Basis::Time(exchange::Timeframe::M1),
            PriceStep::from(ticker_info.min_ticksize),
            &klines,
            vec![],
            &[],
            ticker_info,
            &KlineChartKind::Candles,
            None,
        );
        chart.chart.bounds = Rectangle::new(Point::ORIGIN, Size::new(800.0, 500.0));
        chart.invalidate(None);
        let before = (
            chart.chart.cell_height,
            chart.chart.base_price_y,
            chart.chart.translation.y,
        );

        chart.gex_proxy_history = vec![proxy_test_point_with_levels(
            0,
            Some(1_000_000.0),
            None,
            Some(0.01),
            None,
            Some(500_000.0),
        )];
        chart.invalidate(None);
        let after = (
            chart.chart.cell_height,
            chart.chart.base_price_y,
            chart.chart.translation.y,
        );
        assert_eq!(before, after);
    }

    #[test]
    fn gex_projection_uses_only_existing_future_space() {
        assert_eq!(
            gex_projection_bounds(80.0, 100.0, 100.0),
            Some((80.0, 100.0))
        );
        assert_eq!(gex_projection_bounds(100.0, 100.0, 100.0), None);
    }

    #[test]
    fn gex_profile_and_marker_geometry_are_compact() {
        assert_eq!(gex_profile_width_percent(2.0), 4.0);
        assert_eq!(gex_profile_width_percent(5.0), 5.0);
        assert_eq!(gex_profile_width_percent(12.0), 7.0);
        assert!(gex_screen_width_to_world(30.0, 1.0) <= 30.0);
    }
}
