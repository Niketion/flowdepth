use crate::{
    Event, Kline, PushFrequency, Ticker, TickerInfo, Timeframe, Trade, UnixMs, Volume,
    adapter::{AdapterError, RithmicConfig, StreamKind, StreamTicksize},
    depth::{DeOrder, DepthPayload, DepthUpdate, LocalDepthCache},
    unit::{Price, Qty},
};
use futures::stream::BoxStream;
use rithmic_rs::{
    ConnectStrategy, RithmicConfig as ProtocolConfig, RithmicEnv, RithmicHistoryPlant,
    RithmicTickerPlant,
    rti::{
        messages::RithmicMessage,
        request_search_symbols::{InstrumentType, Pattern},
        request_time_bar_replay::BarType,
    },
};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::OnceCell;

#[derive(Clone)]
pub struct RithmicHandle {
    config: Arc<RithmicConfig>,
    ticker: Arc<OnceCell<RithmicTickerPlant>>,
    history: Arc<OnceCell<RithmicHistoryPlant>>,
    ticker_login: Arc<OnceCell<()>>,
    history_login: Arc<OnceCell<()>>,
}

impl RithmicHandle {
    pub fn new(config: RithmicConfig) -> Result<Self, AdapterError> {
        if !config.is_complete() {
            return Err(AdapterError::InvalidRequest(
                "Rithmic configuration is incomplete or does not use wss".to_string(),
            ));
        }

        Ok(Self {
            config: Arc::new(config),
            ticker: Arc::new(OnceCell::new()),
            history: Arc::new(OnceCell::new()),
            ticker_login: Arc::new(OnceCell::new()),
            history_login: Arc::new(OnceCell::new()),
        })
    }

    fn protocol_config(&self) -> Result<ProtocolConfig, AdapterError> {
        ProtocolConfig::builder(RithmicEnv::Test)
            .url(self.config.url.clone())
            .beta_url(self.config.url.clone())
            .user(self.config.user.clone())
            .password(self.config.password.clone())
            .system_name(self.config.system_name.clone())
            .app_name(self.config.app_name.clone())
            .app_version(self.config.app_version.clone())
            .build()
            .map_err(|error| AdapterError::InvalidRequest(error.to_string()))
    }

    async fn ticker_handle(&self) -> Result<rithmic_rs::RithmicTickerPlantHandle, AdapterError> {
        let config = self.protocol_config()?;
        let plant = self
            .ticker
            .get_or_try_init(|| async move {
                RithmicTickerPlant::connect(&config, ConnectStrategy::Simple)
                    .await
                    .map_err(|error| AdapterError::WebsocketError(error.to_string()))
            })
            .await?;
        self.ticker_login
            .get_or_try_init(|| async {
                plant
                    .get_handle()
                    .login()
                    .await
                    .map(|_| ())
                    .map_err(|error| AdapterError::Unavailable {
                        venue: crate::adapter::Venue::Rithmic,
                        reason: format!("Rithmic login failed: {error}"),
                    })
            })
            .await?;
        Ok(plant.get_handle())
    }

    async fn history_handle(&self) -> Result<rithmic_rs::RithmicHistoryPlantHandle, AdapterError> {
        let config = self.protocol_config()?;
        let plant = self
            .history
            .get_or_try_init(|| async move {
                RithmicHistoryPlant::connect(&config, ConnectStrategy::Simple)
                    .await
                    .map_err(|error| AdapterError::WebsocketError(error.to_string()))
            })
            .await?;
        self.history_login
            .get_or_try_init(|| async {
                plant
                    .get_handle()
                    .login()
                    .await
                    .map(|_| ())
                    .map_err(|error| AdapterError::Unavailable {
                        venue: crate::adapter::Venue::Rithmic,
                        reason: format!("Rithmic history login failed: {error}"),
                    })
            })
            .await?;
        Ok(plant.get_handle())
    }

