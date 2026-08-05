use crate::orderflow::{
    AggressorSide, BookContinuity, BookDeltaEvent, BookLevelDelta, NormalizedTradeEvent,
    OrderFlowDataQuality, OrderFlowEvent,
};
use crate::{
    Event, Kline, Price, PushFrequency, Ticker, TickerInfo, Trade, Volume,
    adapter::{
        MarketKind, StreamKind, StreamTicksize,
        hub::{TradeBuffer, WsAdapter, WsSession, WsTransport},
    },
    depth::{DeOrder, DepthPayload, DepthUpdate, LocalDepthCache},
    serde_util::de_string_to_number,
    unit::qty::{QtyNormalization, SizeUnit, volume_size_unit},
};

use super::{BinanceHandle, exchange_from_market_type, raw_qty_unit_from_market_type};
use crate::adapter::hub::AdapterError;
use futures::Stream;
use serde::Deserialize;
use sonic_rs::{JsonValueTrait, to_object_iter_unchecked};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};
use tokio::sync::oneshot::{self, error::TryRecvError};

const MAX_PENDING_DEPTH_EVENTS: usize = 512;
const BINANCE_OPCODE_PING_PAYLOAD: &[u8] = b"fs";

fn ws_domain_from_market_type(market: MarketKind) -> &'static str {
    match market {
        MarketKind::Spot => "stream.binance.com",
        MarketKind::LinearPerps => "fstream.binance.com",
        MarketKind::InversePerps => "dstream.binance.com",
        MarketKind::Futures => unreachable!("Binance does not support Rithmic futures"),
    }
}

#[derive(Clone, Copy)]
enum WsTrafficKind {
    Public,
    Market,
}

fn ws_stream_path(market: MarketKind, traffic_kind: WsTrafficKind) -> &'static str {
    match market {
        MarketKind::Spot => "stream",
        MarketKind::LinearPerps | MarketKind::InversePerps => match traffic_kind {
            WsTrafficKind::Public => "public/stream",
            WsTrafficKind::Market => "market/stream",
        },
        MarketKind::Futures => unreachable!("Binance does not support Rithmic futures"),
    }
}

async fn connect_stream_socket(
    market: MarketKind,
    traffic_kind: WsTrafficKind,
    stream: &str,
    proxy_cfg: Option<&crate::proxy::Proxy>,
) -> Result<WsTransport, String> {
    let domain = ws_domain_from_market_type(market);
    let stream_path = ws_stream_path(market, traffic_kind);
    let url = format!("wss://{domain}/{stream_path}?streams={stream}");

    WsTransport::establish(domain, &url, proxy_cfg)
        .await
        .map_err(|e| format!("Failed to connect to websocket: {e}"))
}

#[derive(Deserialize, Debug, Clone)]
struct SonicKline {
    #[serde(rename = "t")]
    time: u64,
    #[serde(rename = "o", deserialize_with = "de_string_to_number")]
    open: f64,
    #[serde(rename = "h", deserialize_with = "de_string_to_number")]
    high: f64,
    #[serde(rename = "l", deserialize_with = "de_string_to_number")]
    low: f64,
    #[serde(rename = "c", deserialize_with = "de_string_to_number")]
    close: f64,
    #[serde(rename = "v", deserialize_with = "de_string_to_number")]
    volume: f64,
    #[serde(rename = "V", deserialize_with = "de_string_to_number")]
    taker_buy_base_asset_volume: f64,
    #[serde(rename = "i")]
    interval: String,
}

#[derive(Deserialize, Debug, Clone)]
struct SonicKlineWrap {
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "k")]
    kline: SonicKline,
}

#[derive(Deserialize, Debug)]
struct SonicTrade {
    #[serde(rename = "t")]
    raw_id: Option<u64>,
    #[serde(rename = "a")]
    aggregate_id: Option<u64>,
    #[serde(rename = "E")]
    event_time: u64,
    #[serde(rename = "T")]
    time: u64,
    #[serde(rename = "p", deserialize_with = "de_string_to_number")]
    price: f64,
    #[serde(rename = "q", deserialize_with = "de_string_to_number")]
    qty: f64,
    #[serde(rename = "m")]
    is_sell: bool,
    /// Undocumented fields observed on Binance USDⓈ-M `@trade` non-execution markers.
    #[serde(rename = "X")]
    execution_type: Option<String>,
    #[serde(rename = "st")]
    stream_status: Option<u8>,
}

impl SonicTrade {
    fn stream_id(&self, kind: TradeStreamKind) -> Option<u64> {
        match kind {
            TradeStreamKind::Raw => self.raw_id,
            TradeStreamKind::Aggregate => self.aggregate_id,
        }
    }
}

enum SonicDepth {
    Spot(SpotDepth),
    Perp(PerpDepth),
}

