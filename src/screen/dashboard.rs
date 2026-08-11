pub mod pane;
pub mod panel;
pub mod sidebar;
pub mod tickers_table;

pub use sidebar::Sidebar;

use super::DashboardError;
use crate::{
    chart,
    connector::{
        ResolvedStream,
        client::DataSources,
        fetcher::{self, FetchRange, FetchedData, InfoKind},
    },
    screen::dashboard::tickers_table::TickersTable,
    style,
    widget::toast::Toast,
    window::{self, Window},
    windowing::WindowingMode,
};
use data::{
    UserTimezone,
    layout::{WindowSpec, pane::ContentKind, pane::VisualConfig},
    stream::PersistStreamKind,
};
use exchange::{
    Kline, PushFrequency, StreamPairKind, TickerInfo, Trade, UnixMs,
    adapter::{
        AdapterHandles, MAX_KLINE_STREAMS_PER_STREAM, MAX_TRADE_TICKERS_PER_STREAM, StreamConfig,
        StreamKind, StreamTicksize, UniqueStreams,
    },
    depth::Depth,
};

use iced::{
    Element, Length, Subscription, Task, Vector,
    widget::{
        PaneGrid, center, container,
        pane_grid::{self, Configuration},
    },
};
use std::{collections::HashMap, sync::Arc, time::Instant, vec};

#[derive(Debug, Clone)]
pub enum Message {
    Pane(window::Id, pane::Message),
    ChangePaneStatus(uuid::Uuid, pane::Status),
    FetchCompleted {
        pane_id: uuid::Uuid,
        req_id: Option<uuid::Uuid>,
        fetch: Option<fetcher::FetchRange>,
        trade_outcome: Option<fetcher::TradeFetchOutcome>,
    },
    FetchFailed {
        pane_id: uuid::Uuid,
        error: String,
        req_id: Option<uuid::Uuid>,
        fetch: Option<fetcher::FetchRange>,
    },
    SavePopoutSpecs(HashMap<window::Id, WindowSpec>),
    ErrorOccurred(Option<uuid::Uuid>, DashboardError),
    Notification(Toast),
    DistributeFetchedData {
        layout_id: uuid::Uuid,
        pane_id: uuid::Uuid,
        stream: StreamKind,
        data: FetchedData,
    },
    BackfillFetchUpdate {
        pane_ids: Vec<uuid::Uuid>,
        stream: StreamKind,
        show_activity: bool,
        update: fetcher::FetchUpdate,
    },
    ResolveStreams(uuid::Uuid, Vec<PersistStreamKind>),
    RequestPalette,
}

/// Tracks WS disconnect state for deferred backfill computation.
/// Backfill is not decided at disconnect time because the gap is tiny
/// (last_seen → disconnect ≈ 87ms). Instead, we wait for reconnect and
/// compute the real offline gap (last_seen → reconnect_time).
struct PendingDisconnect {
    disconnected_at: UnixMs,
    /// Per-stream last live timestamp at the time of disconnect.
    stream_last_seen: HashMap<StreamKind, UnixMs>,
}