    pub async fn search_futures(&self, query: &str) -> Result<Vec<TickerInfo>, AdapterError> {
        let handle = self.ticker_handle().await?;
        let responses = handle
            .search_symbols(
                query,
                None,
                None,
                Some(InstrumentType::Future),
                Some(Pattern::Contains),
            )
            .await
            .map_err(|error| AdapterError::InvalidRequest(error.to_string()))?;

        // Search returns the product root plus contracts for many years. On the
        // delayed Test feed the front-month request has no symbol, but search
        // already provides contract codes. Keep only the nearest unexpired one.
        let today = chrono::Utc::now()
            .format("%Y%m%d")
            .to_string()
            .parse::<u32>()
            .unwrap_or_default();
        let mut current_contracts: HashMap<(String, String), (String, Option<String>, u32, bool)> =
            HashMap::new();
        for response in responses {
            let RithmicMessage::ResponseSearchSymbols(symbol) = response.message else {
                continue;
            };
            let (Some(code), Some(route)) = (symbol.symbol, symbol.exchange) else {
                continue;
            };

            let product = symbol.product_code.as_deref().unwrap_or(&code).to_owned();
            let Some(expiration) = symbol
                .expiration_date
                .as_deref()
                .and_then(|date| date.get(..8))
                .and_then(|date| date.parse::<u32>().ok())
            else {
                log::info!(
                    "Rithmic search candidate skipped without expiration query={query} symbol={code} product={product} exchange={route}"
                );
                continue;
            };
            if expiration < today {
                continue;
            }
            log::info!(
                "Rithmic search candidate query={query} symbol={code} product={product} exchange={route} expiration={expiration}"
            );
            let is_product_root = code == product;
            let key = (product, route.clone());
            let replace = current_contracts.get(&key).is_none_or(
                |(_, _, selected_expiration, selected_is_root)| {
                    (expiration, is_product_root) < (*selected_expiration, *selected_is_root)
                },
            );
            if replace {
                current_contracts
                    .insert(key, (code, symbol.symbol_name, expiration, is_product_root));
            }
        }

        let mut result = Vec::new();
        for ((product, route), (code, display_symbol, expiration, _)) in current_contracts {
            log::info!(
                "Rithmic current contract selected product={product} symbol={code} exchange={route} expiration={expiration}"
            );
            let ticker = Ticker::new_routed(
                &code,
                crate::adapter::Exchange::RithmicFutures,
                &route,
                display_symbol.as_deref(),
            );
            match self.reference_data(ticker).await {
                Ok(ticker_info) => {
                    let now = UnixMs::now();
                    let from = UnixMs::new(now.as_u64().saturating_sub(86_400_000));
                    match self
                        .fetch_klines(ticker_info.clone(), Timeframe::M15, Some((from, now)))
                        .await
                    {
                        Ok(bars) if !bars.is_empty() => {
                            log::info!(
                                "Rithmic contract accepted symbol={code} exchange={route} bars={}",
                                bars.len()
                            );
                            result.push(ticker_info);
                        }
                        Ok(_) => log::warn!(
                            "Rithmic contract skipped because it has no recent bars symbol={code} exchange={route}"
                        ),
                        Err(error) => log::warn!(
                            "Rithmic contract skipped because historical validation failed symbol={code} exchange={route}: {error}"
                        ),
                    }
                }
                Err(error) => {
                    log::warn!("Rithmic symbol skipped symbol={code} exchange={route}: {error}")
                }
            }
        }
        Ok(result)
    }

    pub async fn reference_data(&self, ticker: Ticker) -> Result<TickerInfo, AdapterError> {
        let route = ticker.route().ok_or_else(|| {
            AdapterError::InvalidRequest(
                "Rithmic ticker is missing its native exchange".to_string(),
            )
        })?;
        let handle = self.ticker_handle().await?;
        let response = handle
            .get_reference_data(&ticker.to_string(), route)
            .await
            .map_err(|error| AdapterError::InvalidRequest(error.to_string()))?;
        let RithmicMessage::ResponseReferenceData(data) = response.message else {
            return Err(AdapterError::ParseError(
                "Unexpected Rithmic reference-data response".to_string(),
            ));
        };
        let tick = data.min_qprice_change.unwrap_or(0.0) as f32;
        if !tick.is_finite() || tick <= 0.0 {
            return Err(AdapterError::ParseError(format!(
                "Rithmic reference data has no valid tick size for {ticker}"
            )));
        }
        Ok(TickerInfo::new(ticker, tick, 1.0, None).with_point_value(data.single_point_value))
    }

