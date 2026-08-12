use super::persistent_cache::{MarketDataCache, market_cache, merge_bubble_summaries};
use data::chart::kline::{BubbleVolumeSummary, VolumeBubbleConfig, cluster_volume_bubble_trades};
use exchange::adapter::{AdapterError, AdapterHandles, Exchange, StreamKind};
use exchange::unit::PriceStep;
use exchange::{Kline, OpenInterest, TickerInfo, Timeframe, Trade, UnixMs};
use iced::{
    Task,
    task::{Handle, Straw, sipper},
};
use rustc_hash::FxHashMap;
use std::cell::Cell;
use std::fmt;
use std::path::PathBuf;
use std::sync::RwLock;
use std::time::{Duration, Instant};
use uuid::Uuid;

use crate::connector::client::{DataSources, ServerClient};

pub use data::TradeFetchMode;

// ── Human-readable log helpers ──────────────────────────────────────────────

/// Format a ticker symbol for display (e.g., "BTCUSDT")
pub fn format_symbol(ticker_info: &TickerInfo) -> String {
    ticker_info
        .ticker
        .display_symbol()
        .unwrap_or(&ticker_info.ticker.to_string())
        .to_string()
}

/// Format an exchange venue for display (e.g., "BinanceLinear")
pub fn format_venue(ticker_info: &TickerInfo) -> String {
    format!("{:?}", ticker_info.exchange())
}

/// Format a ticker with venue and symbol for structured diagnostics.
pub fn format_ticker(ticker_info: &TickerInfo) -> String {
    format!(
        "{}/{}",
        format_venue(ticker_info),
        format_symbol(ticker_info)
    )
}

/// Format a stream kind compactly while preserving the important route keys.
pub fn format_stream(stream: &StreamKind) -> String {
    match stream {
        StreamKind::Kline {
            ticker_info,
            timeframe,
        } => format!("Kline:{}:{timeframe:?}", format_ticker(ticker_info)),
        StreamKind::Depth {
            ticker_info,
            depth_aggr,
            push_freq,
        } => format!(
            "Depth:{}:aggr={depth_aggr:?}:freq={push_freq:?}",
            format_ticker(ticker_info)
        ),
        StreamKind::Trades { ticker_info } => {
            format!("Trades:{}", format_ticker(ticker_info))
        }
    }
}

/// Format a stream slice for bounded single-line diagnostics.
pub fn format_streams(streams: &[StreamKind]) -> String {
    streams
        .iter()
        .map(format_stream)
        .collect::<Vec<_>>()
        .join(",")
}

/// Format an optional request id.
pub fn format_req_id(req_id: Option<Uuid>) -> String {
    req_id.map_or("-".to_string(), short_id)
}

/// Format an optional timestamp.
pub fn format_optional_time(ms: Option<UnixMs>) -> String {
    ms.map_or("-".to_string(), format_time_short)
}

/// Format a UnixMs timestamp as HH:MM:SS.mmm
pub fn format_time_short(ms: UnixMs) -> String {
    let dt = chrono::DateTime::from_timestamp_millis(ms.as_u64() as i64)
        .unwrap_or_default()
        .with_timezone(&chrono::Local);
    dt.format("%H:%M:%S%.3f").to_string()
}

/// Format a time range as HH:MM:SS.mmm → HH:MM:SS.mmm
pub fn format_time_range(from: UnixMs, to: UnixMs) -> String {
    format!("{} → {}", format_time_short(from), format_time_short(to))
}

/// Format a duration in milliseconds as human-readable (e.g., "3.7s" or "625ms")
pub fn format_duration_ms(ms: u64) -> String {
    if ms >= 1000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{ms}ms")
    }
}

/// Shorten a UUID to first 8 characters
pub fn short_id(id: Uuid) -> String {
    let s = id.to_string();
    s[..8.min(s.len())].to_string()
}

/// Format a FetchRange as a human-readable string
pub fn format_fetch_range(fetch: &FetchRange) -> String {
    match fetch {
        FetchRange::Kline(from, to) => {
            format!("Kline {}", format_time_range(*from, *to))
        }
        FetchRange::OpenInterest {
            from,
            to,
            timeframe,
        } => {
            format!("OI {} source={timeframe}", format_time_range(*from, *to))
        }
        FetchRange::Trades(from, to) => {
            format!("Trades {}", format_time_range(*from, *to))
        }
        FetchRange::BubbleSummary {
            from,
            to,
            timeframe_ms,
            ..
        } => {
            format!(
                "BubbleSummary {} tf_ms={timeframe_ms}",
                format_time_range(*from, *to)
            )
        }
    }
}

pub fn format_fetch_range_compact(fetch: Option<FetchRange>) -> String {
    fetch.map_or("-".to_string(), |fetch| format_fetch_range(&fetch))
}

/// Check if a trade range is fully contained within another
fn trades_contained(
    inner_from: UnixMs,
    inner_to: UnixMs,
    outer_from: UnixMs,
    outer_to: UnixMs,
) -> bool {
    inner_from >= outer_from && inner_to <= outer_to
}

static TRADE_FETCH_MODE: RwLock<TradeFetchMode> = RwLock::new(TradeFetchMode::Off);
const ARROW_LIMIT: usize = 400_000;

pub fn toggle_trade_fetch(value: bool) {
    set_trade_fetch_mode(if value {
        TradeFetchMode::Exchange
    } else {
        TradeFetchMode::Off
    });
}

pub fn set_trade_fetch_mode(mode: TradeFetchMode) {
    if let Ok(mut guard) = TRADE_FETCH_MODE.write() {
        *guard = mode;
    } else {
        log::error!("Trade fetch mode lock poisoned — resetting to Off");
    }
}

pub fn trade_fetch_mode() -> TradeFetchMode {
    TRADE_FETCH_MODE
        .read()
        .map(|guard| guard.clone())
        .unwrap_or(TradeFetchMode::Off)
}

pub fn is_trade_fetch_enabled() -> bool {
    trade_fetch_mode() != TradeFetchMode::Off
}

fn supports_exchange_trade_fetch(exchange: Exchange) -> bool {
    matches!(
        exchange,
        Exchange::BinanceSpot
            | Exchange::BinanceLinear
            | Exchange::BinanceInverse
            | Exchange::MexcLinear
            | Exchange::MexcInverse
    )
}

fn exchange_trade_data_path(exchange: Exchange) -> PathBuf {
    data::data_path(Some(match exchange.venue() {
        exchange::adapter::Venue::Mexc => "market_data/mexc/",
        _ => "market_data/binance/",
    }))
}

const TRADE_REST_REQUEST_TIMEOUT: Duration = Duration::from_secs(35);

/// Overall wall-clock timeout for the entire trade worker (all REST batches).
/// Prevents the worker from hanging indefinitely if individual requests succeed
/// but cursor advancement is slow.
const TRADE_WORKER_TIMEOUT: Duration = Duration::from_secs(85);

/// Stop raw Trades fetch after this many consecutive no-progress batches.
const TRADE_NO_PROGRESS_MAX_CONSECUTIVE: usize = 3;

/// If the remaining gap to `target_to` is within this epsilon (ms) and trades
/// have already been collected, treat the fetch as complete rather than
/// retrying the same tiny tail indefinitely.
const NO_PROGRESS_REMAINING_EPSILON_MS: u64 = 1000;
const TRADE_GAP_COALESCE_BRIDGE_MS: u64 = 60_000;

/// Merge nearby uncovered cache slices before REST pagination. The original
/// gaps are still used to decide which records are new to the caller.
fn coalesce_trade_fetch_gaps(
    gaps: &[(UnixMs, UnixMs)],
    max_bridge_ms: u64,
) -> Vec<(UnixMs, UnixMs)> {
    let mut coalesced: Vec<(UnixMs, UnixMs)> = Vec::with_capacity(gaps.len());
    for &(from, to) in gaps {
        if from >= to {
            continue;
        }
        if let Some((_, previous_to)) = coalesced.last_mut()
            && from.saturating_diff(*previous_to) <= max_bridge_ms
        {
            *previous_to = (*previous_to).max(to);
        } else {
            coalesced.push((from, to));
        }
    }
    coalesced
}

fn trade_time_is_in_gaps(time: UnixMs, gaps: &[(UnixMs, UnixMs)]) -> bool {
    gaps.iter().any(|(from, to)| time >= *from && time < *to)
}

/// How long to remember that a trade range returned empty.
const EMPTY_TRADE_FETCH_TTL: Duration = Duration::from_secs(60);

/// Keep only records inside the requested slice and remove REST pagination
/// overlap by stable exchange id. Id-less records are deliberately preserved:
/// equal price/qty/time values can still represent distinct real executions.
fn filter_new_fetched_trades(
    mut batch: Vec<Trade>,
    from: UnixMs,
    to_exclusive: UnixMs,
    last_trade_id: &mut Option<u64>,
) -> (Vec<Trade>, usize) {
    let raw_count = batch.len();
    batch.sort_by_key(|trade| (trade.time, trade.id));
    batch.retain(|trade| {
        if trade.time < from || trade.time >= to_exclusive {
            return false;
        }
        let Some(id) = trade.id else {
            return true;
        };
        if last_trade_id.is_some_and(|last| id <= last) {
            return false;
        }
        *last_trade_id = Some((*last_trade_id).map_or(id, |last| last.max(id)));
        true
    });
    let discarded = raw_count.saturating_sub(batch.len());
    (batch, discarded)
}

#[derive(Debug, Clone)]
pub enum FetchedData {
    Trades {
        batch: Vec<Trade>,
        until_time: UnixMs,
        req_id: Option<uuid::Uuid>,
    },
    BubbleSummary {
        data: Vec<BubbleVolumeSummary>,
        range: (UnixMs, UnixMs),
        trades_seen: usize,
        raw_discarded: usize,
        req_id: Option<uuid::Uuid>,
    },
    Klines {
        data: Vec<Kline>,
        req_id: Option<uuid::Uuid>,
    },
    OI {
        data: Vec<OpenInterest>,
        req_id: Option<uuid::Uuid>,
    },
}

#[derive(thiserror::Error, Debug, Clone)]
pub enum ReqError {
    #[error("Request is already failed: {0}")]
    Failed(String),
}

#[derive(PartialEq, Debug)]
enum RequestStatus {
    Pending,
    Completed(u64),
    Failed {
        error: String,
        failed_at: u64,
        retry_at: u64,
    },
    Superseded {
        reason: &'static str,
        superseded_at: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchSuppressionReason {
    AlreadyPending,
    CoveredByInflight,
    RecentFailed,
    NoStream,
    InvalidRange,
    Throttled,
}

impl FetchSuppressionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AlreadyPending => "already_pending",
            Self::CoveredByInflight => "covered_by_inflight",
            Self::RecentFailed => "recent_failed",
            Self::NoStream => "no_stream",
            Self::InvalidRange => "invalid_range",
            Self::Throttled => "throttled",
        }
    }
}

impl fmt::Display for FetchSuppressionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Default)]
pub struct RequestHandler {
    requests: FxHashMap<Uuid, FetchRequest>,
    last_suppression: Cell<Option<FetchSuppressionReason>>,
    generation_id: u64,
    /// Cache of recently empty trade fetch ranges to avoid re-fetching.
    /// Keyed by full stream identity + time range to avoid cross-symbol suppression.
    empty_trade_fetches: FxHashMap<EmptyTradeFetchKey, Instant>,
}

/// Full identity for the empty trade fetch cache.
/// Includes venue, symbol, market type, and time range so that
/// an empty result for one stream never suppresses a different stream.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EmptyTradeFetchKey {
    venue: String,
    symbol: String,
    market_type: String,
    from_ms: u64,
    to_ms: u64,
}