pub struct Dashboard {
    pub panes: pane_grid::State<pane::State>,
    pub focus: Option<(window::Id, pane_grid::Pane)>,
    pub popout: HashMap<window::Id, (pane_grid::State<pane::State>, WindowSpec)>,
    pub streams: UniqueStreams,
    layout_id: uuid::Uuid,
    /// Last live timestamp received per stream (trades & klines only).
    /// Used for historical gap backfill after WS disconnects.
    last_live_t: HashMap<StreamKind, UnixMs>,
    /// Tracks recently-queued backfill ranges to prevent duplicate fetches
    /// on repeated disconnects. Key is `(stream, from_ms, to_ms)`, value is
    /// the `Instant` when the entry was inserted so stale entries can expire.
    pending_backfills: HashMap<(StreamKind, u64, u64), std::time::Instant>,
    /// Abort handles for active backfill trade tasks. Stored to prevent
    /// the tasks from being aborted when the handle is dropped prematurely.
    backfill_handles: Vec<iced::task::Handle>,
    /// Pending WS disconnect awaiting reconnect to compute real backfill gap.
    pending_disconnect: Option<PendingDisconnect>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StartupLoadStatus {
    pub pane_count: usize,
    pub unresolved_streams: usize,
    pub initializing_panes: usize,
    pub loading_panes: usize,
    pub loading_gex: usize,
}

impl StartupLoadStatus {
    pub fn is_ready(self) -> bool {
        self.unresolved_streams == 0
            && self.initializing_panes == 0
            && self.loading_panes == 0
            && self.loading_gex == 0
    }
}

impl Default for Dashboard {
    fn default() -> Self {
        Self {
            panes: pane_grid::State::with_configuration(Self::default_pane_config()),
            focus: None,
            streams: UniqueStreams::default(),
            popout: HashMap::new(),
            layout_id: uuid::Uuid::new_v4(),
            last_live_t: HashMap::new(),
            pending_backfills: HashMap::new(),
            backfill_handles: Vec::new(),
            pending_disconnect: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Event {
    Notification(Toast),
    DistributeFetchedData {
        layout_id: uuid::Uuid,
        pane_id: uuid::Uuid,
        data: FetchedData,
        stream: StreamKind,
    },
    ResolveStreams {
        pane_id: uuid::Uuid,
        streams: Vec<PersistStreamKind>,
    },
    RequestPalette,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GexReferenceMissingReason {
    NoCompatibleTicker,
    AmbiguousTickers,
}

fn resolve_gex_liquidity_reference(
    underlying: exchange::options::OptionsUnderlying,
    follow_link_group: bool,
    link_group: Option<data::layout::pane::LinkGroup>,
    current: Option<TickerInfo>,
    current_source: Option<pane::GexLiquidityReferenceSource>,
    candidates: &[(Option<data::layout::pane::LinkGroup>, TickerInfo)],
) -> Result<(TickerInfo, pane::GexLiquidityReferenceSource), GexReferenceMissingReason> {
    let compatible = |ticker: TickerInfo| {
        exchange::options::resolve_gex_source(ticker.ticker)
            .is_some_and(|source| source.source_underlying() == underlying)
    };
    let unique = |tickers: Vec<TickerInfo>| {
        let mut compatible_tickers = Vec::new();
        for ticker in tickers.into_iter().filter(|ticker| compatible(*ticker)) {
            if !compatible_tickers.contains(&ticker) {
                compatible_tickers.push(ticker);
            }
        }
        (compatible_tickers.len() == 1).then(|| compatible_tickers[0])
    };

    if follow_link_group
        && let Some(group) = link_group
        && let Some(ticker) = unique(
            candidates
                .iter()
                .filter_map(|(candidate_group, ticker)| {
                    (*candidate_group == Some(group)).then_some(*ticker)
                })
                .collect(),
        )
    {
        return Ok((ticker, pane::GexLiquidityReferenceSource::LinkGroup));
    }
    if let Some(ticker) = current.filter(|ticker| {
        matches!(
            current_source,
            None | Some(pane::GexLiquidityReferenceSource::Persisted)
                | Some(pane::GexLiquidityReferenceSource::Manual)
        ) && compatible(*ticker)
    }) {
        return Ok((
            ticker,
            current_source.unwrap_or(pane::GexLiquidityReferenceSource::Persisted),
        ));
    }
    let all_compatible = candidates
        .iter()
        .map(|(_, ticker)| *ticker)
        .filter(|ticker| compatible(*ticker))
        .collect::<Vec<_>>();
    if let Some(ticker) = unique(all_compatible.clone()) {
        return Ok((ticker, pane::GexLiquidityReferenceSource::SingleCompatible));
    }
    Err(if all_compatible.is_empty() {
        GexReferenceMissingReason::NoCompatibleTicker
    } else {
        GexReferenceMissingReason::AmbiguousTickers
    })
}

impl Dashboard {
    fn reconcile_gex_liquidity_references(&mut self, main_window: window::Id) -> bool {
        let candidates = self
            .iter_all_panes(main_window)
            .filter_map(|(_, _, state)| {
                state.stream_pair().map(|ticker| (state.link_group, ticker))
            })
            .collect::<Vec<_>>();
        let mut changed = false;

        for (_, _, state) in self.iter_all_panes_mut(main_window) {
            let Some((underlying, follow_link_group, current, current_source)) =
                state.gex_liquidity_resolution()
            else {
                continue;
            };
            let resolution = resolve_gex_liquidity_reference(
                underlying,
                follow_link_group,
                state.link_group,
                current,
                current_source,
                &candidates,
            );
            let (reference, source) = resolution
                .as_ref()
                .map_or((None, None), |(ticker, source)| {
                    (Some(*ticker), Some(*source))
                });

            if state.set_gex_liquidity_reference(reference, source) {
                changed = true;
                if let (Some(ticker), Some(source)) = (reference, source) {
                    let (symbol, _) = ticker.ticker.display_symbol_and_type();
                    log::info!(
                        "GEX LiquidityReferenceResolved underlying={} ticker={} exchange={} source={}",
                        underlying,
                        symbol,
                        ticker.exchange(),
                        match source {
                            pane::GexLiquidityReferenceSource::Persisted => "persisted",
                            pane::GexLiquidityReferenceSource::LinkGroup => "link_group",
                            pane::GexLiquidityReferenceSource::SingleCompatible =>
                                "single_compatible",
                            pane::GexLiquidityReferenceSource::Manual => "manual",
                        }
                    );
                }
            }
            if reference.is_none() {
                state.log_gex_liquidity_missing_once(match resolution {
                    Err(GexReferenceMissingReason::NoCompatibleTicker) => "no_compatible_ticker",
                    Err(GexReferenceMissingReason::AmbiguousTickers) => "ambiguous_tickers",
                    Ok(_) => unreachable!(),
                });
            }
        }
        changed
    }

    pub fn gex_consumers(&self) -> Vec<crate::connector::gex::GexConsumer> {
        self.panes
            .iter()
            .map(|(_, state)| state)
            .chain(
                self.popout
                    .values()
                    .flat_map(|(panes, _)| panes.iter().map(|(_, state)| state)),
            )
            .filter_map(|state| match &state.content {
                pane::Content::Gex {
                    underlying,
                    chart: Some(chart),
                    unsupported: false,
                    ..
                } => Some(crate::connector::gex::GexConsumer {
                    source: exchange::options::GexSource::for_chart_underlying(*underlying),
                    expiry_filter: chart.config().expiry_filter,
                }),
                pane::Content::Kline {
                    chart: Some(chart),
                    indicators,
                    ..
                } if indicators.contains(&data::chart::indicator::KlineIndicator::GexLevels) => {
                    let source = state
                        .stream_pair()
                        .and_then(|ticker| exchange::options::resolve_gex_source(ticker.ticker))?;
                    Some(crate::connector::gex::GexConsumer {
                        source,
                        expiry_filter: chart.visual_config().gex_levels().expiry_filter,
                    })
                }
                _ => None,
            })
            .collect()
    }

    pub fn sync_gex(
        &mut self,
        coordinator: &mut crate::connector::gex::GexDataCoordinator,
        now: UnixMs,
    ) {
        let sync =
            |state: &mut pane::State,
             coordinator: &mut crate::connector::gex::GexDataCoordinator| {
                let market = state.stream_pair();
                let market_source =
                    market.and_then(|ticker| exchange::options::resolve_gex_source(ticker.ticker));
                match &mut state.content {
                    pane::Content::Gex {
                        chart,
                        underlying,
                        unsupported,
                        ..
                    } => {
                        if *unsupported {
                            return;
                        }
                        let Some(chart) = chart else {
                            return;
                        };
                        match exchange::options::GexSource::for_chart_underlying(*underlying) {
                            exchange::options::GexSource::Native {
                                provider: exchange::options::OptionsProvider::Deribit,
                                underlying,
                            } => {
                                let snapshot = coordinator.derived(underlying, chart.config(), now);
                                let freshness = coordinator.freshness(underlying, now);
                                let error = coordinator.last_error(underlying).map(Arc::from);
                                chart.set_snapshot(snapshot, freshness, error);
                                chart.set_quantwheel_quota(None);
                                let flow = coordinator.derive_flow(underlying, chart.config(), now);
                                chart.set_derive_flow(flow);
                            }
                            exchange::options::GexSource::Proxy {
                                provider: exchange::options::OptionsProvider::QuantWheel,
                                source_symbol: exchange::options::OptionsUnderlying::Gld,
                                target_symbol,
                                price_mapping: exchange::options::GexPriceMapping::SpotRatio,
                            } => {
                                let target = chart
                                    .liquidity_reference()
                                    .map(|ticker| ticker.ticker.display_symbol_and_type().0)
                                    .unwrap_or_else(|| target_symbol.to_owned());
                                let snapshot = chart.proxy_target_spot().and_then(|target_spot| {
                                    coordinator.mapped_quantwheel(&target, target_spot)
                                });
                                chart.set_snapshot(
                                    snapshot,
                                    coordinator.quantwheel_freshness(now),
                                    coordinator.quantwheel_error().map(Arc::from),
                                );
                                chart.set_quantwheel_quota(coordinator.quantwheel_quota());
                                chart.set_derive_flow(None);
                            }
                            _ => {}
                        }
                    }
                    pane::Content::Kline {
                        chart: Some(chart),
                        indicators,
                        ..
                    } => {
                        if !indicators.contains(&data::chart::indicator::KlineIndicator::GexLevels)
                        {
                            chart.set_gex_overlay_data(
                                None,
                                Vec::new(),
                                data::chart::gex::GexFreshness::Loading,
                                None,
                                Vec::new(),
                                data::chart::gex::GexFreshness::Loading,
                                None,
                                None,
                                data::chart::gex::GexFreshness::Loading,
                            );
                            return;
                        }
                        let Some(source) = market_source else {
                            chart.set_gex_overlay_data(
                                None,
                                Vec::new(),
                                data::chart::gex::GexFreshness::Loading,
                                None,
                                Vec::new(),
                                data::chart::gex::GexFreshness::Loading,
                                None,
                                None,
                                data::chart::gex::GexFreshness::Loading,
                            );
                            return;
                        };
                        let levels = chart.visual_config().gex_levels();
                        let config = data::chart::gex::Config {
                            sign_model: data::chart::gex::GexSignModel::CallPutOiProxy,
                            expiry_filter: levels.expiry_filter,
                            gamma_source: levels.gamma_source,
                            scenario_resolution:
                                data::chart::gex::GexScenarioResolution::Samples512,
                            min_open_interest: levels.minimum_open_interest,
                            min_absolute_gex: levels.minimum_absolute_gex,
                            ..data::chart::gex::Config::default()
                        };
                        let (
                            snapshot,
                            history,
                            freshness,
                            error,
                            proxy_history,
                            proxy_freshness,
                            proxy_error,
                            derive_flow,
                            derive_freshness,
                        ) = match source {
                            exchange::options::GexSource::Native {
                                provider: exchange::options::OptionsProvider::Deribit,
                                underlying,
                            } => {
                                let history = coordinator.history(
                                    underlying,
                                    &config,
                                    levels.history_minutes,
                                    now,
                                );
                                let snapshot = coordinator
                                    .derived(underlying, &config, now)
                                    .or_else(|| history.last().cloned());
                                (
                                    snapshot,
                                    history,
                                    coordinator.freshness(underlying, now),
                                    coordinator.last_error(underlying).map(Arc::from),
                                    coordinator.proxy_history(underlying, now),
                                    coordinator.proxy_freshness(underlying, now),
                                    coordinator.proxy_error(underlying).map(Arc::from),
                                    coordinator.derive_flow(underlying, &config, now),
                                    coordinator.derive_freshness(underlying, now),
                                )
                            }
                            exchange::options::GexSource::Proxy {
                                provider: exchange::options::OptionsProvider::QuantWheel,
                                source_symbol: exchange::options::OptionsUnderlying::Gld,
                                target_symbol,
                                price_mapping: exchange::options::GexPriceMapping::SpotRatio,
                            } => {
                                let Some(target_spot) = chart.current_market_price() else {
                                    chart.set_gex_overlay_data(
                                        None,
                                        Vec::new(),
                                        data::chart::gex::GexFreshness::Loading,
                                        None,
                                        Vec::new(),
                                        data::chart::gex::GexFreshness::Loading,
                                        None,
                                        None,
                                        data::chart::gex::GexFreshness::Loading,
                                    );
                                    return;
                                };
                                let target = market
                                    .map(|ticker| ticker.ticker.display_symbol_and_type().0)
                                    .unwrap_or_else(|| target_symbol.to_owned());
                                let snapshot = coordinator
                                    .mapped_quantwheel(&target, target_spot)
                                    .or_else(|| {
                                        coordinator
                                            .mapped_quantwheel_history(levels.history_minutes, now)
                                            .last()
                                            .cloned()
                                    });
                                let history = coordinator
                                    .mapped_quantwheel_history(levels.history_minutes, now);
                                (
                                    snapshot,
                                    history,
                                    coordinator.quantwheel_freshness(now),
                                    coordinator.quantwheel_error().map(Arc::from),
                                    Vec::new(),
                                    data::chart::gex::GexFreshness::Loading,
                                    None,
                                    None,
                                    data::chart::gex::GexFreshness::Loading,
                                )
                            }
                            _ => return,
                        };
                        chart.set_gex_overlay_data(
                            snapshot,
                            history,
                            freshness,
                            error,
                            proxy_history,
                            proxy_freshness,
                            proxy_error,
                            derive_flow,
                            derive_freshness,
                        );
                    }
                    _ => {}
                }
            };

        for (_, state) in self.panes.iter_mut() {
            sync(state, coordinator);
        }
        for (panes, _) in self.popout.values_mut() {
            for (_, state) in panes.iter_mut() {
                sync(state, coordinator);
            }
        }
    }

    fn default_pane_config() -> Configuration<pane::State> {
        Configuration::Split {
            axis: pane_grid::Axis::Vertical,
            ratio: 0.8,
            a: Box::new(Configuration::Split {
                axis: pane_grid::Axis::Horizontal,
                ratio: 0.4,
                a: Box::new(Configuration::Split {
                    axis: pane_grid::Axis::Vertical,
                    ratio: 0.5,
                    a: Box::new(Configuration::Pane(pane::State::default())),
                    b: Box::new(Configuration::Pane(pane::State::default())),
                }),
                b: Box::new(Configuration::Split {
                    axis: pane_grid::Axis::Vertical,
                    ratio: 0.5,
                    a: Box::new(Configuration::Pane(pane::State::default())),
                    b: Box::new(Configuration::Pane(pane::State::default())),
                }),
            }),
            b: Box::new(Configuration::Pane(pane::State::default())),
        }
    }

    pub fn empty(layout_id: uuid::Uuid) -> Self {
        Self::from_config(
            Configuration::Pane(pane::State::new()),
            Vec::new(),
            layout_id,
        )
    }

    pub fn from_config(
        panes: Configuration<pane::State>,
        popout_windows: Vec<(Configuration<pane::State>, WindowSpec)>,
        layout_id: uuid::Uuid,
    ) -> Self {
        let panes = pane_grid::State::with_configuration(panes);

        let mut popout = HashMap::new();

        for (pane, specs) in popout_windows {
            popout.insert(
                window::Id::unique(),
                (pane_grid::State::with_configuration(pane), specs),
            );
        }

        Self {
            panes,
            focus: None,
            streams: UniqueStreams::default(),
            popout,
            layout_id,
            last_live_t: HashMap::new(),
            pending_backfills: HashMap::new(),
            backfill_handles: Vec::new(),
            pending_disconnect: None,
        }
    }

    pub fn load_layout(
        &mut self,
        main_window: window::Id,
        windowing_mode: WindowingMode,
        open_native_popouts: bool,
    ) -> Task<Message> {
        let mut keys_to_remove = Vec::new();

        for (old_window_id, (_, specs)) in &self.popout {
            keys_to_remove.push((*old_window_id, *specs));
        }

        let open_popouts = if windowing_mode.allows_native_popout() {
            if open_native_popouts {
                self.open_popout_windows()
            } else {
                log::info!(
                    "WINDOW StartupPopoutsDeferred | count={} reason=startup_loading",
                    self.popout.len()
                );
                Task::none()
            }
        } else {
            // In embedded mode, merge popout panes back into the main pane grid
            log::info!(
                "WINDOW NativePopoutBlocked | pane=load_layout reason={reason}",
                reason = windowing_mode.reason()
            );
            for (_, (pane_state, _)) in keys_to_remove
                .iter()
                .filter_map(|(id, _)| self.popout.remove(id).map(|ps| (*id, ps)))
            {
                // Merge each popout pane into the main grid
                for (_, state) in pane_state.panes {
                    let _ = self.panes.split(
                        pane_grid::Axis::Vertical,
                        self.panes.iter().last().map(|(p, _)| *p).unwrap(),
                        state,
                    );
                }
            }
            Task::none()
        };

        self.reconcile_gex_liquidity_references(main_window);
        open_popouts.chain(self.refresh_streams(main_window))
    }

    pub fn open_popout_windows(&mut self) -> Task<Message> {
        let existing = self
            .popout
            .iter()
            .map(|(id, (_, specs))| (*id, *specs))
            .collect::<Vec<_>>();
        let mut open_tasks = Vec::with_capacity(existing.len());
        let mut rekeyed = Vec::with_capacity(existing.len());

        for (old_window_id, window_spec) in existing {
            let (window, task) = window::open(window::Settings {
                position: window::Position::Specific(window_spec.position()),
                size: window_spec.size(),
                exit_on_close_request: false,
                ..window::settings()
            });
            open_tasks.push(task.then(|_| Task::none()));

            if let Some((pane, specs)) = self.popout.remove(&old_window_id) {
                rekeyed.push((window, (pane, specs)));
            }
        }

        for (window, pane) in rekeyed {
            self.popout.insert(window, pane);
        }

        Task::batch(open_tasks)
    }

    pub fn update(
        &mut self,
        message: Message,
        main_window: &Window,
        layout_id: &uuid::Uuid,
        data_sources: &DataSources,
        windowing_mode: WindowingMode,
    ) -> (Task<Message>, Option<Event>) {
        let handles = &data_sources.exchange;
        match message {
            Message::SavePopoutSpecs(specs) => {
                for (window_id, new_spec) in specs {
                    if let Some((_, spec)) = self.popout.get_mut(&window_id) {
                        *spec = new_spec;
                    }
                }
            }
            Message::ErrorOccurred(pane_id, err) => match pane_id {
                Some(id) => {
                    if let Some(state) = self.get_mut_pane_state_by_uuid(main_window.id, id) {
                        state.status = pane::Status::Ready;
                        state.notifications.push(Toast::error(err.to_string()));
                    }
                }
                _ => {
                    return (
                        Task::done(Message::Notification(Toast::error(err.to_string()))),
                        None,
                    );
                }
            },
            Message::Pane(window, message) => match message {
                pane::Message::PaneClicked(pane) => {
                    self.focus = Some((window, pane));
                }
                pane::Message::PaneResized(pane_grid::ResizeEvent { split, ratio }) => {
                    self.panes.resize(split, ratio);
                }
                pane::Message::PaneDragged(event) => {
                    if let pane_grid::DragEvent::Dropped { pane, target } = event {
                        self.panes.drop(pane, target);
                    }
                }
                pane::Message::SplitPane(axis, pane) => {
                    let focus_pane = if let Some((new_pane, _)) =
                        self.panes.split(axis, pane, pane::State::new())
                    {
                        Some(new_pane)
                    } else {
                        None
                    };

                    if let Some(focus_pane) = focus_pane {
                        self.focus = Some((window, focus_pane));
                    }
                }
                pane::Message::ClosePane(pane) => {
                    if let Some((_, sibling)) = self.panes.close(pane) {
                        self.focus = Some((window, sibling));
                    }
                }
                pane::Message::MaximizePane(pane) => {
                    self.panes.maximize(pane);
                }
                pane::Message::Restore => {
                    self.panes.restore();
                }
                pane::Message::ReplacePane(pane) => {
                    if let Some(pane) = self.panes.get_mut(pane) {
                        *pane = pane::State::new();
                    }

                    return (self.refresh_streams(main_window.id), None);
                }
                pane::Message::VisualConfigChanged(pane, cfg, to_sync) => {
                    let mut refresh_streams = false;

                    if to_sync {
                        if let Some(state) = self.get_pane(main_window.id, window, pane) {
                            let studies_cfg = state.content.studies();
                            let clusters_cfg = state.content.clusters();

                            self.iter_all_panes_mut(main_window.id)
                                .for_each(|(_, _, state)| {
                                    let should_apply = match state.settings.visual_config {
                                        Some(ref current_cfg) => {
                                            std::mem::discriminant(current_cfg)
                                                == std::mem::discriminant(&cfg)
                                        }
                                        None => matches!(
                                            (&cfg, &state.content),
                                            (
                                                data::layout::pane::VisualConfig::Kline(_),
                                                pane::Content::Kline { .. }
                                            ) | (
                                                data::layout::pane::VisualConfig::Heatmap(_),
                                                pane::Content::Heatmap { .. }
                                                    | pane::Content::ShaderHeatmap { .. }
                                            ) | (
                                                data::layout::pane::VisualConfig::TimeAndSales(_),
                                                pane::Content::TimeAndSales(_)
                                            ) | (
                                                data::layout::pane::VisualConfig::Comparison(_),
                                                pane::Content::Comparison(_)
                                            ) | (
                                                data::layout::pane::VisualConfig::Gex(_),
                                                pane::Content::Gex { .. }
                                            )
                                        ),
                                    };

                                    if should_apply {
                                        state.settings.visual_config = Some(cfg.clone());
                                        refresh_streams |=
                                            state.content.change_visual_config(cfg.clone());
                                        state.reconcile_candlestick_trade_stream();
                                        state.reconcile_gex_liquidity_stream();

                                        state.apply_synced_settings(&studies_cfg, &clusters_cfg);
                                    }
                                });
                        }
                    } else if let Some(state) = self.get_mut_pane(main_window.id, window, pane) {
                        state.settings.visual_config = Some(cfg.clone());
                        refresh_streams = state.content.change_visual_config(cfg);
                        state.reconcile_candlestick_trade_stream();
                        state.reconcile_gex_liquidity_stream();
                    }

                    refresh_streams |= self.reconcile_gex_liquidity_references(main_window.id);
                    if refresh_streams {
                        return (self.refresh_streams(main_window.id), None);
                    }
                }
                pane::Message::SwitchLinkGroup(pane, group) => {
                    if group.is_none() {
                        if let Some(state) = self.get_mut_pane(main_window.id, window, pane) {
                            state.link_group = None;
                        }
                        self.reconcile_gex_liquidity_references(main_window.id);
                        return (self.refresh_streams(main_window.id), None);
                    }

                    let maybe_ticker_info = self
                        .iter_all_panes(main_window.id)
                        .filter(|(w, p, _)| !(*w == window && *p == pane))
                        .find_map(|(_, _, other_state)| {
                            if other_state.link_group == group {
                                other_state.stream_pair()
                            } else {
                                None
                            }
                        });

                    let mut link_task = Task::none();
                    if let Some(state) = self.get_mut_pane(main_window.id, window, pane) {
                        state.link_group = group;
                        state.modal = None;

                        if let Some(ticker_info) = maybe_ticker_info
                            && state.stream_pair() != Some(ticker_info)
                        {
                            let pane_id = state.unique_id();
                            let content_kind = state.content.kind();

                            let streams =
                                state.set_content_and_streams(vec![ticker_info], content_kind);
                            self.streams.extend(streams.iter());

                            for stream in &streams {
                                if let StreamKind::Kline { .. } = stream {
                                    link_task = fetcher::kline_fetch_task(
                                        handles.clone(),
                                        *layout_id,
                                        pane_id,
                                        *stream,
                                        None,
                                        None,
                                    )
                                    .map(Message::from);
                                    break;
                                }
                            }
                        }
                    }
                    self.reconcile_gex_liquidity_references(main_window.id);
                    return (link_task.chain(self.refresh_streams(main_window.id)), None);
                }
                pane::Message::Popout => {
                    return (self.popout_pane(main_window, windowing_mode), None);
                }
                pane::Message::Merge => {
                    return (self.merge_pane(main_window), None);
                }
                pane::Message::PaneEvent(pane, local) => {
                    if let Some(state) = self.get_mut_pane(main_window.id, window, pane) {
                        let Some(effect) = state.update(local) else {
                            return (Task::none(), None);
                        };

                        let task = match effect {
                            pane::Effect::RefreshStreams => self.refresh_streams(main_window.id),
                            pane::Effect::RequestFetch(reqs) => {
                                let pane_id = state.unique_id();
                                let ready_streams = state
                                    .streams
                                    .ready_iter()
                                    .map(|iter| iter.copied().collect::<Vec<_>>())
                                    .unwrap_or_default();

                                // Get chart generation for stale detection
                                let chart_generation =
                                    if let pane::Content::Kline { chart: Some(c), .. } =
                                        &state.content
                                    {
                                        c.current_generation()
                                    } else {
                                        0
                                    };

                                fetcher::request_fetch_many(
                                    data_sources,
                                    pane_id,
                                    &ready_streams,
                                    *layout_id,
                                    reqs.into_iter().map(|r| (r.req_id, r.fetch, r.stream)),
                                    |handle| {
                                        if let pane::Content::Kline { chart, .. } =
                                            &mut state.content
                                            && let Some(c) = chart
                                        {
                                            c.set_handle(handle);
                                        }
                                    },
                                    chart_generation,
                                )
                                .map(Message::from)
                                .chain(self.refresh_streams(main_window.id))
                            }
                            pane::Effect::SwitchTickersInGroup(ticker_info) => {
                                self.switch_tickers_in_group(handles, main_window.id, ticker_info)
                            }
                            pane::Effect::FocusWidget(id) => {
                                return (iced::widget::operation::focus(id), None);
                            }
                        };
                        return (task, None);
                    }
                }
            },
            Message::RequestPalette => {
                return (Task::none(), Some(Event::RequestPalette));
            }
            Message::ChangePaneStatus(pane_id, status) => {
                if let Some(pane_state) = self.get_mut_pane_state_by_uuid(main_window.id, pane_id) {
                    pane_state.status = status;
                }
            }
            Message::FetchCompleted {
                pane_id,
                req_id,
                fetch,
                trade_outcome,
            } => {
                self.complete_fetch(main_window.id, pane_id, req_id, fetch, trade_outcome);
            }
            Message::FetchFailed {
                pane_id,
                error,
                req_id,
                fetch,
            } => {
                if let Some(id) = req_id
                    && let Some(pane_state) =
                        self.get_mut_pane_state_by_uuid(main_window.id, pane_id)
                {
                    pane_state.mark_fetch_failed(id, error.clone());
                }

                log::warn!(
                    "FETCH FailedUpdate | pane={} req={} fetch={} error={}",
                    fetcher::short_id(pane_id),
                    fetcher::format_req_id(req_id),
                    fetcher::format_fetch_range_compact(fetch),
                    error
                );
                if let Some(pane_state) = self.get_mut_pane_state_by_uuid(main_window.id, pane_id) {
                    pane_state.status = pane::Status::Ready;
                    pane_state.notifications.push(Toast::error(
                        DashboardError::Fetch(error, req_id).to_string(),
                    ));
                }
            }
            Message::DistributeFetchedData {
                layout_id,
                pane_id,
                data,
                stream,
            } => {
                return (
                    Task::none(),
                    Some(Event::DistributeFetchedData {
                        layout_id,
                        pane_id,
                        data,
                        stream,
                    }),
                );
            }
            Message::BackfillFetchUpdate {
                pane_ids,
                stream,
                show_activity,
                update,
            } => {
                self.apply_backfill_update(main_window.id, pane_ids, stream, show_activity, update);
            }
            Message::ResolveStreams(pane_id, streams) => {
                return (
                    Task::none(),
                    Some(Event::ResolveStreams { pane_id, streams }),
                );
            }
            Message::Notification(toast) => {
                return (Task::none(), Some(Event::Notification(toast)));
            }
        }

        (Task::none(), None)
    }

    fn new_pane(
        &mut self,
        axis: pane_grid::Axis,
        main_window: &Window,
        pane_state: Option<pane::State>,
    ) -> Task<Message> {
        if self
            .focus
            .filter(|(window, _)| *window == main_window.id)
            .is_some()
        {
            // If there is any focused pane on main window, split it
            return self.split_pane(axis, main_window);
        } else {
            // If there is no focused pane, split the last pane or create a new empty grid
            let pane = self.panes.iter().last().map(|(pane, _)| pane).copied();

            if let Some(pane) = pane {
                let result = self.panes.split(axis, pane, pane_state.unwrap_or_default());

                if let Some((pane, _)) = result {
                    return self.focus_pane(main_window.id, pane);
                }
            } else {
                let (state, pane) = pane_grid::State::new(pane_state.unwrap_or_default());
                self.panes = state;

                return self.focus_pane(main_window.id, pane);
            }
        }

        Task::none()
    }

    fn focus_pane(&mut self, window: window::Id, pane: pane_grid::Pane) -> Task<Message> {
        if self.focus != Some((window, pane)) {
            self.focus = Some((window, pane));
        }

        Task::none()
    }

    fn split_pane(&mut self, axis: pane_grid::Axis, main_window: &Window) -> Task<Message> {
        if let Some((window, pane)) = self.focus
            && window == main_window.id
        {
            let result = self.panes.split(axis, pane, pane::State::new());

            if let Some((pane, _)) = result {
                return self.focus_pane(main_window.id, pane);
            }
        }

        Task::none()
    }

    fn popout_pane(
        &mut self,
        main_window: &Window,
        windowing_mode: WindowingMode,
    ) -> Task<Message> {
        if !windowing_mode.allows_native_popout() {
            log::info!(
                "WINDOW NativePopoutBlocked | reason={reason}",
                reason = windowing_mode.reason()
            );
            // In embedded mode, maximize the pane instead of popping out
            if let Some((_, pane_id)) = self.focus {
                self.panes.maximize(pane_id);
            }
            return Task::none();
        }

        if let Some((_, id)) = self.focus.take()
            && let Some((pane, _)) = self.panes.close(id)
        {
            let (window, task) = window::open(window::Settings {
                position: main_window
                    .position
                    .map(|point| window::Position::Specific(point + Vector::new(20.0, 20.0)))
                    .unwrap_or_default(),
                exit_on_close_request: false,
                min_size: Some(iced::Size::new(400.0, 300.0)),
                ..window::settings()
            });

            let (state, id) = pane_grid::State::new(pane);
            self.popout.insert(window, (state, WindowSpec::default()));

            return task.then(move |window| {
                Task::done(Message::Pane(window, pane::Message::PaneClicked(id)))
            });
        }

        Task::none()
    }

    fn merge_pane(&mut self, main_window: &Window) -> Task<Message> {
        if let Some((window, pane)) = self.focus.take()
            && let Some(pane_state) = self
                .popout
                .remove(&window)
                .and_then(|(mut panes, _)| panes.panes.remove(&pane))
        {
            let task = self.new_pane(pane_grid::Axis::Horizontal, main_window, Some(pane_state));

            return Task::batch(vec![window::close(window), task]);
        }

        Task::none()
    }

    pub fn get_pane(
        &self,
        main_window: window::Id,
        window: window::Id,
        pane: pane_grid::Pane,
    ) -> Option<&pane::State> {
        if main_window == window {
            self.panes.get(pane)
        } else {
            self.popout
                .get(&window)
                .and_then(|(panes, _)| panes.get(pane))
        }
    }

    fn get_mut_pane(
        &mut self,
        main_window: window::Id,
        window: window::Id,
        pane: pane_grid::Pane,
    ) -> Option<&mut pane::State> {
        if main_window == window {
            self.panes.get_mut(pane)
        } else {
            self.popout
                .get_mut(&window)
                .and_then(|(panes, _)| panes.get_mut(pane))
        }
    }

    fn get_mut_pane_state_by_uuid(
        &mut self,
        main_window: window::Id,
        uuid: uuid::Uuid,
    ) -> Option<&mut pane::State> {
        self.iter_all_panes_mut(main_window)
            .find(|(_, _, state)| state.unique_id() == uuid)
            .map(|(_, _, state)| state)
    }

    fn get_pane_state_by_uuid(
        &self,
        main_window: window::Id,
        uuid: uuid::Uuid,
    ) -> Option<&pane::State> {
        self.iter_all_panes(main_window)
            .find(|(_, _, state)| state.unique_id() == uuid)
            .map(|(_, _, state)| state)
    }

    fn iter_all_panes(
        &self,
        main_window: window::Id,
    ) -> impl Iterator<Item = (window::Id, pane_grid::Pane, &pane::State)> {
        self.panes
            .iter()
            .map(move |(pane, state)| (main_window, *pane, state))
            .chain(self.popout.iter().flat_map(|(window_id, (panes, _))| {
                panes.iter().map(|(pane, state)| (*window_id, *pane, state))
            }))
    }

    fn iter_all_panes_mut(
        &mut self,
        main_window: window::Id,
    ) -> impl Iterator<Item = (window::Id, pane_grid::Pane, &mut pane::State)> {
        self.panes
            .iter_mut()
            .map(move |(pane, state)| (main_window, *pane, state))
            .chain(self.popout.iter_mut().flat_map(|(window_id, (panes, _))| {
                panes
                    .iter_mut()
                    .map(|(pane, state)| (*window_id, *pane, state))
            }))
    }

    pub fn view<'a>(
        &'a self,
        main_window: &'a Window,
        tickers_table: &'a TickersTable,
        timezone: UserTimezone,
        allow_native_popout: bool,
    ) -> Element<'a, Message> {
        let pane_grid: Element<_> = PaneGrid::new(&self.panes, |id, pane, maximized| {
            let is_focused = self.focus == Some((main_window.id, id));
            pane.view(
                id,
                self.panes.len(),
                is_focused,
                maximized,
                main_window.id,
                main_window,
                timezone,
                tickers_table,
                allow_native_popout,
            )
        })
        .min_size(240)
        .on_click(pane::Message::PaneClicked)
        .on_drag(pane::Message::PaneDragged)
        .on_resize(8, pane::Message::PaneResized)
        .spacing(6)
        .style(style::pane_grid)
        .into();

        pane_grid.map(move |message| Message::Pane(main_window.id, message))
    }

    pub fn view_window<'a>(
        &'a self,
        window: window::Id,
        main_window: &'a Window,
        tickers_table: &'a TickersTable,
        timezone: UserTimezone,
        allow_native_popout: bool,
    ) -> Element<'a, Message> {
        if let Some((state, _)) = self.popout.get(&window) {
            let content = container(
                PaneGrid::new(state, |id, pane, _maximized| {
                    let is_focused = self.focus == Some((window, id));
                    pane.view(
                        id,
                        state.len(),
                        is_focused,
                        false,
                        window,
                        main_window,
                        timezone,
                        tickers_table,
                        allow_native_popout,
                    )
                })
                .on_click(pane::Message::PaneClicked),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(8);

            Element::new(content).map(move |message| Message::Pane(window, message))
        } else {
            Element::new(center("No pane found for window"))
                .map(move |message| Message::Pane(window, message))
        }
    }

    pub fn go_back(&mut self, main_window: window::Id) -> bool {
        let Some((window, pane)) = self.focus else {
            return false;
        };

        let Some(state) = self.get_mut_pane(main_window, window, pane) else {
            return false;
        };

        if state.modal.is_some() {
            state.modal = None;
            return true;
        }
        state.dismiss_drawing_interaction()
    }

    fn handle_error(
        &mut self,
        pane_id: Option<uuid::Uuid>,
        err: &DashboardError,
        main_window: window::Id,
    ) -> Task<Message> {
        match pane_id {
            Some(id) => {
                if let Some(state) = self.get_mut_pane_state_by_uuid(main_window, id) {
                    state.status = pane::Status::Ready;
                    state.notifications.push(Toast::error(err.to_string()));
                }
                Task::none()
            }
            _ => Task::done(Message::Notification(Toast::error(err.to_string()))),
        }
    }

    /// Borrow visual config, studies, and clusters from an existing pane of the same
    /// content kind, so newly created panes inherit the full set of settings
    /// that the manual "Sync all" button would apply.
    ///
    /// `exclude` is the pane that is being initialized, it is skipped so that
    /// a freshly-set placeholder (from `ContentSelected`) doesn't shadow an
    /// already-configured pane of the same kind.
    fn borrow_synced_settings(
        &self,
        main_window: window::Id,
        content_kind: ContentKind,
        exclude: Option<(window::Id, pane_grid::Pane)>,
    ) -> Option<(
        Option<VisualConfig>,
        Option<data::chart::Study>,
        Option<data::chart::kline::ClusterKind>,
    )> {
        self.iter_all_panes(main_window)
            .find_map(|(win, pane, state)| {
                if state.content.kind() == content_kind && (exclude != Some((win, pane))) {
                    let visual_config = state.settings.visual_config.clone();
                    let studies = state.content.studies();
                    let clusters = state.content.clusters();
                    Some((visual_config, studies, clusters))
                } else {
                    None
                }
            })
    }

    fn init_pane(
        &mut self,
        handles: &AdapterHandles,
        main_window: window::Id,
        window: window::Id,
        selected_pane: pane_grid::Pane,
        ticker_info: TickerInfo,
        content_kind: ContentKind,
    ) -> Task<Message> {
        let synced =
            self.borrow_synced_settings(main_window, content_kind, Some((window, selected_pane)));

        let Some(state) = self.get_mut_pane(main_window, window, selected_pane) else {
            return Task::none();
        };

        if let Some((Some(cfg), _, _)) = &synced
            && (state.settings.visual_config.is_none() || state.content.kind() != content_kind)
        {
            state.settings.visual_config = Some(cfg.clone());
        } else if state.content.kind() != content_kind {
            state.settings.visual_config = None;
        }

        let pane_id = state.unique_id();
        let streams = state.set_content_and_streams(vec![ticker_info], content_kind);

        // Apply synced configs now that content is initialized
        if let Some((_, studies, clusters)) = &synced {
            state.apply_synced_settings(studies, clusters);
        }

        self.streams.extend(streams.iter());

        let task = streams
            .iter()
            .find(|stream| matches!(stream, StreamKind::Kline { .. }))
            .map_or_else(Task::none, |stream| {
                fetcher::kline_fetch_task(
                    handles.clone(),
                    self.layout_id,
                    pane_id,
                    *stream,
                    None,
                    None,
                )
                .map(Message::from)
            });
        self.reconcile_gex_liquidity_references(main_window);
        task.chain(self.refresh_streams(main_window))
    }

    pub fn init_focused_pane(
        &mut self,
        handles: &AdapterHandles,
        main_window: window::Id,
        ticker_info: TickerInfo,
        content_kind: ContentKind,
    ) -> Task<Message> {
        if self.focus.is_none()
            && self.panes.len() == 1
            && let Some((pane_id, _)) = self.panes.iter().next()
        {
            self.focus = Some((main_window, *pane_id));
        }

        let Some((window, selected_pane)) = self.focus else {
            return Task::done(Message::Notification(Toast::warn(
                "No focused pane found".to_string(),
            )));
        };

        // Inherit settings from an existing pane of the same type
        // (mirrors what the manual "Sync all" button does)
        let synced =
            self.borrow_synced_settings(main_window, content_kind, Some((window, selected_pane)));

        let Some(state) = self.get_mut_pane(main_window, window, selected_pane) else {
            return Task::done(Message::Notification(Toast::warn(
                "No focused pane found".to_string(),
            )));
        };

        if let Some((Some(cfg), _, _)) = &synced
            && (state.settings.visual_config.is_none() || state.content.kind() != content_kind)
        {
            state.settings.visual_config = Some(cfg.clone());
        } else if state.content.kind() != content_kind {
            state.settings.visual_config = None;
        }

        let previous_ticker = state.stream_pair();
        if previous_ticker.is_some() && previous_ticker != Some(ticker_info) {
            state.link_group = None;
        }

        let streams = state.set_content_and_streams(vec![ticker_info], content_kind);

        if let Some((_, studies, clusters)) = &synced {
            state.apply_synced_settings(studies, clusters);
        }

        let pane_id = state.unique_id();
        self.streams.extend(streams.iter());

        for stream in &streams {
            if let StreamKind::Kline { .. } = stream {
                return fetcher::kline_fetch_task(
                    handles.clone(),
                    self.layout_id,
                    pane_id,
                    *stream,
                    None,
                    None,
                )
                .map(Message::from);
            }
        }

        Task::none()
    }

    pub fn add_view(
        &mut self,
        handles: &AdapterHandles,
        main_window: window::Id,
        content_kind: ContentKind,
    ) -> Task<Message> {
        let inherited_ticker = self.focus.and_then(|(window, pane)| {
            self.get_pane(main_window, window, pane).and_then(|state| {
                state.stream_pair().or_else(|| {
                    state
                        .gex_liquidity_resolution()
                        .and_then(|(_, _, reference, _)| reference)
                })
            })
        });

        let reusable = self.focus.filter(|(window, pane)| {
            self.get_pane(main_window, *window, *pane)
                .is_some_and(|state| {
                    matches!(state.content, pane::Content::Starter) || !state.content.initialized()
                })
        });

        let target = reusable.or_else(|| {
            self.iter_all_panes(main_window)
                .find(|(_, _, state)| matches!(state.content, pane::Content::Starter))
                .map(|(window, pane, _)| (window, pane))
        });

        let (window, target_pane) = if let Some(target) = target {
            target
        } else {
            let target = self
                .focus
                .filter(|(window, _)| *window == main_window)
                .map(|(_, pane)| pane)
                .or_else(|| self.panes.iter().last().map(|(pane, _)| *pane));
            let Some(target) = target else {
                return Task::none();
            };
            let axis = if self.panes.len() == 1 {
                pane_grid::Axis::Vertical
            } else {
                pane_grid::Axis::Horizontal
            };
            let Some((new_pane, _)) = self.panes.split(axis, target, pane::State::new()) else {
                return Task::none();
            };
            (main_window, new_pane)
        };

        self.focus = Some((window, target_pane));

        let compatible_ticker = inherited_ticker.filter(|ticker| {
            content_kind != ContentKind::GexChart
                || exchange::options::resolve_gex_source(ticker.ticker).is_some()
        });
        if let Some(ticker) = compatible_ticker {
            return self.init_pane(
                handles,
                main_window,
                window,
                target_pane,
                ticker,
                content_kind,
            );
        }

        let Some(state) = self.get_mut_pane(main_window, window, target_pane) else {
            return Task::none();
        };
        match state.update(pane::Event::ContentSelected(content_kind)) {
            Some(pane::Effect::FocusWidget(id)) => iced::widget::operation::focus(id),
            _ => Task::none(),
        }
    }

    pub fn switch_tickers_in_group(
        &mut self,
        handles: &AdapterHandles,
        main_window: window::Id,
        ticker_info: TickerInfo,
    ) -> Task<Message> {
        if self.focus.is_none()
            && self.panes.len() == 1
            && let Some((pane_id, _)) = self.panes.iter().next()
        {
            self.focus = Some((main_window, *pane_id));
        }

        let link_group = self.focus.and_then(|(window, pane)| {
            self.get_pane(main_window, window, pane)
                .and_then(|state| state.link_group)
        });

        if let Some(group) = link_group {
            let pane_infos: Vec<(window::Id, pane_grid::Pane, ContentKind)> = self
                .iter_all_panes_mut(main_window)
                .filter_map(|(window, pane, state)| {
                    if state.link_group == Some(group) {
                        Some((window, pane, state.content.kind()))
                    } else {
                        None
                    }
                })
                .collect();

            let tasks: Vec<Task<Message>> = pane_infos
                .iter()
                .map(|(window, pane, content_kind)| {
                    self.init_pane(
                        handles,
                        main_window,
                        *window,
                        *pane,
                        ticker_info,
                        *content_kind,
                    )
                })
                .collect();
            self.reconcile_gex_liquidity_references(main_window);
            Task::batch(tasks).chain(self.refresh_streams(main_window))
        } else if let Some((window, pane)) = self.focus {
            if let Some(state) = self.get_mut_pane(main_window, window, pane) {
                let content_kind = state.content.kind();
                self.init_focused_pane(handles, main_window, ticker_info, content_kind)
            } else {
                Task::done(Message::Notification(Toast::warn(
                    "Couldn't get focused pane's content".to_string(),
                )))
            }
        } else {
            Task::done(Message::Notification(Toast::warn(
                "No link group or focused pane found".to_string(),
            )))
        }
    }

    pub fn toggle_trade_fetch(&mut self, is_enabled: bool, main_window: &Window) {
        fetcher::toggle_trade_fetch(is_enabled);

        self.iter_all_panes_mut(main_window.id)
            .for_each(|(_, _, state)| {
                if let pane::Content::Kline { chart, kind, .. } = &mut state.content
                    && matches!(kind, data::chart::KlineChartKind::Footprint { .. })
                    && let Some(c) = chart
                {
                    c.reset_request_handler();

                    if !is_enabled {
                        state.status = pane::Status::Ready;
                    }
                }
            });
    }

    pub fn invalidate_market_data_cache(&mut self, main_window: &Window) {
        // Dropping the handles aborts old backfill workers so they cannot
        // repopulate the cache immediately after it has been cleared.
        self.backfill_handles.clear();
        self.pending_backfills.clear();

        self.iter_all_panes_mut(main_window.id)
            .for_each(|(_, _, state)| {
                if let pane::Content::Kline {
                    chart: Some(chart), ..
                } = &mut state.content
                {
                    chart.invalidate_market_data_cache();
                    state.status = pane::Status::Ready;
                }
            });
    }

    pub fn distribute_fetched_data(
        &mut self,
        main_window: window::Id,
        pane_id: uuid::Uuid,
        data: FetchedData,
        stream_type: StreamKind,
        skip_stale_check: bool,
    ) -> Task<Message> {
        // Check for stale generation before applying any data
        if !skip_stale_check {
            let req_id = match &data {
                FetchedData::Trades { req_id, .. } => *req_id,
                FetchedData::BubbleSummary { req_id, .. } => *req_id,
                FetchedData::Klines { req_id, .. } => *req_id,
                FetchedData::OI { req_id, .. } => *req_id,
            };

            if let Some(req_id) = req_id
                && let Some(pane_state) = self.get_mut_pane_state_by_uuid(main_window, pane_id)
                && let pane::Content::Kline { chart: Some(c), .. } = &mut pane_state.content
                && c.is_fetch_stale(req_id)
            {
                let request_gen = c.request_generation(req_id);
                let current_gen = c.current_generation();
                log::info!(
                    "FETCH StaleResult | req={} request_generation={} current_generation={} action=discard",
                    fetcher::short_id(req_id),
                    request_gen.map_or("-".to_string(), |g| g.to_string()),
                    current_gen
                );
                // Mark the request as completed to clean up pending state
                c.mark_trade_request_completed(req_id);
                return Task::none();
            }
        }

        match data {
            FetchedData::Trades {
                batch,
                until_time,
                req_id,
            } => {
                let last_trade_time = batch.last().map_or(UnixMs::ZERO, |trade| trade.time);
                let received = batch.len();
                let first_received = batch.first().map(|trade| trade.time);
                let last_received = batch.last().map(|trade| trade.time);

                if last_trade_time < until_time {
                    log::debug!(
                        "DATA Trades Distribute | pane={} req={} stream={} received={} inserted={} dropped_after_until=0 until_time={} first_received={} last_received={}",
                        fetcher::short_id(pane_id),
                        fetcher::format_req_id(req_id),
                        fetcher::format_stream(&stream_type),
                        received,
                        received,
                        fetcher::format_time_short(until_time),
                        fetcher::format_optional_time(first_received),
                        fetcher::format_optional_time(last_received)
                    );
                    if let Err(reason) =
                        self.insert_fetched_trades(main_window, pane_id, &batch, false, req_id)
                    {
                        return self.handle_error(Some(pane_id), &reason, main_window);
                    }
                } else {
                    let filtered_batch = batch
                        .iter()
                        .filter(|trade| trade.time <= until_time)
                        .copied()
                        .collect::<Vec<_>>();
                    let dropped_after_until = received.saturating_sub(filtered_batch.len());

                    log::debug!(
                        "DATA Trades Distribute | pane={} req={} stream={} received={received} inserted={} dropped_after_until={dropped_after_until} until_time={} first_received={} last_received={}",
                        fetcher::short_id(pane_id),
                        fetcher::format_req_id(req_id),
                        fetcher::format_stream(&stream_type),
                        filtered_batch.len(),
                        fetcher::format_time_short(until_time),
                        fetcher::format_optional_time(first_received),
                        fetcher::format_optional_time(last_received)
                    );

                    if let Err(reason) = self.insert_fetched_trades(
                        main_window,
                        pane_id,
                        &filtered_batch,
                        true,
                        req_id,
                    ) {
                        return self.handle_error(Some(pane_id), &reason, main_window);
                    }
                }
            }
            FetchedData::BubbleSummary {
                data,
                range,
                trades_seen,
                raw_discarded,
                req_id,
            } => {
                log::debug!(
                    "BUBBLE Summary Distribute | pane={} req={} stream={} range={} candles={} trades_seen={} raw_discarded={}",
                    fetcher::short_id(pane_id),
                    fetcher::format_req_id(req_id),
                    fetcher::format_stream(&stream_type),
                    fetcher::format_time_range(range.0, range.1),
                    data.len(),
                    trades_seen,
                    raw_discarded
                );
                if let Some(pane_state) = self.get_mut_pane_state_by_uuid(main_window, pane_id) {
                    pane_state.status = pane::Status::Ready;
                    if let pane::Content::Kline { chart: Some(c), .. } = &mut pane_state.content {
                        c.insert_bubble_summaries(
                            data,
                            range.0,
                            range.1,
                            trades_seen,
                            raw_discarded,
                            req_id,
                        );
                    }
                }
            }
            FetchedData::Klines { data, req_id } => {
                log::debug!(
                    "DATA Klines Distribute | pane={} req={} stream={} count={} first={} last={}",
                    fetcher::short_id(pane_id),
                    fetcher::format_req_id(req_id),
                    fetcher::format_stream(&stream_type),
                    data.len(),
                    fetcher::format_optional_time(data.first().map(|kline| kline.time)),
                    fetcher::format_optional_time(data.last().map(|kline| kline.time))
                );
                if let Some(pane_state) = self.get_mut_pane_state_by_uuid(main_window, pane_id) {
                    pane_state.status = pane::Status::Ready;

                    if let StreamKind::Kline {
                        timeframe,
                        ticker_info,
                    } = stream_type
                    {
                        pane_state.insert_hist_klines(req_id, timeframe, ticker_info, &data);
                    }
                }
            }
            FetchedData::OI { data, req_id } => {
                log::debug!(
                    "DATA OI Distribute | pane={} req={} stream={} count={}",
                    fetcher::short_id(pane_id),
                    fetcher::format_req_id(req_id),
                    fetcher::format_stream(&stream_type),
                    data.len()
                );
                if let Some(pane_state) = self.get_mut_pane_state_by_uuid(main_window, pane_id) {
                    pane_state.status = pane::Status::Ready;

                    if let StreamKind::Kline { .. } = stream_type {
                        pane_state.insert_hist_oi(req_id, &data);
                    }
                }
            }
        }

        Task::none()
    }

    fn complete_fetch(
        &mut self,
        main_window: window::Id,
        pane_id: uuid::Uuid,
        req_id: Option<uuid::Uuid>,
        fetch: Option<fetcher::FetchRange>,
        trade_outcome: Option<fetcher::TradeFetchOutcome>,
    ) {
        let Some(pane_state) = self.get_mut_pane_state_by_uuid(main_window, pane_id) else {
            log::warn!(
                "FETCH Complete | pane={} req={} fetch={} found=false reason=no_pane",
                fetcher::short_id(pane_id),
                fetcher::format_req_id(req_id),
                fetcher::format_fetch_range_compact(fetch)
            );
            return;
        };

        let mut chart_found = false;
        if let Some(fetcher::FetchRange::Trades(from, to)) = fetch
            && let pane::Content::Kline { chart: Some(c), .. } = &mut pane_state.content
        {
            chart_found = true;
            c.complete_trade_fetch(
                req_id,
                Some(fetcher::FetchRange::Trades(from, to)),
                trade_outcome.unwrap_or_default(),
            );
        }

        log::debug!(
            "FETCH Complete | pane={} req={} fetch={} found=true chart_found={chart_found}",
            fetcher::short_id(pane_id),
            fetcher::format_req_id(req_id),
            fetcher::format_fetch_range_compact(fetch)
        );
        pane_state.status = pane::Status::Ready;
    }

    fn apply_backfill_update(
        &mut self,
        main_window: window::Id,
        pane_ids: Vec<uuid::Uuid>,
        stream: StreamKind,
        show_activity: bool,
        update: fetcher::FetchUpdate,
    ) {
        match update {
            fetcher::FetchUpdate::Status { status, .. } => match status {
                fetcher::FetchTaskStatus::Loading(info) => {
                    log::debug!(
                        "BACKFILL Update | kind=Loading stream={} pane_ids={} info={info:?}",
                        fetcher::format_stream(&stream),
                        pane_ids.len()
                    );
                    if show_activity {
                        for pane_id in pane_ids {
                            if let Some(pane_state) =
                                self.get_mut_pane_state_by_uuid(main_window, pane_id)
                            {
                                pane_state.status = pane::Status::Loading {
                                    info,
                                    source: pane::LoadingSource::Reconnect,
                                };
                            }
                        }
                    }
                }
                fetcher::FetchTaskStatus::Completed {
                    req_id,
                    fetch,
                    trade_outcome,
                } => {
                    log::info!(
                        "BACKFILL Completed | stream={} pane_ids={} req={} fetch={} tail={}",
                        fetcher::format_stream(&stream),
                        pane_ids.len(),
                        fetcher::format_req_id(req_id),
                        fetcher::format_fetch_range_compact(fetch),
                        trade_outcome
                            .and_then(|outcome| outcome.unfilled_tail)
                            .map(|(f, t)| fetcher::format_time_range(f, t))
                            .unwrap_or_else(|| "-".to_string())
                    );
                    if let Some(fetch) = fetch {
                        self.clear_pending_backfill(stream, fetch);
                        log::info!(
                            "BACKFILL PendingRemove | stream={} fetch={} reason=completed",
                            fetcher::format_stream(&stream),
                            fetcher::format_fetch_range(&fetch)
                        );
                    }

                    // Backfill completion: mark covered ranges on each pane
                    // without going through per-pane RequestHandler.
                    // Only mark panes that support the fetched data type.
                    for pane_id in pane_ids {
                        if let Some(pane_state) =
                            self.get_mut_pane_state_by_uuid(main_window, pane_id)
                        {
                            let supports = fetch
                                .map(|f| pane_state.supports_fetch_range(&f))
                                .unwrap_or(true);

                            if supports {
                                pane_state.mark_backfill_completed(
                                    fetch,
                                    trade_outcome.unwrap_or_default(),
                                );
                                pane_state.status = pane::Status::Ready;
                            } else {
                                log::debug!(
                                    "BACKFILL CompletionSkip | pane={} content={} reason=unsupported_fetch_range",
                                    fetcher::short_id(pane_id),
                                    pane_state.content
                                );
                            }
                        }
                    }
                }
            },
            fetcher::FetchUpdate::Data { data, .. } => {
                let data_summary = match &data {
                    FetchedData::Trades { batch, req_id, .. } => {
                        format!(
                            "Trades:req={}:count={}",
                            fetcher::format_req_id(*req_id),
                            batch.len()
                        )
                    }
                    FetchedData::BubbleSummary { data, req_id, .. } => {
                        format!(
                            "BubbleSummary:req={}:candles={}:candidates={}",
                            fetcher::format_req_id(*req_id),
                            data.len(),
                            data.iter()
                                .map(|summary| summary.candidates.len())
                                .sum::<usize>()
                        )
                    }
                    FetchedData::Klines { data, req_id } => {
                        format!(
                            "Klines:req={}:count={}",
                            fetcher::format_req_id(*req_id),
                            data.len()
                        )
                    }
                    FetchedData::OI { data, req_id } => {
                        format!(
                            "OI:req={}:count={}",
                            fetcher::format_req_id(*req_id),
                            data.len()
                        )
                    }
                };
                log::debug!(
                    "BACKFILL Update | kind=Data stream={} pane_ids={} data={}",
                    fetcher::format_stream(&stream),
                    pane_ids.len(),
                    data_summary
                );
                for pane_id in pane_ids {
                    // Check if this pane supports the fetched data type
                    let supports = self
                        .get_pane_state_by_uuid(main_window, pane_id)
                        .map(|s| s.supports_fetched_data(&data))
                        .unwrap_or(false);

                    if !supports {
                        log::debug!(
                            "BACKFILL DataSkip | pane={} stream={} data={} reason=unsupported_fetched_data",
                            fetcher::short_id(pane_id),
                            fetcher::format_stream(&stream),
                            data_summary_type(&data)
                        );
                        continue;
                    }

                    let _ = self.distribute_fetched_data(
                        main_window,
                        pane_id,
                        data.clone(),
                        stream,
                        true, // backfill: skip stale generation check
                    );

                    // Backfill data insertion normally toggles the pane's
                    // historical loading state. Restore the reconnect context,
                    // or keep short routine repairs entirely silent.
                    if let Some(pane_state) = self.get_mut_pane_state_by_uuid(main_window, pane_id)
                    {
                        if show_activity {
                            let received = match &pane_state.status {
                                pane::Status::Loading {
                                    info: InfoKind::FetchingTrades(count),
                                    ..
                                } => *count,
                                _ => match &data {
                                    FetchedData::Trades { batch, .. } => batch.len(),
                                    _ => 0,
                                },
                            };
                            pane_state.status = pane::Status::Loading {
                                info: match stream {
                                    StreamKind::Trades { .. } => InfoKind::FetchingTrades(received),
                                    StreamKind::Kline { .. } => InfoKind::FetchingKlines,
                                    StreamKind::Depth { .. } => continue,
                                },
                                source: pane::LoadingSource::Reconnect,
                            };
                        } else {
                            pane_state.status = pane::Status::Ready;
                        }
                    }
                }
            }
            fetcher::FetchUpdate::Error {
                pane_id,
                error,
                req_id,
                fetch,
            } => {
                // For backfill, skip stale generation check since requests
                // are tracked globally (pending_backfills), not per-pane.
                let is_timeout = error.contains("TimedOut") || error.contains("timed out");
                if is_timeout {
                    log::error!(
                        "BACKFILL Timeout | stream={} pane_ids={} source_pane={} req={} fetch={} error={}",
                        fetcher::format_stream(&stream),
                        pane_ids.len(),
                        fetcher::short_id(pane_id),
                        fetcher::format_req_id(req_id),
                        fetcher::format_fetch_range_compact(fetch),
                        error
                    );
                } else {
                    log::warn!(
                        "BACKFILL Failed | stream={} pane_ids={} source_pane={} req={} fetch={} error={}",
                        fetcher::format_stream(&stream),
                        pane_ids.len(),
                        fetcher::short_id(pane_id),
                        fetcher::format_req_id(req_id),
                        fetcher::format_fetch_range_compact(fetch),
                        error
                    );
                }
                if let Some(fetch) = fetch {
                    self.clear_pending_backfill(stream, fetch);
                    log::info!(
                        "BACKFILL PendingRemove | stream={} fetch={} reason={}",
                        fetcher::format_stream(&stream),
                        fetcher::format_fetch_range(&fetch),
                        if is_timeout { "timeout" } else { "failed" }
                    );
                }

                // Set all affected panes to Ready (don't call mark_fetch_failed
                // since backfill requests aren't in per-pane RequestHandler).
                for target_pane_id in &pane_ids {
                    if let Some(pane_state) =
                        self.get_mut_pane_state_by_uuid(main_window, *target_pane_id)
                    {
                        pane_state.status = pane::Status::Ready;
                    }
                }

                if let Some(pane_state) = self.get_mut_pane_state_by_uuid(main_window, pane_id) {
                    pane_state.status = pane::Status::Ready;
                    pane_state.notifications.push(Toast::error(
                        DashboardError::Fetch(error, req_id).to_string(),
                    ));
                }
            }
        }
    }

    fn clear_pending_backfill(&mut self, stream: StreamKind, fetch: fetcher::FetchRange) {
        let (from, to) = match fetch {
            fetcher::FetchRange::Kline(from, to) | fetcher::FetchRange::Trades(from, to) => {
                (from, to)
            }
            fetcher::FetchRange::OpenInterest { from, to, .. } => (from, to),
            fetcher::FetchRange::BubbleSummary { from, to, .. } => (from, to),
        };

        let removed = self
            .pending_backfills
            .remove(&(stream, from.as_u64(), to.as_u64()))
            .is_some();
        log::debug!(
            "BACKFILL ClearPending | stream={} range={} removed={removed}",
            fetcher::format_stream(&stream),
            fetcher::format_time_range(from, to)
        );
    }

    fn insert_fetched_trades(
        &mut self,
        main_window: window::Id,
        pane_id: uuid::Uuid,
        trades: &[Trade],
        is_batches_done: bool,
        _req_id: Option<uuid::Uuid>,
    ) -> Result<(), DashboardError> {
        let pane_state = self
            .get_mut_pane_state_by_uuid(main_window, pane_id)
            .ok_or_else(|| {
                DashboardError::Unknown(
                    "No matching pane state found for fetched trades".to_string(),
                )
            })?;

        match &mut pane_state.content {
            pane::Content::Kline { chart, .. } => {
                if let Some(c) = chart {
                    // Update loading status
                    match &mut pane_state.status {
                        pane::Status::Loading {
                            info: InfoKind::FetchingTrades(count),
                            ..
                        } => {
                            *count += trades.len();
                        }
                        _ => {
                            pane_state.status = pane::Status::Loading {
                                info: InfoKind::FetchingTrades(trades.len()),
                                source: pane::LoadingSource::Historical,
                            };
                        }
                    }

                    c.insert_raw_trades(trades.to_owned(), is_batches_done);

                    if is_batches_done {
                        // NOTE: Do NOT mark request completed here.
                        // complete_fetch() -> complete_trade_fetch() handles that,
                        // avoiding the double PendingRemove + StaleResult log.
                        pane_state.status = pane::Status::Ready;
                    }
                    Ok(())
                } else {
                    log::debug!(
                        "FETCH TradesSkip | pane={} content=Kline(no_chart) reason=no_chart",
                        fetcher::short_id(pane_id)
                    );
                    Ok(())
                }
            }
            // Non-Kline panes cannot consume fetched trades.
            // This is an internal routing mismatch, not a user error.
            _ => {
                log::debug!(
                    "FETCH TradesSkip | pane={} content={} reason=unsupported_content_type",
                    fetcher::short_id(pane_id),
                    pane_state.content
                );
                Ok(())
            }
        }
    }

    pub fn update_latest_klines(
        &mut self,
        stream: &StreamKind,
        kline: &Kline,
        main_window: window::Id,
    ) -> Task<Message> {
        // Track last live timestamp for backfill on disconnect.
        let previous_last_live_t = self.last_live_t.get(stream).copied();
        if self
            .last_live_t
            .get(stream)
            .is_none_or(|&prev| kline.time > prev)
        {
            self.last_live_t.insert(*stream, kline.time);
        }
        let new_last_live_t = self.last_live_t.get(stream).copied();

        let mut found_match = false;
        let mut matched_panes = 0usize;

        self.iter_all_panes_mut(main_window)
            .for_each(|(_, _, pane_state)| {
                if pane_state.matches_stream(stream) {
                    matched_panes += 1;
                    match &mut pane_state.content {
                        pane::Content::Kline { chart: Some(c), .. } => {
                            c.update_latest_kline(kline);
                        }
                        pane::Content::Comparison(Some(c)) => {
                            c.update_latest_kline(&stream.ticker_info(), kline);
                        }
                        _ => {}
                    }
                    found_match = true;
                }
            });

        log::trace!(
            "KLINE LiveRoute | stream={} kline_t={} prev_last_live_t={} new_last_live_t={} matched_panes={matched_panes}",
            fetcher::format_stream(stream),
            fetcher::format_time_short(kline.time),
            fetcher::format_optional_time(previous_last_live_t),
            fetcher::format_optional_time(new_last_live_t)
        );

        if found_match {
            Task::none()
        } else {
            log::warn!(
                "KLINE NoMatch | stream={} kline_t={} reason=refresh_streams",
                fetcher::format_stream(stream),
                fetcher::format_time_short(kline.time)
            );
            self.refresh_streams(main_window)
        }
    }

    pub fn ingest_depth(
        &mut self,
        stream: &StreamKind,
        update_t: UnixMs,
        depth: &Depth,
        main_window: window::Id,
    ) -> Task<Message> {
        let mut found_match = false;
        let mut matched_panes = 0usize;

        self.iter_all_panes_mut(main_window)
            .for_each(|(_, _, pane_state)| {
                if pane_state.matches_stream(stream) {
                    matched_panes += 1;
                    match &mut pane_state.content {
                        pane::Content::Heatmap { chart, .. } => {
                            if let Some(c) = chart {
                                c.insert_depth(depth, update_t);
                            }
                        }
                        pane::Content::ShaderHeatmap { chart, .. } => {
                            if let Some(c) = chart {
                                c.insert_depth(depth, update_t);
                            }
                        }
                        pane::Content::Ladder(panel) => {
                            if let Some(panel) = panel {
                                panel.insert_depth(depth, update_t);
                            }
                        }
                        pane::Content::Gex {
                            chart: Some(chart), ..
                        } => {
                            chart.insert_depth(depth, update_t);
                        }
                        _ => {
                            log::error!("No chart found for the stream: {stream:?}");
                        }
                    }
                    found_match = true;
                }
            });

        log::trace!(
            "DATA DepthRoute | stream={} update_t={} matched_panes={matched_panes}",
            fetcher::format_stream(stream),
            fetcher::format_time_short(update_t)
        );

        if found_match {
            Task::none()
        } else {
            log::warn!(
                "DATA DepthNoMatch | stream={} update_t={} reason=refresh_streams",
                fetcher::format_stream(stream),
                fetcher::format_time_short(update_t)
            );
            self.refresh_streams(main_window)
        }
    }

    pub fn iceberg_requirements(
        &self,
        main_window: window::Id,
    ) -> Vec<(
        crate::connector::iceberg::DetectorKey,
        data::orderflow::iceberg::IcebergDetectorConfig,
    )> {
        self.iter_all_panes(main_window)
            .filter_map(|(_, _, pane_state)| match &pane_state.content {
                pane::Content::Heatmap {
                    chart: Some(chart), ..
                } => {
                    let config = chart.visual_config().iceberg_detector;
                    config.enabled.then(|| (chart.detector_key(), config))
                }
                pane::Content::ShaderHeatmap {
                    chart: Some(chart), ..
                } => {
                    let config = chart.visual_config().iceberg_detector;
                    config.enabled.then(|| {
                        (
                            crate::connector::iceberg::DetectorKey {
                                ticker_info: chart.ticker_info,
                                tick_size: chart.tick_size(),
                            },
                            config,
                        )
                    })
                }
                _ => None,
            })
            .collect()
    }

    pub fn ingest_iceberg_events(
        &mut self,
        events: &[data::orderflow::iceberg::IcebergEvent],
        main_window: window::Id,
    ) {
        if events.is_empty() {
            return;
        }
        self.iter_all_panes_mut(main_window)
            .for_each(|(_, _, pane_state)| {
                if let pane::Content::Heatmap {
                    chart: Some(chart), ..
                } = &mut pane_state.content
                {
                    for event in events {
                        chart.insert_iceberg_event(event.clone());
                    }
                } else if let pane::Content::ShaderHeatmap {
                    chart: Some(chart), ..
                } = &mut pane_state.content
                {
                    for event in events {
                        chart.insert_iceberg_event(event.clone());
                    }
                }
            });
    }

    pub fn ingest_trades(
        &mut self,
        stream: &StreamKind,
        buffer: &[Trade],
        update_t: UnixMs,
        main_window: window::Id,
    ) -> Task<Message> {
        // Track last live timestamp for backfill on disconnect.
        let last_trade_t = buffer.last().map_or(update_t, |t| t.time);
        let previous_last_live_t = self.last_live_t.get(stream).copied();
        if self
            .last_live_t
            .get(stream)
            .is_none_or(|&prev| last_trade_t > prev)
        {
            self.last_live_t.insert(*stream, last_trade_t);
        }
        let new_last_live_t = self.last_live_t.get(stream).copied();

        let mut found_match = false;
        let mut matched_panes = 0usize;
        let mut content_updates = Vec::new();

        self.iter_all_panes_mut(main_window)
            .for_each(|(_, _, pane_state)| {
                if pane_state.matches_stream(stream) {
                    matched_panes += 1;
                    match &mut pane_state.content {
                        pane::Content::Heatmap { chart, .. } => {
                            if let Some(c) = chart {
                                c.insert_trades(buffer, update_t);
                                content_updates.push("Heatmap");
                            }
                        }
                        pane::Content::ShaderHeatmap { chart, .. } => {
                            if let Some(c) = chart {
                                c.insert_trades(buffer, update_t);
                                content_updates.push("ShaderHeatmap");
                            }
                        }
                        pane::Content::Kline { chart, .. } => {
                            if let Some(c) = chart {
                                c.insert_trades(buffer);
                                content_updates.push("Kline");
                            }
                        }
                        pane::Content::TimeAndSales(panel) => {
                            if let Some(p) = panel {
                                p.insert_buffer(buffer);
                                content_updates.push("TimeAndSales");
                            }
                        }
                        pane::Content::Ladder(panel) => {
                            if let Some(p) = panel {
                                p.insert_trades(buffer);
                                content_updates.push("Ladder");
                            }
                        }
                        _ => {
                            log::error!("No chart found for the stream: {stream:?}");
                        }
                    }
                    found_match = true;
                }
            });

        log::trace!(
            "TRADE LiveRoute | stream={} buffer_len={} first_trade_t={} last_trade_t={} update_t={} prev_last_live_t={} new_last_live_t={} matched_panes={} content_updates={}",
            fetcher::format_stream(stream),
            buffer.len(),
            fetcher::format_optional_time(buffer.first().map(|trade| trade.time)),
            fetcher::format_optional_time(buffer.last().map(|trade| trade.time)),
            fetcher::format_time_short(update_t),
            fetcher::format_optional_time(previous_last_live_t),
            fetcher::format_optional_time(new_last_live_t),
            matched_panes,
            content_updates.join(",")
        );

        if found_match {
            Task::none()
        } else {
            log::warn!(
                "TRADE NoMatch | stream={} buffer_len={} update_t={} reason=refresh_streams",
                fetcher::format_stream(stream),
                buffer.len(),
                fetcher::format_time_short(update_t)
            );
            self.refresh_streams(main_window)
        }
    }

    pub fn invalidate_all_panes(&mut self, main_window: window::Id) {
        self.iter_all_panes_mut(main_window)
            .for_each(|(_, _, state)| {
                let _ = state.invalidate(Instant::now());
            });
    }

    pub fn park_for_inactive_layout(&mut self, main_window: window::Id) {
        self.iter_all_panes_mut(main_window)
            .for_each(|(_, _, state)| state.park_for_inactive_layout());
    }

    pub fn tick(
        &mut self,
        now: Instant,
        data_sources: &DataSources,
        _main_window: window::Id,
    ) -> Task<Message> {
        // Clean up backfill handles when no backfills are pending.
        if self.pending_backfills.is_empty() && !self.backfill_handles.is_empty() {
            log::debug!(
                "BACKFILL HandleCleanup | cleared={} handles",
                self.backfill_handles.len()
            );
            self.backfill_handles.clear();
        }

        let mut tasks = vec![];

        let mut tick_state = |state: &mut pane::State| match state.tick(now) {
            Some(pane::Action::Chart(action)) => match action {
                chart::Action::ErrorOccurred(err) => {
                    state.status = pane::Status::Ready;
                    state.notifications.push(Toast::error(err.to_string()));
                }
                chart::Action::RequestFetch(reqs) => {
                    let pane_id = state.unique_id();
                    let ready_streams = state
                        .streams
                        .ready_iter()
                        .map(|iter| iter.copied().collect::<Vec<_>>())
                        .unwrap_or_default();

                    // Get chart generation for stale detection
                    let chart_generation =
                        if let pane::Content::Kline { chart: Some(c), .. } = &state.content {
                            c.current_generation()
                        } else {
                            0
                        };

                    let fetch_tasks = fetcher::request_fetch_many(
                        data_sources,
                        pane_id,
                        &ready_streams,
                        self.layout_id,
                        reqs.into_iter().map(|r| (r.req_id, r.fetch, r.stream)),
                        |handle| {
                            if let pane::Content::Kline { chart, .. } = &mut state.content
                                && let Some(c) = chart
                            {
                                c.set_handle(handle);
                            }
                        },
                        chart_generation,
                    )
                    .map(Message::from);

                    tasks.push(fetch_tasks);
                }
                chart::Action::RequestPalette => {
                    tasks.push(Task::done(Message::RequestPalette));
                }
            },
            Some(pane::Action::Panel(_action)) => {}
            Some(pane::Action::ResolveStreams(streams)) => {
                tasks.push(Task::done(Message::ResolveStreams(
                    state.unique_id(),
                    streams,
                )));
            }
            Some(pane::Action::ResolveContent) => match state.stream_pair_kind() {
                Some(StreamPairKind::MultiSource(tickers)) => {
                    state.set_content_and_streams(tickers, state.content.kind());
                }
                Some(StreamPairKind::SingleSource(ticker)) => {
                    state.set_content_and_streams(vec![ticker], state.content.kind());
                }
                None => {}
            },
            None => {}
        };

        // tick only the maximized pane if there is any, otherwise tick all panes
        let maximized_pane = self.panes.maximized();
        for (pane_id, state) in self.panes.iter_mut() {
            if maximized_pane.is_some_and(|maximized| *pane_id != maximized) {
                continue;
            }

            tick_state(state);
        }

        for (popout_state, _) in self.popout.values_mut() {
            for (_, state) in popout_state.iter_mut() {
                tick_state(state);
            }
        }

        Task::batch(tasks)
    }

    pub fn resolve_streams(
        &mut self,
        main_window: window::Id,
        pane_id: uuid::Uuid,
        streams: Vec<StreamKind>,
    ) -> Task<Message> {
        log::debug!(
            "STREAM ResolveReady | pane={} streams={}",
            fetcher::short_id(pane_id),
            fetcher::format_streams(&streams)
        );
        if let Some(state) = self.get_mut_pane_state_by_uuid(main_window, pane_id) {
            state.streams = ResolvedStream::Ready(streams.clone());
        }
        self.refresh_streams(main_window)
    }

    pub fn block_streams(&mut self, main_window: window::Id, pane_id: uuid::Uuid, reason: String) {
        if let Some(state) = self.get_mut_pane_state_by_uuid(main_window, pane_id) {
            match &mut state.streams {
                ResolvedStream::Waiting { streams, .. } => {
                    state.streams = ResolvedStream::Blocked {
                        streams: streams.clone(),
                        reason,
                        last_attempt: None,
                    };
                }
                ResolvedStream::Blocked {
                    reason: old_reason, ..
                } => {
                    *old_reason = reason;
                }
                _ => {}
            }
        }
    }

    pub fn market_subscriptions(&self, handles: &AdapterHandles) -> Subscription<exchange::Event> {
        let pane_count = self.panes.iter().count()
            + self
                .popout
                .values()
                .map(|(state, _)| state.iter().count())
                .sum::<usize>();
        log::debug!("STREAM RebuildSubscriptions | panes={pane_count}");
        let unique_streams = self
            .streams
            .combined_used()
            .flat_map(|(exchange, specs)| {
                let mut subs = vec![];
                log::debug!(
                    "STREAM SubscriptionGroup | exchange={exchange:?} depth={} trade={} kline={}",
                    specs.depth.len(),
                    specs.trade.len(),
                    specs.kline.len()
                );

                if !specs.depth.is_empty() {
                    let depth_subs = specs
                        .depth
                        .iter()
                        .map(|(ticker, aggr, push_freq)| {
                            let tick_mltp = match aggr {
                                StreamTicksize::Client => None,
                                StreamTicksize::ServerSide(tick_mltp) => Some(*tick_mltp),
                            };

                            let config = StreamConfig::new(
                                *ticker,
                                ticker.exchange(),
                                tick_mltp,
                                *push_freq,
                            );

                            let data = (handles.clone(), config);
                            Subscription::run_with(data, |data| data.0.depth_stream(&data.1))
                        })
                        .collect::<Vec<_>>();

                    if !depth_subs.is_empty() {
                        subs.push(Subscription::batch(depth_subs));
                    }
                }

                if !specs.trade.is_empty() {
                    let trade_subs = specs
                        .trade
                        .chunks(MAX_TRADE_TICKERS_PER_STREAM)
                        .enumerate()
                        .map(|(idx, tickers)| {
                            log::debug!(
                                "STREAM TradeChunk | exchange={exchange:?} idx={idx} size={}",
                                tickers.len()
                            );
                            let config = StreamConfig::new(
                                tickers.to_vec(),
                                exchange,
                                None,
                                PushFrequency::ServerDefault,
                            );

                            let data = (handles.clone(), config);
                            Subscription::run_with(data, |data| data.0.trade_stream(&data.1))
                        })
                        .collect::<Vec<_>>();

                    if !trade_subs.is_empty() {
                        subs.push(Subscription::batch(trade_subs));
                    }
                }

                if !specs.kline.is_empty() {
                    let kline_subs = specs
                        .kline
                        .chunks(MAX_KLINE_STREAMS_PER_STREAM)
                        .enumerate()
                        .map(|(idx, streams)| {
                            log::debug!(
                                "STREAM KlineChunk | exchange={exchange:?} idx={idx} size={}",
                                streams.len()
                            );
                            let config = StreamConfig::new(
                                streams.to_vec(),
                                exchange,
                                None,
                                PushFrequency::ServerDefault,
                            );

                            let data = (handles.clone(), config);
                            Subscription::run_with(data, |data| data.0.kline_stream(&data.1))
                        })
                        .collect::<Vec<_>>();

                    if !kline_subs.is_empty() {
                        subs.push(Subscription::batch(kline_subs));
                    }
                }

                subs
            })
            .collect::<Vec<Subscription<exchange::Event>>>();

        if unique_streams.is_empty() && pane_count > 0 {
            // Log at debug level to avoid spamming every frame.
            // This is a normal transient state during startup / stream resolution.
            log::debug!("STREAM EmptySubscriptions | panes={pane_count} reason=no_unique_streams");
        }

        Subscription::batch(unique_streams)
    }

    pub fn theme_updated(&mut self, main_window: window::Id, theme: &iced_core::Theme) {
        self.iter_all_panes_mut(main_window)
            .for_each(|(_, _, state)| {
                state.content.update_theme(theme);
            });
    }

    fn refresh_streams(&mut self, main_window: window::Id) -> Task<Message> {
        let old_streams = all_unique_streams(&self.streams);
        let all_pane_streams = self
            .iter_all_panes(main_window)
            .flat_map(|(_, _, pane_state)| pane_state.streams.ready_iter().into_iter().flatten());
        self.streams = UniqueStreams::from(all_pane_streams);
        let new_streams = all_unique_streams(&self.streams);
        let added = new_streams
            .iter()
            .filter(|stream| !old_streams.contains(stream))
            .copied()
            .collect::<Vec<_>>();
        let removed = old_streams
            .iter()
            .filter(|stream| !new_streams.contains(stream))
            .copied()
            .collect::<Vec<_>>();

        log::debug!(
            "STREAM Refresh | old_count={} new_count={} added={} removed={}",
            old_streams.len(),
            new_streams.len(),
            added.len(),
            removed.len()
        );
        for stream in &added {
            log::debug!("STREAM Added | stream={}", fetcher::format_stream(stream));
        }
        for stream in &removed {
            log::debug!("STREAM Removed | stream={}", fetcher::format_stream(stream));
        }

        Task::none()
    }

    /// Complete set of market streams currently required by the dashboard.
    /// Used by the application-level connectivity tracker to avoid treating a
    /// partial reconnect as a fully restored connection.
    pub fn configured_market_streams(&self) -> Vec<StreamKind> {
        all_unique_streams(&self.streams)
    }

    pub fn startup_load_status(&self, main_window: window::Id) -> StartupLoadStatus {
        let mut status = StartupLoadStatus::default();

        for (_, _, pane_state) in self.iter_all_panes(main_window) {
            if matches!(pane_state.content, pane::Content::Starter) {
                continue;
            }

            let streams_blocked = matches!(pane_state.streams, ResolvedStream::Blocked { .. });

            status.pane_count += 1;
            if matches!(
                &pane_state.streams,
                ResolvedStream::Waiting { streams, .. } if !streams.is_empty()
            ) {
                status.unresolved_streams += 1;
            }
            if !streams_blocked && !pane_state.content.initialized() {
                status.initializing_panes += 1;
            }
            if !streams_blocked && matches!(pane_state.status, pane::Status::Loading { .. }) {
                status.loading_panes += 1;
            }
            if matches!(
                &pane_state.content,
                pane::Content::Gex { chart: Some(chart), unsupported: false, .. }
                    if chart.freshness() == data::chart::gex::GexFreshness::Loading
            ) {
                status.loading_gex += 1;
            }
        }

        status
    }

    /// Historical gap backfill after WS disconnect.
    ///
    /// For each disconnected trade/kline stream, finds all panes that use it and
    /// requests a historical fetch from `last_seen + 1ms` to `now`. Skips depth
    /// streams (stateful snapshot, no gap fill needed).
    ///
    /// Long trade and candle gaps are split into sequential chunks. This
    /// recovers the complete offline interval without launching a burst of
    /// concurrent REST calls or silently dropping older data.
    pub fn backfill_disconnected_streams(
        &mut self,
        data_sources: &DataSources,
        main_window: window::Id,
        streams: &[StreamKind],
        disconnect_last_seen: &HashMap<StreamKind, UnixMs>,
        now: UnixMs,
        reason: &str,
    ) -> Task<Message> {
        /// Minimum gap (ms) to bother backfilling; avoids tiny useless fetches.
        const MIN_BACKFILL_GAP_MS: u64 = 1_000;
        /// Routine reconnect repairs stay silent. Only a meaningful period
        /// without live data gets a visible recovery indicator.
        const RECONNECT_ACTIVITY_THRESHOLD_MS: u64 = 5_000;
        /// Keeps each trade worker comfortably below its global timeout. Chunks
        /// for the same pane/range are chained and therefore run sequentially.
        const TRADE_BACKFILL_CHUNK_MS: u64 = 15 * 60 * 1_000;
        /// Pending backfill entries older than this are pruned to allow re-fetching.
        const PENDING_EXPIRY: std::time::Duration = std::time::Duration::from_secs(30 * 60);

        log::info!(
            "BACKFILL Entry | disconnected_streams={} last_live_t={} pending_backfills={} now={}",
            streams.len(),
            self.last_live_t.len(),
            self.pending_backfills.len(),
            fetcher::format_time_short(now)
        );

        // Prune stale pending entries.
        let pending_before = self.pending_backfills.len();
        self.pending_backfills
            .retain(|_, inserted_at| inserted_at.elapsed() < PENDING_EXPIRY);
        let pruned = pending_before.saturating_sub(self.pending_backfills.len());
        if pruned > 0 {
            log::debug!(
                "BACKFILL Pruned | before={pending_before} after={} pruned={pruned}",
                self.pending_backfills.len()
            );
        }

        let mut fetch_tasks: Vec<Task<Message>> = Vec::new();
        let mut new_backfill_handles: Vec<iced::task::Handle> = Vec::new();

        for stream in streams {
            // Depth streams are stateful snapshots — no historical gap to fill.
            if matches!(stream, StreamKind::Depth { .. }) {
                log::info!(
                    "BACKFILL Decision | stream={} has_last_seen={} last_seen=- full_from=- full_to={} gap_ms=0 capped=false grouped_panes=0 registered_panes=0 reason=depth_not_supported",
                    fetcher::format_stream(stream),
                    self.last_live_t.contains_key(stream),
                    fetcher::format_time_short(now)
                );
                continue;
            }

            // Always use the timestamp captured at disconnect. A subset of WS
            // groups may reconnect and emit live events before the aggregate
            // connection is complete; using the current last_live_t here would
            // skip that stream's actual offline interval.
            let last_t = match disconnect_last_seen.get(stream) {
                Some(&t) => t,
                None => {
                    log::info!(
                        "BACKFILL Decision | stream={} has_last_seen=false last_seen=- full_from=- full_to={} gap_ms=0 capped=false grouped_panes=0 registered_panes=0 reason=no_last_seen",
                        fetcher::format_stream(stream),
                        fetcher::format_time_short(now)
                    );
                    continue;
                }
            };

            let full_from = last_t.saturating_add(1);
            let full_to = now;
            let gap_ms = full_to.saturating_diff(full_from);
            let show_activity = gap_ms >= RECONNECT_ACTIVITY_THRESHOLD_MS;

            if gap_ms < MIN_BACKFILL_GAP_MS {
                log::info!(
                    "BACKFILL Decision | stream={} has_last_seen=true last_seen={} full_from={} full_to={} gap_ms={gap_ms} capped=false grouped_panes=0 registered_panes=0 reason=gap_too_small",
                    fetcher::format_stream(stream),
                    fetcher::format_time_short(last_t),
                    fetcher::format_time_short(full_from),
                    fetcher::format_time_short(full_to)
                );
                continue;
            }

            let capped = false;
            let (from, to) = (full_from, full_to);

            let mut grouped_panes: HashMap<(UnixMs, UnixMs), Vec<uuid::Uuid>> = HashMap::new();

            for (_window, _pane, pane_state) in self.iter_all_panes(main_window) {
                if !pane_state.matches_stream(stream) {
                    continue;
                }

                // Check if this pane supports fetched data for this stream type
                let fetch_range = match stream {
                    StreamKind::Trades { .. } => FetchRange::Trades(from, to),
                    StreamKind::Kline { .. } => FetchRange::Kline(from, to),
                    _ => continue,
                };

                if !pane_state.supports_fetch_range(&fetch_range) {
                    log::debug!(
                        "BACKFILL PaneSkip | stream={} pane={} content={} reason=unsupported_fetch_range",
                        fetcher::format_stream(stream),
                        fetcher::short_id(pane_state.unique_id()),
                        pane_state.content
                    );
                    continue;
                }

                let pane_id = pane_state.unique_id();
                let Some((missing_from, missing_to)) = (match stream {
                    StreamKind::Trades { .. } => pane_state.missing_trade_range(from, to),
                    StreamKind::Kline { .. } => Some((from, to)),
                    _ => None,
                }) else {
                    log::debug!(
                        "BACKFILL PaneCovered | stream={} pane={} requested_range={} reason=no_missing_range",
                        fetcher::format_stream(stream),
                        fetcher::short_id(pane_id),
                        fetcher::format_time_range(from, to)
                    );
                    continue;
                };

                log::debug!(
                    "BACKFILL PaneMissing | stream={} pane={} missing_range={}",
                    fetcher::format_stream(stream),
                    fetcher::short_id(pane_id),
                    fetcher::format_time_range(missing_from, missing_to)
                );
                grouped_panes
                    .entry((missing_from, missing_to))
                    .or_default()
                    .push(pane_id);
            }

            if grouped_panes.is_empty() {
                log::info!(
                    "BACKFILL Decision | stream={} has_last_seen=true last_seen={} full_from={} full_to={} gap_ms={gap_ms} capped={} grouped_panes=0 registered_panes=0 range={} reason=no_matching_pane_or_covered",
                    fetcher::format_stream(stream),
                    fetcher::format_time_short(last_t),
                    fetcher::format_time_short(full_from),
                    fetcher::format_time_short(full_to),
                    capped,
                    fetcher::format_time_range(from, to),
                );
                continue;
            }

            for ((missing_from, missing_to), pane_ids) in grouped_panes {
                let chunk_span_ms = match stream {
                    StreamKind::Trades { .. } => TRADE_BACKFILL_CHUNK_MS,
                    // Stay below the common 1,000-candle REST response limit
                    // and leave headroom for venue boundary differences.
                    StreamKind::Kline { timeframe, .. } => {
                        timeframe.to_milliseconds().saturating_mul(900).max(1)
                    }
                    _ => 1,
                };
                let chunk_ranges = {
                    let mut chunks = Vec::new();
                    let mut chunk_from = missing_from;
                    while chunk_from <= missing_to {
                        let chunk_to = UnixMs::new(
                            chunk_from
                                .as_u64()
                                .saturating_add(chunk_span_ms.saturating_sub(1))
                                .min(missing_to.as_u64()),
                        );
                        chunks.push((chunk_from, chunk_to));
                        if chunk_to == missing_to {
                            break;
                        }
                        chunk_from = chunk_to.saturating_add(1);
                    }
                    chunks
                };

                log::info!(
                    "BACKFILL Plan | stream={} missing_range={} chunks={} execution=sequential panes={}",
                    fetcher::format_stream(stream),
                    fetcher::format_time_range(missing_from, missing_to),
                    chunk_ranges.len(),
                    pane_ids.len()
                );

                let mut sequential_task: Task<Message> = Task::none();
                let mut queued_chunks = 0usize;

                for (chunk_from, chunk_to) in chunk_ranges {
                    let pending_overlap = self.pending_backfills.keys().find(|(s, ef, et)| {
                        *s == *stream && *ef <= chunk_to.as_u64() && *et >= chunk_from.as_u64()
                    });

                    if let Some((_, ef, et)) = pending_overlap {
                        log::info!(
                            "BACKFILL Decision | stream={} has_last_seen=true last_seen={} full_from={} full_to={} gap_ms={gap_ms} capped={} grouped_panes={} missing_range={} pending_overlap={} registered_panes=0 reason=already_pending",
                            fetcher::format_stream(stream),
                            fetcher::format_time_short(last_t),
                            fetcher::format_time_short(full_from),
                            fetcher::format_time_short(full_to),
                            capped,
                            pane_ids.len(),
                            fetcher::format_time_range(chunk_from, chunk_to),
                            fetcher::format_time_range(UnixMs::new(*ef), UnixMs::new(*et))
                        );
                        continue;
                    }

                    let dedupe_key = (*stream, chunk_from.as_u64(), chunk_to.as_u64());
                    self.pending_backfills
                        .insert(dedupe_key, std::time::Instant::now());

                    let req_id = uuid::Uuid::new_v4();
                    let fetch = match stream {
                        StreamKind::Trades { .. } => FetchRange::Trades(chunk_from, chunk_to),
                        StreamKind::Kline { .. } => FetchRange::Kline(chunk_from, chunk_to),
                        _ => continue,
                    };

                    // Backfill uses global pending_backfills for dedup; do NOT
                    // register the request in per-pane RequestHandlers.
                    let pane_id = pane_ids[0];
                    let target_panes = pane_ids.clone();

                    log::info!(
                        "BACKFILL Queued | stream={} has_last_seen=true last_seen={} full_from={} full_to={} gap_ms={gap_ms} capped={} grouped_panes={} req={} fetch_range={} reason={reason}",
                        fetcher::format_stream(stream),
                        fetcher::format_time_short(last_t),
                        fetcher::format_time_short(full_from),
                        fetcher::format_time_short(full_to),
                        capped,
                        target_panes.len(),
                        fetcher::short_id(req_id),
                        fetcher::format_time_range(chunk_from, chunk_to),
                    );

                    let ready_streams = vec![*stream];
                    let stream_kind = *stream;
                    let task = fetcher::request_fetch(
                        data_sources,
                        pane_id,
                        &ready_streams,
                        self.layout_id,
                        req_id,
                        fetch,
                        Some(*stream),
                        &mut |handle| {
                            // Store the abort handle to prevent the backfill
                            // task from being aborted while it is queued.
                            new_backfill_handles.push(handle);
                        },
                        0,
                    );

                    log::info!(
                        "BACKFILL FetchStart | req={} stream={} range={} target_panes={} execution=sequential",
                        fetcher::short_id(req_id),
                        fetcher::format_stream(&stream_kind),
                        fetcher::format_time_range(chunk_from, chunk_to),
                        target_panes.len()
                    );

                    sequential_task = sequential_task.chain(task.map(move |update| {
                        Message::BackfillFetchUpdate {
                            pane_ids: target_panes.clone(),
                            stream: stream_kind,
                            show_activity,
                            update,
                        }
                    }));
                    queued_chunks += 1;
                }

                if queued_chunks > 0 {
                    fetch_tasks.push(sequential_task);
                }
            }
        }

        // Store backfill handles to keep tasks alive.
        self.backfill_handles.extend(new_backfill_handles);

        Task::batch(fetch_tasks)
    }

    /// Records that a WS disconnect happened, deferring backfill until reconnect.
    /// At disconnect time the gap is tiny (last_seen → disconnect ≈ 87ms),
    /// so we wait for reconnect to compute the real offline duration.
    pub fn record_pending_disconnect_gaps(
        &mut self,
        streams: &[StreamKind],
        disconnected_at: UnixMs,
    ) {
        let mut newly_disconnected = Vec::new();
        for stream in streams {
            if matches!(stream, StreamKind::Depth { .. }) {
                log::info!(
                    "BACKFILL PendingGap | stream={} reason=depth_not_supported",
                    fetcher::format_stream(stream),
                );
                continue;
            }
            match self.last_live_t.get(stream) {
                Some(&last_t) => {
                    log::info!(
                        "BACKFILL PendingGap | stream={} last_seen={} disconnected_at={}",
                        fetcher::format_stream(stream),
                        fetcher::format_time_short(last_t),
                        fetcher::format_time_short(disconnected_at),
                    );
                    newly_disconnected.push((*stream, last_t));
                }
                None => {
                    log::info!(
                        "BACKFILL PendingGap | stream={} last_seen=- disconnected_at={} reason=no_last_seen",
                        fetcher::format_stream(stream),
                        fetcher::format_time_short(disconnected_at),
                    );
                }
            }
        }

        let pending = self
            .pending_disconnect
            .get_or_insert_with(|| PendingDisconnect {
                disconnected_at,
                stream_last_seen: HashMap::new(),
            });
        pending.disconnected_at = pending.disconnected_at.min(disconnected_at);
        for (stream, last_t) in newly_disconnected {
            pending
                .stream_last_seen
                .entry(stream)
                .and_modify(|existing| *existing = (*existing).min(last_t))
                .or_insert(last_t);
        }
    }

    /// Computes real offline gap from stored disconnect state and enqueues backfill.
    /// Called when WS reconnects — the gap is now `last_seen → reconnect_time`
    /// which accurately reflects the offline duration.
    pub fn execute_reconnect_backfill(
        &mut self,
        data_sources: &DataSources,
        main_window: window::Id,
        reconnect_time: UnixMs,
    ) -> Task<Message> {
        let Some(pending) = self.pending_disconnect.take() else {
            return Task::none();
        };

        let offline_ms = reconnect_time.saturating_diff(pending.disconnected_at);
        log::info!(
            "BACKFILL ReconnectGap | disconnected_at={} reconnect_time={} offline_ms={offline_ms}",
            fetcher::format_time_short(pending.disconnected_at),
            fetcher::format_time_short(reconnect_time),
        );

        let streams: Vec<StreamKind> = pending.stream_last_seen.keys().copied().collect();
        if streams.is_empty() {
            return Task::none();
        }

        // Delegate to existing backfill logic with reconnect_time as "now".
        // This computes gap = reconnect_time - last_seen for each stream,
        // which reflects the real offline duration.
        self.backfill_disconnected_streams(
            data_sources,
            main_window,
            &streams,
            &pending.stream_last_seen,
            reconnect_time,
            "reconnect_gap",
        )
    }
}

fn all_unique_streams(streams: &UniqueStreams) -> Vec<StreamKind> {
    let mut all = Vec::new();
    all.extend(streams.depth_streams(None).into_iter().map(
        |(ticker_info, depth_aggr, push_freq)| StreamKind::Depth {
            ticker_info,
            depth_aggr,
            push_freq,
        },
    ));
    all.extend(
        streams
            .trade_streams(None)
            .into_iter()
            .map(|ticker_info| StreamKind::Trades { ticker_info }),
    );
    all.extend(
        streams
            .kline_streams(None)
            .into_iter()
            .map(|(ticker_info, timeframe)| StreamKind::Kline {
                ticker_info,
                timeframe,
            }),
    );
    all
}

impl From<fetcher::FetchUpdate> for Message {
    fn from(update: fetcher::FetchUpdate) -> Self {
        match update {
            fetcher::FetchUpdate::Status { pane_id, status } => match status {
                fetcher::FetchTaskStatus::Loading(info) => Message::ChangePaneStatus(
                    pane_id,
                    pane::Status::Loading {
                        info,
                        source: pane::LoadingSource::Historical,
                    },
                ),
                fetcher::FetchTaskStatus::Completed {
                    req_id,
                    fetch,
                    trade_outcome,
                } => Message::FetchCompleted {
                    pane_id,
                    req_id,
                    fetch,
                    trade_outcome,
                },
            },
            fetcher::FetchUpdate::Data {
                layout_id,
                pane_id,
                stream,
                data,
            } => Message::DistributeFetchedData {
                layout_id,
                pane_id,
                stream,
                data,
            },
            fetcher::FetchUpdate::Error {
                pane_id,
                error,
                req_id,
                fetch,
            } => Message::FetchFailed {
                pane_id,
                error,
                req_id,
                fetch,
            },
        }
    }
}

/// Returns a short type label for fetched data (for logging).
fn data_summary_type(data: &FetchedData) -> &'static str {
    match data {
        FetchedData::Trades { .. } => "Trades",
        FetchedData::BubbleSummary { .. } => "BubbleSummary",
        FetchedData::Klines { .. } => "Klines",
        FetchedData::OI { .. } => "OI",
    }
}

#[cfg(test)]
mod gex_liquidity_reference_tests {
    use super::*;
    use data::layout::pane::LinkGroup;
    use exchange::{Ticker, adapter::Exchange, options::OptionsUnderlying};