impl SonicDepth {
    fn apply_depth_diff(
        &self,
        orderbook: &mut LocalDepthCache,
        ticker_info: TickerInfo,
        qty_norm: QtyNormalization,
        prev_id: &mut u64,
        receive_time: crate::UnixMs,
    ) -> ApplyDepthResult {
        let last_update_id = orderbook.last_update_id;

        match self {
            SonicDepth::Perp(de_depth) => {
                if last_update_id == 0 || de_depth.final_id <= last_update_id {
                    return ApplyDepthResult::Skipped;
                }

                let next_expected = last_update_id.saturating_add(1);
                if *prev_id == 0 {
                    if (de_depth.first_id > next_expected) || (next_expected > de_depth.final_id) {
                        return ApplyDepthResult::NeedsResync(format!(
                            "Perp first event out of sync. first_id={}, final_id={}, snapshot_last_id={}",
                            de_depth.first_id, de_depth.final_id, last_update_id
                        ));
                    }
                } else if *prev_id != de_depth.prev_final_id {
                    return ApplyDepthResult::NeedsResync(format!(
                        "Perp out of sync. expected prev_final_id={}, got={}",
                        *prev_id, de_depth.prev_final_id
                    ));
                }

                let delta = normalized_delta_event(
                    self,
                    orderbook,
                    ticker_info,
                    qty_norm,
                    receive_time,
                    BookContinuity::Continuous,
                );
                orderbook.update_with_qty_norm(
                    DepthUpdate::Diff(self.into()),
                    ticker_info.min_ticksize,
                    Some(qty_norm),
                );

                *prev_id = de_depth.final_id;
                ApplyDepthResult::Applied(de_depth.time, delta)
            }
            SonicDepth::Spot(de_depth) => {
                if last_update_id == 0 || de_depth.final_id <= last_update_id {
                    return ApplyDepthResult::Skipped;
                }

                let next_expected = last_update_id.saturating_add(1);
                if *prev_id == 0 {
                    if (de_depth.first_id > next_expected) || (next_expected > de_depth.final_id) {
                        return ApplyDepthResult::NeedsResync(format!(
                            "Spot first event out of sync. first_id={}, final_id={}, snapshot_last_id={}",
                            de_depth.first_id, de_depth.final_id, last_update_id
                        ));
                    }
                } else {
                    let expected_prev = de_depth.first_id.saturating_sub(1);
                    if *prev_id != expected_prev {
                        return ApplyDepthResult::NeedsResync(format!(
                            "Spot out of sync. expected prev_id={}, got={}",
                            *prev_id, expected_prev
                        ));
                    }
                }

                let delta = normalized_delta_event(
                    self,
                    orderbook,
                    ticker_info,
                    qty_norm,
                    receive_time,
                    BookContinuity::Continuous,
                );
                orderbook.update_with_qty_norm(
                    DepthUpdate::Diff(self.into()),
                    ticker_info.min_ticksize,
                    Some(qty_norm),
                );

                *prev_id = de_depth.final_id;
                ApplyDepthResult::Applied(de_depth.time, delta)
            }
        }
    }
}

impl From<&SonicDepth> for DepthPayload {
    fn from(value: &SonicDepth) -> Self {
        let (time, final_id, bids, asks) = match value {
            SonicDepth::Spot(de) => (de.time, de.final_id, &de.bids, &de.asks),
            SonicDepth::Perp(de) => (de.time, de.final_id, &de.bids, &de.asks),
        };

        DepthPayload {
            last_update_id: final_id,
            time: time.into(),
            bids: bids
                .iter()
                .map(|x| DeOrder {
                    price: x.price,
                    qty: x.qty,
                })
                .collect(),
            asks: asks
                .iter()
                .map(|x| DeOrder {
                    price: x.price,
                    qty: x.qty,
                })
                .collect(),
        }
    }
}

#[derive(Deserialize)]
struct SpotDepth {
    #[serde(rename = "E")]
    time: u64,
    #[serde(rename = "U")]
    first_id: u64,
    #[serde(rename = "u")]
    final_id: u64,
    #[serde(rename = "b")]
    bids: Vec<DeOrder>,
    #[serde(rename = "a")]
    asks: Vec<DeOrder>,
}

#[derive(Deserialize)]
struct PerpDepth {
    #[serde(rename = "E")]
    event_time: u64,
    #[serde(rename = "T")]
    time: u64,
    #[serde(rename = "U")]
    first_id: u64,
    #[serde(rename = "u")]
    final_id: u64,
    #[serde(rename = "pu")]
    prev_final_id: u64,
    #[serde(rename = "b")]
    bids: Vec<DeOrder>,
    #[serde(rename = "a")]
    asks: Vec<DeOrder>,
}

fn normalized_delta_event(
    depth: &SonicDepth,
    orderbook: &LocalDepthCache,
    ticker_info: TickerInfo,
    qty_norm: QtyNormalization,
    receive_time: crate::UnixMs,
    continuity: BookContinuity,
) -> BookDeltaEvent {
    let make_levels = |orders: &[DeOrder], is_bid: bool| {
        orders
            .iter()
            .map(|order| {
                let price =
                    Price::from_f64(order.price).round_to_min_tick(ticker_info.min_ticksize);
                let previous_qty = if is_bid {
                    orderbook.depth.bids.get(&price)
                } else {
                    orderbook.depth.asks.get(&price)
                }
                .copied()
                .unwrap_or(crate::unit::Qty::ZERO);
                BookLevelDelta {
                    price,
                    previous_qty,
                    current_qty: crate::unit::Qty::from_f64(
                        qty_norm.normalize(order.qty, order.price),
                    ),
                }
            })
            .collect::<Vec<_>>()
            .into_boxed_slice()
    };

    match depth {
        SonicDepth::Spot(value) => BookDeltaEvent {
            ticker_info,
            exchange_time: value.time.into(),
            transaction_time: None,
            receive_time,
            first_update_id: value.first_id,
            final_update_id: value.final_id,
            previous_final_update_id: None,
            bids: make_levels(&value.bids, true),
            asks: make_levels(&value.asks, false),
            continuity,
        },
        SonicDepth::Perp(value) => BookDeltaEvent {
            ticker_info,
            exchange_time: value.event_time.into(),
            transaction_time: Some(value.time.into()),
            receive_time,
            first_update_id: value.first_id,
            final_update_id: value.final_id,
            previous_final_update_id: Some(value.prev_final_id),
            bids: make_levels(&value.bids, true),
            asks: make_levels(&value.asks, false),
            continuity,
        },
    }
}