impl EmptyTradeFetchKey {
    fn from_ticker_info(ticker_info: &TickerInfo, from: UnixMs, to: UnixMs) -> Self {
        Self {
            venue: format_venue(ticker_info),
            symbol: format_symbol(ticker_info),
            market_type: format!("{:?}", ticker_info.exchange()),
            from_ms: from.as_u64(),
            to_ms: to.as_u64(),
        }
    }

    fn stream_display(&self) -> String {
        format!("{}/{}/{}", self.venue, self.symbol, self.market_type)
    }
}

impl RequestHandler {
    const COMPLETED_RETENTION_MS: u64 = 30_000;
    const FAILED_RETENTION_MS: u64 = 5 * 60_000;
    const PENDING_TIMEOUT_MS: u64 = 90_000;
    const SUPERSEDED_RETENTION_MS: u64 = 10_000;

    /// Get the current generation ID.
    pub fn generation_id(&self) -> u64 {
        self.generation_id
    }

    pub fn add_request(&mut self, fetch: FetchRange) -> Result<Option<Uuid>, ReqError> {
        self.add_request_with_id(Uuid::new_v4(), fetch, None)
    }

    /// Add a request with optional ticker_info for empty trade range checking.
    pub fn add_request_with_ticker(
        &mut self,
        fetch: FetchRange,
        ticker_info: Option<&TickerInfo>,
    ) -> Result<Option<Uuid>, ReqError> {
        self.add_request_with_id(Uuid::new_v4(), fetch, ticker_info)
    }

    pub fn last_suppression_reason(&self) -> Option<FetchSuppressionReason> {
        self.last_suppression.get()
    }

    pub fn cleanup_stale(&mut self) {
        let now = chrono::Utc::now().timestamp_millis() as u64;
        self.cleanup_stale_at(now);
        self.cleanup_empty_trade_fetches();
    }

    pub fn has_pending_trade_requests(&self) -> bool {
        self.requests.values().any(|request| {
            matches!(request.fetch_type, FetchRange::Trades(_, _))
                && matches!(request.status, RequestStatus::Pending)
        })
    }

    pub fn is_trade_request(&self, id: Uuid) -> bool {
        self.requests
            .get(&id)
            .is_some_and(|request| matches!(request.fetch_type, FetchRange::Trades(_, _)))
    }

    fn cleanup_empty_trade_fetches(&mut self) {
        let now = Instant::now();
        self.empty_trade_fetches
            .retain(|_, marked_at| now.duration_since(*marked_at) < EMPTY_TRADE_FETCH_TTL);
    }

    /// Mark a trade range as empty (no trades in the tail).
    pub fn mark_empty_trade_range(&mut self, ticker_info: &TickerInfo, from: UnixMs, to: UnixMs) {
        let key = EmptyTradeFetchKey::from_ticker_info(ticker_info, from, to);
        log::info!(
            "FETCH EmptyCoveredInsert | stream={} range={} reason=no_progress_near_target ttl={:?}",
            key.stream_display(),
            format_time_range(from, to),
            EMPTY_TRADE_FETCH_TTL
        );
        self.empty_trade_fetches.insert(key, Instant::now());
    }

    /// Check if a trade range was recently marked as empty.
    pub fn is_empty_trade_range(&self, ticker_info: &TickerInfo, from: UnixMs, to: UnixMs) -> bool {
        let key = EmptyTradeFetchKey::from_ticker_info(ticker_info, from, to);
        self.empty_trade_fetches
            .get(&key)
            .is_some_and(|marked_at| marked_at.elapsed() < EMPTY_TRADE_FETCH_TTL)
    }

    fn retry_delay_ms(attempts: u32) -> u64 {
        let exp = attempts.saturating_sub(1).min(5);
        2_500u64.saturating_mul(1u64 << exp)
    }

    fn set_suppressed(
        &self,
        reason: FetchSuppressionReason,
        id: Uuid,
        fetch: &FetchRange,
        existing_range: Option<String>,
    ) {
        self.last_suppression.set(Some(reason));
        log::info!(
            "FETCH Suppressed | reason={} req={} {} existing_range={}",
            reason,
            short_id(id),
            format_fetch_range(fetch),
            existing_range.unwrap_or_else(|| "-".to_string())
        );
    }

    fn cleanup_stale_at(&mut self, now: u64) {
        let mut remove_ids = Vec::new();
        for (id, request) in &mut self.requests {
            match &request.status {
                RequestStatus::Pending
                    if now.saturating_sub(request.updated_at) >= Self::PENDING_TIMEOUT_MS =>
                {
                    let error = format!(
                        "request timed out after {}",
                        format_duration_ms(now.saturating_sub(request.updated_at))
                    );
                    request.attempts = request.attempts.saturating_add(1);
                    let retry_delay = Self::retry_delay_ms(request.attempts);
                    request.status = RequestStatus::Failed {
                        error,
                        failed_at: now,
                        retry_at: now.saturating_add(retry_delay),
                    };
                    request.updated_at = now;
                    log::warn!(
                        "FETCH Timeout | req={} {} age={} retry_after={}",
                        short_id(*id),
                        format_fetch_range(&request.fetch_type),
                        format_duration_ms(now.saturating_sub(request.created_at)),
                        format_duration_ms(retry_delay)
                    );
                    log::info!(
                        "FETCH PendingRemove | req={} {} reason=timeout",
                        short_id(*id),
                        format_fetch_range(&request.fetch_type)
                    );
                }
                RequestStatus::Completed(ts)
                    if now.saturating_sub(*ts) > Self::COMPLETED_RETENTION_MS =>
                {
                    remove_ids.push(*id);
                }
                RequestStatus::Failed { failed_at, .. }
                    if now.saturating_sub(*failed_at) > Self::FAILED_RETENTION_MS =>
                {
                    remove_ids.push(*id);
                }
                RequestStatus::Superseded { superseded_at, .. }
                    if now.saturating_sub(*superseded_at) > Self::SUPERSEDED_RETENTION_MS =>
                {
                    remove_ids.push(*id);
                }
                _ => {}
            }
        }

        for id in remove_ids {
            if let Some(request) = self.requests.remove(&id) {
                log::debug!(
                    "FETCH Cleanup | req={} {} reason=stale_terminal",
                    short_id(id),
                    format_fetch_range(&request.fetch_type)
                );
            }
        }
    }

    /// Supersede all pending requests. Increments generation ID.
    /// Returns the list of superseded request IDs for emitting terminal events.
    pub fn supersede_all_pending(&mut self, reason: &'static str) -> Vec<Uuid> {
        self.generation_id = self.generation_id.wrapping_add(1);
        let now = chrono::Utc::now().timestamp_millis() as u64;
        let mut superseded_ids = Vec::new();

        for (id, request) in &mut self.requests {
            if request.status == RequestStatus::Pending {
                request.status = RequestStatus::Superseded {
                    reason,
                    superseded_at: now,
                };
                request.updated_at = now;
                superseded_ids.push(*id);
                log::info!(
                    "FETCH Superseded | req={} {} reason={} generation={}",
                    short_id(*id),
                    format_fetch_range(&request.fetch_type),
                    reason,
                    self.generation_id
                );
                log::info!(
                    "FETCH PendingRemove | req={} {} reason=superseded",
                    short_id(*id),
                    format_fetch_range(&request.fetch_type)
                );
            }
        }

        superseded_ids
    }

    /// Check if a request belongs to a stale generation AND is still active.
    /// Returns false for already-terminal requests (Completed, Failed) since
    /// their results have already been applied.
    pub fn is_stale_generation(&self, req_id: Uuid) -> bool {
        if let Some(request) = self.requests.get(&req_id) {
            // Already-terminal requests are not considered stale
            // (their result was already applied before the reset)
            match request.status {
                RequestStatus::Completed(_) | RequestStatus::Failed { .. } => false,
                _ => request.generation != self.generation_id,
            }
        } else {
            // Request not found - treat as stale
            true
        }
    }

    /// Get the generation ID of a specific request.
    pub fn request_generation(&self, req_id: Uuid) -> Option<u64> {
        self.requests.get(&req_id).map(|r| r.generation)
    }

    fn status_counts(&self) -> (usize, usize, usize, usize) {
        self.requests.values().fold(
            (0, 0, 0, 0),
            |(pending, completed, failed, superseded), request| match &request.status {
                RequestStatus::Pending => (pending + 1, completed, failed, superseded),
                RequestStatus::Completed(_) => (pending, completed + 1, failed, superseded),
                RequestStatus::Failed { .. } => (pending, completed, failed + 1, superseded),
                RequestStatus::Superseded { .. } => (pending, completed, failed, superseded + 1),
            },
        )
    }