    fn ticker(symbol: &str, exchange: Exchange) -> TickerInfo {
        TickerInfo::new(Ticker::new(symbol, exchange), 0.01, 0.001, None)
    }

    #[test]
    fn compatible_persisted_reference_is_conserved() {
        let persisted = ticker("BTCUSDT", Exchange::BinanceLinear);
        let other = ticker("BTCUSDT", Exchange::BybitLinear);
        assert_eq!(
            resolve_gex_liquidity_reference(
                OptionsUnderlying::Btc,
                false,
                None,
                Some(persisted),
                Some(pane::GexLiquidityReferenceSource::Persisted),
                &[(None, other)],
            ),
            Ok((persisted, pane::GexLiquidityReferenceSource::Persisted))
        );
    }

    #[test]
    fn compatible_link_group_ticker_has_priority_and_incompatible_is_ignored() {
        let btc = ticker("BTCUSDT", Exchange::BybitLinear);
        let eth = ticker("ETHUSDT", Exchange::BinanceLinear);
        assert_eq!(
            resolve_gex_liquidity_reference(
                OptionsUnderlying::Btc,
                true,
                Some(LinkGroup::A),
                None,
                None,
                &[(Some(LinkGroup::A), eth), (Some(LinkGroup::A), btc)],
            ),
            Ok((btc, pane::GexLiquidityReferenceSource::LinkGroup))
        );
    }