enum StreamData {
    Trade(Ticker, SonicTrade, TradeStreamKind),
    Depth(SonicDepth),
    Kline(Ticker, SonicKline),
}

enum StreamWrapper {
    Trade(TradeStreamKind),
    Depth,
    Kline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TradeStreamKind {
    Raw,
    Aggregate,
}

impl StreamWrapper {
    fn from_stream_type(stream_type: &str) -> Option<Self> {
        stream_type
            .split('@')
            .nth(1)
            .and_then(|after_at| match after_at {
                s if s.starts_with("de") => Some(StreamWrapper::Depth),
                "trade" => Some(StreamWrapper::Trade(TradeStreamKind::Raw)),
                "aggTrade" => Some(StreamWrapper::Trade(TradeStreamKind::Aggregate)),
                s if s.starts_with("kl") => Some(StreamWrapper::Kline),
                _ => None,
            })
    }
}

fn feed_de(slice: &[u8], market: MarketKind) -> Result<StreamData, AdapterError> {
    let exchange = exchange_from_market_type(market);

    let mut stream_type: Option<StreamWrapper> = None;
    let mut topic_ticker: Option<Ticker> = None;
    let iter: sonic_rs::ObjectJsonIter = unsafe { to_object_iter_unchecked(slice) };

    for elem in iter {
        let (k, v) = elem.map_err(|e| AdapterError::ParseError(e.to_string()))?;

        if k == "stream" {
            let Some(stream_name) = v.as_str() else {
                continue;
            };

            if let Some(s) = StreamWrapper::from_stream_type(stream_name) {
                stream_type = Some(s);
            }

            if let Some(symbol) = stream_name.split('@').next() {
                topic_ticker = Some(Ticker::new(&symbol.to_uppercase(), exchange));
            }
        } else if k == "data" {
            match stream_type {
                Some(StreamWrapper::Trade(kind)) => {
                    let trade: SonicTrade = sonic_rs::from_str(&v.as_raw_faststr())
                        .map_err(|e| AdapterError::ParseError(e.to_string()))?;

                    if let Some(t) = topic_ticker {
                        return Ok(StreamData::Trade(t, trade, kind));
                    }

                    return Err(AdapterError::ParseError(
                        "Missing ticker for trade data".to_string(),
                    ));
                }
                Some(StreamWrapper::Depth) => match market {
                    MarketKind::Spot => {
                        let depth: SpotDepth = sonic_rs::from_str(&v.as_raw_faststr())
                            .map_err(|e| AdapterError::ParseError(e.to_string()))?;

                        return Ok(StreamData::Depth(SonicDepth::Spot(depth)));
                    }
                    MarketKind::LinearPerps | MarketKind::InversePerps => {
                        let depth: PerpDepth = sonic_rs::from_str(&v.as_raw_faststr())
                            .map_err(|e| AdapterError::ParseError(e.to_string()))?;

                        return Ok(StreamData::Depth(SonicDepth::Perp(depth)));
                    }
                    MarketKind::Futures => unreachable!("Binance does not support Rithmic futures"),
                },
                Some(StreamWrapper::Kline) => {
                    let kline_wrap: SonicKlineWrap = sonic_rs::from_str(&v.as_raw_faststr())
                        .map_err(|e| AdapterError::ParseError(e.to_string()))?;

                    return Ok(StreamData::Kline(
                        Ticker::new(&kline_wrap.symbol, exchange),
                        kline_wrap.kline,
                    ));
                }
                _ => {
                    log::error!("Unknown stream type");
                }
            }
        } else {
            log::error!("Unknown data: {:?}", k);
        }
    }

    Err(AdapterError::ParseError(
        "Failed to parse ws data".to_string(),
    ))
}

struct TradeAdapter {
    market: MarketKind,
    buffer: TradeBuffer,
    orderflow_trades: Vec<NormalizedTradeEvent>,
    logged_first_raw_batch: bool,
    stream: String,
    proxy_cfg: Option<crate::proxy::Proxy>,
}

const MAX_PENDING_ORDERFLOW_TRADES: usize = 16_384;

impl TradeAdapter {
    fn flush_trade_events(&mut self) -> Vec<Event> {
        // Compatibility consumers (bubbles, footprint, CVD) deliberately go first. If the
        // bounded application channel is under pressure, their established batch must not be
        // displaced by detector-only traffic.
        let mut events = self.buffer.flush();
        if self.market == MarketKind::LinearPerps && !self.orderflow_trades.is_empty() {
            let trades = std::mem::take(&mut self.orderflow_trades).into_boxed_slice();
            if !self.logged_first_raw_batch {
                log::info!(
                    "BinanceRawTradeStreamActive | batch_len={} legacy_batches={}",
                    trades.len(),
                    events.len()
                );
                self.logged_first_raw_batch = true;
            }
            events.push(Event::OrderFlowTrades(trades));
        }
        events
    }
}

impl WsAdapter for TradeAdapter {
    async fn connect(&mut self) -> Result<WsTransport, String> {
        let traffic_kind = if self.market == MarketKind::LinearPerps {
            WsTrafficKind::Public
        } else {
            WsTrafficKind::Market
        };
        connect_stream_socket(
            self.market,
            // Binance's USDⓈ-M raw `@trade` feed is served by the public stream endpoint.
            // The market endpoint accepts the WebSocket handshake but does not publish this
            // stream, which looks healthy while silently starving all trade consumers.
            traffic_kind,
            &self.stream,
            self.proxy_cfg.as_ref(),
        )
        .await
    }

    async fn on_connected(&mut self) -> Vec<Event> {
        self.flush_trade_events()
    }