    pub fn add_request_with_id(
        &mut self,
        id: Uuid,
        fetch: FetchRange,
        ticker_info: Option<&TickerInfo>,
    ) -> Result<Option<Uuid>, ReqError> {
        let now = chrono::Utc::now().timestamp_millis() as u64;
        self.cleanup_stale_at(now);
        self.last_suppression.set(None);
        if !fetch.is_valid() {
            self.set_suppressed(FetchSuppressionReason::InvalidRange, id, &fetch, None);
            return Ok(None);
        }

        // Check if this trade range was recently marked as empty
        if let FetchRange::Trades(from, to) = fetch
            && let Some(ticker_info) = ticker_info
            && self.is_empty_trade_range(ticker_info, from, to)
        {
            log::info!(
                "FETCH Skip | reason=recent_empty_result stream={}/{}/{:?} range={} req={}",
                format_venue(ticker_info),
                format_symbol(ticker_info),
                ticker_info.exchange(),
                format_time_range(from, to),
                short_id(id)
            );
            self.last_suppression
                .set(Some(FetchSuppressionReason::Throttled));
            return Ok(None);
        }

        let request = FetchRequest::new(fetch, now, self.generation_id);
        let retryable_failed = self
            .requests
            .iter()
            .filter_map(|(existing_id, existing_req)| match &existing_req.status {
                RequestStatus::Failed {
                    error, retry_at, ..
                } if existing_req.same_with(&request) && now >= *retry_at => {
                    Some((*existing_id, error.clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for (existing_id, error) in retryable_failed {
            if let Some(existing_req) = self.requests.remove(&existing_id) {
                log::info!(
                    "FETCH RetryAllowed | req={} previous_req={} {} previous_error={}",
                    short_id(id),
                    short_id(existing_id),
                    format_fetch_range(&existing_req.fetch_type),
                    error
                );
            }
        }
        let (pending, completed, failed, superseded) = self.status_counts();

        log::debug!(
            "FETCH AddRequest | req={} {} map_size={} pending={} completed={} failed={} superseded={}",
            short_id(id),
            format_fetch_range(&fetch),
            self.requests.len(),
            pending,
            completed,
            failed,
            superseded
        );

        if let FetchRange::Trades(new_from, new_to) = fetch {
            for (existing_id, existing_req) in &self.requests {
                let FetchRange::Trades(exist_from, exist_to) = existing_req.fetch_type else {
                    continue;
                };

                let exact = new_from == exist_from && new_to == exist_to;
                let contained = trades_contained(new_from, new_to, exist_from, exist_to);

                match &existing_req.status {
                    RequestStatus::Pending if exact || contained => {
                        let reason = if exact {
                            FetchSuppressionReason::AlreadyPending
                        } else {
                            FetchSuppressionReason::CoveredByInflight
                        };
                        self.set_suppressed(
                            reason,
                            id,
                            &fetch,
                            Some(format!(
                                "req={} {}",
                                short_id(*existing_id),
                                format_time_range(exist_from, exist_to)
                            )),
                        );
                        return Ok(None);
                    }
                    RequestStatus::Completed(ts)
                        if (exact || contained)
                            && now.saturating_sub(*ts) <= Self::COMPLETED_RETENTION_MS =>
                    {
                        log::debug!(
                            "FETCH Skipped | reason=completed_cooldown {} existing_req={} existing_range={} age_ms={}",
                            format_fetch_range(&fetch),
                            short_id(*existing_id),
                            format_time_range(exist_from, exist_to),
                            now.saturating_sub(*ts)
                        );
                        self.last_suppression
                            .set(Some(FetchSuppressionReason::Throttled));
                        return Ok(None);
                    }
                    RequestStatus::Failed {
                        error, retry_at, ..
                    } if exact => {
                        if now >= *retry_at {
                            log::info!(
                                "FETCH RetryAllowed | req={} previous_req={} {} previous_error={}",
                                short_id(id),
                                short_id(*existing_id),
                                format_fetch_range(&fetch),
                                error
                            );
                        } else {
                            self.set_suppressed(
                                FetchSuppressionReason::RecentFailed,
                                id,
                                &fetch,
                                Some(format!(
                                    "req={} retry_in={}",
                                    short_id(*existing_id),
                                    format_duration_ms(retry_at.saturating_sub(now))
                                )),
                            );
                            return Err(ReqError::Failed(error.clone()));
                        }
                    }
                    _ => {}
                }
            }
        }

        if let FetchRange::BubbleSummary {
            from: new_from,
            to: new_to,
            timeframe_ms: new_timeframe,
            ..
        } = fetch
        {
            for (existing_id, existing_req) in &self.requests {
                let FetchRange::BubbleSummary {
                    from: exist_from,
                    to: exist_to,
                    timeframe_ms: exist_timeframe,
                    ..
                } = existing_req.fetch_type
                else {
                    continue;
                };

                let exact = new_from == exist_from
                    && new_to == exist_to
                    && new_timeframe == exist_timeframe;
                let contained = new_timeframe == exist_timeframe
                    && trades_contained(new_from, new_to, exist_from, exist_to);

                match &existing_req.status {
                    RequestStatus::Pending if exact || contained => {
                        let reason = if exact {
                            FetchSuppressionReason::AlreadyPending
                        } else {
                            FetchSuppressionReason::CoveredByInflight
                        };
                        self.set_suppressed(
                            reason,
                            id,
                            &fetch,
                            Some(format!(
                                "req={} {}",
                                short_id(*existing_id),
                                format_time_range(exist_from, exist_to)
                            )),
                        );
                        return Ok(None);
                    }
                    RequestStatus::Completed(ts)
                        if (exact || contained)
                            && now.saturating_sub(*ts) <= Self::COMPLETED_RETENTION_MS =>
                    {
                        log::debug!(
                            "FETCH Skipped | reason=completed_cooldown {} existing_req={} existing_range={} age_ms={}",
                            format_fetch_range(&fetch),
                            short_id(*existing_id),
                            format_time_range(exist_from, exist_to),
                            now.saturating_sub(*ts)
                        );
                        self.last_suppression
                            .set(Some(FetchSuppressionReason::Throttled));
                        return Ok(None);
                    }
                    RequestStatus::Failed {
                        error, retry_at, ..
                    } if exact => {
                        if now >= *retry_at {
                            log::info!(
                                "FETCH RetryAllowed | req={} previous_req={} {} previous_error={}",
                                short_id(id),
                                short_id(*existing_id),
                                format_fetch_range(&fetch),
                                error
                            );
                        } else {
                            self.set_suppressed(
                                FetchSuppressionReason::RecentFailed,
                                id,
                                &fetch,
                                Some(format!(
                                    "req={} retry_in={}",
                                    short_id(*existing_id),
                                    format_duration_ms(retry_at.saturating_sub(now))
                                )),
                            );
                            return Err(ReqError::Failed(error.clone()));
                        }
                    }
                    _ => {}
                }
            }
        }

        if let Some((existing_id, existing_req)) = self.requests.iter().find_map(|(k, v)| {
            if v.same_with(&request) {
                Some((*k, v))
            } else {
                None
            }
        }) {
            return match &existing_req.status {
                RequestStatus::Failed {
                    error, retry_at, ..
                } => {
                    if now >= *retry_at {
                        log::info!(
                            "FETCH RetryAllowed | req={} previous_req={} {} previous_error={}",
                            short_id(id),
                            short_id(existing_id),
                            format_fetch_range(&fetch),
                            error
                        );
                        Ok(Some(existing_id))
                    } else {
                        self.set_suppressed(
                            FetchSuppressionReason::RecentFailed,
                            id,
                            &fetch,
                            Some(format!(
                                "req={} retry_in={}",
                                short_id(existing_id),
                                format_duration_ms(retry_at.saturating_sub(now))
                            )),
                        );
                        Err(ReqError::Failed(error.clone()))
                    }
                }
                RequestStatus::Completed(ts) => {
                    if now.saturating_sub(*ts) > Self::COMPLETED_RETENTION_MS {
                        log::info!(
                            "FETCH Retry | reason=completed_cache_expired {} existing_req={}",
                            format_fetch_range(&fetch),
                            short_id(existing_id)
                        );
                        Ok(Some(existing_id))
                    } else {
                        log::debug!(
                            "FETCH Skipped | reason=completed_cooldown {} existing_req={} age_ms={}",
                            format_fetch_range(&fetch),
                            short_id(existing_id),
                            now.saturating_sub(*ts)
                        );
                        self.last_suppression
                            .set(Some(FetchSuppressionReason::Throttled));
                        Ok(None)
                    }
                }
                RequestStatus::Pending => {
                    self.set_suppressed(
                        FetchSuppressionReason::AlreadyPending,
                        id,
                        &fetch,
                        Some(format!("req={}", short_id(existing_id))),
                    );
                    Ok(None)
                }
                RequestStatus::Superseded { .. } => {
                    // Previous request was superseded, reactivate with new generation
                    if let Some(existing_req) = self.requests.get_mut(&existing_id) {
                        log::info!(
                            "FETCH Reactivate | req={} previous_status=Superseded new_generation={} {}",
                            short_id(existing_id),
                            self.generation_id,
                            format_fetch_range(&fetch)
                        );
                        existing_req.status = RequestStatus::Pending;
                        existing_req.generation = self.generation_id;
                        existing_req.updated_at = now;
                        existing_req.attempts = 0;
                    }
                    Ok(Some(existing_id))
                }
            };
        }

        log::info!(
            "FETCH Queued | {} req={} pending_after={} generation={}",
            format_fetch_range(&fetch),
            short_id(id),
            pending + 1,
            self.generation_id
        );
        self.requests.insert(id, request);
        log::info!(
            "FETCH PendingInsert | req={} {} pending_after={} generation={}",
            short_id(id),
            format_fetch_range(&fetch),
            pending + 1,
            self.generation_id
        );
        Ok(Some(id))
    }

    pub fn mark_completed(&mut self, id: Uuid) {
        if let Some(request) = self.requests.get_mut(&id) {
            // Do not overwrite a terminal status (Superseded, Failed, Completed)
            if matches!(request.status, RequestStatus::Superseded { .. }) {
                log::info!(
                    "FETCH StaleResult | req={} action=discard_already_superseded",
                    short_id(id)
                );
                return;
            }
            if matches!(request.status, RequestStatus::Failed { .. }) {
                log::info!(
                    "FETCH StaleResult | req={} action=discard_already_failed",
                    short_id(id)
                );
                return;
            }
            if matches!(request.status, RequestStatus::Completed(_)) {
                log::debug!(
                    "FETCH StaleResult | req={} action=discard_already_completed",
                    short_id(id)
                );
                return;
            }

            let timestamp = chrono::Utc::now().timestamp_millis() as u64;
            request.status = RequestStatus::Completed(timestamp);
            log::debug!(
                "FETCH Completed | req={} {}",
                short_id(id),
                format_fetch_range(&request.fetch_type)
            );
            log::info!(
                "FETCH PendingRemove | req={} {} reason=completed",
                short_id(id),
                format_fetch_range(&request.fetch_type)
            );
        } else {
            log::warn!("FETCH NotFound | req={}", short_id(id));
        }
    }

    /// Mark a range as terminally empty so it is not immediately requested again.
    pub fn mark_no_data(&mut self, id: Uuid) {
        self.mark_completed(id);
    }

    pub fn mark_failed(&mut self, id: Uuid, error: String) {
        if let Some(request) = self.requests.get_mut(&id) {
            // Do not overwrite a terminal status (Superseded, Completed)
            if matches!(request.status, RequestStatus::Superseded { .. }) {
                log::info!(
                    "FETCH StaleResult | req={} action=discard_already_superseded",
                    short_id(id)
                );
                return;
            }
            if matches!(request.status, RequestStatus::Completed(_)) {
                log::info!(
                    "FETCH StaleResult | req={} action=discard_already_completed",
                    short_id(id)
                );
                return;
            }

            let now = chrono::Utc::now().timestamp_millis() as u64;
            request.attempts = request.attempts.saturating_add(1);
            let retry_delay = Self::retry_delay_ms(request.attempts);
            log::warn!(
                "FETCH Failed | req={} {} error={} retry_after={}",
                short_id(id),
                format_fetch_range(&request.fetch_type),
                error,
                format_duration_ms(retry_delay)
            );
            log::info!(
                "FETCH PendingRemove | req={} {} reason=failed",
                short_id(id),
                format_fetch_range(&request.fetch_type)
            );
            request.status = RequestStatus::Failed {
                error,
                failed_at: now,
                retry_at: now.saturating_add(retry_delay),
            };
            request.updated_at = now;
        } else {
            log::warn!("FETCH NotFound | req={}", short_id(id));
        }
    }
}

#[derive(PartialEq, Debug, Clone, Copy)]
pub enum FetchRange {
    Kline(UnixMs, UnixMs),
    OpenInterest {
        from: UnixMs,
        to: UnixMs,
        timeframe: Timeframe,
    },
    Trades(UnixMs, UnixMs),
    BubbleSummary {
        from: UnixMs,
        to: UnixMs,
        timeframe_ms: u64,
        price_step: PriceStep,
        max_candidates_per_candle: usize,
        cluster_window_ms: u32,
        cluster_price_ticks: u32,
    },
}

impl FetchRange {
    fn is_valid(&self) -> bool {
        match *self {
            FetchRange::Kline(from, to) | FetchRange::Trades(from, to) => from < to,
            FetchRange::OpenInterest { from, to, .. } => from < to,
            FetchRange::BubbleSummary { from, to, .. } => from < to,
        }
    }
}

#[derive(PartialEq, Debug)]
struct FetchRequest {
    fetch_type: FetchRange,
    status: RequestStatus,
    created_at: u64,
    updated_at: u64,
    attempts: u32,
    generation: u64,
}

impl FetchRequest {
    fn new(fetch_type: FetchRange, now: u64, generation: u64) -> Self {
        FetchRequest {
            fetch_type,
            status: RequestStatus::Pending,
            created_at: now,
            updated_at: now,
            attempts: 0,
            generation,
        }
    }

    fn same_with(&self, other: &FetchRequest) -> bool {
        match (&self.fetch_type, &other.fetch_type) {
            (FetchRange::Kline(s1, e1), FetchRange::Kline(s2, e2)) => e1 == e2 && s1 == s2,
            (
                FetchRange::OpenInterest {
                    from: s1,
                    to: e1,
                    timeframe: t1,
                },
                FetchRange::OpenInterest {
                    from: s2,
                    to: e2,
                    timeframe: t2,
                },
            ) => e1 == e2 && s1 == s2 && t1 == t2,
            (FetchRange::Trades(s1, e1), FetchRange::Trades(s2, e2)) => e1 == e2 && s1 == s2,
            (
                FetchRange::BubbleSummary {
                    from: s1,
                    to: e1,
                    timeframe_ms: t1,
                    price_step: p1,
                    max_candidates_per_candle: m1,
                    cluster_window_ms: cw1,
                    cluster_price_ticks: cp1,
                },
                FetchRange::BubbleSummary {
                    from: s2,
                    to: e2,
                    timeframe_ms: t2,
                    price_step: p2,
                    max_candidates_per_candle: m2,
                    cluster_window_ms: cw2,
                    cluster_price_ticks: cp2,
                },
            ) => {
                e1 == e2 && s1 == s2 && t1 == t2 && p1 == p2 && m1 == m2 && cw1 == cw2 && cp1 == cp2
            }
            _ => false,
        }
    }
}

pub struct FetchSpec {
    pub req_id: uuid::Uuid,
    pub fetch: FetchRange,
    pub stream: Option<StreamKind>,
}

impl From<(uuid::Uuid, FetchRange, Option<StreamKind>)> for FetchSpec {
    fn from(t: (uuid::Uuid, FetchRange, Option<StreamKind>)) -> Self {
        FetchSpec {
            req_id: t.0,
            fetch: t.1,
            stream: t.2,
        }
    }
}

impl std::fmt::Debug for FetchSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FetchSpec")
            .field("req_id", &self.req_id)
            .field("fetch", &self.fetch)
            .field("stream", &self.stream)
            .finish()
    }
}

impl Clone for FetchSpec {
    fn clone(&self) -> Self {
        FetchSpec {
            req_id: self.req_id,
            fetch: self.fetch,
            stream: self.stream,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InfoKind {
    FetchingKlines,
    FetchingTrades(usize),
    FetchingBubbleSummaries,
    FetchingOI,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FetchTaskStatus {
    Loading(InfoKind),
    Completed {
        req_id: Option<Uuid>,
        fetch: Option<FetchRange>,
        trade_outcome: Option<TradeFetchOutcome>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TradeFetchOutcome {
    /// The worker verified every timestamp before this tail. The tail itself
    /// still needs another pass.
    pub unfilled_tail: Option<(UnixMs, UnixMs)>,
    /// A short-lived negative-cache hint. A retryable timeout tail must never
    /// be treated as an empty market-data interval.
    pub empty_tail: Option<(UnixMs, UnixMs)>,
}

impl TradeFetchOutcome {
    fn retryable(from: UnixMs, to: UnixMs) -> Self {
        Self {
            unfilled_tail: Some((from, to)),
            empty_tail: None,
        }
    }

    fn empty(from: UnixMs, to: UnixMs) -> Self {
        Self {
            unfilled_tail: Some((from, to)),
            empty_tail: Some((from, to)),
        }
    }
}

fn interrupted_trade_fetch(
    requested_from: UnixMs,
    requested_to: UnixMs,
    retry_from: UnixMs,
    reason: String,
) -> Result<TradeFetchOutcome, AdapterError> {
    if retry_from > requested_from {
        Ok(TradeFetchOutcome::retryable(retry_from, requested_to))
    } else {
        Err(AdapterError::InvalidRequest(format!(
            "TimedOut: {reason} without progress"
        )))
    }
}

#[derive(Debug, Clone)]
pub enum FetchUpdate {
    Status {
        pane_id: Uuid,
        status: FetchTaskStatus,
    },
    Data {
        layout_id: Uuid,
        pane_id: Uuid,
        stream: StreamKind,
        data: FetchedData,
    },
    Error {
        pane_id: Uuid,
        error: String,
        req_id: Option<Uuid>,
        fetch: Option<FetchRange>,
    },
}

pub fn request_fetch(
    sources: &DataSources,
    pane_id: Uuid,
    ready_streams: &[StreamKind],
    layout_id: Uuid,
    req_id: Uuid,
    fetch: FetchRange,
    stream: Option<StreamKind>,
    on_trade_handle: &mut impl FnMut(Handle),
    chart_generation: u64,
) -> Task<FetchUpdate> {
    let handles = sources.exchange.clone();
    let server = sources.server.clone();
    log::info!(
        "FETCH Owner | req={} pane={} chart_generation={} {}",
        short_id(req_id),
        short_id(pane_id),
        chart_generation,
        format_fetch_range(&fetch)
    );
    log::debug!(
        "FETCH Dispatch | {} req={} pane={} layout={} ready_streams={} override_stream={}",
        format_fetch_range(&fetch),
        short_id(req_id),
        short_id(pane_id),
        short_id(layout_id),
        ready_streams.len(),
        stream.as_ref().map_or("-".to_string(), format_stream)
    );
    trace_ready_streams("FETCH DispatchStreams", ready_streams);

    match fetch {
        FetchRange::Kline(from, to) => {
            let kline_stream = if let Some(s) = stream {
                Some((s, pane_id))
            } else {
                ready_streams.iter().find_map(|stream| {
                    if let StreamKind::Kline { .. } = stream {
                        Some((*stream, pane_id))
                    } else {
                        None
                    }
                })
            };

            if let Some((stream, pane_uid)) = kline_stream {
                log::info!(
                    "KLINE Start | pane={} req={} stream={} range={}",
                    short_id(pane_uid),
                    short_id(req_id),
                    format_stream(&stream),
                    format_time_range(from, to)
                );
                return kline_fetch_task(
                    handles.clone(),
                    layout_id,
                    pane_uid,
                    stream,
                    Some(req_id),
                    Some((from, to)),
                );
            }

            log::warn!(
                "KLINE Skip | pane={} req={} reason=no_kline_stream ready_streams={}",
                short_id(pane_id),
                short_id(req_id),
                format_streams(ready_streams)
            );
        }
        FetchRange::OpenInterest {
            from,
            to,
            timeframe,
        } => {
            let kline_stream = if let Some(s) = stream {
                match s {
                    StreamKind::Kline { ticker_info, .. } => Some((
                        StreamKind::Kline {
                            ticker_info,
                            timeframe,
                        },
                        pane_id,
                    )),
                    _ => None,
                }
            } else {
                ready_streams.iter().find_map(|stream| {
                    if let StreamKind::Kline { ticker_info, .. } = stream {
                        Some((
                            StreamKind::Kline {
                                ticker_info: *ticker_info,
                                timeframe,
                            },
                            pane_id,
                        ))
                    } else {
                        None
                    }
                })
            };

            if let Some((stream, pane_uid)) = kline_stream {
                log::info!(
                    "OI Start | pane={} req={} stream={} range={}",
                    short_id(pane_uid),
                    short_id(req_id),
                    format_stream(&stream),
                    format_time_range(from, to)
                );
                return oi_fetch_task(
                    handles.clone(),
                    layout_id,
                    pane_uid,
                    stream,
                    Some(req_id),
                    Some((from, to)),
                );
            }

            log::warn!(
                "OI Skip | pane={} req={} reason=no_kline_stream ready_streams={}",
                short_id(pane_id),
                short_id(req_id),
                format_streams(ready_streams)
            );
        }
        FetchRange::Trades(from_time, to_time) => {
            let trade_info = ready_streams.iter().find_map(|stream| {
                if let StreamKind::Trades { ticker_info } = stream {
                    Some((*ticker_info, pane_id, *stream))
                } else {
                    None
                }
            });

            if let Some((ticker_info, pane_id, stream)) = trade_info {
                let supports_exchange_fetch = supports_exchange_trade_fetch(ticker_info.exchange());
                let mode = trade_fetch_mode();

                if mode == TradeFetchMode::Server && server.is_none() {
                    return Task::done(FetchUpdate::Error {
                        pane_id,
                        error: "Server mode selected but the server URL is invalid.".to_string(),
                        req_id: Some(req_id),
                        fetch: Some(FetchRange::Trades(from_time, to_time)),
                    });
                }

                if mode == TradeFetchMode::Exchange && !supports_exchange_fetch {
                    return Task::done(FetchUpdate::Error {
                        pane_id,
                        error: format!(
                            "Trade fetch via exchange API is not supported for {}",
                            ticker_info.exchange()
                        ),
                        req_id: Some(req_id),
                        fetch: Some(FetchRange::Trades(from_time, to_time)),
                    });
                }

                if server.is_some() || supports_exchange_fetch {
                    let data_path = exchange_trade_data_path(ticker_info.exchange());
                    log::info!(
                        "TRADE Start | venue={} symbol={} range={} req={} pane={} stream={} path={}",
                        format_venue(&ticker_info),
                        format_symbol(&ticker_info),
                        format_time_range(from_time, to_time),
                        short_id(req_id),
                        short_id(pane_id),
                        format_stream(&stream),
                        data_path.display()
                    );

                    let (task, handle) = Task::sip(
                        fetch_trades_batched(
                            server,
                            handles.clone(),
                            ticker_info,
                            from_time,
                            to_time,
                            data_path,
                            req_id,
                        ),
                        move |batch| {
                            log::debug!(
                                "TRADE Batch | venue={} symbol={} trades={} req={}",
                                format_venue(&ticker_info),
                                format_symbol(&ticker_info),
                                batch.len(),
                                short_id(req_id)
                            );
                            let data = FetchedData::Trades {
                                batch,
                                until_time: to_time,
                                req_id: Some(req_id),
                            };

                            FetchUpdate::Data {
                                layout_id,
                                pane_id,
                                data,
                                stream,
                            }
                        },
                        move |result| match result {
                            Ok(trade_outcome) => {
                                log::info!(
                                    "TRADE Done | venue={} symbol={} req={} range={} tail={}",
                                    format_venue(&ticker_info),
                                    format_symbol(&ticker_info),
                                    short_id(req_id),
                                    format_time_range(from_time, to_time),
                                    trade_outcome.unfilled_tail
                                        .map(|(f, t)| format_time_range(f, t))
                                        .unwrap_or_else(|| "-".to_string())
                                );
                                FetchUpdate::Status {
                                    pane_id,
                                    status: FetchTaskStatus::Completed {
                                        req_id: Some(req_id),
                                        fetch: Some(FetchRange::Trades(from_time, to_time)),
                                        trade_outcome: Some(trade_outcome),
                                    },
                                }
                            }
                            Err(err) => {
                                let terminal = match &err {
                                    AdapterError::InvalidRequest(message)
                                        if message.starts_with("TimedOut:") =>
                                    {
                                        "TimedOut"
                                    }
                                    _ => "Failed",
                                };
                                log::error!(
                                    "TRADE {terminal} | venue={} symbol={} req={} range={} error={err}",
                                    format_venue(&ticker_info),
                                    format_symbol(&ticker_info),
                                    short_id(req_id),
                                    format_time_range(from_time, to_time)
                                );
                                FetchUpdate::Error {
                                    pane_id,
                                    error: err.ui_message(),
                                    req_id: Some(req_id),
                                    fetch: Some(FetchRange::Trades(from_time, to_time)),
                                }
                            }
                        },
                    )
                    .abortable();

                    on_trade_handle(handle.abort_on_drop());

                    return task;
                } else {
                    log::warn!(
                        "TRADE Skip | venue={} symbol={} req={} reason=unsupported_exchange range={}",
                        format_venue(&ticker_info),
                        format_symbol(&ticker_info),
                        short_id(req_id),
                        format_time_range(from_time, to_time)
                    );
                }
            } else {
                log::warn!(
                    "TRADE Skip | pane={} req={} reason=no_trade_stream ready_streams={}",
                    short_id(pane_id),
                    short_id(req_id),
                    format_streams(ready_streams)
                );
            }
        }
        FetchRange::BubbleSummary {
            from,
            to,
            timeframe_ms,
            price_step,
            max_candidates_per_candle,
            cluster_window_ms,
            cluster_price_ticks,
        } => {
            let kline_info = if let Some(
                kline_stream @ StreamKind::Kline {
                    ticker_info,
                    timeframe,
                },
            ) = stream
            {
                Some((ticker_info, timeframe, pane_id, kline_stream))
            } else {
                ready_streams.iter().find_map(|stream| {
                    if let StreamKind::Kline {
                        ticker_info,
                        timeframe,
                    } = stream
                    {
                        Some((*ticker_info, *timeframe, pane_id, *stream))
                    } else {
                        None
                    }
                })
            };

            if let Some((ticker_info, timeframe, pane_id, stream)) = kline_info {
                let supports_exchange_fetch = supports_exchange_trade_fetch(ticker_info.exchange());

                if supports_exchange_fetch {
                    let data_path = exchange_trade_data_path(ticker_info.exchange());
                    log::info!(
                        "BUBBLE Summary Request | venue={} symbol={} range={} tf={timeframe:?} timeframe_ms={} max_candidates={} req={} pane={}",
                        format_venue(&ticker_info),
                        format_symbol(&ticker_info),
                        format_time_range(from, to),
                        timeframe_ms,
                        max_candidates_per_candle,
                        short_id(req_id),
                        short_id(pane_id)
                    );

                    return bubble_summary_fetch_task(
                        handles.clone(),
                        layout_id,
                        pane_id,
                        stream,
                        req_id,
                        ticker_info,
                        from,
                        to,
                        timeframe_ms,
                        price_step,
                        max_candidates_per_candle,
                        cluster_window_ms,
                        cluster_price_ticks,
                        data_path,
                    );
                }

                log::warn!(
                    "BUBBLE Summary Skip | venue={} symbol={} req={} reason=unsupported_exchange range={}",
                    format_venue(&ticker_info),
                    format_symbol(&ticker_info),
                    short_id(req_id),
                    format_time_range(from, to)
                );
            } else {
                log::warn!(
                    "BUBBLE Summary Skip | pane={} req={} reason=no_kline_stream ready_streams={}",
                    short_id(pane_id),
                    short_id(req_id),
                    format_streams(ready_streams)
                );
            }
        }
    }

    log::warn!(
        "FETCH Suppressed | reason={} req={} pane={} {}",
        FetchSuppressionReason::NoStream,
        short_id(req_id),
        short_id(pane_id),
        format_fetch_range(&fetch)
    );
    Task::done(FetchUpdate::Error {
        pane_id,
        error: "No compatible stream available for fetch".to_string(),
        req_id: Some(req_id),
        fetch: Some(fetch),
    })
}

#[allow(clippy::too_many_arguments)]
fn bubble_summary_fetch_task(
    handles: AdapterHandles,
    layout_id: Uuid,
    pane_id: Uuid,
    stream: StreamKind,
    req_id: Uuid,
    ticker_info: TickerInfo,
    from_time: UnixMs,
    to_time: UnixMs,
    timeframe_ms: u64,
    price_step: PriceStep,
    max_candidates_per_candle: usize,
    cluster_window_ms: u32,
    cluster_price_ticks: u32,
    data_path: PathBuf,
) -> Task<FetchUpdate> {
    let update_status = Task::done(FetchUpdate::Status {
        pane_id,
        status: FetchTaskStatus::Loading(InfoKind::FetchingBubbleSummaries),
    });

    let fetch = async move {
        fetch_bubble_summaries_cached(
            handles,
            ticker_info,
            from_time,
            to_time,
            timeframe_ms,
            price_step,
            max_candidates_per_candle,
            cluster_window_ms,
            cluster_price_ticks,
            data_path,
        )
        .await
        .map_err(|err| err.ui_message())
    };

    update_status.chain(Task::perform(fetch, move |result| match result {
        Ok((data, trades_seen, raw_discarded)) => FetchUpdate::Data {
            layout_id,
            pane_id,
            stream,
            data: FetchedData::BubbleSummary {
                data,
                range: (from_time, to_time),
                trades_seen,
                raw_discarded,
                req_id: Some(req_id),
            },
        },
        Err(error) => FetchUpdate::Error {
            pane_id,
            error,
            req_id: Some(req_id),
            fetch: Some(FetchRange::BubbleSummary {
                from: from_time,
                to: to_time,
                timeframe_ms,
                price_step,
                max_candidates_per_candle,
                cluster_window_ms,
                cluster_price_ticks,
            }),
        },
    }))
}

fn trace_ready_streams(prefix: &str, ready_streams: &[StreamKind]) {
    for (idx, stream) in ready_streams.iter().enumerate() {
        log::trace!("{prefix} | idx={idx} stream={}", format_stream(stream));
    }
}

pub fn request_fetch_many(
    sources: &DataSources,
    pane_id: Uuid,
    ready_streams: &[StreamKind],
    layout_id: Uuid,
    reqs: impl IntoIterator<Item = (Uuid, FetchRange, Option<StreamKind>)>,
    mut on_trade_handle: impl FnMut(Handle),
    chart_generation: u64,
) -> Task<FetchUpdate> {
    let mut tasks = Vec::new();
    let reqs = reqs.into_iter().collect::<Vec<_>>();

    log::debug!(
        "FETCH Many | pane={} layout={} requests={} ready_streams={} chart_generation={}",
        short_id(pane_id),
        short_id(layout_id),
        reqs.len(),
        ready_streams.len(),
        chart_generation
    );
    for (idx, (req_id, fetch, stream)) in reqs.iter().enumerate() {
        log::debug!(
            "FETCH Spec | idx={idx} req={} {} override_stream={}",
            short_id(*req_id),
            format_fetch_range(fetch),
            stream.as_ref().map_or("-".to_string(), format_stream)
        );
    }

    for (req_id, fetch, stream) in reqs {
        tasks.push(request_fetch(
            sources,
            pane_id,
            ready_streams,
            layout_id,
            req_id,
            fetch,
            stream,
            &mut on_trade_handle,
            chart_generation,
        ));
    }

    Task::batch(tasks)
}

async fn fetch_klines_cached(
    handles: &AdapterHandles,
    ticker_info: TickerInfo,
    timeframe: Timeframe,
    range: Option<(UnixMs, UnixMs)>,
) -> Result<Vec<Kline>, AdapterError> {
    let Some((from, to)) = range else {
        return handles.fetch_klines(ticker_info, timeframe, None).await;
    };
    let to_exclusive = to.saturating_add(1);
    let Some(cache) = market_cache() else {
        return handles
            .fetch_klines(ticker_info, timeframe, Some((from, to)))
            .await;
    };

    let cached = cache.read_klines(ticker_info, timeframe, from, to_exclusive);
    let cache_complete = cached.is_complete();
    let mut combined = cached.records;
    for (gap_from, gap_to_exclusive) in cached.gaps {
        let gap_to = gap_to_exclusive.saturating_sub(1);
        let mut fetched = handles
            .fetch_klines(ticker_info, timeframe, Some((gap_from, gap_to)))
            .await?;
        fetched.sort_by_key(|kline| kline.time);
        fetched.dedup_by_key(|kline| kline.time);
        store_contiguous_klines(
            cache,
            ticker_info,
            timeframe,
            gap_from,
            gap_to_exclusive,
            &fetched,
        );
        combined.extend(fetched);
    }
    combined.sort_by_key(|kline| kline.time);
    combined.dedup_by_key(|kline| kline.time);
    if cache_complete {
        log::debug!(
            "KLINE Network Skipped | venue={} symbol={} tf={timeframe:?} range={}",
            format_venue(&ticker_info),
            format_symbol(&ticker_info),
            format_time_range(from, to)
        );
    }
    Ok(combined)
}

/// Persist only ranges proven by contiguous candles. Storing the complete REST
/// request around a partial response makes a missing candle look permanently
/// covered and prevents both the integrity repair and reconnect backfill.
fn store_contiguous_klines(
    cache: &MarketDataCache,
    ticker_info: TickerInfo,
    timeframe: Timeframe,
    requested_from: UnixMs,
    requested_to_exclusive: UnixMs,
    klines: &[Kline],
) {
    let interval = timeframe.to_milliseconds();
    if interval == 0 || klines.is_empty() {
        return;
    }

    let mut run_start = 0usize;
    for index in 1..=klines.len() {
        let run_ended = index == klines.len()
            || klines[index].time > klines[index - 1].time.saturating_add(interval);
        if !run_ended {
            continue;
        }

        let run = &klines[run_start..index];
        let first = run[0].time;
        let last = run[run.len() - 1].time;
        let coverage_from =
            if first <= requested_from && first.saturating_add(interval) > requested_from {
                requested_from
            } else {
                first.max(requested_from)
            };
        let coverage_to = last.saturating_add(interval).min(requested_to_exclusive);

        if coverage_from < coverage_to {
            cache.store_klines(ticker_info, timeframe, coverage_from, coverage_to, run);
        }
        run_start = index;
    }
}

async fn fetch_open_interest_cached(
    handles: &AdapterHandles,
    ticker_info: TickerInfo,
    timeframe: Timeframe,
    range: Option<(UnixMs, UnixMs)>,
) -> Result<Vec<OpenInterest>, AdapterError> {
    let Some((from, to)) = range else {
        return handles
            .fetch_open_interest(ticker_info, timeframe, None)
            .await;
    };
    let to_exclusive = to.saturating_add(1);
    let Some(cache) = market_cache() else {
        return handles
            .fetch_open_interest(ticker_info, timeframe, Some((from, to)))
            .await;
    };

    let cached = cache.read_open_interest(ticker_info, timeframe, from, to_exclusive);
    let cache_complete = cached.is_complete();
    let mut combined = cached.records;
    for (gap_from, gap_to_exclusive) in cached.gaps {
        let gap_to = gap_to_exclusive.saturating_sub(1);
        let fetched = handles
            .fetch_open_interest(ticker_info, timeframe, Some((gap_from, gap_to)))
            .await?;
        cache.store_open_interest(ticker_info, timeframe, gap_from, gap_to_exclusive, &fetched);
        combined.extend(fetched);
    }
    combined.sort_by_key(|oi| oi.time);
    combined.dedup_by_key(|oi| oi.time);
    if cache_complete {
        log::debug!(
            "OI Network Skipped | venue={} symbol={} tf={timeframe:?} range={}",
            format_venue(&ticker_info),
            format_symbol(&ticker_info),
            format_time_range(from, to)
        );
    }
    Ok(combined)
}

pub fn oi_fetch_task(
    handles: AdapterHandles,
    layout_id: Uuid,
    pane_id: Uuid,
    stream: StreamKind,
    req_id: Option<Uuid>,
    range: Option<(UnixMs, UnixMs)>,
) -> Task<FetchUpdate> {
    let update_status = Task::done(FetchUpdate::Status {
        pane_id,
        status: FetchTaskStatus::Loading(InfoKind::FetchingOI),
    });

    let fetch_task = match stream {
        StreamKind::Kline {
            ticker_info,
            timeframe,
        } => {
            let fetch = async move {
                let started = Instant::now();
                log::info!(
                    "OI Request | pane={} req={} venue={} symbol={} tf={timeframe:?} range={}",
                    short_id(pane_id),
                    format_req_id(req_id),
                    format_venue(&ticker_info),
                    format_symbol(&ticker_info),
                    range.map_or("-".to_string(), |(from, to)| format_time_range(from, to))
                );

                let result =
                    fetch_open_interest_cached(&handles, ticker_info, timeframe, range).await;

                match &result {
                    Ok(oi) => {
                        if oi.is_empty() {
                            log::warn!(
                                "OI Empty | pane={} req={} range={} duration={}",
                                short_id(pane_id),
                                format_req_id(req_id),
                                range.map_or("-".to_string(), |(from, to)| format_time_range(
                                    from, to
                                )),
                                format_duration_ms(started.elapsed().as_millis() as u64)
                            );
                        }
                        log::info!(
                            "OI Done | pane={} req={} records={} first={} last={} duration={}",
                            short_id(pane_id),
                            format_req_id(req_id),
                            oi.len(),
                            oi.first()
                                .map_or("-".to_string(), |oi| format_time_short(oi.time)),
                            oi.last()
                                .map_or("-".to_string(), |oi| format_time_short(oi.time)),
                            format_duration_ms(started.elapsed().as_millis() as u64)
                        )
                    }
                    Err(err) => log::error!(
                        "OI Error | pane={} req={} duration={} error={err}",
                        short_id(pane_id),
                        format_req_id(req_id),
                        format_duration_ms(started.elapsed().as_millis() as u64)
                    ),
                }

                result
            };

            Task::perform(
                iced::futures::TryFutureExt::map_err(fetch, |err| err.ui_message()),
                move |result| match result {
                    Ok(oi) => {
                        let data = FetchedData::OI { data: oi, req_id };
                        FetchUpdate::Data {
                            layout_id,
                            pane_id,
                            data,
                            stream,
                        }
                    }
                    Err(err) => FetchUpdate::Error {
                        pane_id,
                        error: err,
                        req_id,
                        fetch: range.map(|(from, to)| FetchRange::OpenInterest {
                            from,
                            to,
                            timeframe,
                        }),
                    },
                },
            )
        }
        _ => {
            log::debug!(
                "Open interest fetch skipped: pane_id={pane_id}, req_id={req_id:?}, stream={stream:?}, reason=not_kline_stream"
            );
            Task::none()
        }
    };

    update_status.chain(fetch_task)
}

pub fn kline_fetch_task(
    handles: AdapterHandles,
    layout_id: Uuid,
    pane_id: Uuid,
    stream: StreamKind,
    req_id: Option<Uuid>,
    range: Option<(UnixMs, UnixMs)>,
) -> Task<FetchUpdate> {
    let update_status = Task::done(FetchUpdate::Status {
        pane_id,
        status: FetchTaskStatus::Loading(InfoKind::FetchingKlines),
    });

    let fetch_task = match stream {
        StreamKind::Kline {
            ticker_info,
            timeframe,
        } => {
            let fetch = async move {
                let started = Instant::now();
                log::info!(
                    "KLINE Request | pane={} req={} venue={} symbol={} tf={timeframe:?} range={}",
                    short_id(pane_id),
                    format_req_id(req_id),
                    format_venue(&ticker_info),
                    format_symbol(&ticker_info),
                    range.map_or("-".to_string(), |(from, to)| format_time_range(from, to))
                );

                let mut result = fetch_klines_cached(&handles, ticker_info, timeframe, range).await;
                let raw_count = result.as_ref().map_or(0, Vec::len);
                if let (Ok(klines), Some((from, to))) = (&mut result, range) {
                    let timeframe_ms = timeframe.to_milliseconds();
                    klines.retain(|kline| {
                        kline.time.saturating_add(timeframe_ms) > from && kline.time <= to
                    });
                }

                match &result {
                    Ok(klines) => {
                        if klines.is_empty() {
                            log::warn!(
                                "KLINE Empty | pane={} req={} range={} raw_records={} duration={}",
                                short_id(pane_id),
                                format_req_id(req_id),
                                range.map_or("-".to_string(), |(from, to)| format_time_range(
                                    from, to
                                )),
                                raw_count,
                                format_duration_ms(started.elapsed().as_millis() as u64)
                            );
                        }
                        log::info!(
                            "KLINE Done | pane={} req={} raw_records={} retained_records={} first={} last={} duration={}",
                            short_id(pane_id),
                            format_req_id(req_id),
                            raw_count,
                            klines.len(),
                            klines
                                .first()
                                .map_or("-".to_string(), |kline| format_time_short(kline.time)),
                            klines
                                .last()
                                .map_or("-".to_string(), |kline| format_time_short(kline.time)),
                            format_duration_ms(started.elapsed().as_millis() as u64)
                        )
                    }
                    Err(err) => log::error!(
                        "KLINE Error | pane={} req={} duration={} error={err}",
                        short_id(pane_id),
                        format_req_id(req_id),
                        format_duration_ms(started.elapsed().as_millis() as u64)
                    ),
                }

                result
            };

            Task::perform(
                iced::futures::TryFutureExt::map_err(fetch, |err| err.ui_message()),
                std::convert::identity,
            )
            .then(move |result| match result {
                Ok(klines) => {
                    let (data, completed) =
                        kline_success_updates(layout_id, pane_id, stream, req_id, range, klines);

                    // Data must reach every target pane before the terminal update
                    // clears reconnect activity and global backfill bookkeeping.
                    Task::done(data).chain(Task::done(completed))
                }
                Err(err) => Task::done(FetchUpdate::Error {
                    pane_id,
                    error: err,
                    req_id,
                    fetch: range.map(|(from, to)| FetchRange::Kline(from, to)),
                }),
            })
        }
        _ => {
            log::debug!(
                "KLINE Skip | pane={} req={:?} reason=not_kline_stream",
                short_id(pane_id),
                req_id.map(short_id)
            );
            Task::none()
        }
    };

    update_status.chain(fetch_task)
}

fn kline_success_updates(
    layout_id: Uuid,
    pane_id: Uuid,
    stream: StreamKind,
    req_id: Option<Uuid>,
    range: Option<(UnixMs, UnixMs)>,
    klines: Vec<Kline>,
) -> (FetchUpdate, FetchUpdate) {
    let data = FetchUpdate::Data {
        layout_id,
        pane_id,
        data: FetchedData::Klines {
            data: klines,
            req_id,
        },
        stream,
    };
    let completed = FetchUpdate::Status {
        pane_id,
        status: FetchTaskStatus::Completed {
            req_id,
            fetch: range.map(|(from, to)| FetchRange::Kline(from, to)),
            trade_outcome: None,
        },
    };

    (data, completed)
}

pub fn fetch_trades_batched(
    server: Option<ServerClient>,
    handles: AdapterHandles,
    ticker_info: TickerInfo,
    from_time: UnixMs,
    to_time: UnixMs,
    data_path: PathBuf,
    req_id: Uuid,
) -> impl Straw<TradeFetchOutcome, Vec<Trade>, AdapterError> {
    let venue = format_venue(&ticker_info);
    let symbol = format_symbol(&ticker_info);

    sipper(async move |mut progress| {
        let started = Instant::now();
        let mut total_trades = 0usize;
        let mut request_count = 0usize;
        let mut last_progress_log = Instant::now();
        let to_exclusive = to_time.saturating_add(1);
        let (cached_trades, gaps) = if let Some(cache) = market_cache() {
            let cached = cache.read_trades(ticker_info, from_time, to_exclusive);
            (cached.records, cached.gaps)
        } else {
            (Vec::new(), vec![(from_time, to_exclusive)])
        };

        log::info!(
            "TRADE Worker | {venue} {symbol} req={} range={} path={} cached={} gaps={}",
            short_id(req_id),
            format_time_range(from_time, to_time),
            data_path.display(),
            cached_trades.len(),
            gaps.len()
        );

        let mut final_latest = cached_trades
            .last()
            .map_or(from_time, |trade| trade.time.min(to_time));
        for chunk in cached_trades.chunks(10_000) {
            total_trades = total_trades.saturating_add(chunk.len());
            let () = progress.send(chunk.to_vec()).await;
        }

        let network_started = Instant::now();
        let network_gaps = coalesce_trade_fetch_gaps(&gaps, TRADE_GAP_COALESCE_BRIDGE_MS);
        if network_gaps.len() < gaps.len() {
            log::info!(
                "TRADE GapCoalesce | venue={venue} symbol={symbol} req={} original={} coalesced={}",
                short_id(req_id),
                gaps.len(),
                network_gaps.len()
            );
        }
        for (gap_from, gap_to_exclusive) in network_gaps {
            if gap_from >= gap_to_exclusive {
                continue;
            }
            let target_to = gap_to_exclusive.saturating_sub(1);
            let mut latest_trade_t = gap_from;
            let mut consecutive_no_progress = 0usize;
            let mut prev_request_from = None;
            let mut prev_request_to = None;
            let mut last_trade_id = None;

            loop {
                if latest_trade_t > target_to {
                    break;
                }
                // Overall worker timeout covers all network gaps, but cached
                // batches are emitted immediately and do not consume it.
                if network_started.elapsed() >= TRADE_WORKER_TIMEOUT {
                    log::warn!(
                        "TRADE Worker Yield | venue={venue} symbol={symbol} req={} range={} elapsed={} requests={request_count} trades={total_trades} retry_from={} target_to={}",
                        short_id(req_id),
                        format_time_range(from_time, to_time),
                        format_duration_ms(started.elapsed().as_millis() as u64),
                        format_time_short(latest_trade_t),
                        format_time_short(to_time)
                    );
                    return interrupted_trade_fetch(
                        from_time,
                        to_time,
                        latest_trade_t,
                        format!(
                            "worker exceeded {}",
                            format_duration_ms(TRADE_WORKER_TIMEOUT.as_millis() as u64)
                        ),
                    );
                }

                request_count += 1;
                if prev_request_from == Some(latest_trade_t) && prev_request_to == Some(target_to) {
                    log::warn!(
                        "TRADE Stop | venue={venue} symbol={symbol} reason=duplicate_request_bounds from={} target_to={} requests={request_count} trades={total_trades} duration={}",
                        format_time_short(latest_trade_t),
                        format_time_short(target_to),
                        format_duration_ms(started.elapsed().as_millis() as u64)
                    );
                    break;
                }
                prev_request_from = Some(latest_trade_t);
                prev_request_to = Some(target_to);

                let request_from = latest_trade_t;
                let request_started = Instant::now();
                log::debug!(
                    "TRADE Request | venue={venue} symbol={symbol} req={} request_idx={request_count} from={} target_to={}",
                    short_id(req_id),
                    format_time_short(request_from),
                    format_time_short(target_to)
                );

                let fetch_result = tokio::time::timeout(TRADE_REST_REQUEST_TIMEOUT, async {
                    if let Some(client) = server.as_ref() {
                        client
                            .fetch_trades_arrow(ticker_info, request_from, target_to, ARROW_LIMIT)
                            .await
                            .map(|parsed| parsed.trades)
                    } else {
                        handles
                            .fetch_trades(ticker_info, request_from, Some(data_path.clone()))
                            .await
                    }
                })
                .await;

                match fetch_result {
                    Err(_) => {
                        let elapsed = request_started.elapsed().as_millis() as u64;
                        log::error!(
                            "TRADE TimedOut | venue={venue} symbol={symbol} req={} request_idx={request_count} from={} target_to={} timeout={} duration={}",
                            short_id(req_id),
                            format_time_short(request_from),
                            format_time_short(target_to),
                            format_duration_ms(TRADE_REST_REQUEST_TIMEOUT.as_millis() as u64),
                            format_duration_ms(elapsed)
                        );
                        return interrupted_trade_fetch(
                            from_time,
                            to_time,
                            request_from,
                            format!(
                                "REST trade fetch exceeded {}",
                                format_duration_ms(TRADE_REST_REQUEST_TIMEOUT.as_millis() as u64)
                            ),
                        );
                    }
                    Ok(Ok(raw_batch)) => {
                        let elapsed = request_started.elapsed().as_millis() as u64;
                        let prev_latest_trade_t = latest_trade_t;
                        log::debug!(
                            "TRADE Response | venue={venue} symbol={symbol} req={} request_idx={request_count} trades={} first={} last={} duration={}",
                            short_id(req_id),
                            raw_batch.len(),
                            format_optional_time(raw_batch.first().map(|trade| trade.time)),
                            format_optional_time(raw_batch.last().map(|trade| trade.time)),
                            format_duration_ms(elapsed)
                        );

                        if raw_batch.is_empty() {
                            log::warn!(
                                "TRADE Stop | venue={venue} symbol={symbol} reason=empty_batch_before_target latest={} target_to={} remaining_ms={} requests={request_count} trades={total_trades} total_duration={}",
                                format_time_short(latest_trade_t),
                                format_time_short(target_to),
                                target_to.saturating_diff(latest_trade_t),
                                format_duration_ms(started.elapsed().as_millis() as u64)
                            );
                            return Ok(TradeFetchOutcome::empty(request_from, to_time));
                        }

                        latest_trade_t =
                            raw_batch.last().map_or(latest_trade_t, |trade| trade.time);
                        final_latest = final_latest.max(latest_trade_t.min(target_to));
                        let covered_to_exclusive = latest_trade_t.min(target_to).saturating_add(1);
                        let (cache_batch, discarded) = filter_new_fetched_trades(
                            raw_batch,
                            request_from,
                            gap_to_exclusive,
                            &mut last_trade_id,
                        );
                        let mut batch = cache_batch.clone();
                        batch.retain(|trade| trade_time_is_in_gaps(trade.time, &gaps));
                        if discarded > 0 {
                            log::debug!(
                                "TRADE Dedup | venue={venue} symbol={symbol} request_idx={request_count} discarded={discarded} retained={}",
                                cache_batch.len()
                            );
                        }
                        if let Some(cache) = market_cache()
                            && request_from < covered_to_exclusive
                        {
                            cache.store_trades(
                                ticker_info,
                                request_from,
                                covered_to_exclusive,
                                &cache_batch,
                            );
                        }

                        if latest_trade_t <= prev_latest_trade_t {
                            consecutive_no_progress += 1;
                            let remaining_ms = target_to.saturating_diff(latest_trade_t);
                            log::warn!(
                                "TRADE NoProgress | venue={venue} symbol={symbol} request_idx={request_count} prev_latest={} new_latest={} target_to={} remaining_ms={remaining_ms} consecutive={consecutive_no_progress} reason=batch_last_not_after_latest",
                                format_time_short(prev_latest_trade_t),
                                format_time_short(latest_trade_t),
                                format_time_short(target_to)
                            );

                            if remaining_ms <= NO_PROGRESS_REMAINING_EPSILON_MS
                                || consecutive_no_progress >= TRADE_NO_PROGRESS_MAX_CONSECUTIVE
                            {
                                let tail_from = latest_trade_t.saturating_add(1);
                                if !batch.is_empty() {
                                    let () = progress.send(batch).await;
                                }
                                return Ok(TradeFetchOutcome::retryable(tail_from, to_time));
                            }

                            let advanced = latest_trade_t.saturating_add(1);
                            if advanced >= target_to {
                                if !batch.is_empty() {
                                    let () = progress.send(batch).await;
                                }
                                return Ok(TradeFetchOutcome::retryable(advanced, to_time));
                            }
                            latest_trade_t = advanced;
                            prev_request_from = None;
                        } else {
                            consecutive_no_progress = 0;
                        }

                        total_trades = total_trades.saturating_add(batch.len());
                        if last_progress_log.elapsed().as_secs() >= 5
                            || request_count.is_multiple_of(10)
                        {
                            log::info!(
                                "TRADE Progress | venue={venue} symbol={symbol} request_idx={request_count} trades={total_trades} latest={} target_to={} remaining_ms={} elapsed={}",
                                format_time_short(latest_trade_t),
                                format_time_short(target_to),
                                target_to.saturating_diff(latest_trade_t),
                                format_duration_ms(started.elapsed().as_millis() as u64)
                            );
                            last_progress_log = Instant::now();
                        }
                        if !batch.is_empty() {
                            let () = progress.send(batch).await;
                        }
                        if latest_trade_t >= target_to {
                            break;
                        }
                    }
                    Ok(Err(err)) => {
                        log::error!(
                            "TRADE Error | venue={venue} symbol={symbol} req={} request_idx={request_count} from={} duration={} error={err}",
                            short_id(req_id),
                            format_time_short(latest_trade_t),
                            format_duration_ms(request_started.elapsed().as_millis() as u64)
                        );
                        return Err(err);
                    }
                }
            }
        }

        log::info!(
            "TRADE Worker Done | venue={venue} symbol={symbol} req={} range={} requests={request_count} returned_trades={total_trades} cached={} final_latest={} duration={}",
            short_id(req_id),
            format_time_range(from_time, to_time),
            cached_trades.len(),
            format_time_short(final_latest),
            format_duration_ms(started.elapsed().as_millis() as u64)
        );

        Ok(TradeFetchOutcome::default())
    })
}

#[allow(clippy::too_many_arguments)]
async fn fetch_bubble_summaries_cached(
    handles: AdapterHandles,
    ticker_info: TickerInfo,
    from_time: UnixMs,
    to_time: UnixMs,
    timeframe_ms: u64,
    price_step: PriceStep,
    max_candidates_per_candle: usize,
    cluster_window_ms: u32,
    cluster_price_ticks: u32,
    data_path: PathBuf,
) -> Result<(Vec<BubbleVolumeSummary>, usize, usize), AdapterError> {
    let to_exclusive = to_time.saturating_add(1);
    let Some(cache) = market_cache() else {
        let (summaries, trades_seen, raw_discarded, _) = fetch_bubble_summaries(
            handles,
            ticker_info,
            from_time,
            to_time,
            timeframe_ms,
            price_step,
            max_candidates_per_candle,
            cluster_window_ms,
            cluster_price_ticks,
            data_path,
        )
        .await?;
        return Ok((summaries, trades_seen, raw_discarded));
    };

    let cached = cache.read_bubble_summaries(
        ticker_info,
        timeframe_ms,
        price_step.units,
        max_candidates_per_candle,
        cluster_window_ms,
        cluster_price_ticks,
        from_time,
        to_exclusive,
    );
    let cache_complete = cached.is_complete();
    let mut summaries = cached.records;
    let mut trades_seen = summaries
        .iter()
        .flat_map(|summary| &summary.candidates)
        .map(|candidate| candidate.trade_count)
        .sum::<usize>();
    let mut raw_discarded = 0usize;
    let mut verified_ranges = Vec::new();

    for (gap_from, gap_to_exclusive) in cached.gaps {
        let gap_to = gap_to_exclusive.saturating_sub(1);
        let (fetched, gap_trades_seen, gap_raw_discarded, verified_to_exclusive) =
            fetch_bubble_summaries(
                handles.clone(),
                ticker_info,
                gap_from,
                gap_to,
                timeframe_ms,
                price_step,
                max_candidates_per_candle,
                cluster_window_ms,
                cluster_price_ticks,
                data_path.clone(),
            )
            .await?;
        if gap_from < verified_to_exclusive {
            verified_ranges.push((gap_from, verified_to_exclusive.min(gap_to_exclusive)));
        }
        trades_seen = trades_seen.saturating_add(gap_trades_seen);
        raw_discarded = raw_discarded.saturating_add(gap_raw_discarded);
        summaries.extend(fetched);
    }

    let summaries = merge_bubble_summaries(summaries, max_candidates_per_candle);
    for (verified_from, verified_to_exclusive) in verified_ranges {
        let verified_summaries = summaries
            .iter()
            .filter(|summary| {
                summary.candle_time < verified_to_exclusive
                    && summary.candle_time.saturating_add(timeframe_ms) > verified_from
            })
            .cloned()
            .collect::<Vec<_>>();
        cache.store_bubble_summaries(
            ticker_info,
            timeframe_ms,
            price_step.units,
            max_candidates_per_candle,
            cluster_window_ms,
            cluster_price_ticks,
            verified_from,
            verified_to_exclusive,
            &verified_summaries,
        );
    }
    if cache_complete {
        log::debug!(
            "BUBBLE Network Skipped | venue={} symbol={} range={} timeframe_ms={timeframe_ms}",
            format_venue(&ticker_info),
            format_symbol(&ticker_info),
            format_time_range(from_time, to_time)
        );
    }
    Ok((summaries, trades_seen, raw_discarded))
}

async fn fetch_bubble_summaries(
    handles: AdapterHandles,
    ticker_info: TickerInfo,
    from_time: UnixMs,
    to_time: UnixMs,
    timeframe_ms: u64,
    price_step: PriceStep,
    max_candidates_per_candle: usize,
    cluster_window_ms: u32,
    cluster_price_ticks: u32,
    data_path: PathBuf,
) -> Result<(Vec<BubbleVolumeSummary>, usize, usize, UnixMs), AdapterError> {
    let venue = format_venue(&ticker_info);
    let symbol = format_symbol(&ticker_info);
    let started = Instant::now();
    let mut latest_trade_t = from_time;
    let mut request_count = 0usize;
    let mut trades_seen = 0usize;
    let mut raw_discarded = 0usize;
    let mut last_trade_id = None;
    let mut buckets: FxHashMap<UnixMs, Vec<Trade>> = FxHashMap::default();

    log::info!(
        "CHART Bubbles | action=fetch_summary venue={venue} symbol={symbol} range={} timeframe_ms={} max_candidates={max_candidates_per_candle}",
        format_time_range(from_time, to_time),
        timeframe_ms
    );

    while latest_trade_t < to_time {
        request_count += 1;
        let batch_started = Instant::now();
        let request_from = latest_trade_t;
        let raw_batch = handles
            .fetch_trades(ticker_info, latest_trade_t, Some(data_path.clone()))
            .await?;

        if raw_batch.is_empty() {
            log::warn!(
                "BUBBLE Summary Batch | trades_seen={trades_seen} buckets={} retained_candidates={} reason=empty_batch",
                buckets.len(),
                buckets.values().map(Vec::len).sum::<usize>()
            );
            break;
        }

        let prev_latest_trade_t = latest_trade_t;
        latest_trade_t = raw_batch.last().map_or(latest_trade_t, |trade| trade.time);
        let (batch, discarded) = filter_new_fetched_trades(
            raw_batch,
            request_from,
            to_time.saturating_add(1),
            &mut last_trade_id,
        );
        raw_discarded = raw_discarded.saturating_add(discarded);
        for trade in &batch {
            let candle_time =
                UnixMs::new(trade.time.as_u64() - (trade.time.as_u64() % timeframe_ms));
            buckets.entry(candle_time).or_default().push(*trade);
        }

        trades_seen += batch.len();
        if let Some(cache) = market_cache() {
            let cache_to_exclusive = latest_trade_t.min(to_time).saturating_add(1);
            let cache_batch = batch
                .iter()
                .copied()
                .filter(|trade| trade.time >= request_from && trade.time < cache_to_exclusive)
                .collect::<Vec<_>>();
            cache.store_trades(ticker_info, request_from, cache_to_exclusive, &cache_batch);
        }
        log::debug!(
            "BUBBLE Summary Batch | trades_seen={trades_seen} buckets={} retained_candidates={} request_idx={request_count} first={} last={} duration={}",
            buckets.len(),
            buckets.values().map(Vec::len).sum::<usize>(),
            format_optional_time(batch.first().map(|trade| trade.time)),
            format_optional_time(batch.last().map(|trade| trade.time)),
            format_duration_ms(batch_started.elapsed().as_millis() as u64)
        );

        if latest_trade_t <= prev_latest_trade_t {
            log::warn!(
                "BUBBLE Summary Stop | reason=no_progress latest={} target_to={} requests={request_count}",
                format_time_short(latest_trade_t),
                format_time_short(to_time)
            );
            break;
        }
    }

    let mut bubble_config = VolumeBubbleConfig {
        cluster_window_ms,
        cluster_price_ticks,
        ..VolumeBubbleConfig::default()
    };
    bubble_config.max_candidates_per_candle = max_candidates_per_candle;
    let summaries = bubble_summaries_from_buckets(
        buckets,
        timeframe_ms,
        price_step,
        max_candidates_per_candle,
        &bubble_config,
    );
    let candidate_count = summaries
        .iter()
        .map(|summary| summary.candidates.len())
        .sum::<usize>();

    log::info!(
        "BUBBLE Summary Done | range={} candles={} candidates={} raw_discarded={} trades_seen={} requests={request_count} duration={}",
        format_time_range(from_time, to_time),
        summaries.len(),
        candidate_count,
        raw_discarded,
        trades_seen,
        format_duration_ms(started.elapsed().as_millis() as u64)
    );

    let verified_to_exclusive = if latest_trade_t > from_time {
        latest_trade_t.min(to_time).saturating_add(1)
    } else {
        from_time
    };

    Ok((summaries, trades_seen, raw_discarded, verified_to_exclusive))
}

fn bubble_summaries_from_buckets(
    buckets: FxHashMap<UnixMs, Vec<Trade>>,
    timeframe_ms: u64,
    price_step: PriceStep,
    max_candidates_per_candle: usize,
    config: &VolumeBubbleConfig,
) -> Vec<BubbleVolumeSummary> {
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
            candidates.truncate(max_candidates_per_candle);
            BubbleVolumeSummary::new(candle_time, candidates)
        })
        .collect::<Vec<_>>();

    summaries.sort_by_key(|summary| summary.candle_time);
    summaries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticker_info() -> TickerInfo {
        TickerInfo::new(
            exchange::Ticker::new("BTCUSDT", Exchange::BinanceLinear),
            0.1,
            0.001,
            None,
        )
    }

    #[test]
    fn successful_kline_fetch_emits_data_then_terminal_completion() {
        let layout_id = Uuid::new_v4();
        let pane_id = Uuid::new_v4();
        let req_id = Uuid::new_v4();
        let range = (UnixMs::new(1_000), UnixMs::new(61_000));
        let stream = StreamKind::Kline {
            ticker_info: ticker_info(),
            timeframe: Timeframe::M1,
        };

        let (first, second) = kline_success_updates(
            layout_id,
            pane_id,
            stream,
            Some(req_id),
            Some(range),
            Vec::new(),
        );

        assert!(matches!(
            first,
            FetchUpdate::Data {
                layout_id: actual_layout,
                pane_id: actual_pane,
                stream: actual_stream,
                data: FetchedData::Klines {
                    req_id: Some(actual_req),
                    ..
                },
            } if actual_layout == layout_id
                && actual_pane == pane_id
                && actual_stream == stream
                && actual_req == req_id
        ));
        assert!(matches!(
            second,
            FetchUpdate::Status {
                pane_id: actual_pane,
                status: FetchTaskStatus::Completed {
                    req_id: Some(actual_req),
                    fetch: Some(FetchRange::Kline(from, to)),
                    trade_outcome: None,
                },
            } if actual_pane == pane_id
                && actual_req == req_id
                && (from, to) == range
        ));
    }

    fn trade(id: Option<u64>, time: u64) -> Trade {
        Trade {
            id,
            time: UnixMs::new(time),
            is_sell: false,
            price: exchange::unit::Price::from_f64(100.0),
            qty: exchange::unit::Qty::from_f64(1.0),
        }
    }

    #[test]
    fn fetched_trade_dedup_uses_exchange_id_not_trade_values() {
        let mut last_id = None;
        let (first, discarded) = filter_new_fetched_trades(
            vec![
                trade(Some(10), 1_000),
                trade(Some(10), 1_000),
                trade(Some(11), 1_000),
                trade(None, 1_000),
                trade(None, 1_000),
            ],
            UnixMs::new(1_000),
            UnixMs::new(2_000),
            &mut last_id,
        );
        assert_eq!(discarded, 1);
        assert_eq!(first.len(), 4);

        let (second, discarded) = filter_new_fetched_trades(
            vec![trade(Some(11), 1_000), trade(Some(12), 1_001)],
            UnixMs::new(1_000),
            UnixMs::new(2_000),
            &mut last_id,
        );
        assert_eq!(discarded, 1);
        assert_eq!(
            second
                .iter()
                .filter_map(|trade| trade.id)
                .collect::<Vec<_>>(),
            vec![12]
        );
    }

    #[test]
    fn nearby_trade_cache_gaps_are_fetched_as_one_span() {
        let gaps = vec![
            (UnixMs::new(1_000), UnixMs::new(2_000)),
            (UnixMs::new(2_500), UnixMs::new(3_000)),
            (UnixMs::new(70_000), UnixMs::new(71_000)),
        ];

        assert_eq!(
            coalesce_trade_fetch_gaps(&gaps, 1_000),
            vec![
                (UnixMs::new(1_000), UnixMs::new(3_000)),
                (UnixMs::new(70_000), UnixMs::new(71_000)),
            ]
        );
    }

    #[test]
    fn coalesced_trade_fetch_only_emits_original_cache_gaps() {
        let gaps = vec![
            (UnixMs::new(1_000), UnixMs::new(2_000)),
            (UnixMs::new(2_500), UnixMs::new(3_000)),
        ];

        assert!(trade_time_is_in_gaps(UnixMs::new(1_500), &gaps));
        assert!(!trade_time_is_in_gaps(UnixMs::new(2_250), &gaps));
        assert!(trade_time_is_in_gaps(UnixMs::new(2_750), &gaps));
    }

    #[test]
    fn interrupted_trade_worker_preserves_progress_as_retryable_tail() {
        let outcome = interrupted_trade_fetch(
            UnixMs::new(1_000),
            UnixMs::new(5_000),
            UnixMs::new(3_000),
            "test budget".to_string(),
        )
        .unwrap();
        assert_eq!(
            outcome.unfilled_tail,
            Some((UnixMs::new(3_000), UnixMs::new(5_000)))
        );
        assert_eq!(outcome.empty_tail, None);
    }

    #[test]
    fn interrupted_trade_worker_without_progress_remains_an_error() {
        assert!(
            interrupted_trade_fetch(
                UnixMs::new(1_000),
                UnixMs::new(5_000),
                UnixMs::new(1_000),
                "test budget".to_string(),
            )
            .is_err()
        );
    }

    #[test]
    fn request_kind_lookup_does_not_confuse_oi_with_trades() {
        let mut handler = RequestHandler::default();
        let trade_id = handler
            .add_request(FetchRange::Trades(UnixMs::new(1_000), UnixMs::new(2_000)))
            .unwrap()
            .unwrap();
        let oi_id = handler
            .add_request(FetchRange::OpenInterest {
                from: UnixMs::new(1_000),
                to: UnixMs::new(2_000),
                timeframe: Timeframe::M5,
            })
            .unwrap()
            .unwrap();

        assert!(handler.is_trade_request(trade_id));
        assert!(!handler.is_trade_request(oi_id));
    }

    #[test]
    fn test_generation_id_increments_on_supersede() {
        let mut handler = RequestHandler::default();
        assert_eq!(handler.generation_id(), 0);

        // Add a pending request
        let req_id = handler
            .add_request(FetchRange::Trades(UnixMs::new(1000), UnixMs::new(2000)))
            .unwrap()
            .unwrap();
        assert_eq!(handler.generation_id(), 0);

        // Supersede should increment generation
        let superseded = handler.supersede_all_pending("settings_changed");
        assert_eq!(superseded.len(), 1);
        assert_eq!(superseded[0], req_id);
        assert_eq!(handler.generation_id(), 1);

        // Second supersede increments again
        handler.supersede_all_pending("settings_changed");
        assert_eq!(handler.generation_id(), 2);
    }

    #[test]
    fn test_mark_completed_does_not_overwrite_superseded() {
        let mut handler = RequestHandler::default();

        let req_id = handler
            .add_request(FetchRange::Trades(UnixMs::new(1000), UnixMs::new(2000)))
            .unwrap()
            .unwrap();

        // Supersede the request
        handler.supersede_all_pending("settings_changed");

        // Try to mark as completed - should not overwrite Superseded
        handler.mark_completed(req_id);

        // Verify it's still Superseded, not Completed
        assert!(handler.is_stale_generation(req_id));
    }

    #[test]
    fn test_mark_failed_does_not_overwrite_superseded() {
        let mut handler = RequestHandler::default();

        let req_id = handler
            .add_request(FetchRange::Trades(UnixMs::new(1000), UnixMs::new(2000)))
            .unwrap()
            .unwrap();

        // Supersede the request
        handler.supersede_all_pending("settings_changed");

        // Try to mark as failed - should not overwrite Superseded
        handler.mark_failed(req_id, "some error".to_string());

        // Verify it's still stale (Superseded)
        assert!(handler.is_stale_generation(req_id));
    }

    #[test]
    fn test_stale_generation_detection() {
        let mut handler = RequestHandler::default();

        // Add request in generation 0
        let req_id = handler
            .add_request(FetchRange::Trades(UnixMs::new(1000), UnixMs::new(2000)))
            .unwrap()
            .unwrap();

        // Request is in current generation
        assert!(!handler.is_stale_generation(req_id));
        assert_eq!(handler.request_generation(req_id), Some(0));

        // Supersede increments generation to 1
        handler.supersede_all_pending("settings_changed");
        assert_eq!(handler.generation_id(), 1);

        // Request from generation 0 is now stale
        assert!(handler.is_stale_generation(req_id));
        assert_eq!(handler.request_generation(req_id), Some(0));
    }

    #[test]
    fn test_supersede_only_affects_pending() {
        let mut handler = RequestHandler::default();

        // Add two requests
        let req1 = handler
            .add_request(FetchRange::Trades(UnixMs::new(1000), UnixMs::new(2000)))
            .unwrap()
            .unwrap();
        let req2 = handler
            .add_request(FetchRange::Trades(UnixMs::new(3000), UnixMs::new(4000)))
            .unwrap()
            .unwrap();

        // Complete one request
        handler.mark_completed(req1);

        // Supersede should only affect pending requests
        let superseded = handler.supersede_all_pending("settings_changed");
        assert_eq!(superseded.len(), 1);
        assert_eq!(superseded[0], req2);

        // req1 should be Completed, not Superseded
        assert!(!handler.is_stale_generation(req1));
        // req2 should be Superseded (stale)
        assert!(handler.is_stale_generation(req2));
    }

    #[test]
    fn test_timeout_watchdog() {
        let mut handler = RequestHandler::default();

        // Add a request
        let _req_id = handler
            .add_request(FetchRange::Trades(UnixMs::new(1000), UnixMs::new(2000)))
            .unwrap()
            .unwrap();

        // Simulate time passing beyond timeout
        let future_time = chrono::Utc::now().timestamp_millis() as u64
            + RequestHandler::PENDING_TIMEOUT_MS
            + 1000;
        handler.cleanup_stale_at(future_time);

        // Request should now be Failed with timeout
        // Note: cleanup_stale_at doesn't remove immediately, just changes status
        // It will be removed on next cleanup after FAILED_RETENTION_MS
    }

    #[test]
    fn test_new_request_after_supersede() {
        let mut handler = RequestHandler::default();

        // Add request in generation 0
        let req1 = handler
            .add_request(FetchRange::Trades(UnixMs::new(1000), UnixMs::new(2000)))
            .unwrap()
            .unwrap();

        // Supersede
        handler.supersede_all_pending("settings_changed");
        assert!(handler.is_stale_generation(req1));

        // Add new request with same range - reactivates the old request
        let req2 = handler
            .add_request(FetchRange::Trades(UnixMs::new(1000), UnixMs::new(2000)))
            .unwrap()
            .unwrap();

        // req2 reuses req1 ID and reactivates it with new generation
        assert_eq!(req1, req2);
        assert!(!handler.is_stale_generation(req1));
        assert_eq!(handler.request_generation(req1), Some(1));

        // A different range gets a new ID in generation 1
        let req3 = handler
            .add_request(FetchRange::Trades(UnixMs::new(3000), UnixMs::new(4000)))
            .unwrap()
            .unwrap();
        assert_ne!(req1, req3);
        assert!(!handler.is_stale_generation(req3));
        assert_eq!(handler.request_generation(req3), Some(1));
    }

    #[test]
    fn test_not_found_request_is_stale() {
        let handler = RequestHandler::default();
        let random_id = Uuid::new_v4();

        // Unknown request should be treated as stale
        assert!(handler.is_stale_generation(random_id));
        assert_eq!(handler.request_generation(random_id), None);
    }
}