    pub async fn fetch_klines(
        &self,
        ticker_info: TickerInfo,
        timeframe: Timeframe,
        range: Option<(UnixMs, UnixMs)>,
    ) -> Result<Vec<Kline>, AdapterError> {
        let route = ticker_info.ticker.route().ok_or_else(|| {
            AdapterError::InvalidRequest(
                "Rithmic ticker is missing its native exchange".to_string(),
            )
        })?;
        let now = chrono::Utc::now().timestamp() as i32;
        let (from, to) = range.map_or((now - 86_400, now), |(from, to)| {
            (
                from.as_u64().saturating_div(1_000) as i32,
                to.as_u64().saturating_div(1_000) as i32,
            )
        });
        let (bar_type, period) = if timeframe == Timeframe::D1 {
            (BarType::DailyBar, 1)
        } else {
            (BarType::MinuteBar, i32::from(timeframe.to_minutes()))
        };
        // Symbol discovery returns delayed routes (e.g. CME-Delayed). The
        // History Plant may require the native code (CME), while live market
        // data subscriptions must retain the delayed route. Try both routes.
        let history_routes = route
            .strip_suffix("-Delayed")
            .map(|native_route| vec![native_route.to_string()])
            .unwrap_or_else(|| vec![route.to_string()]);
        let history = self.history_handle().await?;
        for history_route in history_routes {
            let responses = match history
                .load_time_bars(
                    ticker_info.ticker.to_string(),
                    history_route.clone(),
                    bar_type,
                    period,
                    from,
                    to,
                )
                .await
            {
                Ok(responses) => responses,
                Err(error) => {
                    log::warn!(
                        "Rithmic historical request failed symbol={} exchange={history_route}: {error}",
                        ticker_info.ticker
                    );
                    continue;
                }
            };
            let response_count = responses.len();
            let mut raw_bars = Vec::new();
            for response in responses {
                if let Some(error) = response.error {
                    log::warn!(
                        "Rithmic historical response rejected symbol={} exchange={history_route}: {error}",
                        ticker_info.ticker
                    );
                    continue;
                }
                let RithmicMessage::ResponseTimeBarReplay(bar) = response.message else {
                    continue;
                };
                let (Some(open), Some(high), Some(low), Some(close)) = (
                    bar.open_price,
                    bar.high_price,
                    bar.low_price,
                    bar.close_price,
                ) else {
                    continue;
                };
                let buy = Qty::from_f64(bar.ask_volume.unwrap_or_default() as f64);
                let sell = Qty::from_f64(bar.bid_volume.unwrap_or_default() as f64);
                raw_bars.push((open, high, low, close, buy, sell));
            }
            // Time-bar replay does not include a timestamp. Rithmic returns the
            // latest bar first, so restore chronological order and anchor the
            // sequence at the end of the requested range.
            let interval = timeframe.to_milliseconds();
            let end =
                u64::try_from(to).unwrap_or_default().saturating_mul(1_000) / interval * interval;
            let start = end
                .saturating_sub((raw_bars.len().saturating_sub(1) as u64).saturating_mul(interval));
            let bars = raw_bars
                .into_iter()
                .rev()
                .enumerate()
                .map(|(index, (open, high, low, close, buy, sell))| {
                    Kline::new(
                        start.saturating_add((index as u64).saturating_mul(interval)),
                        open,
                        high,
                        low,
                        close,
                        Volume::BuySell(buy, sell),
                        ticker_info.min_ticksize,
                    )
                })
                .collect::<Vec<_>>();
            log::info!(
                "Rithmic historical response symbol={} exchange={history_route} raw_responses={response_count} bars={}",
                ticker_info.ticker,
                bars.len()
            );
            if !bars.is_empty() {
                return Ok(bars);
            }
        }
        Ok(Vec::new())
    }

