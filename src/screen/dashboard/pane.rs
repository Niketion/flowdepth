use crate::{
    chart::{
        self, comparison::ComparisonChart, gex::GexChart, heatmap::HeatmapChart, kline::KlineChart,
    },
    connector::{
        ResolvedStream,
        fetcher::{self, FetchSpec, InfoKind},
    },
    modal::{
        self, ModifierKind,
        pane::{
            Modal,
            mini_tickers_list::MiniPanel,
            settings::{
                comparison_cfg_view, gex_cfg_view, heatmap_cfg_view, heatmap_shader_cfg_view,
                kline_cfg_view,
            },
            stack_modal,
        },
    },
    screen::dashboard::{
        panel::{self, ladder::Ladder, timeandsales::TimeAndSales},
        tickers_table::TickersTable,
    },
    style::{self, Icon, icon_text},
    widget::{
        self, button_with_tooltip, chart::heatmap::HeatmapShader, column_drag, link_group_button,
        toast::Toast,
    },
    window::{self, Window},
};
use data::{
    UserTimezone,
    chart::{
        Basis, ViewConfig,
        heatmap::HeatmapStudy,
        indicator::{HeatmapIndicator, Indicator, KlineIndicator, UiIndicator},
    },
    layout::pane::{ContentKind, LinkGroup, PaneSetup, Settings, VisualConfig},
    stream::PersistStreamKind,
};
use exchange::{
    Kline, OpenInterest, PushFrequency, StreamPairKind, TickMultiplier, TickerInfo, Timeframe,
    UnixMs,
    adapter::{MarketKind, StreamKind, StreamTicksize},
    unit::PriceStep,
};
use iced::{
    Alignment, Element, Length, Renderer, Theme, padding,
    widget::{
        button, center, column, container, pane_grid, pick_list, responsive, row, rule, text,
        tooltip,
    },
};
use std::time::Instant;

#[derive(Debug, Clone)]
pub enum Effect {
    RefreshStreams,
    RequestFetch(Vec<FetchSpec>),
    SwitchTickersInGroup(TickerInfo),
    FocusWidget(iced::widget::Id),
}

#[derive(Debug, Default, Clone, PartialEq)]
pub enum Status {
    #[default]
    Ready,
    Loading {
        info: InfoKind,
        source: LoadingSource,
    },
    Stale(String),
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum LoadingSource {
    #[default]
    Historical,
    Reconnect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GexLiquidityReferenceSource {
    Persisted,
    LinkGroup,
    SingleCompatible,
    Manual,
}

pub enum Action {
    Chart(chart::Action),
    Panel(panel::Action),
    ResolveStreams(Vec<PersistStreamKind>),
    ResolveContent,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Message {
    PaneClicked(pane_grid::Pane),
    PaneResized(pane_grid::ResizeEvent),
    PaneDragged(pane_grid::DragEvent),
    ClosePane(pane_grid::Pane),
    SplitPane(pane_grid::Axis, pane_grid::Pane),
    MaximizePane(pane_grid::Pane),
    Restore,
    ReplacePane(pane_grid::Pane),
    Popout,
    Merge,
    SwitchLinkGroup(pane_grid::Pane, Option<LinkGroup>),
    VisualConfigChanged(pane_grid::Pane, VisualConfig, bool),
    PaneEvent(pane_grid::Pane, Event),
}

#[derive(Debug, Clone)]
pub enum Event {
    ShowModal(Modal),
    HideModal,
    ContentSelected(ContentKind),
    ChartInteraction(super::chart::Message),
    PanelInteraction(super::panel::Message),
    ToggleIndicator(UiIndicator),
    DeleteNotification(usize),
    ReorderIndicator(column_drag::DragEvent),
    ClusterKindSelected(data::chart::kline::ClusterKind),
    ClusterScalingSelected(data::chart::kline::ClusterScaling),
    StudyConfigurator(modal::pane::settings::study::StudyMessage),
    StreamModifierChanged(modal::stream::Message),
    ComparisonChartInteraction(super::chart::comparison::Message),
    GexChartInteraction(crate::chart::gex::Message),
    GexAssetSelected(GexChartAsset),
    HeatmapShaderInteraction(crate::widget::chart::heatmap::Message),
    MiniTickersListInteraction(modal::pane::mini_tickers_list::Message),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GexChartAsset {
    Btc,
    Eth,
    Xau,
}

impl GexChartAsset {
    const ALL: [Self; 3] = [Self::Btc, Self::Eth, Self::Xau];

    const fn underlying(self) -> exchange::options::OptionsUnderlying {
        match self {
            Self::Btc => exchange::options::OptionsUnderlying::Btc,
            Self::Eth => exchange::options::OptionsUnderlying::Eth,
            Self::Xau => exchange::options::OptionsUnderlying::Gld,
        }
    }

    const fn from_underlying(value: exchange::options::OptionsUnderlying) -> Self {
        match value {
            exchange::options::OptionsUnderlying::Btc => Self::Btc,
            exchange::options::OptionsUnderlying::Eth => Self::Eth,
            exchange::options::OptionsUnderlying::Gld => Self::Xau,
        }
    }
}

impl std::fmt::Display for GexChartAsset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Btc => "BTC",
            Self::Eth => "ETH",
            Self::Xau => "XAU",
        })
    }
}