    async fn on_text(&mut self, payload: &[u8]) -> Result<Vec<Event>, String> {
        let receive_time = crate::UnixMs::now();
        if let Ok(StreamData::Trade(ticker, de_trade, stream_kind)) = feed_de(payload, self.market)
        {
            if let Some((ticker_info, qty_norm)) = self.buffer.ticker_info(&ticker) {
                let ticker_info = *ticker_info;
                let Some(trade_id) = de_trade.stream_id(stream_kind) else {
                    return Err(format!("Missing {stream_kind:?} trade ID"));
                };
                let price =
                    Price::from_f64(de_trade.price).round_to_min_tick(ticker_info.min_ticksize);
                let quantity = qty_norm.normalize_qty(de_trade.qty, de_trade.price);

                if !de_trade.price.is_finite()
                    || de_trade.price <= 0.0
                    || !de_trade.qty.is_finite()
                    || de_trade.qty <= 0.0
                    || price.units <= 0
                    || quantity.is_zero()
                {
                    let is_non_execution_marker = de_trade.price == 0.0
                        && de_trade.qty == 0.0
                        && de_trade.execution_type.as_deref() == Some("NA")
                        && de_trade.stream_status == Some(1);
                    let reason = if !de_trade.price.is_finite() || !de_trade.qty.is_finite() {
                        "non_finite_raw_value"
                    } else if de_trade.price <= 0.0 || de_trade.qty <= 0.0 {
                        "non_positive_raw_value"
                    } else if price.units <= 0 {
                        "non_positive_normalized_price"
                    } else {
                        "zero_normalized_quantity"
                    };
                    let public_payload = String::from_utf8_lossy(payload);
                    if is_non_execution_marker {
                        log::debug!(
                            "BINANCE NonExecutionMarkerIgnored | stream={stream_kind:?} market={:?} ticker={} trade_id={trade_id} event_time_ms={} trade_time_ms={} execution_type=NA stream_status=1 action=discarded_before_trade_buffer",
                            self.market,
                            ticker,
                            de_trade.event_time,
                            de_trade.time,
                        );
                    } else {
                        log::warn!(
                            "BINANCE InvalidTradeDiscarded | stream={stream_kind:?} market={:?} ticker={} trade_id={trade_id} event_time_ms={} trade_time_ms={} side={} raw_price={} raw_qty={} normalized_price={} normalized_price_units={} normalized_qty={} min_tick={:?} execution_type={:?} stream_status={:?} reason={reason} action=discarded_before_trade_buffer payload={public_payload:?}",
                            self.market,
                            ticker,
                            de_trade.event_time,
                            de_trade.time,
                            if de_trade.is_sell { "sell" } else { "buy" },
                            de_trade.price,
                            de_trade.qty,
                            price.to_f64(),
                            price.units,
                            quantity.to_f64(),
                            ticker_info.min_ticksize,
                            de_trade.execution_type,
                            de_trade.stream_status,
                        );
                    }
                    return Ok(Vec::new());
                }

                let trade = Trade {
                    id: Some(trade_id),
                    time: de_trade.time.into(),
                    is_sell: de_trade.is_sell,
                    price,
                    qty: quantity,
                };

                if stream_kind == TradeStreamKind::Raw
                    && self.market == MarketKind::LinearPerps
                    && self.orderflow_trades.len() < MAX_PENDING_ORDERFLOW_TRADES
                {
                    self.orderflow_trades.push(NormalizedTradeEvent {
                        ticker_info,
                        event_time: de_trade.event_time.into(),
                        trade_time: de_trade.time.into(),
                        receive_time,
                        trade_id,
                        price,
                        quantity: trade.qty,
                        aggressor: if de_trade.is_sell {
                            AggressorSide::Sell
                        } else {
                            AggressorSide::Buy
                        },
                    });
                }
                // A single raw stream feeds the established live consumers too. Historical
                // aggTrades use a different ID namespace; chart ingestion reconciles overlap by
                // time bucket rather than pretending those IDs are comparable.
                if (self.market == MarketKind::LinearPerps && stream_kind == TradeStreamKind::Raw)
                    || (self.market != MarketKind::LinearPerps
                        && stream_kind == TradeStreamKind::Aggregate)
                {
                    self.buffer.push(ticker, trade);
                }
            } else {
                log::error!("Ticker info not found for ticker: {ticker}");
                return Err("Received trade for unknown ticker".to_string());
            }
        }

        Ok(Vec::new())
    }

    async fn on_disconnected(&mut self, _reason: &str) -> Vec<Event> {
        let mut events = self.flush_trade_events();
        if self.market == MarketKind::LinearPerps {
            let at = crate::UnixMs::now();
            events.extend(self.buffer.ticker_infos().map(|ticker_info| {
                Event::OrderFlow(OrderFlowEvent::Reconnect { ticker_info, at })
            }));
        }
        events
    }