    #[test]
    fn single_dashboard_candidate_is_inferred_but_multiple_are_ambiguous() {
        let first = ticker("BTCUSDT", Exchange::BinanceLinear);
        let second = ticker("BTCUSDT", Exchange::BybitLinear);
        assert_eq!(
            resolve_gex_liquidity_reference(
                OptionsUnderlying::Btc,
                false,
                None,
                None,
                None,
                &[(None, first)],
            ),
            Ok((first, pane::GexLiquidityReferenceSource::SingleCompatible))
        );
        assert_eq!(
            resolve_gex_liquidity_reference(
                OptionsUnderlying::Btc,
                false,
                None,
                None,
                None,
                &[(None, first), (None, second)],
            ),
            Err(GexReferenceMissingReason::AmbiguousTickers)
        );
    }

    #[test]
    fn incompatible_persisted_reference_is_not_reused() {
        let eth = ticker("ETHUSDT", Exchange::BinanceLinear);
        assert_eq!(
            resolve_gex_liquidity_reference(
                OptionsUnderlying::Btc,
                false,
                None,
                Some(eth),
                Some(pane::GexLiquidityReferenceSource::Persisted),
                &[],
            ),
            Err(GexReferenceMissingReason::NoCompatibleTicker)
        );
    }

    #[test]
    fn linked_ticker_change_updates_reference_and_replaces_stream() {
        let first = ticker("BTCUSDT", Exchange::BinanceLinear);
        let second = ticker("BTCUSDT", Exchange::BybitLinear);
        let mut state = pane::State::from_config(
            pane::Content::Gex {
                chart: Some(crate::chart::gex::GexChart::new(
                    OptionsUnderlying::Btc,
                    None,
                    None,
                )),
                underlying: OptionsUnderlying::Btc,
                liquidity_reference: None,
                liquidity_reference_source: None,
                unsupported: false,
            },
            Vec::new(),
            data::layout::pane::Settings::default(),
            Some(LinkGroup::A),
        );
        for expected in [first, second] {
            let (reference, source) = resolve_gex_liquidity_reference(
                OptionsUnderlying::Btc,
                true,
                Some(LinkGroup::A),
                state
                    .gex_liquidity_resolution()
                    .and_then(|(_, _, reference, _)| reference),
                state
                    .gex_liquidity_resolution()
                    .and_then(|(_, _, _, source)| source),
                &[(Some(LinkGroup::A), expected)],
            )
            .expect("compatible linked ticker");
            state.set_gex_liquidity_reference(Some(reference), Some(source));
            let depth = state
                .streams
                .ready_iter()
                .expect("ready")
                .filter_map(|stream| match stream {
                    StreamKind::Depth { ticker_info, .. } => Some(*ticker_info),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(depth, vec![expected]);
        }
    }
}

#[cfg(test)]
mod startup_load_status_tests {
    use super::*;

    #[test]
    fn blocked_streams_do_not_prevent_startup() {
        let mut pane = pane::State::from_config(
            pane::Content::TimeAndSales(None),
            Vec::new(),
            data::layout::pane::Settings::default(),
            None,
        );
        pane.streams = ResolvedStream::Blocked {
            streams: Vec::new(),
            reason: "metadata unavailable".to_string(),
            last_attempt: None,
        };

        let dashboard =
            Dashboard::from_config(Configuration::Pane(pane), Vec::new(), uuid::Uuid::new_v4());
        let status = dashboard.startup_load_status(window::Id::unique());

        assert_eq!(status.pane_count, 1);
        assert_eq!(status.initializing_panes, 0);
        assert_eq!(status.loading_panes, 0);
        assert!(status.is_ready());
    }
}