pub struct State {
    id: uuid::Uuid,
    pub modal: Option<Modal>,
    pub content: Content,
    pub settings: Settings,
    pub notifications: Vec<Toast>,
    pub streams: ResolvedStream,
    pub status: Status,
    pub link_group: Option<LinkGroup>,
    gex_liquidity_missing_logged: bool,
}

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_config(
        content: Content,
        streams: Vec<PersistStreamKind>,
        settings: Settings,
        link_group: Option<LinkGroup>,
    ) -> Self {
        let mut state = Self {
            content,
            settings,
            streams: ResolvedStream::waiting(streams),
            link_group,
            ..Default::default()
        };
        if matches!(state.content, Content::Gex { .. }) {
            state.streams = ResolvedStream::Ready(Vec::new());
            state.reconcile_gex_liquidity_stream();
        }
        state
    }

    pub fn stream_pair(&self) -> Option<TickerInfo> {
        if matches!(self.content, Content::Gex { .. }) {
            return None;
        }
        self.streams.find_ready_map(|stream| match stream {
            StreamKind::Kline { ticker_info, .. } => Some(*ticker_info),
            StreamKind::Depth { ticker_info, .. } => Some(*ticker_info),
            StreamKind::Trades { ticker_info, .. } => Some(*ticker_info),
        })
    }

    pub fn stream_pair_kind(&self) -> Option<StreamPairKind> {
        if matches!(self.content, Content::Gex { .. }) {
            return None;
        }
        let ready_streams = self.streams.ready_iter()?;
        let mut unique = vec![];

        for stream in ready_streams {
            let ticker = stream.ticker_info();
            if !unique.contains(&ticker) {
                unique.push(ticker);
            }
        }

        match unique.len() {
            0 => None,
            1 => Some(StreamPairKind::SingleSource(unique[0])),
            _ => Some(StreamPairKind::MultiSource(unique)),
        }
    }

    /// Keep the optional candlestick trade stream in sync with studies that
    /// consume trades. This also fixes runtime enable/disable without requiring
    /// an application restart.
    pub fn reconcile_candlestick_trade_stream(&mut self) {
        let Some(ticker_info) = self.stream_pair() else {
            return;
        };
        let needs_trades = match &self.content {
            Content::Kline {
                indicators,
                chart,
                kind: data::chart::KlineChartKind::Candles,
                ..
            } => {
                indicators
                    .iter()
                    .any(|indicator| indicator.requires_trades(ticker_info.exchange()))
                    || chart
                        .as_ref()
                        .is_some_and(|chart| chart.has_fixed_volume_profiles())
            }
            _ => return,
        };
        let ResolvedStream::Ready(streams) = &mut self.streams else {
            return;
        };
        let trade_stream = StreamKind::Trades { ticker_info };
        if needs_trades {
            if !streams.contains(&trade_stream) {
                streams.push(trade_stream);
            }
        } else {
            streams.retain(|stream| !matches!(stream, StreamKind::Trades { .. }));
        }
    }

    pub fn reconcile_gex_liquidity_stream(&mut self) {
        let Content::Gex {
            chart,
            liquidity_reference,
            ..
        } = &mut self.content
        else {
            return;
        };
        let (enabled, reference) = chart
            .as_ref()
            .map_or((false, *liquidity_reference), |chart| {
                (
                    chart.config().show_gamma_liquidity_panel
                        || chart.underlying() == exchange::options::OptionsUnderlying::Gld,
                    chart.liquidity_reference().or(*liquidity_reference),
                )
            });
        *liquidity_reference = reference;
        if let Some(chart) = chart {
            chart.set_liquidity_reference(reference);
        }
        let ResolvedStream::Ready(streams) = &mut self.streams else {
            return;
        };
        streams.retain(|stream| !matches!(stream, StreamKind::Depth { .. }));
        if enabled && let Some(ticker_info) = reference {
            let setup = PaneSetup::new(ContentKind::GexChart, ticker_info, None, None, None);
            streams.push(StreamKind::Depth {
                ticker_info,
                depth_aggr: setup.depth_aggr,
                push_freq: PushFrequency::ServerDefault,
            });
        }
    }

    pub fn gex_liquidity_resolution(
        &self,
    ) -> Option<(
        exchange::options::OptionsUnderlying,
        bool,
        Option<TickerInfo>,
        Option<GexLiquidityReferenceSource>,
    )> {
        let Content::Gex {
            chart,
            underlying,
            liquidity_reference,
            liquidity_reference_source,
            ..
        } = &self.content
        else {
            return None;
        };
        Some((
            *underlying,
            chart
                .as_ref()
                .is_some_and(|chart| chart.config().liquidity_reference_follow_link_group),
            chart
                .as_ref()
                .and_then(GexChart::liquidity_reference)
                .or(*liquidity_reference),
            *liquidity_reference_source,
        ))
    }

    pub fn set_gex_liquidity_reference(
        &mut self,
        reference: Option<TickerInfo>,
        source: Option<GexLiquidityReferenceSource>,
    ) -> bool {
        let Content::Gex {
            chart,
            liquidity_reference,
            liquidity_reference_source,
            ..
        } = &mut self.content
        else {
            return false;
        };
        let changed = *liquidity_reference != reference || *liquidity_reference_source != source;
        *liquidity_reference = reference;
        *liquidity_reference_source = source;
        if let Some(chart) = chart {
            chart.set_liquidity_reference(reference);
        }
        if changed {
            self.gex_liquidity_missing_logged = false;
        }
        self.reconcile_gex_liquidity_stream();
        changed
    }

    pub fn log_gex_liquidity_missing_once(&mut self, reason: &str) {
        if self.gex_liquidity_missing_logged {
            return;
        }
        let Some((underlying, ..)) = self.gex_liquidity_resolution() else {
            return;
        };
        log::warn!(
            "GEX LiquidityReferenceMissing underlying={} reason={reason}",
            underlying
        );
        self.gex_liquidity_missing_logged = true;
    }

    pub fn set_content_and_streams(
        &mut self,
        tickers: Vec<TickerInfo>,
        kind: ContentKind,
    ) -> Vec<StreamKind> {
        if !(self.content.kind() == kind) {
            self.settings.selected_basis = None;
            self.settings.tick_multiply = None;
        }

        let base_ticker = tickers[0];
        if kind == ContentKind::GexChart {
            let existing_reference = match &self.content {
                Content::Gex {
                    liquidity_reference,
                    chart,
                    ..
                } => chart
                    .as_ref()
                    .and_then(GexChart::liquidity_reference)
                    .or(*liquidity_reference),
                _ => None,
            };
            let resolved = exchange::options::resolve_gex_source(base_ticker.ticker);
            if resolved.is_none() {
                log::warn!(
                    "GEX UnsupportedUnderlying symbol={}",
                    base_ticker.ticker.display_symbol_and_type().0
                );
            }
            let underlying = resolved
                .map(exchange::options::GexSource::source_underlying)
                .unwrap_or(exchange::options::OptionsUnderlying::Btc);
            let config = self
                .settings
                .visual_config
                .as_ref()
                .and_then(VisualConfig::gex);
            let liquidity_reference = if config
                .unwrap_or_default()
                .liquidity_reference_follow_link_group
            {
                Some(base_ticker)
            } else {
                existing_reference
                    .filter(|reference| {
                        exchange::options::resolve_gex_source(reference.ticker)
                            .is_some_and(|source| source.source_underlying() == underlying)
                    })
                    .or(Some(base_ticker))
            };
            self.content = Content::Gex {
                chart: resolved.map(|_| GexChart::new(underlying, config, liquidity_reference)),
                underlying,
                liquidity_reference,
                liquidity_reference_source: liquidity_reference
                    .map(|_| GexLiquidityReferenceSource::Manual),
                unsupported: resolved.is_none(),
            };
            self.streams = ResolvedStream::Ready(Vec::new());
            self.reconcile_gex_liquidity_stream();
            return self
                .streams
                .ready_iter()
                .map(|streams| streams.copied().collect())
                .unwrap_or_default();
        }
        let prev_base_ticker = self.stream_pair();

        let derived_plan = PaneSetup::new(
            kind,
            base_ticker,
            prev_base_ticker,
            self.settings.selected_basis,
            self.settings.tick_multiply,
        );

        self.settings.selected_basis = derived_plan.basis;
        self.settings.tick_multiply = derived_plan.tick_multiplier;

        let (content, streams) = {
            let kline_stream = |ti: TickerInfo, tf: Timeframe| StreamKind::Kline {
                ticker_info: ti,
                timeframe: tf,
            };
            let depth_stream = |derived_plan: &PaneSetup| StreamKind::Depth {
                ticker_info: derived_plan.ticker_info,
                depth_aggr: derived_plan.depth_aggr,
                push_freq: derived_plan.push_freq,
            };
            let trades_stream = |derived_plan: &PaneSetup| StreamKind::Trades {
                ticker_info: derived_plan.ticker_info,
            };
            let trade_overlay_enabled = matches!(
                &self.content,
                Content::Kline { indicators, drawings, chart, .. }
                    if indicators.iter().any(|indicator| indicator.requires_trades(
                        derived_plan.ticker_info.exchange()
                    )) || chart.as_ref().is_some_and(|chart| chart.has_fixed_volume_profiles())
                        || drawings.iter().any(|drawing| matches!(
                            drawing.geometry,
                            data::chart::kline::drawing::DrawingGeometry::FixedRangeVolumeProfile { .. }
                        ))
            );

            match kind {
                ContentKind::HeatmapChart => {
                    let content = Content::new_heatmap(
                        &self.content,
                        derived_plan.ticker_info,
                        &self.settings,
                        derived_plan.price_step,
                    );

                    let streams = vec![depth_stream(&derived_plan), trades_stream(&derived_plan)];

                    (content, streams)
                }
                ContentKind::FootprintChart => {
                    let content = Content::new_kline(
                        kind,
                        &self.content,
                        derived_plan.ticker_info,
                        &self.settings,
                        derived_plan.price_step,
                    );

                    let streams = by_basis_default(
                        derived_plan.basis,
                        Timeframe::M5,
                        |tf| {
                            vec![
                                trades_stream(&derived_plan),
                                kline_stream(derived_plan.ticker_info, tf),
                            ]
                        },
                        || vec![trades_stream(&derived_plan)],
                    );

                    (content, streams)
                }
                ContentKind::CandlestickChart => {
                    let content = {
                        let base_ticker = tickers[0];
                        Content::new_kline(
                            kind,
                            &self.content,
                            derived_plan.ticker_info,
                            &self.settings,
                            base_ticker.min_ticksize.into(),
                        )
                    };

                    let time_basis_stream = |tf| {
                        let mut streams = vec![kline_stream(derived_plan.ticker_info, tf)];
                        if trade_overlay_enabled {
                            streams.push(trades_stream(&derived_plan));
                        }
                        streams
                    };
                    let tick_basis_stream = || {
                        let depth_aggr = derived_plan
                            .ticker_info
                            .exchange()
                            .stream_ticksize(None, TickMultiplier(50));
                        let temp = PaneSetup {
                            depth_aggr,
                            ..derived_plan
                        };
                        vec![trades_stream(&temp)]
                    };

                    let streams = by_basis_default(
                        derived_plan.basis,
                        Timeframe::M15,
                        time_basis_stream,
                        tick_basis_stream,
                    );

                    (content, streams)
                }
                ContentKind::TimeAndSales => {
                    let config = self
                        .settings
                        .visual_config
                        .clone()
                        .and_then(|cfg| cfg.time_and_sales());
                    let content = Content::TimeAndSales(Some(TimeAndSales::new(
                        config,
                        derived_plan.ticker_info,
                    )));

                    let temp = PaneSetup {
                        push_freq: exchange::PushFrequency::ServerDefault,
                        ..derived_plan
                    };

                    let streams = vec![trades_stream(&temp)];

                    (content, streams)
                }
                ContentKind::Ladder => {
                    let config = self
                        .settings
                        .visual_config
                        .clone()
                        .and_then(|cfg| cfg.ladder());
                    let content = Content::Ladder(Some(Ladder::new(
                        config,
                        derived_plan.ticker_info,
                        derived_plan.price_step,
                    )));

                    let streams = vec![depth_stream(&derived_plan), trades_stream(&derived_plan)];

                    (content, streams)
                }
                ContentKind::ComparisonChart => {
                    let config = self
                        .settings
                        .visual_config
                        .clone()
                        .and_then(|cfg| cfg.comparison());

                    let timeframe = {
                        let supports = |tf| {
                            tickers
                                .iter()
                                .all(|ti| ti.exchange().supports_kline_timeframe(tf))
                        };

                        if let Some(tf) = derived_plan.basis.and_then(|basis| match basis {
                            Basis::Time(tf) => Some(tf),
                            Basis::Tick(_) => None,
                        }) && supports(tf)
                        {
                            tf
                        } else {
                            let fallback = Timeframe::M15;
                            if supports(fallback) {
                                fallback
                            } else {
                                Timeframe::KLINE
                                    .iter()
                                    .copied()
                                    .find(|tf| supports(*tf))
                                    .unwrap_or(fallback)
                            }
                        }
                    };

                    let basis = Basis::Time(timeframe);
                    self.settings.selected_basis = Some(basis);
                    let content =
                        Content::Comparison(Some(ComparisonChart::new(basis, &tickers, config)));

                    let streams = tickers
                        .iter()
                        .copied()
                        .map(|ti| kline_stream(ti, timeframe))
                        .collect();

                    (content, streams)
                }
                ContentKind::ShaderHeatmap => {
                    let basis = derived_plan
                        .basis
                        .unwrap_or(Basis::default_heatmap_time(Some(derived_plan.ticker_info)));

                    let (studies, indicators) = if let Content::ShaderHeatmap {
                        chart,
                        indicators,
                        studies,
                    } = &self.content
                    {
                        (
                            chart
                                .as_ref()
                                .map_or(studies.clone(), |c| c.studies.clone()),
                            indicators.clone(),
                        )
                    } else {
                        (
                            vec![HeatmapStudy::VolumeProfile(
                                data::chart::heatmap::ProfileKind::default(),
                            )],
                            vec![HeatmapIndicator::Volume],
                        )
                    };

                    let config = self
                        .settings
                        .visual_config
                        .clone()
                        .and_then(|cfg| cfg.heatmap());

                    let content = Content::ShaderHeatmap {
                        chart: Some(Box::new(HeatmapShader::new(
                            basis,
                            derived_plan.price_step,
                            base_ticker,
                            studies.clone(),
                            indicators.clone(),
                            config,
                        ))),
                        studies,
                        indicators,
                    };

                    let streams = vec![depth_stream(&derived_plan), trades_stream(&derived_plan)];

                    (content, streams)
                }
                ContentKind::Starter => unreachable!(),
                ContentKind::GexChart => unreachable!("handled before stream planning"),
            }
        };

        self.content = content;
        self.streams = ResolvedStream::Ready(streams.clone());
        let final_trade_overlays = matches!(
            &self.content,
            Content::Kline { indicators, drawings, chart, .. }
                if indicators.iter().any(|indicator| indicator.requires_trades(
                    base_ticker.exchange()
                )) || chart.as_ref().is_some_and(|chart| chart.has_fixed_volume_profiles())
                    || drawings.iter().any(|drawing| matches!(
                        drawing.geometry,
                        data::chart::kline::drawing::DrawingGeometry::FixedRangeVolumeProfile { .. }
                    ))
        );

        log::info!(
            "STREAM PaneContent | pane={} content={kind:?} base_ticker={} basis={:?} tick_multiplier={:?} streams={}",
            fetcher::short_id(self.id),
            fetcher::format_ticker(&base_ticker),
            self.settings.selected_basis,
            self.settings.tick_multiply,
            fetcher::format_streams(&streams)
        );
        if matches!(kind, ContentKind::CandlestickChart) {
            log::debug!(
                "STREAM Candlestick | pane={} trade_overlay_enabled={} trades_stream_included={}",
                fetcher::short_id(self.id),
                final_trade_overlays,
                streams
                    .iter()
                    .any(|stream| matches!(stream, StreamKind::Trades { .. }))
            );
        }
        if matches!(kind, ContentKind::FootprintChart) {
            log::debug!(
                "STREAM Footprint | pane={} requires=Trades+Kline streams={}",
                fetcher::short_id(self.id),
                fetcher::format_streams(&streams)
            );
        }

        streams
    }

    pub fn insert_hist_oi(&mut self, req_id: Option<uuid::Uuid>, oi: &[OpenInterest]) {
        match &mut self.content {
            Content::Kline { chart, .. } => {
                let Some(chart) = chart else {
                    panic!("Kline chart wasn't initialized when inserting open interest");
                };
                chart.insert_open_interest(req_id, oi);
            }
            _ => {
                log::error!("pane content not candlestick");
            }
        }
    }

    pub fn insert_hist_klines(
        &mut self,
        req_id: Option<uuid::Uuid>,
        timeframe: Timeframe,
        ticker_info: TickerInfo,
        klines: &[Kline],
    ) {
        match &mut self.content {
            Content::Kline {
                chart, indicators, ..
            } => {
                let Some(chart) = chart else {
                    panic!("chart wasn't initialized when inserting klines");
                };

                if let Some(id) = req_id {
                    if chart.basis() != Basis::Time(timeframe) {
                        log::warn!(
                            "KLINE StaleFetch | pane={} req={} fetched_timeframe={:?} current_basis={:?} count={} reason=timeframe_mismatch",
                            fetcher::short_id(self.id),
                            fetcher::short_id(id),
                            timeframe,
                            chart.basis(),
                            klines.len()
                        );
                        return;
                    }
                    chart.insert_hist_klines(id, klines);
                } else {
                    let (raw_trades, tick_size) = (chart.raw_trades(), chart.tick_size());
                    let layout = chart.chart_layout();
                    let visual_config = chart.visual_config();
                    let drawings = chart.drawings();

                    *chart = KlineChart::new(
                        layout,
                        Basis::Time(timeframe),
                        tick_size,
                        klines,
                        raw_trades,
                        indicators,
                        ticker_info,
                        chart.kind(),
                        Some(visual_config),
                    );
                    chart.set_drawings(drawings);
                }
            }
            Content::Comparison(chart) => {
                let Some(chart) = chart else {
                    panic!("Comparison chart wasn't initialized when inserting klines");
                };

                if let Some(id) = req_id {
                    if chart.timeframe != timeframe {
                        log::warn!(
                            "KLINE StaleFetch | pane={} req={} fetched_timeframe={:?} current_timeframe={:?} count={} reason=comparison_timeframe_mismatch",
                            fetcher::short_id(self.id),
                            fetcher::short_id(id),
                            timeframe,
                            chart.timeframe,
                            klines.len()
                        );
                        return;
                    }
                    chart.insert_history(id, ticker_info, klines);
                } else {
                    *chart = ComparisonChart::new(
                        Basis::Time(timeframe),
                        &[ticker_info],
                        Some(chart.serializable_config()),
                    );
                }
            }
            _ => {
                log::error!("pane content not candlestick or footprint");
            }
        }
    }

    pub fn register_backfill_request(
        &mut self,
        req_id: uuid::Uuid,
        fetch: crate::connector::fetcher::FetchRange,
    ) -> bool {
        let registered = match &mut self.content {
            Content::Kline {
                chart: Some(chart), ..
            } => chart.register_backfill_request(req_id, fetch),
            _ => false,
        };
        log::debug!(
            "BACKFILL RegisterPane | pane={} content={} req={} fetch={} registered={registered}",
            fetcher::short_id(self.id),
            self.content,
            fetcher::short_id(req_id),
            fetcher::format_fetch_range(&fetch)
        );
        registered
    }

    pub fn missing_trade_range(&self, from: UnixMs, to: UnixMs) -> Option<(UnixMs, UnixMs)> {
        let missing = match &self.content {
            Content::Kline {
                chart: Some(chart), ..
            } => chart.missing_trade_range(from, to),
            _ => Some((from, to)),
        };
        log::trace!(
            "BACKFILL MissingTradeRange | pane={} requested_range={} returned_range={}",
            fetcher::short_id(self.id),
            fetcher::format_time_range(from, to),
            missing.map_or("-".to_string(), |(from, to)| fetcher::format_time_range(
                from, to
            ))
        );
        missing
    }

    pub fn mark_fetch_failed(&mut self, req_id: uuid::Uuid, error: String) {
        if let Content::Kline {
            chart: Some(chart), ..
        } = &mut self.content
        {
            chart.mark_request_failed(req_id, error);
        }
    }

    /// Mark a backfill as completed without going through per-pane RequestHandler.
    /// Backfill requests are tracked globally (pending_backfills), not per-pane.
    pub fn mark_backfill_completed(
        &mut self,
        fetch: Option<crate::connector::fetcher::FetchRange>,
        trade_outcome: crate::connector::fetcher::TradeFetchOutcome,
    ) {
        if let Content::Kline {
            chart: Some(chart), ..
        } = &mut self.content
        {
            chart.complete_backfill(fetch, trade_outcome);
        }
        self.status = Status::Ready;
    }

    fn has_stream(&self) -> bool {
        match &self.streams {
            ResolvedStream::Ready(streams) => !streams.is_empty(),
            ResolvedStream::Waiting { streams, .. } => !streams.is_empty(),
            ResolvedStream::Blocked { streams, .. } => !streams.is_empty(),
        }
    }

    pub fn view<'a>(
        &'a self,
        id: pane_grid::Pane,
        panes: usize,
        is_focused: bool,
        maximized: bool,
        window: window::Id,
        main_window: &'a Window,
        timezone: UserTimezone,
        tickers_table: &'a TickersTable,
        allow_native_popout: bool,
    ) -> pane_grid::Content<'a, Message, Theme, Renderer> {
        let mut top_left_buttons = if Content::Starter == self.content {
            row![]
        } else {
            row![link_group_button(id, self.link_group, |id| {
                Message::PaneEvent(id, Event::ShowModal(Modal::LinkGroup))
            })]
        };

        if let Content::Gex { underlying, .. } = &self.content {
            let provider = if *underlying == exchange::options::OptionsUnderlying::Gld {
                "GLD Proxy QuantWheel"
            } else {
                "Deribit"
            };
            top_left_buttons = top_left_buttons.push(
                row![
                    pick_list(
                        GexChartAsset::ALL,
                        Some(GexChartAsset::from_underlying(*underlying)),
                        move |asset| Message::PaneEvent(id, Event::GexAssetSelected(asset)),
                    )
                    .text_size(crate::style::text_size::SECTION)
                    .width(Length::Fixed(82.0)),
                    text(provider)
                        .size(crate::style::text_size::SMALL)
                        .align_y(Alignment::Center),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            );
        } else if let Some(kind) = self.stream_pair_kind() {
            let (base_ti, extra) = match kind {
                StreamPairKind::MultiSource(list) => (list[0], list.len().saturating_sub(1)),
                StreamPairKind::SingleSource(ti) => (ti, 0),
            };

            let exchange_icon = icon_text(style::venue_icon(base_ti.ticker.exchange.venue()), 14);
            let mut label = {
                let symbol = base_ti.ticker.display_symbol_and_type().0;
                match base_ti.ticker.market_type() {
                    MarketKind::Spot => symbol,
                    MarketKind::LinearPerps | MarketKind::InversePerps => symbol + " PERP",
                }
            };
            if extra > 0 {
                label = format!("{label} +{extra}");
            }

            let content = row![
                exchange_icon.align_y(Alignment::Center).line_height(1.4),
                text(label)
                    .size(crate::style::text_size::SECTION)
                    .align_y(Alignment::Center)
                    .line_height(1.4)
            ]
            .align_y(Alignment::Center)
            .spacing(4);

            let tickers_list_btn = button(content)
                .on_press(Message::PaneEvent(
                    id,
                    Event::ShowModal(Modal::MiniTickersList(MiniPanel::new())),
                ))
                .style(|theme, status| {
                    style::button::modifier(
                        theme,
                        status,
                        !matches!(self.modal, Some(Modal::MiniTickersList(_))),
                    )
                })
                .height(widget::PANE_CONTROL_BTN_HEIGHT);

            top_left_buttons = top_left_buttons.push(tickers_list_btn);
        } else if !matches!(self.content, Content::Starter) && !self.has_stream() {
            let content = row![
                text("Choose a ticker")
                    .size(crate::style::text_size::EMPHASIS)
                    .align_y(Alignment::Center)
                    .line_height(1.4)
            ]
            .align_y(Alignment::Center);

            let tickers_list_btn = button(content)
                .on_press(Message::PaneEvent(
                    id,
                    Event::ShowModal(Modal::MiniTickersList(MiniPanel::new())),
                ))
                .style(|theme, status| {
                    style::button::modifier(
                        theme,
                        status,
                        !matches!(self.modal, Some(Modal::MiniTickersList(_))),
                    )
                })
                .height(widget::PANE_CONTROL_BTN_HEIGHT);

            top_left_buttons = top_left_buttons.push(tickers_list_btn);
        }

        let modifier: Option<modal::stream::Modifier> = self.modal.clone().and_then(|m| {
            if let Modal::StreamModifier(modifier) = m {
                Some(modifier)
            } else {
                None
            }
        });

        let compact_controls = if self.modal == Some(Modal::Controls) {
            Some(
                container(self.view_controls(
                    id,
                    panes,
                    maximized,
                    window != main_window.id,
                    allow_native_popout,
                ))
                .style(style::chart_modal)
                .into(),
            )
        } else {
            None
        };

        let uninitialized_base = |kind: ContentKind| -> Element<'a, Message> {
            match &self.streams {
                ResolvedStream::Waiting { streams, .. } if !streams.is_empty() => {
                    center(text("Waiting for metadata…").size(crate::style::text_size::TITLE))
                        .into()
                }
                ResolvedStream::Ready(streams) if !streams.is_empty() => center(
                    text("Waiting for pane initialization...").size(crate::style::text_size::TITLE),
                )
                .into(),
                ResolvedStream::Blocked {
                    streams, reason, ..
                } => {
                    let blocked_exchanges = streams
                        .iter()
                        .map(|s| s.exchange())
                        .collect::<std::collections::BTreeSet<_>>();

                    center(
                        column![
                            text(format!(
                                "Couldn't resolve streams for {}",
                                blocked_exchanges
                                    .iter()
                                    .map(|v| v.to_string())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ))
                            .size(crate::style::text_size::SECTION),
                            text((if reason.is_empty() { "" } else { reason }).to_string())
                                .size(crate::style::text_size::BODY),
                        ]
                        .spacing(8)
                        .align_x(Alignment::Center),
                    )
                    .into()
                }
                _ => {
                    let content = column![
                        text(kind.to_string()).size(crate::style::text_size::TITLE),
                        text("No ticker selected").size(crate::style::text_size::SECTION)
                    ]
                    .spacing(8)
                    .align_x(Alignment::Center);

                    center(content).into()
                }
            }
        };

        let body = match &self.content {
            Content::Starter => {
                let base: Element<_> = widget::toast::Manager::new(
                    center(
                        column![
                            text("Choose a view to get started")
                                .size(crate::style::text_size::TITLE),
                            container(responsive(move |size| widget::add_view::selector(
                                if size.width < 420.0 { 1 } else { 2 },
                                move |kind| Message::PaneEvent(id, Event::ContentSelected(kind)),
                            )))
                            .width(Length::Fill)
                            .max_width(620.0)
                        ]
                        .align_x(Alignment::Center)
                        .spacing(12),
                    ),
                    &self.notifications,
                    Alignment::End,
                    move |msg| Message::PaneEvent(id, Event::DeleteNotification(msg)),
                )
                .into();

                self.compose_stack_view(
                    base,
                    id,
                    None,
                    compact_controls,
                    || column![].into(),
                    None,
                    tickers_table,
                )
            }
            Content::Comparison(chart) => {
                if let Some(c) = chart {
                    let selected_basis = Basis::Time(c.timeframe);
                    let kind = ModifierKind::Comparison(selected_basis);

                    let modifiers =
                        row![basis_modifier(id, selected_basis, modifier, kind),].spacing(4);

                    top_left_buttons = top_left_buttons.push(modifiers);

                    let base = c.view(timezone).map(move |message| {
                        Message::PaneEvent(id, Event::ComparisonChartInteraction(message))
                    });

                    let settings_modal = || comparison_cfg_view(id, c);

                    self.compose_stack_view(
                        base,
                        id,
                        None,
                        compact_controls,
                        settings_modal,
                        Some(c.selected_tickers()),
                        tickers_table,
                    )
                } else {
                    let base = uninitialized_base(ContentKind::ComparisonChart);
                    self.compose_stack_view(
                        base,
                        id,
                        None,
                        compact_controls,
                        || column![].into(),
                        None,
                        tickers_table,
                    )
                }
            }
            Content::Gex {
                chart, unsupported, ..
            } => {
                if *unsupported {
                    let base = center(text(
                        "GEX data is currently available for BTC, ETH, and XAUT via GLD proxy.",
                    ))
                    .into();
                    self.compose_stack_view(
                        base,
                        id,
                        None,
                        compact_controls,
                        || column![].into(),
                        None,
                        tickers_table,
                    )
                } else if let Some(chart) = chart {
                    let base = chart.view().map(move |message| {
                        Message::PaneEvent(id, Event::GexChartInteraction(message))
                    });
                    let settings_modal = || gex_cfg_view(*chart.config(), id);
                    self.compose_stack_view(
                        base,
                        id,
                        None,
                        compact_controls,
                        settings_modal,
                        None,
                        tickers_table,
                    )
                } else {
                    let base = uninitialized_base(ContentKind::GexChart);
                    self.compose_stack_view(
                        base,
                        id,
                        None,
                        compact_controls,
                        || column![].into(),
                        None,
                        tickers_table,
                    )
                }
            }
            Content::TimeAndSales(panel) => {
                if let Some(panel) = panel {
                    let base = panel::view(panel, timezone).map(move |message| {
                        Message::PaneEvent(id, Event::PanelInteraction(message))
                    });

                    let settings_modal =
                        || modal::pane::settings::timesales_cfg_view(panel.config, id);

                    self.compose_stack_view(
                        base,
                        id,
                        None,
                        compact_controls,
                        settings_modal,
                        None,
                        tickers_table,
                    )
                } else {
                    let base = uninitialized_base(ContentKind::TimeAndSales);
                    self.compose_stack_view(
                        base,
                        id,
                        None,
                        compact_controls,
                        || column![].into(),
                        None,
                        tickers_table,
                    )
                }
            }
            Content::Ladder(panel) => {
                if let Some(panel) = panel {
                    let basis = self
                        .settings
                        .selected_basis
                        .unwrap_or(Basis::default_heatmap_time(self.stream_pair()));
                    let tick_multiply = self.settings.tick_multiply.unwrap_or(TickMultiplier(1));

                    let stream_pair = self.stream_pair();

                    let price_step = stream_pair
                        .map(|ti| {
                            tick_multiply.unscale_step_or_min_tick(panel.step, ti.min_ticksize)
                        })
                        .unwrap_or_else(|| tick_multiply.unscale_step(panel.step));

                    let exchange = stream_pair.map(|ti| ti.ticker.exchange);
                    let min_ticksize = stream_pair.map(|ti| ti.min_ticksize);

                    let modifiers = ticksize_modifier(
                        id,
                        price_step,
                        min_ticksize,
                        tick_multiply,
                        modifier,
                        ModifierKind::Orderbook(basis, tick_multiply),
                        exchange,
                    );

                    top_left_buttons = top_left_buttons.push(modifiers);

                    let base = panel::view(panel, timezone).map(move |message| {
                        Message::PaneEvent(id, Event::PanelInteraction(message))
                    });

                    let settings_modal =
                        || modal::pane::settings::ladder_cfg_view(panel.config, id);

                    self.compose_stack_view(
                        base,
                        id,
                        None,
                        compact_controls,
                        settings_modal,
                        None,
                        tickers_table,
                    )
                } else {
                    let base = uninitialized_base(ContentKind::Ladder);
                    self.compose_stack_view(
                        base,
                        id,
                        None,
                        compact_controls,
                        || column![].into(),
                        None,
                        tickers_table,
                    )
                }
            }
            Content::Heatmap {
                chart, indicators, ..
            } => {
                if let Some(chart) = chart {
                    let ticker_info = self.stream_pair();
                    let exchange = ticker_info.as_ref().map(|info| info.ticker.exchange);

                    let basis = self
                        .settings
                        .selected_basis
                        .unwrap_or(Basis::default_heatmap_time(ticker_info));
                    let tick_multiply = self.settings.tick_multiply.unwrap_or(TickMultiplier(5));

                    let kind = ModifierKind::Heatmap(basis, tick_multiply);
                    let price_step = ticker_info
                        .map(|ti| {
                            tick_multiply
                                .unscale_step_or_min_tick(chart.tick_size(), ti.min_ticksize)
                        })
                        .unwrap_or_else(|| tick_multiply.unscale_step(chart.tick_size()));
                    let min_ticksize = ticker_info.map(|ti| ti.min_ticksize);

                    let modifiers = row![
                        basis_modifier(id, basis, modifier, kind),
                        ticksize_modifier(
                            id,
                            price_step,
                            min_ticksize,
                            tick_multiply,
                            modifier,
                            kind,
                            exchange
                        ),
                    ]
                    .spacing(4);

                    top_left_buttons = top_left_buttons.push(modifiers);

                    let base = chart::view(chart, indicators, timezone).map(move |message| {
                        Message::PaneEvent(id, Event::ChartInteraction(message))
                    });
                    let settings_modal = || {
                        heatmap_cfg_view(
                            chart.visual_config(),
                            id,
                            chart.study_configurator(),
                            &chart.studies,
                            basis,
                        )
                    };

                    let indicator_modal = if self.modal == Some(Modal::Indicators) {
                        Some(modal::indicators::view(
                            id,
                            self,
                            indicators,
                            self.stream_pair().map(|i| i.ticker.market_type()),
                        ))
                    } else {
                        None
                    };

                    self.compose_stack_view(
                        base,
                        id,
                        indicator_modal,
                        compact_controls,
                        settings_modal,
                        None,
                        tickers_table,
                    )
                } else {
                    let base = uninitialized_base(ContentKind::HeatmapChart);
                    self.compose_stack_view(
                        base,
                        id,
                        None,
                        compact_controls,
                        || column![].into(),
                        None,
                        tickers_table,
                    )
                }
            }
            Content::Kline {
                chart,
                indicators,
                kind: chart_kind,
                ..
            } => {
                if let Some(chart) = chart {
                    match chart_kind {
                        data::chart::KlineChartKind::Footprint { .. } => {
                            let basis = chart.basis();
                            let tick_multiply =
                                self.settings.tick_multiply.unwrap_or(TickMultiplier(10));

                            let kind = ModifierKind::Footprint(basis, tick_multiply);
                            let stream_pair = self.stream_pair();
                            let price_step = stream_pair
                                .map(|ti| {
                                    tick_multiply.unscale_step_or_min_tick(
                                        chart.tick_size(),
                                        ti.min_ticksize,
                                    )
                                })
                                .unwrap_or_else(|| tick_multiply.unscale_step(chart.tick_size()));

                            let exchange = stream_pair.as_ref().map(|info| info.ticker.exchange);
                            let min_ticksize = stream_pair.map(|ti| ti.min_ticksize);

                            let modifiers = row![
                                basis_modifier(id, basis, modifier, kind),
                                ticksize_modifier(
                                    id,
                                    price_step,
                                    min_ticksize,
                                    tick_multiply,
                                    modifier,
                                    kind,
                                    exchange
                                ),
                            ]
                            .spacing(4);

                            top_left_buttons = top_left_buttons.push(modifiers);
                        }
                        data::chart::KlineChartKind::Candles => {
                            let selected_basis = chart.basis();
                            let kind = ModifierKind::Candlestick(selected_basis);

                            let modifiers =
                                row![basis_modifier(id, selected_basis, modifier, kind),]
                                    .spacing(4);

                            top_left_buttons = top_left_buttons.push(modifiers);
                        }
                    }

                    let base = chart::view(chart, indicators, timezone).map(move |message| {
                        Message::PaneEvent(id, Event::ChartInteraction(message))
                    });
                    let settings_modal = || {
                        kline_cfg_view(
                            chart.study_configurator(),
                            chart.visual_config(),
                            chart_kind,
                            id,
                            chart.basis(),
                        )
                    };

                    let indicator_modal = if self.modal == Some(Modal::Indicators) {
                        Some(modal::indicators::view_kline(
                            id,
                            self,
                            indicators,
                            self.stream_pair().map(|i| i.ticker.market_type()),
                            chart.visual_config(),
                            chart.volume_bubble_qty_scale(),
                        ))
                    } else {
                        None
                    };

                    self.compose_stack_view(
                        base,
                        id,
                        indicator_modal,
                        compact_controls,
                        settings_modal,
                        None,
                        tickers_table,
                    )
                } else {
                    let content_kind = match chart_kind {
                        data::chart::KlineChartKind::Candles => ContentKind::CandlestickChart,
                        data::chart::KlineChartKind::Footprint { .. } => {
                            ContentKind::FootprintChart
                        }
                    };
                    let base = uninitialized_base(content_kind);
                    self.compose_stack_view(
                        base,
                        id,
                        None,
                        compact_controls,
                        || column![].into(),
                        None,
                        tickers_table,
                    )
                }
            }
            Content::ShaderHeatmap {
                chart, indicators, ..
            } => {
                if let Some(chart) = chart {
                    let base = HeatmapShader::view(chart, timezone).map(move |message| {
                        Message::PaneEvent(id, Event::HeatmapShaderInteraction(message))
                    });

                    let ticker_info = self.stream_pair();
                    let exchange = ticker_info.as_ref().map(|info| info.ticker.exchange);

                    let basis = self
                        .settings
                        .selected_basis
                        .unwrap_or(Basis::default_heatmap_time(ticker_info));
                    let tick_multiply = self.settings.tick_multiply.unwrap_or(TickMultiplier(5));

                    let kind = ModifierKind::Heatmap(basis, tick_multiply);

                    let price_step = ticker_info
                        .map(|ti| {
                            tick_multiply
                                .unscale_step_or_min_tick(chart.tick_size(), ti.min_ticksize)
                        })
                        .unwrap_or_else(|| tick_multiply.unscale_step(chart.tick_size()));
                    let min_ticksize = ticker_info.map(|ti| ti.min_ticksize);

                    let settings_modal = || {
                        heatmap_shader_cfg_view(
                            chart.visual_config(),
                            id,
                            chart.study_configurator(),
                            &chart.studies,
                            basis,
                        )
                    };

                    let indicator_modal = if self.modal == Some(Modal::Indicators) {
                        Some(modal::indicators::view(
                            id,
                            self,
                            indicators,
                            self.stream_pair().map(|i| i.ticker.market_type()),
                        ))
                    } else {
                        None
                    };

                    let modifiers = row![
                        basis_modifier(id, basis, modifier, kind),
                        ticksize_modifier(
                            id,
                            price_step,
                            min_ticksize,
                            tick_multiply,
                            modifier,
                            kind,
                            exchange
                        ),
                    ]
                    .spacing(4);

                    top_left_buttons = top_left_buttons.push(modifiers);

                    self.compose_stack_view(
                        base,
                        id,
                        indicator_modal,
                        compact_controls,
                        settings_modal,
                        None,
                        tickers_table,
                    )
                } else {
                    let base = uninitialized_base(ContentKind::HeatmapChart);
                    self.compose_stack_view(
                        base,
                        id,
                        None,
                        compact_controls,
                        || column![].into(),
                        None,
                        tickers_table,
                    )
                }
            }
        };

        match &self.status {
            Status::Loading { info, source } => {
                let action = match (source, info) {
                    (LoadingSource::Historical, InfoKind::FetchingKlines) => {
                        "Loading historical candlesticks".to_string()
                    }
                    (LoadingSource::Historical, InfoKind::FetchingTrades(count)) => {
                        format!("Loading historical trades ({count} received)")
                    }
                    (LoadingSource::Historical, InfoKind::FetchingBubbleSummaries) => {
                        "Analyzing historical trades for volume bubbles".to_string()
                    }
                    (LoadingSource::Historical, InfoKind::FetchingOI) => {
                        "Loading historical open interest".to_string()
                    }
                    (LoadingSource::Reconnect, InfoKind::FetchingKlines) => {
                        "Connection restored: recovering missed candlesticks".to_string()
                    }
                    (LoadingSource::Reconnect, InfoKind::FetchingTrades(count)) => {
                        format!("Connection restored: recovering missed trades ({count} received)")
                    }
                    (LoadingSource::Reconnect, InfoKind::FetchingBubbleSummaries) => {
                        "Connection restored: rebuilding missed volume bubbles".to_string()
                    }
                    (LoadingSource::Reconnect, InfoKind::FetchingOI) => {
                        "Connection restored: recovering missed open interest".to_string()
                    }
                };
                let indicator = iced::widget::tooltip(
                    widget::loading_spinner(),
                    container(text(action)).style(style::tooltip).padding(8),
                    tooltip::Position::Bottom,
                )
                .delay(widget::DEFAULT_TOOLTIP_DELAY);
                top_left_buttons = top_left_buttons.push(indicator);
            }
            Status::Stale(msg) => {
                top_left_buttons = top_left_buttons.push(text(msg));
            }
            Status::Ready => {}
        }

        let content = pane_grid::Content::new(body)
            .style(move |theme| style::pane_background(theme, is_focused));

        let top_right_buttons = {
            let compact_control = container(
                button(
                    text("...")
                        .size(crate::style::text_size::EMPHASIS)
                        .align_y(Alignment::End),
                )
                .on_press(Message::PaneEvent(id, Event::ShowModal(Modal::Controls)))
                .style(move |theme, status| {
                    style::button::transparent(
                        theme,
                        status,
                        self.modal == Some(Modal::Controls) || self.modal == Some(Modal::Settings),
                    )
                }),
            )
            .align_y(Alignment::Center)
            .padding(4);

            if self.modal == Some(Modal::Controls) {
                pane_grid::Controls::new(compact_control)
            } else {
                pane_grid::Controls::dynamic(
                    self.view_controls(
                        id,
                        panes,
                        maximized,
                        window != main_window.id,
                        allow_native_popout,
                    ),
                    compact_control,
                )
            }
        };

        let title_bar = pane_grid::TitleBar::new(
            top_left_buttons
                .padding(padding::left(4))
                .align_y(Alignment::Center)
                .spacing(8)
                .height(Length::Fixed(32.0)),
        )
        .controls(top_right_buttons)
        .style(style::pane_title_bar);

        content.title_bar(if self.modal.is_none() {
            title_bar
        } else {
            title_bar.always_show_controls()
        })
    }

    pub fn update(&mut self, msg: Event) -> Option<Effect> {
        match msg {
            Event::ShowModal(requested_modal) => {
                return self.show_modal_with_focus(requested_modal);
            }
            Event::HideModal => {
                self.modal = None;
            }
            Event::ContentSelected(kind) => {
                if kind == ContentKind::GexChart {
                    let underlying = exchange::options::OptionsUnderlying::Btc;
                    self.content = Content::Gex {
                        chart: Some(GexChart::new(underlying, None, None)),
                        underlying,
                        liquidity_reference: None,
                        liquidity_reference_source: None,
                        unsupported: false,
                    };
                    self.settings.visual_config = None;
                    self.streams = ResolvedStream::Ready(Vec::new());
                    return None;
                }
                self.content = Content::placeholder(kind);
                self.settings.visual_config = None;

                if !matches!(kind, ContentKind::Starter) {
                    self.streams = ResolvedStream::waiting(vec![]);
                    let mini_panel = MiniPanel::new();
                    let modal = Modal::MiniTickersList(mini_panel);

                    if let Some(effect) = self.show_modal_with_focus(modal) {
                        return Some(effect);
                    }
                }
            }
            Event::ChartInteraction(msg) => match &mut self.content {
                Content::Heatmap { chart: Some(c), .. } => {
                    super::chart::update(c, &msg);
                }
                Content::Kline { chart: Some(c), .. } => {
                    if let super::chart::Message::Drawing(drawing) = &msg {
                        let had_fixed_volume_profiles = c.has_fixed_volume_profiles();
                        c.handle_drawing(drawing);
                        let has_fixed_volume_profiles = c.has_fixed_volume_profiles();
                        if had_fixed_volume_profiles != has_fixed_volume_profiles {
                            self.reconcile_candlestick_trade_stream();
                            return Some(Effect::RefreshStreams);
                        }
                        if matches!(drawing, super::chart::DrawingMessage::PointerPressed(_, _))
                            && let Some(id) = c.drawing_text_input_id()
                        {
                            return Some(Effect::FocusWidget(id));
                        }
                    } else {
                        super::chart::update(c, &msg);
                    }
                }
                _ => {}
            },
            Event::GexChartInteraction(message) => {
                if matches!(
                    message,
                    crate::chart::gex::Message::SelectLiquidityReference
                ) && let Content::Gex { underlying, .. } = &self.content
                {
                    return self.show_modal_with_focus(Modal::GexLiquidityReference(
                        MiniPanel::for_options_underlying(*underlying),
                    ));
                }
                if let Content::Gex {
                    chart: Some(chart), ..
                } = &mut self.content
                {
                    chart.update(message);
                }
            }
            Event::GexAssetSelected(asset) => {
                let next = asset.underlying();
                if let Content::Gex {
                    chart,
                    underlying,
                    liquidity_reference,
                    liquidity_reference_source,
                    unsupported,
                } = &mut self.content
                {
                    if *underlying == next {
                        return None;
                    }
                    let compatible_reference = liquidity_reference.filter(|reference| {
                        exchange::options::resolve_gex_source(reference.ticker)
                            .is_some_and(|source| source.source_underlying() == next)
                    });
                    *underlying = next;
                    *unsupported = false;
                    *liquidity_reference = compatible_reference;
                    if compatible_reference.is_none() {
                        *liquidity_reference_source = None;
                    }
                    if let Some(chart) = chart {
                        chart.set_underlying(next);
                        chart.set_liquidity_reference(compatible_reference);
                    }
                    self.reconcile_gex_liquidity_stream();
                    return Some(Effect::RefreshStreams);
                }
            }
            Event::PanelInteraction(msg) => match &mut self.content {
                Content::Ladder(Some(p)) => super::panel::update(p, msg),
                Content::TimeAndSales(Some(p)) => super::panel::update(p, msg),
                _ => {}
            },
            Event::ToggleIndicator(ind) => {
                let exchange = self.stream_pair().map(|ticker| ticker.exchange());
                let refresh_streams = matches!(
                    (ind, exchange),
                    (UiIndicator::Kline(indicator), Some(exchange))
                        if indicator.requires_trades(exchange)
                );
                self.content.toggle_indicator(ind);
                if refresh_streams {
                    self.reconcile_candlestick_trade_stream();
                    return Some(Effect::RefreshStreams);
                }
            }
            Event::DeleteNotification(idx) => {
                if idx < self.notifications.len() {
                    self.notifications.remove(idx);
                }
            }
            Event::ReorderIndicator(e) => {
                self.content.reorder_indicators(&e);
            }
            Event::ClusterKindSelected(kind) => {
                if let Content::Kline {
                    chart, kind: cur, ..
                } = &mut self.content
                    && let Some(c) = chart
                {
                    c.set_cluster_kind(kind);
                    *cur = c.kind.clone();
                }
            }
            Event::ClusterScalingSelected(scaling) => {
                if let Content::Kline { chart, kind, .. } = &mut self.content
                    && let Some(c) = chart
                {
                    c.set_cluster_scaling(scaling);
                    *kind = c.kind.clone();
                }
            }
            Event::StudyConfigurator(study_msg) => match study_msg {
                modal::pane::settings::study::StudyMessage::Footprint(m) => {
                    if let Content::Kline { chart, kind, .. } = &mut self.content
                        && let Some(c) = chart
                    {
                        c.update_study_configurator(m);
                        *kind = c.kind.clone();
                    }
                }
                modal::pane::settings::study::StudyMessage::Heatmap(m) => {
                    if let Content::Heatmap { chart, studies, .. } = &mut self.content
                        && let Some(c) = chart
                    {
                        c.update_study_configurator(m);
                        *studies = c.studies.clone();
                    } else if let Content::ShaderHeatmap { chart, studies, .. } = &mut self.content
                        && let Some(c) = chart
                    {
                        c.update_study_configurator(m);
                        *studies = c.studies.clone();
                    }
                }
            },
            Event::StreamModifierChanged(message) => {
                if let Some(Modal::StreamModifier(mut modifier)) = self.modal.take() {
                    let mut effect: Option<Effect> = None;

                    if let Some(action) = modifier.update(message) {
                        match action {
                            modal::stream::Action::TabSelected(tab) => {
                                modifier.tab = tab;
                            }
                            modal::stream::Action::TicksizeSelected(tm) => {
                                modifier.update_kind_with_multiplier(tm);
                                self.settings.tick_multiply = Some(tm);

                                if let Some(ticker) = self.stream_pair() {
                                    match &mut self.content {
                                        Content::Kline { chart: Some(c), .. } => {
                                            c.change_tick_size(
                                                tm.multiply_with_min_tick_step(ticker),
                                            );
                                            c.reset_request_handler();
                                        }
                                        Content::Heatmap { chart: Some(c), .. } => {
                                            c.change_tick_size(
                                                tm.multiply_with_min_tick_step(ticker),
                                            );
                                        }
                                        Content::Ladder(Some(p)) => {
                                            p.set_tick_size(tm.multiply_with_min_tick_step(ticker));
                                        }
                                        Content::ShaderHeatmap {
                                            chart: Some(c),
                                            indicators,
                                            studies,
                                            ..
                                        } => {
                                            let saved_config = c.config;
                                            **c = HeatmapShader::new(
                                                c.basis,
                                                tm.multiply_with_min_tick_step(ticker),
                                                c.ticker_info,
                                                studies.clone(),
                                                indicators.clone(),
                                                Some(saved_config),
                                            );
                                        }
                                        _ => {}
                                    }
                                }

                                let is_client = self
                                    .stream_pair()
                                    .map(|ti| ti.exchange().is_depth_client_aggr())
                                    .unwrap_or(false);

                                if let Some(mut it) = self.streams.ready_iter_mut() {
                                    for s in &mut it {
                                        if let StreamKind::Depth { depth_aggr, .. } = s {
                                            *depth_aggr = if is_client {
                                                StreamTicksize::Client
                                            } else {
                                                StreamTicksize::ServerSide(tm)
                                            };
                                        }
                                    }
                                }
                                if !is_client {
                                    effect = Some(Effect::RefreshStreams);
                                }
                            }
                            modal::stream::Action::BasisSelected(new_basis) => {
                                modifier.update_kind_with_basis(new_basis);
                                self.settings.selected_basis = Some(new_basis);

                                let base_ticker = self.stream_pair();

                                match &mut self.content {
                                    Content::Heatmap { chart: Some(c), .. } => {
                                        c.set_basis(new_basis);

                                        if let Some(stream_type) =
                                            self.streams.ready_iter_mut().and_then(|mut it| {
                                                it.find(|s| matches!(s, StreamKind::Depth { .. }))
                                            })
                                            && let StreamKind::Depth {
                                                push_freq,
                                                ticker_info,
                                                ..
                                            } = stream_type
                                            && ticker_info.exchange().is_custom_push_freq()
                                        {
                                            match new_basis {
                                                Basis::Time(tf) => {
                                                    *push_freq = exchange::PushFrequency::Custom(tf)
                                                }
                                                Basis::Tick(_) => {
                                                    *push_freq =
                                                        exchange::PushFrequency::ServerDefault
                                                }
                                            }
                                        }

                                        effect = Some(Effect::RefreshStreams);
                                    }
                                    Content::ShaderHeatmap {
                                        chart: Some(c),
                                        indicators,
                                        ..
                                    } => {
                                        let saved_config = c.config;
                                        let saved_studies = c.studies.clone();
                                        **c = HeatmapShader::new(
                                            new_basis,
                                            c.tick_size(),
                                            c.ticker_info,
                                            saved_studies,
                                            indicators.clone(),
                                            Some(saved_config),
                                        );

                                        if let Some(stream_type) =
                                            self.streams.ready_iter_mut().and_then(|mut it| {
                                                it.find(|s| matches!(s, StreamKind::Depth { .. }))
                                            })
                                            && let StreamKind::Depth {
                                                push_freq,
                                                ticker_info,
                                                ..
                                            } = stream_type
                                            && ticker_info.exchange().is_custom_push_freq()
                                        {
                                            match new_basis {
                                                Basis::Time(tf) => {
                                                    *push_freq = exchange::PushFrequency::Custom(tf)
                                                }
                                                Basis::Tick(_) => {
                                                    *push_freq =
                                                        exchange::PushFrequency::ServerDefault
                                                }
                                            }
                                        }

                                        effect = Some(Effect::RefreshStreams);
                                    }
                                    Content::Kline { chart: Some(c), .. } => {
                                        if let Some(base_ticker) = base_ticker {
                                            match new_basis {
                                                Basis::Time(tf) => {
                                                    let kline_stream = StreamKind::Kline {
                                                        ticker_info: base_ticker,
                                                        timeframe: tf,
                                                    };
                                                    let mut streams = vec![kline_stream];

                                                    let needs_trades = matches!(
                                                        c.kind,
                                                        data::chart::KlineChartKind::Footprint { .. }
                                                    ) || (matches!(
                                                        c.kind,
                                                        data::chart::KlineChartKind::Candles
                                                    ) && c
                                                        .visual_config()
                                                        .volume_bubbles
                                                        .enabled)
                                                        || c.has_fixed_volume_profiles();

                                                    if needs_trades {
                                                        streams.push(StreamKind::Trades {
                                                            ticker_info: base_ticker,
                                                        });
                                                    }

                                                    self.streams = ResolvedStream::Ready(streams);
                                                    let action = c.set_basis(new_basis);

                                                    if let Some(chart::Action::RequestFetch(
                                                        fetch,
                                                    )) = action
                                                    {
                                                        effect = Some(Effect::RequestFetch(fetch));
                                                    }
                                                }
                                                Basis::Tick(_) => {
                                                    self.streams = ResolvedStream::Ready(vec![
                                                        StreamKind::Trades {
                                                            ticker_info: base_ticker,
                                                        },
                                                    ]);
                                                    c.set_basis(new_basis);

                                                    self.status = Status::Ready;
                                                    effect = Some(Effect::RefreshStreams);
                                                }
                                            }
                                        }
                                    }
                                    Content::Comparison(Some(c)) => {
                                        if let Basis::Time(tf) = new_basis {
                                            let streams: Vec<StreamKind> = c
                                                .selected_tickers()
                                                .iter()
                                                .copied()
                                                .map(|ti| StreamKind::Kline {
                                                    ticker_info: ti,
                                                    timeframe: tf,
                                                })
                                                .collect();

                                            self.streams = ResolvedStream::Ready(streams);
                                            let action = c.set_basis(new_basis);

                                            if let Some(chart::Action::RequestFetch(fetch)) = action
                                            {
                                                effect = Some(Effect::RequestFetch(fetch));
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }

                    self.modal = Some(Modal::StreamModifier(modifier));

                    if let Some(e) = effect {
                        return Some(e);
                    }
                }
            }
            Event::ComparisonChartInteraction(message) => {
                if let Content::Comparison(chart_opt) = &mut self.content
                    && let Some(chart) = chart_opt
                    && let Some(action) = chart.update(message)
                {
                    match action {
                        super::chart::comparison::Action::SeriesColorChanged(t, color) => {
                            chart.set_series_color(t, color);
                        }
                        super::chart::comparison::Action::SeriesNameChanged(t, name) => {
                            chart.set_series_name(t, name);
                        }
                        super::chart::comparison::Action::OpenSeriesEditor => {
                            self.modal = Some(Modal::Settings);
                        }
                        super::chart::comparison::Action::RemoveSeries(ti) => {
                            let rebuilt = chart.remove_ticker(&ti);
                            self.streams = ResolvedStream::Ready(rebuilt);

                            return Some(Effect::RefreshStreams);
                        }
                    }
                }
            }
            Event::HeatmapShaderInteraction(message) => {
                if let Content::ShaderHeatmap { chart: Some(c), .. } = &mut self.content {
                    c.update(message);
                }
            }
            Event::MiniTickersListInteraction(message) => {
                if let Some(Modal::GexLiquidityReference(ref mut mini_panel)) = self.modal
                    && let Some(action) = mini_panel.update(message.clone())
                {
                    let crate::modal::pane::mini_tickers_list::Action::RowSelected(sel) = action;
                    if let crate::modal::pane::mini_tickers_list::RowSelection::Switch(ticker) = sel
                    {
                        let compatible =
                            self.gex_liquidity_resolution()
                                .is_some_and(|(underlying, ..)| {
                                    exchange::options::resolve_gex_source(ticker.ticker)
                                        .is_some_and(|source| {
                                            source.source_underlying() == underlying
                                        })
                                });
                        if compatible {
                            self.modal = None;
                            self.set_gex_liquidity_reference(
                                Some(ticker),
                                Some(GexLiquidityReferenceSource::Manual),
                            );
                            let (symbol, _) = ticker.ticker.display_symbol_and_type();
                            log::info!(
                                "GEX LiquidityReferenceResolved ticker={} exchange={} source=manual",
                                symbol,
                                ticker.exchange()
                            );
                            return Some(Effect::RefreshStreams);
                        }
                    }
                    return None;
                }
                if let Some(Modal::MiniTickersList(ref mut mini_panel)) = self.modal
                    && let Some(action) = mini_panel.update(message)
                {
                    self.modal = Some(Modal::MiniTickersList(mini_panel.clone()));

                    let crate::modal::pane::mini_tickers_list::Action::RowSelected(sel) = action;
                    match sel {
                        crate::modal::pane::mini_tickers_list::RowSelection::Add(ti) => {
                            if let Content::Comparison(chart) = &mut self.content
                                && let Some(c) = chart
                            {
                                let rebuilt = c.add_ticker(&ti);
                                self.streams = ResolvedStream::Ready(rebuilt);
                                return Some(Effect::RefreshStreams);
                            }
                        }
                        crate::modal::pane::mini_tickers_list::RowSelection::Remove(ti) => {
                            if let Content::Comparison(chart) = &mut self.content
                                && let Some(c) = chart
                            {
                                let rebuilt = c.remove_ticker(&ti);
                                self.streams = ResolvedStream::Ready(rebuilt);
                                return Some(Effect::RefreshStreams);
                            }
                        }
                        crate::modal::pane::mini_tickers_list::RowSelection::Switch(ti) => {
                            return Some(Effect::SwitchTickersInGroup(ti));
                        }
                    }
                }
            }
        }
        None
    }

    pub fn dismiss_drawing_interaction(&mut self) -> bool {
        if let Content::Kline {
            chart: Some(chart), ..
        } = &mut self.content
            && matches!(chart.kind(), data::chart::KlineChartKind::Candles)
        {
            return chart.handle_drawing(&super::chart::DrawingMessage::CancelOrCommit);
        }
        false
    }

    fn view_controls(
        &'_ self,
        pane: pane_grid::Pane,
        total_panes: usize,
        is_maximized: bool,
        is_popout: bool,
        allow_native_popout: bool,
    ) -> Element<'_, Message> {
        let modal_btn_style = |modal: Modal| {
            let is_active = self.modal == Some(modal);
            move |theme: &Theme, status: button::Status| {
                style::button::transparent(theme, status, is_active)
            }
        };

        let control_btn_style = |is_active: bool| {
            move |theme: &Theme, status: button::Status| {
                style::button::transparent(theme, status, is_active)
            }
        };

        let treat_as_starter =
            matches!(&self.content, Content::Starter) || !self.content.initialized();

        let tooltip_pos = tooltip::Position::Bottom;
        let mut buttons = row![];

        let show_modal = |modal: Modal| Message::PaneEvent(pane, Event::ShowModal(modal));

        if !treat_as_starter {
            buttons = buttons.push(button_with_tooltip(
                icon_text(Icon::Cog, 12),
                show_modal(Modal::Settings),
                Some("Settings"),
                tooltip_pos,
                modal_btn_style(Modal::Settings),
            ));
        }
        if !treat_as_starter
            && matches!(
                &self.content,
                Content::Heatmap { .. } | Content::Kline { .. } | Content::ShaderHeatmap { .. }
            )
        {
            buttons = buttons.push(button_with_tooltip(
                icon_text(Icon::ChartOutline, 12),
                show_modal(Modal::Indicators),
                Some("Indicators"),
                tooltip_pos,
                modal_btn_style(Modal::Indicators),
            ));
        }

        if is_popout && allow_native_popout {
            buttons = buttons.push(button_with_tooltip(
                icon_text(Icon::Popout, 12),
                Message::Merge,
                Some("Merge"),
                tooltip_pos,
                control_btn_style(is_popout),
            ));
        } else if total_panes > 1 && allow_native_popout {
            buttons = buttons.push(button_with_tooltip(
                icon_text(Icon::Popout, 12),
                Message::Popout,
                Some("Pop out"),
                tooltip_pos,
                control_btn_style(is_popout),
            ));
        }

        if total_panes > 1 {
            let (resize_icon, message) = if is_maximized {
                (Icon::ResizeSmall, Message::Restore)
            } else {
                (Icon::ResizeFull, Message::MaximizePane(pane))
            };

            buttons = buttons.push(button_with_tooltip(
                icon_text(resize_icon, 12),
                message,
                Some(if is_maximized { "Restore" } else { "Maximize" }),
                tooltip_pos,
                control_btn_style(is_maximized),
            ));

            buttons = buttons.push(button_with_tooltip(
                icon_text(Icon::Close, 12),
                Message::ClosePane(pane),
                Some("Close pane"),
                tooltip_pos,
                control_btn_style(false),
            ));
        }

        buttons
            .padding(padding::right(4).left(4))
            .align_y(Alignment::Center)
            .height(Length::Fixed(32.0))
            .into()
    }

    fn compose_stack_view<'a, F>(
        &'a self,
        base: Element<'a, Message>,
        pane: pane_grid::Pane,
        indicator_modal: Option<Element<'a, Message>>,
        compact_controls: Option<Element<'a, Message>>,
        settings_modal: F,
        selected_tickers: Option<&'a [TickerInfo]>,
        tickers_table: &'a TickersTable,
    ) -> Element<'a, Message>
    where
        F: FnOnce() -> Element<'a, Message>,
    {
        let base =
            widget::toast::Manager::new(base, &self.notifications, Alignment::End, move |msg| {
                Message::PaneEvent(pane, Event::DeleteNotification(msg))
            })
            .into();

        let on_blur = Message::PaneEvent(pane, Event::HideModal);

        match &self.modal {
            Some(Modal::LinkGroup) => {
                let content = link_group_modal(pane, self.link_group);

                stack_modal(
                    base,
                    content,
                    on_blur,
                    padding::right(12).left(4),
                    Alignment::Start,
                )
            }
            Some(Modal::StreamModifier(modifier)) => stack_modal(
                base,
                modifier.view(self.stream_pair_kind()).map(move |message| {
                    Message::PaneEvent(pane, Event::StreamModifierChanged(message))
                }),
                Message::PaneEvent(pane, Event::HideModal),
                padding::right(12).left(48),
                Alignment::Start,
            ),
            Some(Modal::MiniTickersList(panel)) => {
                let mini_list = panel
                    .view(tickers_table, selected_tickers, self.stream_pair())
                    .map(move |msg| {
                        Message::PaneEvent(pane, Event::MiniTickersListInteraction(msg))
                    });

                let content: Element<_> = container(mini_list)
                    .max_width(260)
                    .padding(16)
                    .style(style::chart_modal)
                    .into();

                stack_modal(
                    base,
                    content,
                    Message::PaneEvent(pane, Event::HideModal),
                    padding::left(12),
                    Alignment::Start,
                )
            }
            Some(Modal::GexLiquidityReference(panel)) => {
                let reference = match &self.content {
                    Content::Gex {
                        chart,
                        liquidity_reference,
                        ..
                    } => chart
                        .as_ref()
                        .and_then(GexChart::liquidity_reference)
                        .or(*liquidity_reference),
                    _ => None,
                };
                let mini_list = panel.view(tickers_table, None, reference).map(move |msg| {
                    Message::PaneEvent(pane, Event::MiniTickersListInteraction(msg))
                });
                let content = column![
                    text("Reference market").size(crate::style::text_size::SECTION),
                    text("Choose a market with the same GEX underlying.")
                        .size(crate::style::text_size::SMALL),
                    mini_list
                ]
                .spacing(8);
                stack_modal(
                    base,
                    container(content)
                        .max_width(280)
                        .padding(16)
                        .style(style::chart_modal),
                    on_blur,
                    padding::left(12),
                    Alignment::Start,
                )
            }
            Some(Modal::Settings) => {
                let settings = column![
                    settings_modal(),
                    rule::horizontal(1.0).style(style::split_ruler),
                    button(text("Reset view"))
                        .width(Length::Fill)
                        .on_press(Message::ReplacePane(pane)),
                ]
                .spacing(12);
                stack_modal(
                    base,
                    settings,
                    on_blur,
                    padding::right(12).left(12),
                    Alignment::End,
                )
            }
            Some(Modal::Indicators) => stack_modal(
                base,
                indicator_modal.unwrap_or_else(|| column![].into()),
                on_blur,
                padding::right(12).left(12),
                Alignment::End,
            ),
            Some(Modal::Controls) => stack_modal(
                base,
                if let Some(controls) = compact_controls {
                    controls
                } else {
                    column![].into()
                },
                on_blur,
                padding::left(12),
                Alignment::End,
            ),
            None => base,
        }
    }

    pub fn matches_stream(&self, stream: &StreamKind) -> bool {
        self.streams.matches_stream(stream)
    }

    /// Check if this pane can consume a specific type of fetched data.
    ///
    /// Live WS events (trades, depth) can be consumed by Heatmap, Ladder,
    /// TimeAndSales etc. But historical fetched data (REST backfill) has
    /// stricter requirements:
    /// - `FetchedData::Trades` → only Kline chart panes
    /// - `FetchedData::BubbleSummary` → only Kline chart panes
    /// - `FetchedData::Klines` → only Kline chart panes
    /// - `FetchedData::OI` → only Kline chart panes
    pub fn supports_fetched_data(&self, data: &crate::connector::fetcher::FetchedData) -> bool {
        use crate::connector::fetcher::FetchedData;
        match data {
            FetchedData::Trades { .. }
            | FetchedData::BubbleSummary { .. }
            | FetchedData::Klines { .. }
            | FetchedData::OI { .. } => {
                matches!(self.content, Content::Kline { chart: Some(_), .. })
            }
        }
    }

    /// Check if this pane supports a specific fetch range for backfill.
    pub fn supports_fetch_range(&self, fetch: &crate::connector::fetcher::FetchRange) -> bool {
        use crate::connector::fetcher::FetchRange;
        match fetch {
            FetchRange::Trades(_, _)
            | FetchRange::BubbleSummary { .. }
            | FetchRange::Kline(_, _)
            | FetchRange::OpenInterest { .. } => {
                matches!(self.content, Content::Kline { chart: Some(_), .. })
            }
        }
    }

    fn show_modal_with_focus(&mut self, requested_modal: Modal) -> Option<Effect> {
        let should_toggle_close = match (&self.modal, &requested_modal) {
            (Some(Modal::StreamModifier(open)), Modal::StreamModifier(req)) => {
                open.view_mode == req.view_mode
            }
            (Some(open), req) => core::mem::discriminant(open) == core::mem::discriminant(req),
            _ => false,
        };

        if should_toggle_close {
            self.modal = None;
            return None;
        }

        let focus_widget_id = match &requested_modal {
            Modal::MiniTickersList(m) | Modal::GexLiquidityReference(m) => {
                Some(m.search_box_id.clone())
            }
            _ => None,
        };

        self.modal = Some(requested_modal);
        focus_widget_id.map(Effect::FocusWidget)
    }

    pub fn invalidate(&mut self, now: Instant) -> Option<Action> {
        match &mut self.content {
            Content::Heatmap { chart, .. } => chart
                .as_mut()
                .and_then(|c| c.invalidate(Some(now)).map(Action::Chart)),
            Content::Kline { chart, .. } => chart
                .as_mut()
                .and_then(|c| c.invalidate(Some(now)).map(Action::Chart)),
            Content::TimeAndSales(panel) => panel
                .as_mut()
                .and_then(|p| p.invalidate(Some(now)).map(Action::Panel)),
            Content::Ladder(panel) => panel
                .as_mut()
                .and_then(|p| p.invalidate(Some(now)).map(Action::Panel)),
            Content::Starter => None,
            Content::Comparison(chart) => chart
                .as_mut()
                .and_then(|c| c.invalidate(Some(now)).map(Action::Chart)),
            Content::ShaderHeatmap { chart, .. } => chart
                .as_mut()
                .and_then(|c| c.invalidate(Some(now)).map(Action::Chart)),
            Content::Gex { .. } => None,
        }
    }

    pub fn park_for_inactive_layout(&mut self) {
        if let Content::ShaderHeatmap { chart, .. } = &mut self.content {
            *chart = None;
            self.status = Status::Ready;
        }
    }

    pub fn update_interval(&self) -> Option<u64> {
        match &self.content {
            Content::Kline { .. } | Content::Comparison(_) | Content::Gex { .. } => Some(1000),
            Content::Heatmap { chart, .. } => {
                if let Some(chart) = chart {
                    chart.basis_interval()
                } else {
                    None
                }
            }
            Content::Ladder(_) | Content::TimeAndSales(_) => Some(100),
            Content::ShaderHeatmap { .. } => None,
            Content::Starter => None,
        }
    }

    pub fn last_tick(&self) -> Option<Instant> {
        self.content.last_tick()
    }

    pub fn tick(&mut self, now: Instant) -> Option<Action> {
        let invalidate_interval: Option<u64> = self.update_interval();
        let last_tick: Option<Instant> = self.last_tick();

        if let Some(streams) = self.streams.due_streams_to_resolve(now) {
            log::debug!(
                "STREAM PaneResolveDue | pane={} waiting_streams={}",
                fetcher::short_id(self.id),
                streams.len()
            );
            return Some(Action::ResolveStreams(streams));
        }

        if !self.content.initialized() {
            log::debug!(
                "CHART ResolveContent | pane={} content={} reason=uninitialized",
                fetcher::short_id(self.id),
                self.content
            );
            return Some(Action::ResolveContent);
        }

        match (invalidate_interval, last_tick) {
            (Some(interval_ms), Some(previous_tick_time)) => {
                if interval_ms > 0 {
                    let interval_duration = std::time::Duration::from_millis(interval_ms);
                    if now.duration_since(previous_tick_time) >= interval_duration {
                        log::trace!(
                            "CHART Invalidate | pane={} interval_ms={interval_ms} reason=elapsed",
                            fetcher::short_id(self.id)
                        );
                        return self.invalidate(now);
                    }
                }
            }
            (Some(interval_ms), None) => {
                if interval_ms > 0 {
                    log::debug!(
                        "CHART Invalidate | pane={} interval_ms={interval_ms} reason=no_last_tick",
                        fetcher::short_id(self.id)
                    );
                    return self.invalidate(now);
                }
            }
            (None, _) => {
                log::trace!(
                    "CHART Invalidate | pane={} reason=no_interval",
                    fetcher::short_id(self.id)
                );
                return self.invalidate(now);
            }
        }

        None
    }

    pub fn unique_id(&self) -> uuid::Uuid {
        self.id
    }

    pub fn apply_synced_settings(
        &mut self,
        studies: &Option<data::chart::Study>,
        clusters: &Option<data::chart::kline::ClusterKind>,
    ) {
        if let Some(studies) = studies {
            self.content.update_studies(studies.clone());
        }
        if let Some(cluster_kind) = clusters
            && let Content::Kline { chart, kind, .. } = &mut self.content
            && let Some(c) = chart
        {
            c.set_cluster_kind(*cluster_kind);
            *kind = c.kind.clone();
        }
    }
}

impl Default for State {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            modal: None,
            content: Content::Starter,
            settings: Settings::default(),
            streams: ResolvedStream::waiting(vec![]),
            notifications: vec![],
            status: Status::Ready,
            link_group: None,
            gex_liquidity_missing_logged: false,
        }
    }
}

#[derive(Default)]
#[allow(clippy::large_enum_variant)]
pub enum Content {
    #[default]
    Starter,
    Heatmap {
        chart: Option<HeatmapChart>,
        indicators: Vec<HeatmapIndicator>,
        layout: data::chart::ViewConfig,
        studies: Vec<data::chart::heatmap::HeatmapStudy>,
    },
    ShaderHeatmap {
        chart: Option<Box<HeatmapShader>>,
        indicators: Vec<HeatmapIndicator>,
        studies: Vec<data::chart::heatmap::HeatmapStudy>,
    },
    Kline {
        chart: Option<KlineChart>,
        indicators: Vec<KlineIndicator>,
        layout: data::chart::ViewConfig,
        kind: data::chart::KlineChartKind,
        drawings: Vec<data::chart::kline::drawing::Drawing>,
    },
    TimeAndSales(Option<TimeAndSales>),
    Ladder(Option<Ladder>),
    Comparison(Option<ComparisonChart>),
    Gex {
        chart: Option<GexChart>,
        underlying: exchange::options::OptionsUnderlying,
        liquidity_reference: Option<TickerInfo>,
        liquidity_reference_source: Option<GexLiquidityReferenceSource>,
        unsupported: bool,
    },
}

impl Content {
    fn new_heatmap(
        current_content: &Content,
        ticker_info: TickerInfo,
        settings: &Settings,
        price_step: exchange::unit::PriceStep,
    ) -> Self {
        let (enabled_indicators, layout, prev_studies) = if let Content::Heatmap {
            chart,
            indicators,
            studies,
            layout,
        } = current_content
        {
            (
                indicators.clone(),
                chart
                    .as_ref()
                    .map(|c| c.chart_layout())
                    .unwrap_or(layout.clone()),
                chart
                    .as_ref()
                    .map_or(studies.clone(), |c| c.studies.clone()),
            )
        } else {
            (
                vec![HeatmapIndicator::Volume],
                ViewConfig {
                    splits: vec![],
                    autoscale: Some(data::chart::Autoscale::CenterLatest),
                },
                vec![],
            )
        };

        let basis = settings
            .selected_basis
            .unwrap_or_else(|| Basis::default_heatmap_time(Some(ticker_info)));
        let config = settings.visual_config.clone().and_then(|cfg| cfg.heatmap());

        let chart = HeatmapChart::new(
            layout.clone(),
            basis,
            price_step,
            &enabled_indicators,
            ticker_info,
            config,
            prev_studies.clone(),
        );

        Content::Heatmap {
            chart: Some(chart),
            indicators: enabled_indicators,
            layout,
            studies: prev_studies,
        }
    }

    fn new_kline(
        content_kind: ContentKind,
        current_content: &Content,
        ticker_info: TickerInfo,
        settings: &Settings,
        step: exchange::unit::PriceStep,
    ) -> Self {
        let (prev_indis, prev_layout, prev_kind_opt, prev_drawings) = if let Content::Kline {
            chart,
            indicators,
            kind,
            layout,
            drawings,
        } = current_content
        {
            (
                Some(indicators.clone()),
                Some(chart.as_ref().map_or(layout.clone(), |c| c.chart_layout())),
                Some(chart.as_ref().map_or(kind.clone(), |c| c.kind().clone())),
                chart.as_ref().map_or(drawings.clone(), |c| c.drawings()),
            )
        } else {
            (None, None, None, vec![])
        };

        let (default_tf, determined_chart_kind) = match content_kind {
            ContentKind::FootprintChart => (
                Timeframe::M5,
                prev_kind_opt
                    .filter(|k| matches!(k, data::chart::KlineChartKind::Footprint { .. }))
                    .unwrap_or_else(|| data::chart::KlineChartKind::Footprint {
                        clusters: data::chart::kline::ClusterKind::default(),
                        scaling: data::chart::kline::ClusterScaling::default(),
                        studies: vec![],
                    }),
            ),
            ContentKind::CandlestickChart => (Timeframe::M15, data::chart::KlineChartKind::Candles),
            _ => unreachable!("invalid content kind for kline chart"),
        };

        let basis = settings.selected_basis.unwrap_or(Basis::Time(default_tf));

        let enabled_indicators = {
            let available = KlineIndicator::for_market(ticker_info.market_type());
            prev_indis.map_or_else(
                || vec![KlineIndicator::Volume],
                |indis| {
                    indis
                        .into_iter()
                        .filter(|i| {
                            available.contains(i) && determined_chart_kind.allows_indicator(*i)
                        })
                        .collect()
                },
            )
        };

        let splits = {
            let main_chart_split: f32 = 0.8;
            let mut splits_vec = vec![main_chart_split];

            let num_indicators = enabled_indicators
                .iter()
                .filter(|indicator| !indicator.is_overlay())
                .count();
            if num_indicators > 0 {
                let indicator_total_height_ratio = 1.0 - main_chart_split;
                let height_per_indicator_pane =
                    indicator_total_height_ratio / num_indicators as f32;

                let mut current_split_pos = main_chart_split;
                for _ in 0..(num_indicators - 1) {
                    current_split_pos += height_per_indicator_pane;
                    splits_vec.push(current_split_pos);
                }
            }
            splits_vec
        };

        let layout = prev_layout
            .filter(|l| l.splits.len() == splits.len())
            .unwrap_or(ViewConfig {
                splits,
                autoscale: Some(data::chart::Autoscale::FitToVisible),
            });
        let visual_config = settings.visual_config.as_ref().and_then(|cfg| cfg.kline());

        let mut chart = KlineChart::new(
            layout.clone(),
            basis,
            step,
            &[],
            vec![],
            &enabled_indicators,
            ticker_info,
            &determined_chart_kind,
            visual_config,
        );
        if matches!(determined_chart_kind, data::chart::KlineChartKind::Candles) {
            chart.set_drawings(prev_drawings.clone());
        }

        Content::Kline {
            chart: Some(chart),
            indicators: enabled_indicators,
            layout,
            kind: determined_chart_kind,
            drawings: prev_drawings,
        }
    }

    fn placeholder(kind: ContentKind) -> Self {
        match kind {
            ContentKind::Starter => Content::Starter,
            ContentKind::CandlestickChart => Content::Kline {
                chart: None,
                indicators: vec![KlineIndicator::Volume],
                kind: data::chart::KlineChartKind::Candles,
                layout: ViewConfig {
                    splits: vec![],
                    autoscale: Some(data::chart::Autoscale::FitToVisible),
                },
                drawings: vec![],
            },
            ContentKind::FootprintChart => Content::Kline {
                chart: None,
                indicators: vec![KlineIndicator::Volume],
                kind: data::chart::KlineChartKind::Footprint {
                    clusters: data::chart::kline::ClusterKind::default(),
                    scaling: data::chart::kline::ClusterScaling::default(),
                    studies: vec![],
                },
                layout: ViewConfig {
                    splits: vec![],
                    autoscale: Some(data::chart::Autoscale::FitToVisible),
                },
                drawings: vec![],
            },
            ContentKind::ShaderHeatmap => Content::ShaderHeatmap {
                chart: None,
                indicators: vec![HeatmapIndicator::Volume],
                studies: vec![data::chart::heatmap::HeatmapStudy::VolumeProfile(
                    data::chart::heatmap::ProfileKind::default(),
                )],
            },
            ContentKind::HeatmapChart => Content::Heatmap {
                chart: None,
                indicators: vec![HeatmapIndicator::Volume],
                studies: vec![],
                layout: ViewConfig {
                    splits: vec![],
                    autoscale: Some(data::chart::Autoscale::CenterLatest),
                },
            },
            ContentKind::ComparisonChart => Content::Comparison(None),
            ContentKind::GexChart => Content::Gex {
                chart: None,
                underlying: exchange::options::OptionsUnderlying::Btc,
                liquidity_reference: None,
                liquidity_reference_source: None,
                unsupported: false,
            },
            ContentKind::TimeAndSales => Content::TimeAndSales(None),
            ContentKind::Ladder => Content::Ladder(None),
        }
    }

    pub fn last_tick(&self) -> Option<Instant> {
        match self {
            Content::Heatmap { chart, .. } => Some(chart.as_ref()?.last_update()),
            Content::Kline { chart, .. } => Some(chart.as_ref()?.last_update()),
            Content::TimeAndSales(panel) => Some(panel.as_ref()?.last_update()),
            Content::Ladder(panel) => Some(panel.as_ref()?.last_update()),
            Content::Comparison(chart) => Some(chart.as_ref()?.last_update()),
            Content::Gex { chart, .. } => Some(chart.as_ref()?.last_tick()),
            Content::Starter => None,
            Content::ShaderHeatmap { chart, .. } => Some(chart.as_ref()?.last_tick?),
        }
    }

    pub fn chart_kind(&self) -> Option<data::chart::KlineChartKind> {
        match self {
            Content::Kline { chart, .. } => Some(chart.as_ref()?.kind().clone()),
            _ => None,
        }
    }

    pub fn toggle_indicator(&mut self, indicator: UiIndicator) {
        match (self, indicator) {
            (
                Content::Heatmap {
                    chart, indicators, ..
                },
                UiIndicator::Heatmap(ind),
            ) => {
                let Some(chart) = chart else {
                    return;
                };

                if indicators.contains(&ind) {
                    indicators.retain(|i| i != &ind);
                } else {
                    indicators.push(ind);
                }
                chart.toggle_indicator(ind);
            }
            (
                Content::Kline {
                    chart,
                    indicators,
                    kind,
                    ..
                },
                UiIndicator::Kline(ind),
            ) => {
                let Some(chart) = chart else {
                    return;
                };
                if !kind.allows_indicator(ind) {
                    return;
                }

                if indicators.contains(&ind) {
                    indicators.retain(|i| i != &ind);
                } else {
                    indicators.push(ind);
                }
                chart.toggle_indicator(ind);
            }
            (
                Content::ShaderHeatmap {
                    chart, indicators, ..
                },
                UiIndicator::Heatmap(ind),
            ) => {
                let Some(chart) = chart else {
                    return;
                };

                if indicators.contains(&ind) {
                    indicators.retain(|i| i != &ind);
                } else {
                    indicators.push(ind);
                }
                chart.toggle_indicator(ind);
            }
            _ => panic!("indicator toggle on {indicator:?} pane",),
        }
    }

    pub fn reorder_indicators(&mut self, event: &column_drag::DragEvent) {
        match self {
            Content::Heatmap { indicators, .. } => column_drag::reorder_vec(indicators, event),
            Content::Kline { indicators, .. } => column_drag::reorder_vec(indicators, event),
            Content::TimeAndSales(_)
            | Content::Ladder(_)
            | Content::Starter
            | Content::Comparison(_)
            | Content::Gex { .. }
            | Content::ShaderHeatmap { .. } => {
                panic!("indicator reorder on {} pane", self)
            }
        }
    }

    pub fn change_visual_config(&mut self, config: VisualConfig) -> bool {
        match (self, config) {
            (Content::Kline { chart: Some(c), .. }, VisualConfig::Kline(cfg)) => {
                c.set_visual_config(cfg);
                false
            }
            (Content::Heatmap { chart: Some(c), .. }, VisualConfig::Heatmap(cfg)) => {
                c.set_visual_config(cfg);
                false
            }
            (Content::ShaderHeatmap { chart: Some(c), .. }, VisualConfig::Heatmap(cfg)) => {
                c.set_visual_config(cfg);
                false
            }
            (Content::Comparison(Some(chart)), VisualConfig::Comparison(cfg)) => {
                chart.config = cfg;
                false
            }
            (
                Content::Gex {
                    chart: Some(chart), ..
                },
                VisualConfig::Gex(cfg),
            ) => {
                let refresh_streams =
                    chart.config().show_gamma_liquidity_panel != cfg.show_gamma_liquidity_panel;
                chart.set_config(cfg);
                refresh_streams
            }
            (Content::TimeAndSales(Some(panel)), VisualConfig::TimeAndSales(cfg)) => {
                panel.config = cfg;
                false
            }
            (Content::Ladder(Some(panel)), VisualConfig::Ladder(cfg)) => {
                panel.config = cfg;
                false
            }
            _ => false,
        }
    }

    pub fn studies(&self) -> Option<data::chart::Study> {
        match &self {
            Content::Heatmap { studies, .. } => Some(data::chart::Study::Heatmap(studies.clone())),
            Content::ShaderHeatmap { studies, .. } => {
                Some(data::chart::Study::Heatmap(studies.clone()))
            }
            Content::Kline { kind, .. } => {
                if let data::chart::KlineChartKind::Footprint { studies, .. } = kind {
                    Some(data::chart::Study::Footprint(studies.clone()))
                } else {
                    None
                }
            }
            Content::TimeAndSales(_)
            | Content::Ladder(_)
            | Content::Starter
            | Content::Comparison(_) => None,
            Content::Gex { .. } => None,
        }
    }

    pub fn clusters(&self) -> Option<data::chart::kline::ClusterKind> {
        match self {
            Content::Kline {
                kind: data::chart::KlineChartKind::Footprint { clusters, .. },
                ..
            } => Some(*clusters),
            _ => None,
        }
    }

    pub fn update_studies(&mut self, studies: data::chart::Study) {
        match (self, studies) {
            (
                Content::Heatmap {
                    chart,
                    studies: previous,
                    ..
                },
                data::chart::Study::Heatmap(studies),
            ) => {
                chart
                    .as_mut()
                    .expect("heatmap chart not initialized")
                    .studies = studies.clone();
                *previous = studies;
            }
            (
                Content::ShaderHeatmap {
                    chart,
                    studies: previous,
                    ..
                },
                data::chart::Study::Heatmap(studies),
            ) => {
                chart
                    .as_mut()
                    .expect("shader heatmap chart not initialized")
                    .studies = studies.clone();
                *previous = studies;
            }
            (Content::Kline { chart, kind, .. }, data::chart::Study::Footprint(studies)) => {
                let chart = chart.as_mut().expect("kline chart not initialized");
                chart.set_studies(studies.clone());
                if let data::chart::KlineChartKind::Footprint {
                    studies: k_studies, ..
                } = kind
                {
                    *k_studies = chart.studies().unwrap_or_default();
                }
            }
            _ => {}
        }
    }

    pub fn kind(&self) -> ContentKind {
        match self {
            Content::Heatmap { .. } => ContentKind::HeatmapChart,
            Content::Kline { kind, .. } => match kind {
                data::chart::KlineChartKind::Footprint { .. } => ContentKind::FootprintChart,
                data::chart::KlineChartKind::Candles => ContentKind::CandlestickChart,
            },
            Content::TimeAndSales(_) => ContentKind::TimeAndSales,
            Content::Ladder(_) => ContentKind::Ladder,
            Content::Comparison(_) => ContentKind::ComparisonChart,
            Content::Gex { .. } => ContentKind::GexChart,
            Content::Starter => ContentKind::Starter,
            Content::ShaderHeatmap { .. } => ContentKind::ShaderHeatmap,
        }
    }

    pub fn update_theme(&mut self, theme: &iced_core::Theme) {
        if let Content::ShaderHeatmap { chart: Some(c), .. } = self {
            c.update_theme(theme);
        }
    }

    pub(super) fn initialized(&self) -> bool {
        match self {
            Content::Heatmap { chart, .. } => chart.is_some(),
            Content::ShaderHeatmap { chart, .. } => chart.is_some(),
            Content::Kline { chart, .. } => chart.is_some(),
            Content::TimeAndSales(panel) => panel.is_some(),
            Content::Ladder(panel) => panel.is_some(),
            Content::Comparison(chart) => chart.is_some(),
            Content::Gex {
                chart, unsupported, ..
            } => chart.is_some() || *unsupported,
            Content::Starter => true,
        }
    }

    pub fn allows_indicator(&self, indicator: UiIndicator) -> bool {
        match (self, indicator) {
            (Content::Kline { kind, .. }, UiIndicator::Kline(indicator)) => {
                kind.allows_indicator(indicator)
            }
            (Content::Heatmap { .. } | Content::ShaderHeatmap { .. }, UiIndicator::Heatmap(_)) => {
                true
            }
            _ => false,
        }
    }
}

impl std::fmt::Display for Content {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.kind())
    }
}