    async fn on_tick(&mut self) -> Vec<Event> {
        self.flush_trade_events()
    }
}

pub fn connect_trade_stream(
    tickers: Vec<TickerInfo>,
    market: MarketKind,
    proxy_cfg: Option<crate::proxy::Proxy>,
) -> impl Stream<Item = Event> {
    let stream_scope: Arc<[StreamKind]> = Arc::from(
        tickers
            .iter()
            .map(|ticker_info| StreamKind::Trades {
                ticker_info: *ticker_info,
            })
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    );

    let stream = tickers
        .iter()
        .map(|ticker_info| {
            let symbol = ticker_info
                .ticker
                .to_full_symbol_and_type()
                .0
                .to_lowercase();
            if market == MarketKind::LinearPerps {
                format!("{symbol}@trade")
            } else {
                format!("{symbol}@aggTrade")
            }
        })
        .collect::<Vec<_>>()
        .join("/");

    let ticker_info_map = tickers
        .iter()
        .map(|ticker_info| {
            (
                ticker_info.ticker,
                (
                    *ticker_info,
                    QtyNormalization::with_raw_qty_unit(
                        volume_size_unit() == SizeUnit::Quote,
                        *ticker_info,
                        raw_qty_unit_from_market_type(market),
                    ),
                ),
            )
        })
        .collect();

    let adapter = TradeAdapter {
        market,
        buffer: TradeBuffer::new(ticker_info_map),
        orderflow_trades: Vec::new(),
        logged_first_raw_batch: false,
        stream: stream.clone(),
        proxy_cfg: proxy_cfg.clone(),
    };

    WsSession::with_opcode_ping(BINANCE_OPCODE_PING_PAYLOAD, stream_scope).run(adapter)
}

struct DepthAdapter {
    handle: BinanceHandle,
    market: MarketKind,
    ticker_info: TickerInfo,
    qty_norm: QtyNormalization,
    stream: StreamKind,
    ws_stream: String,
    proxy_cfg: Option<crate::proxy::Proxy>,
    sync_machine: DepthSyncMachine,
}

impl WsAdapter for DepthAdapter {
    async fn connect(&mut self) -> Result<WsTransport, String> {
        let websocket = connect_stream_socket(
            self.market,
            WsTrafficKind::Public,
            &self.ws_stream,
            self.proxy_cfg.as_ref(),
        )
        .await?;

        self.sync_machine = DepthSyncMachine::new(self.handle.clone(), self.ticker_info.ticker);
        Ok(websocket)
    }

    async fn on_connected(&mut self) -> Vec<Event> {
        self.sync_machine.begin_resync();
        if self.market == MarketKind::LinearPerps {
            vec![Event::OrderFlow(OrderFlowEvent::Quality {
                ticker_info: self.ticker_info,
                at: crate::UnixMs::now(),
                quality: OrderFlowDataQuality::Synchronizing,
            })]
        } else {
            Vec::new()
        }
    }

    async fn on_text(&mut self, payload: &[u8]) -> Result<Vec<Event>, String> {
        let receive_time = crate::UnixMs::now();
        let mut events = self.sync_machine.poll_snapshot_if_ready(
            self.ticker_info,
            self.qty_norm,
            receive_time,
        )?;

        if let Ok(StreamData::Depth(depth_type)) = feed_de(payload, self.market) {
            if let Some((time, delta)) = self.sync_machine.handle_depth_update(
                depth_type,
                self.ticker_info,
                self.qty_norm,
                receive_time,
            )? {
                if self.market == MarketKind::LinearPerps {
                    events.push(Event::OrderFlow(OrderFlowEvent::BookDelta(delta)));
                }
                events.push(Event::DepthReceived(
                    self.stream,
                    time.into(),
                    self.sync_machine.current.depth.clone(),
                ));
            }
            if self.market == MarketKind::LinearPerps && self.sync_machine.take_gap() {
                events.push(Event::OrderFlow(OrderFlowEvent::Quality {
                    ticker_info: self.ticker_info,
                    at: receive_time,
                    quality: OrderFlowDataQuality::Gap,
                }));
            }
        }

        Ok(events)
    }

    async fn on_disconnected(&mut self, _reason: &str) -> Vec<Event> {
        if self.market == MarketKind::LinearPerps {
            vec![Event::OrderFlow(OrderFlowEvent::Reconnect {
                ticker_info: self.ticker_info,
                at: crate::UnixMs::now(),
            })]
        } else {
            Vec::new()
        }
    }
}

enum ApplyDepthResult {
    Applied(u64, BookDeltaEvent),
    Skipped,
    NeedsResync(String),
}

enum DepthSyncState {
    /// Unsynced state where we need snapshots to correctly apply diff. updates.
    /// Buffers incoming diff. updates until snapshot is applied, then replays them.
    /// Never emits local orderbook to the caller in this state.
    WaitingSnapshot(oneshot::Receiver<Result<DepthPayload, AdapterError>>),
    /// Synced and applying live diff. updates, without needing snapshots.
    /// Emits local orderbook to the caller only as live diff. updates are applied.
    Live,
}

struct DepthSyncMachine {
    handle: BinanceHandle,
    ticker: Ticker,
    state: DepthSyncState,
    prev_id: u64,
    pending: VecDeque<SonicDepth>,
    current: LocalDepthCache,
    gap_detected: bool,
}

impl DepthSyncMachine {
    fn new(handle: BinanceHandle, ticker: Ticker) -> Self {
        Self {
            state: DepthSyncState::Live,
            handle,
            ticker,
            prev_id: 0,
            current: LocalDepthCache::default(),
            gap_detected: false,
            pending: VecDeque::new(),
        }
    }

    fn begin_resync(&mut self) {
        let fetch_snapshot = {
            let handle = self.handle.clone();
            let ticker = self.ticker;
            let (tx, rx) = oneshot::channel();

            tokio::spawn(async move {
                let result = handle.fetch_depth_snapshot(ticker).await;
                let _ = tx.send(result);
            });

            rx
        };

        self.state = DepthSyncState::WaitingSnapshot(fetch_snapshot);
    }

    fn take_gap(&mut self) -> bool {
        std::mem::take(&mut self.gap_detected)
    }