    pub async fn fetch_trades(
        &self,
        ticker_info: TickerInfo,
        from: UnixMs,
    ) -> Result<Vec<Trade>, AdapterError> {
        let route = ticker_info.ticker.route().ok_or_else(|| {
            AdapterError::InvalidRequest(
                "Rithmic ticker is missing its native exchange".to_string(),
            )
        })?;
        let now = chrono::Utc::now().timestamp() as i32;
        let responses = self
            .history_handle()
            .await?
            .load_ticks(
                ticker_info.ticker.to_string(),
                route.to_string(),
                from.as_u64().saturating_div(1_000) as i32,
                now,
            )
            .await
            .map_err(|error| AdapterError::InvalidRequest(error.to_string()))?;
        let mut trades = Vec::new();
        for response in responses {
            let RithmicMessage::ResponseTickBarReplay(bar) = response.message else {
                continue;
            };
            let Some(time) = bar.data_bar_ssboe.first().copied() else {
                continue;
            };
            let (Some(price), Some(volume)) = (bar.close_price, bar.volume) else {
                continue;
            };
            let is_sell = match (
                bar.bid_volume.unwrap_or_default(),
                bar.ask_volume.unwrap_or_default(),
            ) {
                (bid, 0) if bid > 0 => true,
                (0, ask) if ask > 0 => false,
                _ => continue,
            };
            trades.push(Trade {
                id: None,
                time: (u64::try_from(time).unwrap_or_default() * 1_000).into(),
                is_sell,
                price: Price::from_f64(price).round_to_min_tick(ticker_info.min_ticksize),
                qty: Qty::from_f64(volume as f64),
            });
        }
        Ok(trades)
    }