impl PartialEq for Content {
    fn eq(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Content::Starter, Content::Starter)
                | (Content::Heatmap { .. }, Content::Heatmap { .. })
                | (Content::Kline { .. }, Content::Kline { .. })
                | (Content::TimeAndSales(_), Content::TimeAndSales(_))
                | (Content::Ladder(_), Content::Ladder(_))
                | (Content::Gex { .. }, Content::Gex { .. })
        )
    }
}

fn link_group_modal<'a>(
    pane: pane_grid::Pane,
    selected_group: Option<LinkGroup>,
) -> Element<'a, Message> {
    let mut grid = column![].spacing(4);
    let rows = LinkGroup::ALL.chunks(3);

    for row_groups in rows {
        let mut button_row = row![].spacing(4);

        for &group in row_groups {
            let is_selected = selected_group == Some(group);
            let btn_content = text(group.to_string()).font(style::AZERET_MONO);

            let btn = if is_selected {
                button_with_tooltip(
                    btn_content.align_x(iced::Alignment::Center),
                    Message::SwitchLinkGroup(pane, None),
                    Some("Unlink"),
                    tooltip::Position::Bottom,
                    move |theme, status| style::button::menu_body(theme, status, true),
                )
            } else {
                button(btn_content.align_x(iced::Alignment::Center))
                    .on_press(Message::SwitchLinkGroup(pane, Some(group)))
                    .style(move |theme, status| style::button::menu_body(theme, status, false))
                    .into()
            };

            button_row = button_row.push(btn);
        }

        grid = grid.push(button_row);
    }

    container(grid)
        .max_width(240)
        .padding(16)
        .style(style::chart_modal)
        .into()
}