    fn handle_snapshot_result(
        &mut self,
        snapshot_result: Result<DepthPayload, AdapterError>,
        ticker_info: TickerInfo,
        qty_norm: QtyNormalization,
        receive_time: crate::UnixMs,
    ) -> Result<Option<BookDeltaEvent>, String> {
        let snapshot = match snapshot_result {
            Ok(snapshot) => snapshot,
            Err(e) => return Err(format!("Depth fetch failed: {e}")),
        };

        self.current.update_with_qty_norm(
            DepthUpdate::Snapshot(snapshot),
            ticker_info.min_ticksize,
            Some(qty_norm),
        );
        self.prev_id = 0;

        while let Some(depth_type) = self.pending.pop_front() {
            match depth_type.apply_depth_diff(
                &mut self.current,
                ticker_info,
                qty_norm,
                &mut self.prev_id,
                receive_time,
            ) {
                ApplyDepthResult::Applied(_, _) => {}
                ApplyDepthResult::Skipped => {}
                ApplyDepthResult::NeedsResync(reason) => {
                    log::warn!("{}", reason);
                    self.gap_detected = true;
                    self.begin_resync();
                    return Ok(None);
                }
            }
        }

        // Publish one coherent boundary after every buffered diff has been replayed. The
        // detector never observes the stale REST image followed by silently consumed deltas.
        let boundary = BookDeltaEvent {
            ticker_info,
            exchange_time: self.current.time,
            transaction_time: None,
            receive_time,
            first_update_id: self.current.last_update_id,
            final_update_id: self.current.last_update_id,
            previous_final_update_id: None,
            bids: self
                .current
                .depth
                .bids
                .iter()
                .map(|(price, qty)| BookLevelDelta {
                    price: *price,
                    previous_qty: crate::unit::Qty::ZERO,
                    current_qty: *qty,
                })
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            asks: self
                .current
                .depth
                .asks
                .iter()
                .map(|(price, qty)| BookLevelDelta {
                    price: *price,
                    previous_qty: crate::unit::Qty::ZERO,
                    current_qty: *qty,
                })
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            continuity: BookContinuity::SnapshotBoundary,
        };

        self.state = DepthSyncState::Live;
        Ok(Some(boundary))
    }

    fn on_live_diff(
        &mut self,
        diff_update: SonicDepth,
        ticker_info: TickerInfo,
        qty_norm: QtyNormalization,
        receive_time: crate::UnixMs,
    ) -> Result<Option<(u64, BookDeltaEvent)>, String> {
        match diff_update.apply_depth_diff(
            &mut self.current,
            ticker_info,
            qty_norm,
            &mut self.prev_id,
            receive_time,
        ) {
            ApplyDepthResult::Applied(time, delta) => Ok(Some((time, delta))),
            ApplyDepthResult::Skipped => Ok(None),
            ApplyDepthResult::NeedsResync(reason) => {
                log::warn!("{}", reason);
                self.gap_detected = true;
                self.pending.clear();
                self.pending.push_back(diff_update);
                self.prev_id = 0;
                self.begin_resync();
                Ok(None)
            }
        }
    }

    fn queue_pending_diff(&mut self, diff_update: SonicDepth) {
        if self.pending.len() == MAX_PENDING_DEPTH_EVENTS {
            self.pending.pop_front();
        }

        self.pending.push_back(diff_update);
    }

    fn poll_snapshot_if_ready(
        &mut self,
        ticker_info: TickerInfo,
        qty_norm: QtyNormalization,
        receive_time: crate::UnixMs,
    ) -> Result<Vec<Event>, String> {
        let snapshot_result = {
            let DepthSyncState::WaitingSnapshot(snapshot_rx) = &mut self.state else {
                return Ok(Vec::new());
            };

            match snapshot_rx.try_recv() {
                Ok(snapshot_result) => Some(snapshot_result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Closed) => {
                    return Err("Depth fetch channel error: channel closed".to_string());
                }
            }
        };

        if let Some(snapshot_result) = snapshot_result
            && let Some(boundary) =
                self.handle_snapshot_result(snapshot_result, ticker_info, qty_norm, receive_time)?
        {
            return Ok(vec![
                Event::OrderFlow(OrderFlowEvent::BookDelta(boundary)),
                Event::OrderFlow(OrderFlowEvent::Quality {
                    ticker_info,
                    at: receive_time,
                    quality: OrderFlowDataQuality::Healthy,
                }),
            ]);
        }

        Ok(Vec::new())
    }

    fn handle_depth_update(
        &mut self,
        diff_update: SonicDepth,
        ticker_info: TickerInfo,
        qty_norm: QtyNormalization,
        receive_time: crate::UnixMs,
    ) -> Result<Option<(u64, BookDeltaEvent)>, String> {
        if matches!(self.state, DepthSyncState::WaitingSnapshot(_)) {
            self.queue_pending_diff(diff_update);
            Ok(None)
        } else {
            self.on_live_diff(diff_update, ticker_info, qty_norm, receive_time)
        }
    }
}

pub fn connect_depth_stream(
    handle: BinanceHandle,
    ticker_info: TickerInfo,
    depth_aggr: StreamTicksize,
    push_freq: PushFrequency,
    proxy_cfg: Option<crate::proxy::Proxy>,
) -> impl Stream<Item = Event> {
    let stream = StreamKind::Depth {
        ticker_info,
        depth_aggr,
        push_freq,
    };
    let stream_scope: Arc<[StreamKind]> = Arc::from(vec![stream].into_boxed_slice());
    let ticker = ticker_info.ticker;
    let (symbol_str, market) = ticker.to_full_symbol_and_type();

    let qty_norm = QtyNormalization::with_raw_qty_unit(
        volume_size_unit() == SizeUnit::Quote,
        ticker_info,
        raw_qty_unit_from_market_type(market),
    );

    let ws_stream = format!("{}@depth@100ms", symbol_str.to_lowercase());

    let adapter = DepthAdapter {
        handle: handle.clone(),
        market,
        ticker_info,
        qty_norm,
        stream,
        ws_stream: ws_stream.clone(),
        proxy_cfg,
        sync_machine: DepthSyncMachine::new(handle, ticker),
    };

    WsSession::with_opcode_ping(BINANCE_OPCODE_PING_PAYLOAD, stream_scope.clone()).run(adapter)
}

struct KlineAdapter {
    market: MarketKind,
    ticker_info_map: HashMap<Ticker, (TickerInfo, QtyNormalization)>,
    timeframe_by_interval: HashMap<String, crate::Timeframe>,
    stream_str: String,
    proxy_cfg: Option<crate::proxy::Proxy>,
}

impl WsAdapter for KlineAdapter {
    async fn connect(&mut self) -> Result<WsTransport, String> {
        connect_stream_socket(
            self.market,
            WsTrafficKind::Market,
            &self.stream_str,
            self.proxy_cfg.as_ref(),
        )
        .await
    }