    pub fn connect_trade_stream(self, tickers: Vec<TickerInfo>) -> BoxStream<'static, Event> {
        let scope: Arc<[StreamKind]> = Arc::from(
            tickers
                .iter()
                .copied()
                .map(|ticker_info| StreamKind::Trades { ticker_info })
                .collect::<Vec<_>>(),
        );
        Box::pin(async_stream::stream! {
            let handle = match self.ticker_handle().await {
                Ok(handle) => handle,
                Err(error) => {
                    yield Event::Disconnected(scope.clone(), error.ui_message());
                    return;
                }
            };
            for ticker_info in &tickers {
                let Some(route) = ticker_info.ticker.route() else {
                    yield Event::Disconnected(scope.clone(), "Rithmic ticker is missing its native exchange".to_string());
                    return;
                };
                if let Err(error) = handle.subscribe(&ticker_info.ticker.to_string(), route).await {
                    yield Event::Disconnected(scope.clone(), error.to_string());
                    return;
                }
            }
            yield Event::Connected(scope.clone());
            let mut receiver = handle.subscription_receiver;
            loop {
                let update = match receiver.recv().await {
                    Ok(update) => update,
                    Err(error) => {
                        yield Event::Disconnected(scope.clone(), format!("Rithmic trade stream ended: {error}"));
                        return;
                    }
                };
                let RithmicMessage::LastTrade(trade) = update.message else { continue; };
                let Some(ticker_info) = tickers.iter().copied().find(|info| {
                    info.ticker.to_string() == trade.symbol.as_deref().unwrap_or_default()
                        && info.ticker.route() == trade.exchange.as_deref()
                }) else { continue; };
                let (Some(price), Some(size), Some(aggressor)) =
                    (trade.trade_price, trade.trade_size, trade.aggressor)
                else { continue; };
                let is_sell = aggressor == 2;
                let seconds = trade.source_ssboe.or(trade.ssboe).unwrap_or_default();
                let micros = trade.source_usecs.or(trade.usecs).unwrap_or_default();
                let time = (u64::try_from(seconds).unwrap_or_default() * 1_000)
                    .saturating_add(u64::try_from(micros).unwrap_or_default() / 1_000);
                yield Event::TradesReceived(
                    StreamKind::Trades { ticker_info },
                    time.into(),
                    vec![Trade {
                        id: None,
                        time: time.into(),
                        is_sell,
                        price: Price::from_f64(price).round_to_min_tick(ticker_info.min_ticksize),
                        qty: Qty::from_f64(size.unsigned_abs() as f64),
                    }].into_boxed_slice(),
                );
            }
        })
    }

    pub fn connect_depth_stream(
        self,
        ticker_info: TickerInfo,
        depth_aggr: StreamTicksize,
        push_freq: PushFrequency,
    ) -> BoxStream<'static, Event> {
        let scope: Arc<[StreamKind]> = Arc::from(vec![StreamKind::Depth {
            ticker_info,
            depth_aggr,
            push_freq,
        }]);
        Box::pin(async_stream::stream! {
            let Some(route) = ticker_info.ticker.route().map(str::to_string) else {
                yield Event::Disconnected(scope.clone(), "Rithmic ticker is missing its native exchange".to_string());
                return;
            };
            let handle = match self.ticker_handle().await {
                Ok(handle) => handle,
                Err(error) => {
                    yield Event::Disconnected(scope.clone(), error.ui_message());
                    return;
                }
            };
            if let Err(error) = handle.subscribe_order_book_summary(&ticker_info.ticker.to_string(), &route).await {
                yield Event::Disconnected(scope.clone(), error.to_string());
                return;
            }
            yield Event::Connected(scope.clone());
            let mut cache = LocalDepthCache::default();
            let mut receiver = handle.subscription_receiver;
            loop {
                let update = match receiver.recv().await {
                    Ok(update) => update,
                    Err(error) => {
                        yield Event::Disconnected(scope.clone(), format!("Rithmic depth stream ended: {error}"));
                        return;
                    }
                };
                let RithmicMessage::OrderBook(book) = update.message else { continue; };
                if book.symbol.as_deref() != Some(ticker_info.ticker.to_string().as_str())
                    || book.exchange.as_deref() != Some(route.as_str()) {
                    continue;
                }
                let bids = book.bid_price.iter().zip(book.bid_size.iter()).map(|(price, size)| DeOrder {
                    price: *price,
                    qty: f64::from(*size),
                }).collect();
                let asks = book.ask_price.iter().zip(book.ask_size.iter()).map(|(price, size)| DeOrder {
                    price: *price,
                    qty: f64::from(*size),
                }).collect();
                let seconds = book.ssboe.unwrap_or_default();
                let micros = book.usecs.unwrap_or_default();
                let time = (u64::try_from(seconds).unwrap_or_default() * 1_000)
                    .saturating_add(u64::try_from(micros).unwrap_or_default() / 1_000);
                let payload = DepthPayload { last_update_id: time, time: time.into(), bids, asks };
                let update_type = book.update_type.unwrap_or_default();
                match update_type {
                    1 | 2 => cache = LocalDepthCache::default(),
                    3 => cache.update(DepthUpdate::Snapshot(payload), ticker_info.min_ticksize),
                    4..=7 => cache.update(DepthUpdate::Diff(payload), ticker_info.min_ticksize),
                    _ => continue,
                }
                if matches!(update_type, 3 | 6 | 7) {
                    yield Event::DepthReceived(
                        StreamKind::Depth { ticker_info, depth_aggr, push_freq },
                        time.into(),
                        cache.depth.clone(),
                    );
                }
            }
        })
    }

    pub fn connect_kline_stream(
        self,
        streams: Vec<(TickerInfo, Timeframe)>,
    ) -> BoxStream<'static, Event> {
        let scope: Arc<[StreamKind]> = Arc::from(
            streams
                .iter()
                .map(|(ticker_info, timeframe)| StreamKind::Kline {
                    ticker_info: *ticker_info,
                    timeframe: *timeframe,
                })
                .collect::<Vec<_>>(),
        );
        Box::pin(async_stream::stream! {
            yield Event::Connected(scope.clone());
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                interval.tick().await;
                for (ticker_info, timeframe) in &streams {
                    match self.fetch_klines(*ticker_info, *timeframe, None).await {
                        Ok(klines) => {
                            if let Some(kline) = klines.last().copied() {
                                yield Event::KlineReceived(StreamKind::Kline { ticker_info: *ticker_info, timeframe: *timeframe }, kline);
                            }
                        }
                        Err(error) => {
                            yield Event::Disconnected(scope.clone(), error.ui_message());
                            return;
                        }
                    }
                }
            }
        })
    }
}