fn ticksize_modifier<'a>(
    id: pane_grid::Pane,
    price_step: PriceStep,
    min_ticksize: Option<exchange::unit::MinTicksize>,
    multiplier: TickMultiplier,
    modifier: Option<modal::stream::Modifier>,
    kind: ModifierKind,
    exchange: Option<exchange::adapter::Exchange>,
) -> Element<'a, Message> {
    let modifier_modal =
        Modal::StreamModifier(modal::stream::Modifier::new(kind).with_ticksize_view(
            price_step,
            min_ticksize,
            multiplier,
            exchange,
        ));

    let is_active = modifier.is_some_and(|m| {
        matches!(
            m.view_mode,
            modal::stream::ViewMode::TicksizeSelection { .. }
        )
    });

    button(text(multiplier.to_string()).align_y(Alignment::Center))
        .style(move |theme, status| style::button::modifier(theme, status, !is_active))
        .on_press(Message::PaneEvent(id, Event::ShowModal(modifier_modal)))
        .height(widget::PANE_CONTROL_BTN_HEIGHT)
        .into()
}

fn basis_modifier<'a>(
    id: pane_grid::Pane,
    selected_basis: Basis,
    modifier: Option<modal::stream::Modifier>,
    kind: ModifierKind,
) -> Element<'a, Message> {
    let modifier_modal = Modal::StreamModifier(
        modal::stream::Modifier::new(kind).with_view_mode(modal::stream::ViewMode::BasisSelection),
    );

    let is_active =
        modifier.is_some_and(|m| m.view_mode == modal::stream::ViewMode::BasisSelection);

    button(text(selected_basis.to_string()).align_y(Alignment::Center))
        .style(move |theme, status| style::button::modifier(theme, status, !is_active))
        .on_press(Message::PaneEvent(id, Event::ShowModal(modifier_modal)))
        .height(widget::PANE_CONTROL_BTN_HEIGHT)
        .into()
}