    async fn on_connected(&mut self) -> Vec<Event> {
        Vec::new()
    }

    async fn on_text(&mut self, payload: &[u8]) -> Result<Vec<Event>, String> {
        if let Ok(StreamData::Kline(ticker, de_kline)) = feed_de(payload, self.market) {
            let Some(timeframe) = self.timeframe_by_interval.get(&de_kline.interval) else {
                return Ok(Vec::new());
            };

            if let Some((ticker_info, qty_norm)) = self.ticker_info_map.get(&ticker) {
                let ticker_info = *ticker_info;

                let buy_volume_raw = de_kline.taker_buy_base_asset_volume;
                let sell_volume_raw = de_kline.volume - buy_volume_raw;

                let buy_volume = qty_norm.normalize_qty(buy_volume_raw, de_kline.close);
                let sell_volume = qty_norm.normalize_qty(sell_volume_raw, de_kline.close);

                let kline = Kline::new(
                    de_kline.time,
                    de_kline.open,
                    de_kline.high,
                    de_kline.low,
                    de_kline.close,
                    Volume::BuySell(buy_volume, sell_volume),
                    ticker_info.min_ticksize,
                );

                return Ok(vec![Event::KlineReceived(
                    StreamKind::Kline {
                        ticker_info,
                        timeframe: *timeframe,
                    },
                    kline,
                )]);
            } else {
                log::error!("Ticker info not found for ticker: {ticker}");
                return Err("Received kline for unknown ticker".to_string());
            }
        }

        Ok(Vec::new())
    }