fn by_basis_default<T>(
    basis: Option<Basis>,
    default_tf: Timeframe,
    on_time: impl FnOnce(Timeframe) -> T,
    on_tick: impl FnOnce() -> T,
) -> T {
    match basis.unwrap_or(Basis::Time(default_tf)) {
        Basis::Time(tf) => on_time(tf),
        Basis::Tick(_) => on_tick(),
    }
}

/// Determines whether the popout button should be shown in pane controls.
///
/// Returns `false` when native popout is not allowed (e.g., Windows / SingleWindowEmbedded)
/// to avoid showing a button that would be blocked at runtime.
#[cfg(test)]
pub fn should_show_popout_button(
    total_panes: usize,
    is_popout: bool,
    allow_native_popout: bool,
) -> bool {
    if is_popout {
        // Merge button only shown for real native popout windows
        allow_native_popout
    } else {
        // Pop out button only shown when there are multiple panes AND native popout is allowed
        total_panes > 1 && allow_native_popout
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use exchange::{Ticker, adapter::Exchange};

    fn ticker() -> TickerInfo {
        TickerInfo::new(
            Ticker::new("BTCUSDT", Exchange::BinanceLinear),
            0.01,
            0.001,
            None,
        )
    }

    fn gex_state(config: data::chart::gex::Config) -> State {
        let reference = ticker();
        State::from_config(
            Content::Gex {
                chart: Some(GexChart::new(
                    exchange::options::OptionsUnderlying::Btc,
                    Some(config),
                    Some(reference),
                )),
                underlying: exchange::options::OptionsUnderlying::Btc,
                liquidity_reference: Some(reference),
                liquidity_reference_source: Some(GexLiquidityReferenceSource::Persisted),
                unsupported: false,
            },
            Vec::new(),
            Settings::default(),
            None,
        )
    }

    #[test]
    fn popout_button_hidden_in_embedded_mode() {
        // SingleWindowEmbedded (Windows): no popout button even with multiple panes
        assert!(!should_show_popout_button(2, false, false));
        assert!(!should_show_popout_button(3, false, false));
    }

    #[test]
    fn popout_button_shown_in_native_multi_window() {
        // NativeMultiWindow (macOS/Linux): popout button shown with multiple panes
        assert!(should_show_popout_button(2, false, true));
        assert!(should_show_popout_button(3, false, true));
    }

    #[test]
    fn popout_button_hidden_for_single_pane() {
        // Single pane: no popout button regardless of mode
        assert!(!should_show_popout_button(1, false, true));
        assert!(!should_show_popout_button(1, false, false));
    }

    #[test]
    fn merge_button_only_for_native_popout() {
        // Merge button shown only when is_popout AND native popout is allowed
        assert!(should_show_popout_button(1, true, true));
        // In embedded mode, no merge button (no native popout windows should exist)
        assert!(!should_show_popout_button(1, true, false));
    }

    #[test]
    fn gex_liquidity_stream_toggles_without_duplicates_or_primary_pair() {
        let mut state = gex_state(data::chart::gex::Config::default());
        assert_eq!(
            state
                .streams
                .ready_iter()
                .expect("ready")
                .filter(|stream| matches!(stream, StreamKind::Depth { .. }))
                .count(),
            1
        );
        assert_eq!(state.stream_pair(), None);
        state.reconcile_gex_liquidity_stream();
        assert_eq!(state.streams.ready_iter().expect("ready").count(), 1);

        let disabled = data::chart::gex::Config {
            show_gamma_liquidity_panel: false,
            ..data::chart::gex::Config::default()
        };
        assert!(
            state
                .content
                .change_visual_config(VisualConfig::Gex(disabled))
        );
        state.reconcile_gex_liquidity_stream();
        assert_eq!(state.streams.ready_iter().expect("ready").count(), 0);

        assert!(
            state
                .content
                .change_visual_config(VisualConfig::Gex(data::chart::gex::Config::default()))
        );
        state.reconcile_gex_liquidity_stream();
        assert_eq!(state.streams.ready_iter().expect("ready").count(), 1);
        assert_eq!(state.stream_pair(), None);
    }

    #[test]
    fn changing_gex_reference_replaces_the_depth_stream() {
        let mut state = gex_state(data::chart::gex::Config::default());
        let old = ticker();
        let next = TickerInfo::new(
            Ticker::new("BTCUSDT", Exchange::BybitLinear),
            0.01,
            0.001,
            None,
        );
        assert!(
            state
                .set_gex_liquidity_reference(Some(next), Some(GexLiquidityReferenceSource::Manual))
        );
        let depth_streams = state
            .streams
            .ready_iter()
            .expect("ready")
            .filter_map(|stream| match stream {
                StreamKind::Depth { ticker_info, .. } => Some(*ticker_info),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(depth_streams, vec![next]);
        assert!(!depth_streams.contains(&old));
        state.reconcile_gex_liquidity_stream();
        assert_eq!(state.streams.ready_iter().expect("ready").count(), 1);
    }

    #[test]
    fn xaut_proxy_keeps_target_spot_depth_stream_when_liquidity_panel_is_hidden() {
        let reference = TickerInfo::new(
            Ticker::new("XAUTUSDT", Exchange::BybitLinear),
            0.01,
            0.001,
            None,
        );
        let config = data::chart::gex::Config {
            show_gamma_liquidity_panel: false,
            ..data::chart::gex::Config::default()
        };
        let mut state = State::from_config(
            Content::Gex {
                chart: Some(GexChart::new(
                    exchange::options::OptionsUnderlying::Gld,
                    Some(config),
                    Some(reference),
                )),
                underlying: exchange::options::OptionsUnderlying::Gld,
                liquidity_reference: Some(reference),
                liquidity_reference_source: Some(GexLiquidityReferenceSource::Persisted),
                unsupported: false,
            },
            Vec::new(),
            Settings::default(),
            None,
        );
        state.reconcile_gex_liquidity_stream();
        let depth_references = state
            .streams
            .ready_iter()
            .expect("ready")
            .filter_map(|stream| match stream {
                StreamKind::Depth { ticker_info, .. } => Some(*ticker_info),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(depth_references, [reference]);
    }

    #[test]
    fn dedicated_gex_chart_can_switch_from_btc_to_xaut_proxy() {
        let mut state = gex_state(data::chart::gex::Config::default());
        let xaut = TickerInfo::new(
            Ticker::new("XAUTUSDT", Exchange::BybitLinear),
            0.01,
            0.001,
            None,
        );
        state.set_content_and_streams(vec![xaut], ContentKind::GexChart);
        let Content::Gex {
            chart,
            underlying,
            liquidity_reference,
            unsupported,
            ..
        } = &state.content
        else {
            panic!("GEX content");
        };
        assert_eq!(*underlying, exchange::options::OptionsUnderlying::Gld);
        assert_eq!(*liquidity_reference, Some(xaut));
        assert_eq!(
            chart.as_ref().and_then(GexChart::liquidity_reference),
            Some(xaut)
        );
        assert!(!unsupported);
    }

    #[test]
    fn gex_asset_selector_contains_only_btc_eth_and_xau() {
        assert_eq!(
            GexChartAsset::ALL.map(|asset| asset.to_string()),
            ["BTC", "ETH", "XAU"]
        );

        let mut state = gex_state(data::chart::gex::Config::default());
        assert!(matches!(
            state.update(Event::GexAssetSelected(GexChartAsset::Xau)),
            Some(Effect::RefreshStreams)
        ));
        let Content::Gex {
            chart,
            underlying,
            liquidity_reference,
            ..
        } = &state.content
        else {
            panic!("GEX content");
        };
        assert_eq!(*underlying, exchange::options::OptionsUnderlying::Gld);
        assert_eq!(*liquidity_reference, None);
        assert_eq!(
            chart.as_ref().map(GexChart::underlying),
            Some(exchange::options::OptionsUnderlying::Gld)
        );
    }
}