    async fn on_disconnected(&mut self, _reason: &str) -> Vec<Event> {
        Vec::new()
    }
}

pub fn connect_kline_stream(
    streams: Vec<(TickerInfo, crate::Timeframe)>,
    market: MarketKind,
    proxy_cfg: Option<crate::proxy::Proxy>,
) -> impl Stream<Item = Event> {
    let stream_scope: Arc<[StreamKind]> = Arc::from(
        streams
            .iter()
            .map(|(ticker_info, timeframe)| StreamKind::Kline {
                ticker_info: *ticker_info,
                timeframe: *timeframe,
            })
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    );

    let stream_str = streams
        .iter()
        .map(|(ticker_info, timeframe)| {
            let ticker = ticker_info.ticker;
            format!(
                "{}@kline_{}",
                ticker.to_full_symbol_and_type().0.to_lowercase(),
                timeframe
            )
        })
        .collect::<Vec<String>>()
        .join("/");

    let ticker_info_map = streams
        .iter()
        .map(|(ticker_info, _)| {
            (
                ticker_info.ticker,
                (
                    *ticker_info,
                    QtyNormalization::with_raw_qty_unit(
                        volume_size_unit() == SizeUnit::Quote,
                        *ticker_info,
                        raw_qty_unit_from_market_type(market),
                    ),
                ),
            )
        })
        .collect();

    let timeframe_by_interval = streams
        .iter()
        .map(|(_, timeframe)| (timeframe.to_string(), *timeframe))
        .collect();

    let adapter = KlineAdapter {
        market,
        ticker_info_map,
        timeframe_by_interval,
        stream_str: stream_str.clone(),
        proxy_cfg: proxy_cfg.clone(),
    };

    WsSession::with_opcode_ping(BINANCE_OPCODE_PING_PAYLOAD, stream_scope).run(adapter)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticker_info() -> TickerInfo {
        TickerInfo::new(
            Ticker::new("BTCUSDT", crate::adapter::Exchange::BinanceLinear),
            0.1,
            0.001,
            None,
        )
    }

    fn trade_adapter(ticker_info: TickerInfo) -> TradeAdapter {
        let ticker_info_map = [(
            ticker_info.ticker,
            (
                ticker_info,
                QtyNormalization::with_raw_qty_unit(
                    false,
                    ticker_info,
                    raw_qty_unit_from_market_type(MarketKind::LinearPerps),
                ),
            ),
        )]
        .into_iter()
        .collect();
        TradeAdapter {
            market: MarketKind::LinearPerps,
            buffer: TradeBuffer::new(ticker_info_map),
            orderflow_trades: Vec::new(),
            logged_first_raw_batch: false,
            stream: "btcusdt@trade".to_string(),
            proxy_cfg: None,
        }
    }

    #[test]
    fn parses_raw_trade_id_times_and_maker_semantics() {
        let payload = include_bytes!("../../../../tests/fixtures/binance_raw_trade.json");
        let StreamData::Trade(_, trade, kind) = feed_de(payload, MarketKind::LinearPerps).unwrap()
        else {
            panic!("expected trade");
        };
        assert_eq!(kind, TradeStreamKind::Raw);
        assert_eq!(trade.stream_id(kind), Some(987_654_321));
        assert_eq!(trade.event_time, 1_720_000_000_001);
        assert_eq!(trade.time, 1_720_000_000_000);
        assert!(trade.is_sell, "m=true means seller aggressor");
        assert_eq!(trade.price, 65_000.10);
        assert_eq!(trade.qty, 0.125);

        let buyer = br#"{"stream":"btcusdt@trade","data":{"E":1720000000002,"t":987654322,"T":1720000000001,"p":"65000.20","q":"0.010","m":false}}"#;
        let StreamData::Trade(_, buyer, kind) = feed_de(buyer, MarketKind::LinearPerps).unwrap()
        else {
            panic!("expected buyer trade");
        };
        assert_eq!(kind, TradeStreamKind::Raw);
        assert!(!buyer.is_sell, "m=false means buyer aggressor");
    }

    #[tokio::test]
    async fn raw_trade_feeds_legacy_and_detector_batches() {
        let ticker_info = ticker_info();
        let mut adapter = trade_adapter(ticker_info);
        let raw = include_bytes!("../../../../tests/fixtures/binance_raw_trade.json");
        let StreamData::Trade(raw_ticker, _, raw_kind) =
            feed_de(raw, MarketKind::LinearPerps).unwrap()
        else {
            panic!("expected raw trade");
        };
        assert_eq!(raw_ticker, ticker_info.ticker);
        assert_eq!(raw_kind, TradeStreamKind::Raw);
        assert!(adapter.on_text(raw).await.unwrap().is_empty());
        assert_eq!(adapter.orderflow_trades.len(), 1);
        let batches = adapter.on_tick().await;
        let [
            Event::TradesReceived(_, _, trades),
            Event::OrderFlowTrades(raw_trades),
        ] = batches.as_slice()
        else {
            panic!("legacy trades must be delivered before the detector batch: {batches:?}");
        };
        assert_eq!(trades.len(), 1);
        assert_eq!(trades[0].id, Some(987_654_321));
        assert_eq!(raw_trades.len(), 1);
        assert_eq!(raw_trades[0].trade_id, 987_654_321);
    }

    #[tokio::test]
    async fn non_execution_marker_is_rejected_before_all_consumers() {
        let mut adapter = trade_adapter(ticker_info());
        let marker = br#"{"stream":"btcusdt@trade","data":{"E":1720000000002,"t":987654322,"T":1720000000001,"p":"0","q":"0","X":"NA","m":false,"st":1}}"#;

        assert!(adapter.on_text(marker).await.unwrap().is_empty());
        assert!(adapter.orderflow_trades.is_empty());
        assert!(adapter.on_tick().await.is_empty());
    }

    #[test]
    fn parses_perp_depth_sequence_times_and_zero_quantity() {
        let payload = include_bytes!("../../../../tests/fixtures/binance_diff_depth.json");
        let StreamData::Depth(SonicDepth::Perp(depth)) =
            feed_de(payload, MarketKind::LinearPerps).unwrap()
        else {
            panic!("expected perpetual depth");
        };
        assert_eq!(
            (depth.first_id, depth.final_id, depth.prev_final_id),
            (102, 104, 101)
        );
        assert_eq!(
            (depth.event_time, depth.time),
            (1_720_000_000_100, 1_720_000_000_098)
        );
        assert!(depth.bids[1].qty == 0.0);
        assert_eq!(depth.asks[0].price, 65_000.1);
    }

    #[test]
    fn applies_snapshot_boundary_then_detects_pu_gap() {
        let ticker_info = ticker_info();
        let qty_norm = QtyNormalization::with_raw_qty_unit(
            false,
            ticker_info,
            raw_qty_unit_from_market_type(MarketKind::LinearPerps),
        );
        let mut book = LocalDepthCache::default();
        book.update_with_qty_norm(
            DepthUpdate::Snapshot(DepthPayload {
                last_update_id: 101,
                time: 1_720_000_000_000u64.into(),
                bids: vec![DeOrder {
                    price: 65_000.0,
                    qty: 1.0,
                }],
                asks: vec![DeOrder {
                    price: 65_000.1,
                    qty: 1.1,
                }],
            }),
            ticker_info.min_ticksize,
            Some(qty_norm),
        );
        let payload = include_bytes!("../../../../tests/fixtures/binance_diff_depth.json");
        let StreamData::Depth(first) = feed_de(payload, MarketKind::LinearPerps).unwrap() else {
            panic!("expected depth");
        };
        let mut previous = 0;
        let ApplyDepthResult::Applied(_, delta) = first.apply_depth_diff(
            &mut book,
            ticker_info,
            qty_norm,
            &mut previous,
            1_720_000_000_101u64.into(),
        ) else {
            panic!("expected first boundary update");
        };
        assert_eq!(previous, 104);
        assert_eq!(delta.bids[0].previous_qty, crate::unit::Qty::from_f64(1.0));
        assert_eq!(delta.bids[0].current_qty, crate::unit::Qty::from_f64(1.25));

        let gap_payload = include_bytes!("../../../../tests/fixtures/binance_depth_gap.json");
        let StreamData::Depth(gap) = feed_de(gap_payload, MarketKind::LinearPerps).unwrap() else {
            panic!("expected gap depth");
        };
        assert!(matches!(
            gap.apply_depth_diff(
                &mut book,
                ticker_info,
                qty_norm,
                &mut previous,
                1_720_000_000_201u64.into(),
            ),
            ApplyDepthResult::NeedsResync(_)
        ));
    }
}
