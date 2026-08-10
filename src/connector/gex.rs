use data::chart::gex::{
    Config, DeriveMakerGammaFlow, GexExpiryFilter, GexFreshness, GexGammaSource,
    GexScenarioResolution, GexSignModel, GexSnapshot, calculate_derive_maker_gamma_flow,
    calculate_gex_at, map_proxy_snapshot, quantwheel_snapshot,
};
use exchange::{
    UnixMs,
    options::{
        GexSource, OptionContractMatchKey, OptionInstrument, OptionsProvider, OptionsUnderlying,
        RawOptionChainSnapshot,
        deribit::{DeribitError, DeribitOptionsClient},
        derive::{DeriveMakerTrade, DeriveOptionInstrument, DeriveOptionsClient},
        gex_monitor::{GexMonitorClient, GexProxyHistoryPoint, GexProxyHistoryResponse},
        quantwheel::{QuantWheelError, QuantWheelGexClient, QuantWheelGexSnapshot},
    },
};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::Arc,
};

pub const INSTRUMENT_TTL_MS: u64 = 10 * 60 * 1_000;
pub const MARKET_SNAPSHOT_TTL_MS: u64 = 15 * 1_000;
pub const FRESH_THRESHOLD_MS: u64 = 45 * 1_000;
pub const EXPIRED_THRESHOLD_MS: u64 = 5 * 60 * 1_000;
pub const PROXY_REFRESH_MS: u64 = 5 * 60 * 1_000;
pub const QUANTWHEEL_REFRESH_MS: u64 = 5 * 60 * 1_000;
pub const DERIVE_INSTRUMENT_REFRESH_MS: u64 = 10 * 60 * 1_000;
pub const DERIVE_TRADE_REFRESH_MS: u64 = 5 * 1_000;
pub const DERIVE_INITIAL_BACKFILL_MS: u64 = 2 * 60 * 60 * 1_000;
pub const DERIVE_FETCH_OVERLAP_MS: u64 = 10 * 1_000;
pub const DERIVE_RETENTION_MS: u64 = 24 * 60 * 60 * 1_000;
const FAILURE_BACKOFF_BASE_MS: u64 = 5_000;
const FAILURE_BACKOFF_MAX_MS: u64 = 2 * 60 * 1_000;
const CACHE_SCHEMA: u32 = 1;
const CACHE_FILENAME: &str = "gex_option_chain_v1.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub struct OptionsChainKey {
    pub provider: OptionsProvider,
    pub underlying: OptionsUnderlying,
}

impl OptionsChainKey {
    pub const fn deribit(underlying: OptionsUnderlying) -> Self {
        Self {
            provider: OptionsProvider::Deribit,
            underlying,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GexFetchKind {
    Instruments(OptionsChainKey),
    Snapshot(OptionsChainKey),
}

impl GexFetchKind {
    pub const fn key(self) -> OptionsChainKey {
        match self {
            Self::Instruments(key) | Self::Snapshot(key) => key,
        }
    }
}

#[derive(Debug, Clone)]
pub enum GexFetchResult {
    Instruments {
        key: OptionsChainKey,
        result: Result<Vec<OptionInstrument>, Arc<str>>,
    },
    Snapshot {
        key: OptionsChainKey,
        result: Result<RawOptionChainSnapshot, Arc<str>>,
    },
}

#[derive(Debug, Clone)]
pub struct DeriveInstrumentsFetchResult {
    pub underlying: OptionsUnderlying,
    pub result: Result<Vec<DeriveOptionInstrument>, Arc<str>>,
}

#[derive(Debug, Clone)]
pub struct DeriveTradesFetchResult {
    pub underlying: OptionsUnderlying,
    pub result: Result<Vec<DeriveMakerTrade>, Arc<str>>,
}

#[derive(Debug, Clone, Copy)]
pub struct DeriveTradeFetchRequest {
    pub underlying: OptionsUnderlying,
    pub from: UnixMs,
    pub to: UnixMs,
}

#[derive(Debug, Clone)]
struct CachedInstruments {
    values: Arc<[OptionInstrument]>,
    refreshed_at: UnixMs,
}

#[derive(Debug, Clone)]
struct CachedRawSnapshot {
    value: Arc<RawOptionChainSnapshot>,
    received_at: UnixMs,
    revision: u64,
    loaded_from_disk: bool,
}

#[derive(Debug, Clone)]
struct CachedGexSnapshot {
    value: Arc<GexSnapshot>,
}

#[derive(Debug, Clone)]
struct CachedQuantWheelSnapshot {
    value: Arc<GexSnapshot>,
    received_at: UnixMs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct DerivedGexKey {
    chain: OptionsChainKey,
    model: GexSignModel,
    expiry: GexExpiryFilter,
    min_oi_bits: u64,
    min_gex_bits: u64,
    gamma_source: GexGammaSource,
    scenario_resolution: GexScenarioResolution,
    revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub struct DerivedGexSeriesKey {
    pub chain: OptionsChainKey,
    pub model: GexSignModel,
    pub expiry: GexExpiryFilter,
    pub min_oi_bits: u64,
    pub min_gex_bits: u64,
    pub gamma_source: GexGammaSource,
    pub scenario_resolution: GexScenarioResolution,
}

#[derive(Debug, Clone)]
struct HistoricalGexSnapshot {
    revision: u64,
    value: Arc<GexSnapshot>,
}

#[derive(Debug, Clone)]
struct FailureState {
    attempts: u32,
    retry_after: UnixMs,
    last_error: Arc<str>,
}

#[derive(Debug)]
pub struct GexDataCoordinator {
    instruments: FxHashMap<OptionsChainKey, CachedInstruments>,
    raw_snapshots: FxHashMap<OptionsChainKey, CachedRawSnapshot>,
    derived_snapshots: FxHashMap<DerivedGexKey, CachedGexSnapshot>,
    derived_history: FxHashMap<DerivedGexSeriesKey, VecDeque<HistoricalGexSnapshot>>,
    loaded_history: FxHashSet<DerivedGexSeriesKey>,
    in_flight: FxHashSet<GexFetchKind>,
    failures: FxHashMap<OptionsChainKey, FailureState>,
    subscribers: FxHashMap<OptionsChainKey, usize>,
    force_refresh: FxHashSet<OptionsChainKey>,
    last_freshness: FxHashMap<OptionsChainKey, GexFreshness>,
    quantwheel_subscribers: usize,
    quantwheel_snapshot: Option<CachedQuantWheelSnapshot>,
    quantwheel_history: VecDeque<Arc<GexSnapshot>>,
    quantwheel_in_flight: bool,
    quantwheel_failure: Option<FailureState>,
    quantwheel_force_refresh: bool,
    proxy_history: FxHashMap<OptionsUnderlying, Vec<Arc<GexProxyHistoryPoint>>>,
    proxy_loaded: FxHashSet<OptionsUnderlying>,
    proxy_in_flight: FxHashSet<OptionsUnderlying>,
    proxy_failures: FxHashMap<OptionsUnderlying, FailureState>,
    proxy_force_refresh: FxHashSet<OptionsUnderlying>,
    proxy_refreshed_at: FxHashMap<OptionsUnderlying, UnixMs>,
    proxy_stale: FxHashSet<OptionsUnderlying>,
    derive_instruments: FxHashMap<OptionsUnderlying, Arc<[DeriveOptionInstrument]>>,
    derive_trades: FxHashMap<OptionsUnderlying, Vec<DeriveMakerTrade>>,
    derive_loaded: FxHashSet<OptionsUnderlying>,
    derive_instruments_in_flight: FxHashSet<OptionsUnderlying>,
    derive_trades_in_flight: FxHashSet<OptionsUnderlying>,
    derive_instrument_failures: FxHashMap<OptionsUnderlying, FailureState>,
    derive_trade_failures: FxHashMap<OptionsUnderlying, FailureState>,
    derive_instruments_refreshed_at: FxHashMap<OptionsUnderlying, UnixMs>,
    derive_trades_refreshed_at: FxHashMap<OptionsUnderlying, UnixMs>,
    derive_watermarks: FxHashMap<OptionsUnderlying, UnixMs>,
    derive_force_instruments: FxHashSet<OptionsUnderlying>,
    derive_force_trades: FxHashSet<OptionsUnderlying>,
    derive_stale: FxHashSet<OptionsUnderlying>,
    next_revision: u64,
    cache_path: PathBuf,
    persist_heatmap: bool,
}

impl Default for GexDataCoordinator {
    fn default() -> Self {
        Self::new(data::data_path(Some(CACHE_FILENAME)))
    }
}

impl GexDataCoordinator {
    pub fn new(cache_path: PathBuf) -> Self {
        let persist_heatmap = cache_path == data::data_path(Some(CACHE_FILENAME));
        let mut coordinator = Self {
            instruments: FxHashMap::default(),
            raw_snapshots: FxHashMap::default(),
            derived_snapshots: FxHashMap::default(),
            derived_history: FxHashMap::default(),
            loaded_history: FxHashSet::default(),
            in_flight: FxHashSet::default(),
            failures: FxHashMap::default(),
            subscribers: FxHashMap::default(),
            force_refresh: FxHashSet::default(),
            last_freshness: FxHashMap::default(),
            quantwheel_subscribers: 0,
            quantwheel_snapshot: None,
            quantwheel_history: VecDeque::new(),
            quantwheel_in_flight: false,
            quantwheel_failure: None,
            quantwheel_force_refresh: false,
            proxy_history: FxHashMap::default(),
            proxy_loaded: FxHashSet::default(),
            proxy_in_flight: FxHashSet::default(),
            proxy_failures: FxHashMap::default(),
            proxy_force_refresh: FxHashSet::default(),
            proxy_refreshed_at: FxHashMap::default(),
            proxy_stale: FxHashSet::default(),
            derive_instruments: FxHashMap::default(),
            derive_trades: FxHashMap::default(),
            derive_loaded: FxHashSet::default(),
            derive_instruments_in_flight: FxHashSet::default(),
            derive_trades_in_flight: FxHashSet::default(),
            derive_instrument_failures: FxHashMap::default(),
            derive_trade_failures: FxHashMap::default(),
            derive_instruments_refreshed_at: FxHashMap::default(),
            derive_trades_refreshed_at: FxHashMap::default(),
            derive_watermarks: FxHashMap::default(),
            derive_force_instruments: FxHashSet::default(),
            derive_force_trades: FxHashSet::default(),
            derive_stale: FxHashSet::default(),
            next_revision: 1,
            cache_path,
            persist_heatmap,
        };
        coordinator.load_persistent();
        for underlying in OptionsUnderlying::ALL {
            coordinator.ensure_proxy_loaded(underlying);
        }
        coordinator
    }

    pub fn set_consumers<I>(&mut self, consumers: I)
    where
        I: IntoIterator,
        I::Item: Into<GexSource>,
    {
        let mut next = FxHashMap::default();
        let mut quantwheel_subscribers = 0usize;
        for source in consumers.into_iter().map(Into::into) {
            match source {
                GexSource::Native {
                    provider: OptionsProvider::Deribit,
                    underlying,
                } => {
                    *next
                        .entry(OptionsChainKey::deribit(underlying))
                        .or_insert(0usize) += 1;
                }
                GexSource::Proxy {
                    provider: OptionsProvider::QuantWheel,
                    source_symbol: OptionsUnderlying::Gld,
                    ..
                } => quantwheel_subscribers = quantwheel_subscribers.saturating_add(1),
                _ => {}
            }
        }
        for (&key, &count) in &next {
            if count > 0 && self.subscribers.get(&key).copied().unwrap_or(0) == 0 {
                self.force_refresh.insert(key);
                self.proxy_force_refresh.insert(key.underlying);
                self.ensure_derive_loaded(key.underlying);
                self.derive_force_instruments.insert(key.underlying);
                self.derive_force_trades.insert(key.underlying);
            }
        }
        if quantwheel_subscribers > 0 && self.quantwheel_subscribers == 0 {
            self.quantwheel_force_refresh = true;
        }
        self.quantwheel_subscribers = quantwheel_subscribers;
        self.subscribers = next;
    }

    pub fn subscriber_count(&self, underlying: OptionsUnderlying) -> usize {
        self.subscribers
            .get(&OptionsChainKey::deribit(underlying))
            .copied()
            .unwrap_or(0)
    }

    pub fn reconnect(&mut self) {
        self.force_refresh.extend(
            self.subscribers
                .iter()
                .filter_map(|(&key, &count)| (count > 0).then_some(key)),
        );
        self.proxy_force_refresh.extend(
            self.subscribers
                .iter()
                .filter_map(|(&key, &count)| (count > 0).then_some(key.underlying)),
        );
        self.derive_force_instruments.extend(
            self.subscribers
                .iter()
                .filter_map(|(&key, &count)| (count > 0).then_some(key.underlying)),
        );
        self.derive_force_trades.extend(
            self.subscribers
                .iter()
                .filter_map(|(&key, &count)| (count > 0).then_some(key.underlying)),
        );
        if self.quantwheel_subscribers > 0 {
            self.quantwheel_force_refresh = true;
        }
    }

    pub fn due_quantwheel_fetch(&mut self, now: UnixMs, online: bool) -> bool {
        if !online || self.quantwheel_subscribers == 0 || self.quantwheel_in_flight {
            return false;
        }
        if self
            .quantwheel_failure
            .as_ref()
            .is_some_and(|failure| now < failure.retry_after)
            && !self.quantwheel_force_refresh
        {
            return false;
        }
        let expired = self
            .quantwheel_snapshot
            .as_ref()
            .is_none_or(|cached| now.saturating_diff(cached.received_at) >= QUANTWHEEL_REFRESH_MS);
        if expired || self.quantwheel_force_refresh {
            self.quantwheel_in_flight = true;
            true
        } else {
            false
        }
    }

    pub fn complete_quantwheel(
        &mut self,
        result: Result<QuantWheelGexSnapshot, Arc<str>>,
        now: UnixMs,
    ) {
        self.quantwheel_in_flight = false;
        self.quantwheel_force_refresh = false;
        match result {
            Ok(source) => {
                let value = Arc::new(quantwheel_snapshot(source, now));
                self.quantwheel_history.push_back(value.clone());
                let cutoff = now.saturating_sub(24 * 60 * 60 * 1_000);
                while self
                    .quantwheel_history
                    .front()
                    .is_some_and(|snapshot| snapshot.observed_at < cutoff)
                {
                    self.quantwheel_history.pop_front();
                }
                self.quantwheel_snapshot = Some(CachedQuantWheelSnapshot {
                    value,
                    received_at: now,
                });
                self.quantwheel_failure = None;
            }
            Err(error) => {
                let attempts = self
                    .quantwheel_failure
                    .as_ref()
                    .map_or(1, |failure| failure.attempts.saturating_add(1));
                let delay = FAILURE_BACKOFF_BASE_MS
                    .saturating_mul(1u64 << attempts.saturating_sub(1).min(8))
                    .min(FAILURE_BACKOFF_MAX_MS);
                log::warn!(
                    "GEX FetchFailed kind=snapshot underlying=GLD provider=QuantWheel attempt={attempts} backoff_ms={delay} error={error}"
                );
                self.quantwheel_failure = Some(FailureState {
                    attempts,
                    retry_after: now.saturating_add(delay),
                    last_error: error,
                });
            }
        }
    }

    pub fn mapped_quantwheel(
        &self,
        target_symbol: &str,
        target_spot: f64,
    ) -> Option<Arc<GexSnapshot>> {
        self.quantwheel_snapshot.as_ref().and_then(|cached| {
            map_proxy_snapshot(&cached.value, "GLD", target_symbol, target_spot).map(Arc::new)
        })
    }

    pub fn mapped_quantwheel_history(
        &self,
        target_symbol: &str,
        target_spot: f64,
        retention_minutes: u16,
        now: UnixMs,
    ) -> Vec<Arc<GexSnapshot>> {
        let cutoff = now
            .saturating_sub(u64::from(retention_minutes.clamp(30, 24 * 60)).saturating_mul(60_000));
        self.quantwheel_history
            .iter()
            .filter(|snapshot| snapshot.observed_at >= cutoff)
            .filter_map(|snapshot| {
                map_proxy_snapshot(snapshot, "GLD", target_symbol, target_spot).map(Arc::new)
            })
            .collect()
    }

    pub fn quantwheel_freshness(&self, now: UnixMs) -> GexFreshness {
        if self.quantwheel_failure.is_some() {
            GexFreshness::Error
        } else if self.quantwheel_in_flight && self.quantwheel_snapshot.is_none() {
            GexFreshness::Loading
        } else if let Some(cached) = &self.quantwheel_snapshot {
            if now.saturating_diff(cached.received_at) <= QUANTWHEEL_REFRESH_MS {
                GexFreshness::Fresh
            } else {
                GexFreshness::Stale
            }
        } else {
            GexFreshness::Loading
        }
    }

    pub fn quantwheel_error(&self) -> Option<&str> {
        self.quantwheel_failure
            .as_ref()
            .map(|failure| failure.last_error.as_ref())
    }

    pub fn due_proxy_fetches(&mut self, now: UnixMs, online: bool) -> Vec<OptionsUnderlying> {
        if !online {
            return Vec::new();
        }
        let underlyings = self
            .subscribers
            .iter()
            .filter_map(|(&key, &count)| (count > 0).then_some(key.underlying))
            .collect::<FxHashSet<_>>();
        let mut due = Vec::new();
        for underlying in underlyings {
            self.ensure_proxy_loaded(underlying);
            if self
                .proxy_failures
                .get(&underlying)
                .is_some_and(|failure| now < failure.retry_after)
                && !self.proxy_force_refresh.contains(&underlying)
            {
                continue;
            }
            let expired = self
                .proxy_refreshed_at
                .get(&underlying)
                .is_none_or(|last| now.saturating_diff(*last) >= PROXY_REFRESH_MS);
            if (expired || self.proxy_force_refresh.contains(&underlying))
                && self.proxy_in_flight.insert(underlying)
            {
                due.push(underlying);
            }
        }
        due
    }

    pub fn complete_proxy(
        &mut self,
        underlying: OptionsUnderlying,
        result: Result<GexProxyHistoryResponse, Arc<str>>,
        now: UnixMs,
    ) {
        self.proxy_in_flight.remove(&underlying);
        self.proxy_force_refresh.remove(&underlying);
        match result {
            Ok(response)
                if response.stale
                    && self
                        .proxy_history
                        .get(&underlying)
                        .is_some_and(|v| !v.is_empty()) =>
            {
                self.proxy_stale.insert(underlying);
                self.proxy_refreshed_at.insert(underlying, now);
                self.proxy_failures.remove(&underlying);
            }
            Ok(response) => {
                let mut points = self
                    .proxy_history
                    .remove(&underlying)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|point| (*point).clone())
                    .chain(response.points)
                    .collect::<Vec<_>>();
                normalize_proxy_points(&mut points, now);
                self.proxy_history
                    .insert(underlying, points.iter().cloned().map(Arc::new).collect());
                self.proxy_refreshed_at.insert(underlying, now);
                self.proxy_failures.remove(&underlying);
                if response.stale {
                    self.proxy_stale.insert(underlying);
                } else {
                    self.proxy_stale.remove(&underlying);
                }
                if self.persist_heatmap
                    && let Some(cache) = crate::connector::persistent_cache::market_cache()
                {
                    cache.store_gex_proxy_history(
                        &proxy_cache_key(underlying),
                        &points,
                        i64::try_from(now.as_u64()).unwrap_or(i64::MAX),
                    );
                }
            }
            Err(error) => self.record_proxy_failure(underlying, error, now),
        }
    }

    pub fn proxy_history(
        &mut self,
        underlying: OptionsUnderlying,
        now: UnixMs,
    ) -> Vec<Arc<GexProxyHistoryPoint>> {
        self.ensure_proxy_loaded(underlying);
        let cutoff = i64::try_from(now.as_u64())
            .unwrap_or(i64::MAX)
            .saturating_sub(24 * 60 * 60 * 1_000);
        self.proxy_history
            .get(&underlying)
            .into_iter()
            .flatten()
            .filter(|point| point.observed_at >= cutoff)
            .cloned()
            .collect()
    }

    pub fn proxy_freshness(&self, underlying: OptionsUnderlying, now: UnixMs) -> GexFreshness {
        if self.proxy_failures.contains_key(&underlying) {
            GexFreshness::Error
        } else if self.proxy_in_flight.contains(&underlying)
            && self
                .proxy_history
                .get(&underlying)
                .is_none_or(Vec::is_empty)
        {
            GexFreshness::Loading
        } else if self.proxy_stale.contains(&underlying) {
            GexFreshness::Stale
        } else if let Some(last) = self.proxy_refreshed_at.get(&underlying) {
            if now.saturating_diff(*last) <= PROXY_REFRESH_MS {
                GexFreshness::Fresh
            } else {
                GexFreshness::Stale
            }
        } else if self
            .proxy_history
            .get(&underlying)
            .is_some_and(|v| !v.is_empty())
        {
            GexFreshness::Stale
        } else {
            GexFreshness::Loading
        }
    }

    pub fn proxy_error(&self, underlying: OptionsUnderlying) -> Option<&str> {
        self.proxy_failures
            .get(&underlying)
            .map(|failure| failure.last_error.as_ref())
    }

    fn ensure_proxy_loaded(&mut self, underlying: OptionsUnderlying) {
        if !self.persist_heatmap || !self.proxy_loaded.insert(underlying) {
            return;
        }
        let Some(cache) = crate::connector::persistent_cache::market_cache() else {
            return;
        };
        let points = cache.read_gex_proxy_history(&proxy_cache_key(underlying));
        if !points.is_empty() {
            self.proxy_history
                .insert(underlying, points.into_iter().map(Arc::new).collect());
            self.proxy_stale.insert(underlying);
        }
    }

    fn record_proxy_failure(
        &mut self,
        underlying: OptionsUnderlying,
        error: Arc<str>,
        now: UnixMs,
    ) {
        log::debug!(
            "GEX Monitor FetchFailed underlying={underlying} error={error} provider=GEXMonitor"
        );
        let attempts = self
            .proxy_failures
            .get(&underlying)
            .map_or(1, |failure| failure.attempts.saturating_add(1));
        let exponent = attempts.saturating_sub(1).min(10);
        let delay = FAILURE_BACKOFF_BASE_MS
            .saturating_mul(1u64 << exponent)
            .min(FAILURE_BACKOFF_MAX_MS);
        self.proxy_failures.insert(
            underlying,
            FailureState {
                attempts,
                retry_after: now.saturating_add(delay),
                last_error: error,
            },
        );
    }

    pub fn due_derive_instrument_fetches(
        &mut self,
        now: UnixMs,
        online: bool,
    ) -> Vec<OptionsUnderlying> {
        if !online {
            return Vec::new();
        }
        let underlyings = self.active_underlyings();
        let mut due = Vec::new();
        for underlying in underlyings {
            self.ensure_derive_loaded(underlying);
            let force = self.derive_force_instruments.contains(&underlying);
            if !force
                && self
                    .derive_instrument_failures
                    .get(&underlying)
                    .is_some_and(|failure| now < failure.retry_after)
            {
                continue;
            }
            let expired = self
                .derive_instruments_refreshed_at
                .get(&underlying)
                .is_none_or(|last| now.saturating_diff(*last) >= DERIVE_INSTRUMENT_REFRESH_MS);
            if (force || expired) && self.derive_instruments_in_flight.insert(underlying) {
                due.push(underlying);
            }
        }
        due
    }

    pub fn due_derive_trade_fetches(
        &mut self,
        now: UnixMs,
        online: bool,
    ) -> Vec<DeriveTradeFetchRequest> {
        if !online {
            return Vec::new();
        }
        let underlyings = self.active_underlyings();
        let mut due = Vec::new();
        for underlying in underlyings {
            self.ensure_derive_loaded(underlying);
            if self
                .derive_instruments
                .get(&underlying)
                .is_none_or(|values| values.is_empty())
            {
                continue;
            }
            let force = self.derive_force_trades.contains(&underlying);
            if !force
                && self
                    .derive_trade_failures
                    .get(&underlying)
                    .is_some_and(|failure| now < failure.retry_after)
            {
                continue;
            }
            let expired = self
                .derive_trades_refreshed_at
                .get(&underlying)
                .is_none_or(|last| now.saturating_diff(*last) >= DERIVE_TRADE_REFRESH_MS);
            if (force || expired) && self.derive_trades_in_flight.insert(underlying) {
                let from = self
                    .derive_watermarks
                    .get(&underlying)
                    .copied()
                    .map(|watermark| watermark.saturating_sub(DERIVE_FETCH_OVERLAP_MS))
                    .unwrap_or_else(|| now.saturating_sub(DERIVE_INITIAL_BACKFILL_MS));
                due.push(DeriveTradeFetchRequest {
                    underlying,
                    from,
                    to: now,
                });
            }
        }
        due
    }

    pub fn derive_instruments_for(
        &self,
        underlying: OptionsUnderlying,
    ) -> Arc<[DeriveOptionInstrument]> {
        self.derive_instruments
            .get(&underlying)
            .cloned()
            .unwrap_or_default()
    }

    pub fn complete_derive_instruments(
        &mut self,
        completion: DeriveInstrumentsFetchResult,
        now: UnixMs,
    ) {
        let underlying = completion.underlying;
        self.derive_instruments_in_flight.remove(&underlying);
        self.derive_force_instruments.remove(&underlying);
        match completion.result {
            Ok(values) if !values.is_empty() => {
                log::info!(
                    "Derive InstrumentsRefreshed underlying={underlying} count={} refreshed_at={now}",
                    values.len()
                );
                self.derive_instruments.insert(underlying, values.into());
                self.derive_instruments_refreshed_at.insert(underlying, now);
                self.derive_instrument_failures.remove(&underlying);
                self.derive_force_trades.insert(underlying);
            }
            Ok(_) => self.record_derive_failure(
                underlying,
                "empty Derive instrument metadata".into(),
                now,
                true,
            ),
            Err(error) => self.record_derive_failure(underlying, error, now, true),
        }
    }

    pub fn complete_derive_trades(&mut self, completion: DeriveTradesFetchResult, now: UnixMs) {
        let underlying = completion.underlying;
        self.derive_trades_in_flight.remove(&underlying);
        self.derive_force_trades.remove(&underlying);
        match completion.result {
            Ok(values) => {
                let received = values.len();
                let mut by_id = self
                    .derive_trades
                    .remove(&underlying)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|trade| (trade.trade_id.clone(), trade))
                    .collect::<FxHashMap<_, _>>();
                for trade in values {
                    by_id.insert(trade.trade_id.clone(), trade);
                }
                let cutoff = now.saturating_sub(DERIVE_RETENTION_MS);
                let mut trades = by_id
                    .into_values()
                    .filter(|trade| trade.timestamp >= cutoff && trade.timestamp <= now)
                    .collect::<Vec<_>>();
                trades.sort_by_key(|trade| trade.timestamp);
                if trades.len() > 50_000 {
                    trades.drain(..trades.len() - 50_000);
                }
                if let Some(latest) = trades.last().map(|trade| trade.timestamp) {
                    self.derive_watermarks.insert(underlying, latest);
                }
                self.derive_trades.insert(underlying, trades);
                self.derive_trades_refreshed_at.insert(underlying, now);
                self.derive_trade_failures.remove(&underlying);
                self.derive_stale.remove(&underlying);
                let retained = self.derive_trades.get(&underlying).map_or(0, Vec::len);
                let watermark = self
                    .derive_watermarks
                    .get(&underlying)
                    .copied()
                    .unwrap_or(UnixMs::ZERO);
                let (window_30m, exact_matches_30m) = self
                    .raw_snapshots
                    .get(&OptionsChainKey::deribit(underlying))
                    .and_then(|raw| {
                        self.derive_trades
                            .get(&underlying)
                            .map(|trades| derive_exact_match_count(&raw.value, trades, now, 30))
                    })
                    .unwrap_or_default();
                log::info!(
                    "Derive TradesRefreshed underlying={underlying} received={received} retained={retained} window_30m={window_30m} exact_matches_30m={exact_matches_30m} watermark={watermark} refreshed_at={now}"
                );
                if self.persist_heatmap
                    && let Some(cache) = crate::connector::persistent_cache::market_cache()
                    && let Some(trades) = self.derive_trades.get(&underlying)
                {
                    cache.store_derive_maker_trades(&derive_cache_key(underlying), trades, now);
                }
            }
            Err(error) => self.record_derive_failure(underlying, error, now, false),
        }
    }

    pub fn derive_flow(
        &self,
        underlying: OptionsUnderlying,
        config: &Config,
        now: UnixMs,
    ) -> Option<Arc<DeriveMakerGammaFlow>> {
        let raw = self
            .raw_snapshots
            .get(&OptionsChainKey::deribit(underlying))?;
        let trades = self.derive_trades.get(&underlying)?;
        Some(Arc::new(calculate_derive_maker_gamma_flow(
            &raw.value, trades, config, now,
        )))
    }

    pub fn derive_freshness(&self, underlying: OptionsUnderlying, now: UnixMs) -> GexFreshness {
        if self.derive_trade_failures.contains_key(&underlying)
            || self.derive_instrument_failures.contains_key(&underlying)
        {
            GexFreshness::Error
        } else if self.derive_stale.contains(&underlying) {
            GexFreshness::Stale
        } else if self.derive_trades_in_flight.contains(&underlying)
            && self
                .derive_trades
                .get(&underlying)
                .is_none_or(Vec::is_empty)
        {
            GexFreshness::Loading
        } else if self
            .derive_trades_refreshed_at
            .get(&underlying)
            .is_some_and(|last| now.saturating_diff(*last) < DERIVE_TRADE_REFRESH_MS * 2)
        {
            GexFreshness::Fresh
        } else if self.derive_trades.contains_key(&underlying) {
            GexFreshness::Stale
        } else {
            GexFreshness::Loading
        }
    }

    fn active_underlyings(&self) -> FxHashSet<OptionsUnderlying> {
        self.subscribers
            .iter()
            .filter_map(|(&key, &count)| (count > 0).then_some(key.underlying))
            .collect()
    }

    fn ensure_derive_loaded(&mut self, underlying: OptionsUnderlying) {
        if !self.persist_heatmap || !self.derive_loaded.insert(underlying) {
            return;
        }
        let Some(cache) = crate::connector::persistent_cache::market_cache() else {
            return;
        };
        let trades = cache.read_derive_maker_trades(&derive_cache_key(underlying));
        if trades.is_empty() {
            return;
        }
        if let Some(latest) = trades.last().map(|trade| trade.timestamp) {
            self.derive_watermarks.insert(underlying, latest);
        }
        log::info!(
            "Derive CacheLoaded underlying={underlying} trades={} watermark={} stale=true",
            trades.len(),
            trades
                .last()
                .map(|trade| trade.timestamp)
                .unwrap_or(UnixMs::ZERO)
        );
        self.derive_trades.insert(underlying, trades);
        self.derive_stale.insert(underlying);
    }

    fn record_derive_failure(
        &mut self,
        underlying: OptionsUnderlying,
        error: Arc<str>,
        now: UnixMs,
        instruments: bool,
    ) {
        let failures = if instruments {
            &mut self.derive_instrument_failures
        } else {
            &mut self.derive_trade_failures
        };
        let attempts = failures
            .get(&underlying)
            .map_or(1, |failure| failure.attempts.saturating_add(1));
        let delay = FAILURE_BACKOFF_BASE_MS
            .saturating_mul(1u64 << attempts.saturating_sub(1).min(10))
            .min(FAILURE_BACKOFF_MAX_MS);
        let kind = if instruments { "instruments" } else { "trades" };
        log::warn!(
            "Derive FetchFailed kind={kind} underlying={underlying} attempt={attempts} backoff_ms={delay} error={error}"
        );
        failures.insert(
            underlying,
            FailureState {
                attempts,
                retry_after: now.saturating_add(delay),
                last_error: error,
            },
        );
    }

    pub fn due_fetches(&mut self, now: UnixMs, online: bool) -> Vec<GexFetchKind> {
        if !online {
            return Vec::new();
        }
        let keys = self
            .subscribers
            .iter()
            .filter_map(|(&key, &count)| (count > 0).then_some(key))
            .collect::<Vec<_>>();
        let mut due = Vec::new();
        for key in keys {
            if self
                .failures
                .get(&key)
                .is_some_and(|failure| now < failure.retry_after)
            {
                continue;
            }
            let instruments_due = self
                .instruments
                .get(&key)
                .is_none_or(|cached| now.saturating_diff(cached.refreshed_at) >= INSTRUMENT_TTL_MS);
            let force = self.force_refresh.contains(&key);
            let raw_due = self.raw_snapshots.get(&key).is_none_or(|cached| {
                now.saturating_diff(cached.received_at) >= MARKET_SNAPSHOT_TTL_MS
            });

            let kind = if instruments_due {
                Some(GexFetchKind::Instruments(key))
            } else if force || raw_due {
                Some(GexFetchKind::Snapshot(key))
            } else {
                None
            };
            if let Some(kind) = kind
                && self.in_flight.insert(kind)
            {
                due.push(kind);
            }
        }
        due
    }

    pub fn instruments_for(&self, key: OptionsChainKey) -> Arc<[OptionInstrument]> {
        self.instruments
            .get(&key)
            .map(|cached| cached.values.clone())
            .unwrap_or_default()
    }

    pub fn complete(&mut self, completion: GexFetchResult, now: UnixMs) {
        match completion {
            GexFetchResult::Instruments { key, result } => {
                self.in_flight.remove(&GexFetchKind::Instruments(key));
                match result {
                    Ok(values) if !values.is_empty() => {
                        self.instruments.insert(
                            key,
                            CachedInstruments {
                                values: values.into(),
                                refreshed_at: now,
                            },
                        );
                        self.failures.remove(&key);
                        self.force_refresh.insert(key);
                    }
                    Ok(_) => self.record_failure(key, "empty instrument metadata".into(), now),
                    Err(error) => self.record_failure(key, error, now),
                }
            }
            GexFetchResult::Snapshot { key, result } => {
                self.in_flight.remove(&GexFetchKind::Snapshot(key));
                self.force_refresh.remove(&key);
                match result {
                    Ok(value) if !value.contracts.is_empty() => {
                        let revision = self.next_revision;
                        self.next_revision = self.next_revision.saturating_add(1);
                        self.raw_snapshots.insert(
                            key,
                            CachedRawSnapshot {
                                value: Arc::new(value),
                                received_at: now,
                                revision,
                                loaded_from_disk: false,
                            },
                        );
                        self.derived_snapshots
                            .retain(|derived, _| derived.chain != key);
                        self.failures.remove(&key);
                        if let Err(error) = self.save_persistent() {
                            log::warn!("GEX cache write failed: {error}");
                        }
                    }
                    Ok(_) => self.record_failure(key, "empty option chain".into(), now),
                    Err(error) => self.record_failure(key, error, now),
                }
            }
        }
    }

    pub fn derived(
        &mut self,
        underlying: OptionsUnderlying,
        config: &Config,
        now: UnixMs,
    ) -> Option<Arc<GexSnapshot>> {
        let chain = OptionsChainKey::deribit(underlying);
        let raw = self.raw_snapshots.get(&chain)?;
        let key = DerivedGexKey {
            chain,
            model: config.sign_model,
            expiry: config.expiry_filter,
            min_oi_bits: config.min_open_interest.to_bits(),
            min_gex_bits: config.min_absolute_gex.to_bits(),
            gamma_source: config.gamma_source,
            scenario_resolution: config.scenario_resolution,
            revision: raw.revision,
        };
        if let Some(cached) = self.derived_snapshots.get(&key) {
            return Some(cached.value.clone());
        }
        let value = Arc::new(calculate_gex_at(&raw.value, config, now));
        self.derived_snapshots.insert(
            key,
            CachedGexSnapshot {
                value: value.clone(),
            },
        );
        let series_key = DerivedGexSeriesKey {
            chain,
            model: config.sign_model,
            expiry: config.expiry_filter,
            min_oi_bits: config.min_open_interest.to_bits(),
            min_gex_bits: config.min_absolute_gex.to_bits(),
            gamma_source: config.gamma_source,
            scenario_resolution: config.scenario_resolution,
        };
        let revision = raw.revision;
        self.ensure_history_loaded(series_key, now);
        self.append_history(series_key, revision, value.clone(), now);
        Some(value)
    }

    pub fn history(
        &mut self,
        underlying: OptionsUnderlying,
        config: &Config,
        retention_minutes: u16,
        now: UnixMs,
    ) -> Vec<Arc<GexSnapshot>> {
        let key = DerivedGexSeriesKey {
            chain: OptionsChainKey::deribit(underlying),
            model: config.sign_model,
            expiry: config.expiry_filter,
            min_oi_bits: config.min_open_interest.to_bits(),
            min_gex_bits: config.min_absolute_gex.to_bits(),
            gamma_source: config.gamma_source,
            scenario_resolution: config.scenario_resolution,
        };
        self.ensure_history_loaded(key, now);
        let retention_ms = u64::from(retention_minutes.clamp(30, 24 * 60)) * 60_000;
        let cutoff = now.saturating_sub(retention_ms);
        self.derived_history
            .get(&key)
            .into_iter()
            .flatten()
            .filter(|entry| entry.value.observed_at >= cutoff)
            .map(|entry| entry.value.clone())
            .collect()
    }

    fn append_history(
        &mut self,
        key: DerivedGexSeriesKey,
        revision: u64,
        value: Arc<GexSnapshot>,
        now: UnixMs,
    ) {
        const DISK_RETENTION_MS: u64 = 24 * 60 * 60 * 1_000;
        const MAX_HISTORY_SNAPSHOTS: usize = 5_760;
        let history = self.derived_history.entry(key).or_default();
        if history
            .iter()
            .any(|entry| entry.revision == revision || entry.value.observed_at == value.observed_at)
        {
            return;
        }
        let position = history
            .iter()
            .position(|entry| entry.value.observed_at > value.observed_at)
            .unwrap_or(history.len());
        let persisted = value.clone();
        history.insert(position, HistoricalGexSnapshot { revision, value });
        let cutoff = now.saturating_sub(DISK_RETENTION_MS);
        while history
            .front()
            .is_some_and(|entry| entry.value.observed_at < cutoff)
            || history.len() > MAX_HISTORY_SNAPSHOTS
        {
            history.pop_front();
        }
        if self.persist_heatmap
            && let Some(cache) = crate::connector::persistent_cache::market_cache()
        {
            cache.store_gex_snapshot(&series_cache_key(key), persisted.as_ref());
        }
    }

    fn ensure_history_loaded(&mut self, key: DerivedGexSeriesKey, now: UnixMs) {
        if self.loaded_history.contains(&key) || !self.persist_heatmap {
            return;
        }
        let Some(cache) = crate::connector::persistent_cache::market_cache() else {
            return;
        };
        self.loaded_history.insert(key);
        let from = now.saturating_sub(24 * 60 * 60 * 1_000);
        let canonical = series_cache_key(key);
        let mut report = cache.read_gex_history_detailed(&canonical, from, now);
        let legacy = legacy_series_cache_key(key);
        let legacy_report = cache.read_gex_history_detailed(&legacy, from, now);
        if !legacy_report.snapshots.is_empty() {
            log::debug!(
                "GEX HistoryLegacyKey | canonical={} legacy={} snapshots={}",
                canonical,
                legacy,
                legacy_report.snapshots.len()
            );
            report.snapshots.extend(legacy_report.snapshots);
        }
        let mut stored = report
            .snapshots
            .into_iter()
            .map(|value| HistoricalGexSnapshot {
                revision: 0,
                value: Arc::new(value),
            })
            .collect::<Vec<_>>();
        stored.sort_by_key(|entry| entry.value.observed_at);
        stored.dedup_by_key(|entry| entry.value.observed_at);
        let first = stored.first().map(|entry| entry.value.observed_at.as_u64());
        let last = stored.last().map(|entry| entry.value.observed_at.as_u64());
        let loaded = stored.len();
        self.derived_history.entry(key).or_default().extend(stored);
        log::debug!(
            "GEX HistoryLoaded | key={} requested_buckets={} found_buckets={} decoded={} valid={} discarded={} deduplicated={} corrupt_buckets={} loaded={} first={:?} last={:?}",
            canonical,
            report.buckets_requested,
            report.buckets_found,
            report.decoded,
            report.valid,
            report.discarded,
            report.deduplicated,
            report.corrupt_buckets,
            loaded,
            first,
            last,
        );
    }

    pub fn freshness(&mut self, underlying: OptionsUnderlying, now: UnixMs) -> GexFreshness {
        let key = OptionsChainKey::deribit(underlying);
        let freshness = if self.failures.contains_key(&key) {
            GexFreshness::Error
        } else if let Some(raw) = self.raw_snapshots.get(&key) {
            if raw.loaded_from_disk {
                GexFreshness::Stale
            } else {
                let age = now.saturating_diff(raw.received_at);
                if age <= FRESH_THRESHOLD_MS {
                    GexFreshness::Fresh
                } else if age <= EXPIRED_THRESHOLD_MS {
                    GexFreshness::Stale
                } else {
                    GexFreshness::Expired
                }
            }
        } else {
            GexFreshness::Loading
        };
        let previous = self.last_freshness.insert(key, freshness);
        if freshness == GexFreshness::Stale && previous != Some(GexFreshness::Stale) {
            log::warn!("GEX SnapshotStale underlying={underlying}");
        }
        freshness
    }

    pub fn last_error(&self, underlying: OptionsUnderlying) -> Option<&str> {
        self.failures
            .get(&OptionsChainKey::deribit(underlying))
            .map(|failure| failure.last_error.as_ref())
    }

    pub fn invalidate_persistent(&mut self) -> std::io::Result<()> {
        self.raw_snapshots.clear();
        self.derived_snapshots.clear();
        self.derived_history.clear();
        self.loaded_history.clear();
        if self.cache_path.exists() {
            std::fs::remove_file(&self.cache_path)?;
        }
        Ok(())
    }

    fn record_failure(&mut self, key: OptionsChainKey, error: Arc<str>, now: UnixMs) {
        let attempts = self
            .failures
            .get(&key)
            .map_or(1, |failure| failure.attempts.saturating_add(1));
        let exponent = attempts.saturating_sub(1).min(8);
        let backoff = FAILURE_BACKOFF_BASE_MS
            .saturating_mul(1u64 << exponent)
            .min(FAILURE_BACKOFF_MAX_MS);
        self.failures.insert(
            key,
            FailureState {
                attempts,
                retry_after: now.saturating_add(backoff),
                last_error: error,
            },
        );
    }

    fn load_persistent(&mut self) {
        let Ok(bytes) = std::fs::read(&self.cache_path) else {
            return;
        };
        let Ok(stored) = serde_json::from_slice::<StoredCache>(&bytes) else {
            log::warn!("GEX persistent snapshot is corrupt; ignoring it");
            return;
        };
        if stored.schema != CACHE_SCHEMA {
            return;
        }
        for snapshot in stored.snapshots {
            if snapshot.contracts.is_empty() {
                continue;
            }
            let key = OptionsChainKey {
                provider: snapshot.provider,
                underlying: snapshot.underlying,
            };
            let revision = self.next_revision;
            self.next_revision = self.next_revision.saturating_add(1);
            self.raw_snapshots.insert(
                key,
                CachedRawSnapshot {
                    received_at: snapshot.observed_at,
                    value: Arc::new(snapshot),
                    revision,
                    loaded_from_disk: true,
                },
            );
        }
    }

    fn save_persistent(&self) -> std::io::Result<()> {
        let stored = StoredCache {
            schema: CACHE_SCHEMA,
            snapshots: self
                .raw_snapshots
                .values()
                .map(|cached| (*cached.value).clone())
                .collect(),
        };
        let bytes = serde_json::to_vec(&stored).map_err(std::io::Error::other)?;
        atomic_write(&self.cache_path, &bytes)
    }
}

fn derive_exact_match_count(
    chain: &RawOptionChainSnapshot,
    trades: &[DeriveMakerTrade],
    now: UnixMs,
    lookback_minutes: u16,
) -> (usize, usize) {
    const MAX_EXPIRY_DIFFERENCE_MS: u64 = 12 * 60 * 60 * 1_000;
    let cutoff = now.saturating_sub(u64::from(lookback_minutes) * 60 * 1_000);
    let mut expiries_by_key: FxHashMap<OptionContractMatchKey, Vec<UnixMs>> = FxHashMap::default();
    for contract in chain
        .contracts
        .iter()
        .filter(|contract| contract.instrument.expiration_timestamp > now)
    {
        let Some(key) = OptionContractMatchKey::new(
            chain.underlying,
            contract.instrument.expiration_timestamp,
            contract.instrument.strike,
            contract.instrument.right,
        ) else {
            continue;
        };
        expiries_by_key
            .entry(key)
            .or_default()
            .push(contract.instrument.expiration_timestamp);
    }
    let window = trades
        .iter()
        .filter(|trade| trade.timestamp >= cutoff && trade.timestamp <= now)
        .collect::<Vec<_>>();
    let exact_matches = window
        .iter()
        .filter(|trade| {
            expiries_by_key.get(&trade.key).is_some_and(|expiries| {
                expiries.iter().any(|expiry| {
                    expiry
                        .as_u64()
                        .abs_diff(trade.expiration_timestamp.as_u64())
                        <= MAX_EXPIRY_DIFFERENCE_MS
                })
            })
        })
        .count();
    (window.len(), exact_matches)
}

pub fn series_cache_key(key: DerivedGexSeriesKey) -> String {
    format!(
        "gex|provider={:?}|underlying={:?}|model={:?}|expiry={:?}|min_oi={:016x}|min_gex={:016x}|gamma_source={:?}|scenario={:?}",
        key.chain.provider,
        key.chain.underlying,
        key.model,
        key.expiry,
        key.min_oi_bits,
        key.min_gex_bits,
        key.gamma_source,
        key.scenario_resolution,
    )
}

fn proxy_cache_key(underlying: OptionsUnderlying) -> String {
    format!("source=gexmonitor|underlying={}", underlying.as_str())
}

fn derive_cache_key(underlying: OptionsUnderlying) -> String {
    format!("source=derive|underlying={}", underlying.as_str())
}

fn normalize_proxy_points(points: &mut Vec<GexProxyHistoryPoint>, now: UnixMs) {
    const RETENTION_WITH_MARGIN_MS: i64 = 24 * 60 * 60 * 1_000 + 10 * 60 * 1_000;
    const MAX_PROXY_RECORDS: usize = 320;
    let cutoff = i64::try_from(now.as_u64())
        .unwrap_or(i64::MAX)
        .saturating_sub(RETENTION_WITH_MARGIN_MS);
    points.retain(|point| point.is_semantically_valid() && point.observed_at >= cutoff);
    points.sort_by_key(|point| point.observed_at);
    points.dedup_by_key(|point| point.observed_at);
    if points.len() > MAX_PROXY_RECORDS {
        points.drain(..points.len() - MAX_PROXY_RECORDS);
    }
}

fn legacy_series_cache_key(key: DerivedGexSeriesKey) -> String {
    format!(
        "gex|provider={:?}|underlying={:?}|model={:?}|expiry={:?}|min_oi={:016x}|min_gex={:016x}",
        key.chain.provider,
        key.chain.underlying,
        key.model,
        key.expiry,
        key.min_oi_bits,
        key.min_gex_bits,
    )
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredCache {
    schema: u32,
    snapshots: Vec<RawOptionChainSnapshot>,
}

fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, bytes)?;
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    std::fs::rename(temporary, path)
}

pub async fn execute_fetch(
    client: DeribitOptionsClient,
    request: GexFetchKind,
    instruments: Arc<[OptionInstrument]>,
) -> GexFetchResult {
    let key = request.key();
    match request {
        GexFetchKind::Instruments(_) => GexFetchResult::Instruments {
            key,
            result: client
                .fetch_instruments(key.underlying)
                .await
                .map_err(error_text),
        },
        GexFetchKind::Snapshot(_) => GexFetchResult::Snapshot {
            key,
            result: client
                .fetch_chain(key.underlying, &instruments)
                .await
                .map_err(error_text),
        },
    }
}

pub async fn execute_proxy_fetch(
    client: GexMonitorClient,
    underlying: OptionsUnderlying,
) -> (OptionsUnderlying, Result<GexProxyHistoryResponse, Arc<str>>) {
    let result = client
        .fetch_history(underlying)
        .await
        .map_err(|error| Arc::from(error.to_string()));
    (underlying, result)
}

pub async fn execute_quantwheel_fetch(
    client: QuantWheelGexClient,
) -> Result<QuantWheelGexSnapshot, Arc<str>> {
    client
        .fetch_gex()
        .await
        .map_err(|error: QuantWheelError| Arc::from(error.to_string()))
}

pub async fn execute_derive_instruments_fetch(
    client: DeriveOptionsClient,
    underlying: OptionsUnderlying,
) -> DeriveInstrumentsFetchResult {
    DeriveInstrumentsFetchResult {
        underlying,
        result: client
            .fetch_instruments(underlying)
            .await
            .map_err(|error| Arc::from(error.to_string())),
    }
}

pub async fn execute_derive_trades_fetch(
    client: DeriveOptionsClient,
    request: DeriveTradeFetchRequest,
    instruments: Arc<[DeriveOptionInstrument]>,
) -> DeriveTradesFetchResult {
    DeriveTradesFetchResult {
        underlying: request.underlying,
        result: client
            .fetch_trade_history(request.underlying, request.from, request.to, &instruments)
            .await
            .map_err(|error| Arc::from(error.to_string())),
    }
}

fn error_text(error: DeribitError) -> Arc<str> {
    Arc::from(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use exchange::options::{
        OptionInstrument, OptionMarketPoint, OptionRight, RawOptionContractSnapshot,
    };

    fn coordinator() -> GexDataCoordinator {
        GexDataCoordinator::new(std::env::temp_dir().join(format!(
            "flowsurface-gex-test-{}.json",
            uuid::Uuid::new_v4()
        )))
    }

    fn instrument() -> OptionInstrument {
        OptionInstrument {
            instrument_name: "BTC-TEST".into(),
            underlying: OptionsUnderlying::Btc,
            expiration_timestamp: UnixMs::new(2_000_000_000_000),
            strike: 100_000.0,
            right: OptionRight::Call,
            contract_size: 1.0,
        }
    }

    fn snapshot(observed_at: UnixMs) -> RawOptionChainSnapshot {
        let instrument = instrument();
        RawOptionChainSnapshot {
            provider: OptionsProvider::Deribit,
            underlying: OptionsUnderlying::Btc,
            source_spot: 100_000.0,
            contracts: vec![RawOptionContractSnapshot {
                market: OptionMarketPoint {
                    instrument_name: instrument.instrument_name.clone(),
                    open_interest_underlying: 10.0,
                    mark_iv_percent: 50.0,
                    underlying_price: 100_000.0,
                    interest_rate: 0.0,
                    observed_at,
                    native_gamma: None,
                    native_gamma_observed_at: None,
                },
                instrument,
            }]
            .into(),
            observed_at,
        }
    }

    fn seed_instruments(coordinator: &mut GexDataCoordinator, now: UnixMs) {
        let key = OptionsChainKey::deribit(OptionsUnderlying::Btc);
        coordinator.complete(
            GexFetchResult::Instruments {
                key,
                result: Ok(vec![instrument()]),
            },
            now,
        );
    }

    #[test]
    fn consumers_and_inflight_deduplicate_fetches() {
        let now = UnixMs::new(1_800_000_000_000);
        let mut value = coordinator();
        assert!(value.due_fetches(now, true).is_empty());
        value.set_consumers([OptionsUnderlying::Btc, OptionsUnderlying::Btc]);
        assert_eq!(value.subscriber_count(OptionsUnderlying::Btc), 2);
        assert_eq!(value.due_fetches(now, true).len(), 1);
        assert!(value.due_fetches(now, true).is_empty());
        value.set_consumers([] as [OptionsUnderlying; 0]);
        assert!(
            value
                .due_fetches(now.saturating_add(INSTRUMENT_TTL_MS), true)
                .is_empty()
        );
    }

    #[test]
    fn btc_and_eth_are_separate_and_offline_stops_polling() {
        let now = UnixMs::new(1_800_000_000_000);
        let mut value = coordinator();
        value.set_consumers([OptionsUnderlying::Btc, OptionsUnderlying::Eth]);
        assert!(value.due_fetches(now, false).is_empty());
        let due = value.due_fetches(now, true);
        assert_eq!(due.len(), 2);
        assert_ne!(due[0].key(), due[1].key());
    }

    #[test]
    fn unsupported_market_never_creates_a_proxy_request() {
        let ticker = exchange::Ticker::new("SOLUSDT", exchange::adapter::Exchange::BinanceLinear);
        let mut value = coordinator();
        value.set_consumers(exchange::options::resolve_options_underlying(ticker));
        assert!(
            value
                .due_proxy_fetches(UnixMs::new(1_800_000_000_000), true)
                .is_empty()
        );
    }

    fn quantwheel_source(now: UnixMs) -> QuantWheelGexSnapshot {
        QuantWheelGexSnapshot {
            provider: OptionsProvider::QuantWheel,
            underlying: OptionsUnderlying::Gld,
            stock_price: 400.0,
            total_gex: 9_689.0,
            call_wall: None,
            put_wall: None,
            gamma_inflection: None,
            levels: vec![exchange::options::quantwheel::QuantWheelGexLevel {
                strike: 405.0,
                call_gex: 11_413.0,
                put_gex: 1_724.0,
                net_gex: 9_689.0,
                call_open_interest: 2_075.0,
                put_open_interest: 254.0,
                cumulative_gex: 14_047.0,
            }],
            observed_at: now,
        }
    }

    #[test]
    fn quantwheel_refresh_is_independent_from_realtime_mapping() {
        let now = UnixMs::new(1_800_000_000_000);
        let mut value = coordinator();
        value.set_consumers([GexSource::xaut_gld()]);
        assert!(value.due_quantwheel_fetch(now, true));
        value.complete_quantwheel(Ok(quantwheel_source(now)), now);

        let first = value
            .mapped_quantwheel("XAUTUSDT", 4_000.0)
            .expect("mapped snapshot");
        let moved = value
            .mapped_quantwheel("XAUTUSDT", 4_100.0)
            .expect("remapped snapshot");
        assert_eq!(first.strikes[0].strike, 4_050.0);
        assert_eq!(moved.strikes[0].strike, 4_151.25);
        assert_eq!(first.strikes[0].net_gex_1pct, moved.strikes[0].net_gex_1pct);
        assert!(!value.due_quantwheel_fetch(now.saturating_add(1), true));
        assert!(value.due_quantwheel_fetch(now.saturating_add(QUANTWHEEL_REFRESH_MS), true));
    }

    #[test]
    fn quantwheel_failure_keeps_last_valid_snapshot() {
        let now = UnixMs::new(1_800_000_000_000);
        let mut value = coordinator();
        value.set_consumers([GexSource::xaut_gld()]);
        assert!(value.due_quantwheel_fetch(now, true));
        value.complete_quantwheel(Ok(quantwheel_source(now)), now);
        assert!(value.due_quantwheel_fetch(now.saturating_add(QUANTWHEEL_REFRESH_MS), true));
        value.complete_quantwheel(
            Err("remote unavailable".into()),
            now.saturating_add(QUANTWHEEL_REFRESH_MS),
        );
        assert!(value.mapped_quantwheel("XAUTUSDT", 4_000.0).is_some());
        assert_eq!(
            value.quantwheel_freshness(now.saturating_add(QUANTWHEEL_REFRESH_MS)),
            GexFreshness::Error
        );
    }

    #[test]
    fn failures_backoff_and_keep_last_valid_snapshot() {
        let now = UnixMs::new(1_800_000_000_000);
        let key = OptionsChainKey::deribit(OptionsUnderlying::Btc);
        let mut value = coordinator();
        value.set_consumers([OptionsUnderlying::Btc]);
        seed_instruments(&mut value, now);
        value.complete(
            GexFetchResult::Snapshot {
                key,
                result: Ok(snapshot(now)),
            },
            now,
        );
        assert!(
            value
                .derived(OptionsUnderlying::Btc, &Config::default(), now)
                .is_some()
        );
        value.complete(
            GexFetchResult::Snapshot {
                key,
                result: Err("network".into()),
            },
            now.saturating_add(MARKET_SNAPSHOT_TTL_MS),
        );
        assert!(
            value
                .derived(OptionsUnderlying::Btc, &Config::default(), now)
                .is_some()
        );
        assert!(
            value
                .due_fetches(now.saturating_add(MARKET_SNAPSHOT_TTL_MS + 1), true)
                .is_empty()
        );
    }

    #[test]
    fn config_only_changes_derived_cache_and_raw_revision_invalidates_it() {
        let now = UnixMs::new(1_800_000_000_000);
        let key = OptionsChainKey::deribit(OptionsUnderlying::Btc);
        let mut value = coordinator();
        value.set_consumers([OptionsUnderlying::Btc]);
        seed_instruments(&mut value, now);
        value.complete(
            GexFetchResult::Snapshot {
                key,
                result: Ok(snapshot(now)),
            },
            now,
        );
        let first = value
            .derived(OptionsUnderlying::Btc, &Config::default(), now)
            .expect("derived");
        let absolute = value
            .derived(
                OptionsUnderlying::Btc,
                &Config {
                    sign_model: GexSignModel::AbsoluteGamma,
                    ..Config::default()
                },
                now,
            )
            .expect("derived");
        assert_ne!(first.model, absolute.model);
        assert!(value.due_fetches(now, true).is_empty());
        value.complete(
            GexFetchResult::Snapshot {
                key,
                result: Ok(snapshot(now.saturating_add(1))),
            },
            now.saturating_add(1),
        );
        let second = value
            .derived(OptionsUnderlying::Btc, &Config::default(), now)
            .expect("derived");
        assert!(!Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn reconnect_forces_refresh_and_freshness_transitions() {
        let now = UnixMs::new(1_800_000_000_000);
        let key = OptionsChainKey::deribit(OptionsUnderlying::Btc);
        let mut value = coordinator();
        value.set_consumers([OptionsUnderlying::Btc]);
        seed_instruments(&mut value, now);
        value.complete(
            GexFetchResult::Snapshot {
                key,
                result: Ok(snapshot(now)),
            },
            now,
        );
        assert_eq!(
            value.freshness(OptionsUnderlying::Btc, now),
            GexFreshness::Fresh
        );
        assert_eq!(
            value.freshness(
                OptionsUnderlying::Btc,
                now.saturating_add(FRESH_THRESHOLD_MS + 1)
            ),
            GexFreshness::Stale
        );
        assert_eq!(
            value.freshness(
                OptionsUnderlying::Btc,
                now.saturating_add(EXPIRED_THRESHOLD_MS + 1)
            ),
            GexFreshness::Expired
        );
        value.reconnect();
        assert_eq!(value.due_fetches(now.saturating_add(1), true).len(), 1);
    }

    #[test]
    fn corrupt_persistent_snapshot_is_ignored() {
        let path = std::env::temp_dir().join(format!(
            "flowsurface-gex-corrupt-{}.json",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, b"not-json").expect("fixture");
        let value = GexDataCoordinator::new(path.clone());
        assert!(value.raw_snapshots.is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn persistent_snapshot_loads_stale_and_refreshes_with_consumer() {
        let path = std::env::temp_dir().join(format!(
            "flowsurface-gex-persisted-{}.json",
            uuid::Uuid::new_v4()
        ));
        let now = UnixMs::new(1_800_000_000_000);
        let key = OptionsChainKey::deribit(OptionsUnderlying::Btc);
        let mut first = GexDataCoordinator::new(path.clone());
        first.complete(
            GexFetchResult::Snapshot {
                key,
                result: Ok(snapshot(now)),
            },
            now,
        );
        let mut restored = GexDataCoordinator::new(path.clone());
        assert_eq!(
            restored.freshness(OptionsUnderlying::Btc, now),
            GexFreshness::Stale
        );
        restored.set_consumers([OptionsUnderlying::Btc]);
        assert!(matches!(
            restored.due_fetches(now, true).as_slice(),
            [GexFetchKind::Instruments(_)]
        ));
        let _ = std::fs::remove_file(path);
    }

    fn proxy_point(observed_at: i64, total_gex: f64) -> GexProxyHistoryPoint {
        GexProxyHistoryPoint {
            observed_at,
            source_spot: 100_000.0,
            total_gex,
            flip_level: Some(100_000.0),
            call_wall: Some(102_000.0),
            put_wall: Some(98_000.0),
            positive_level_1: Some(101_000.0),
            positive_level_2: None,
            negative_level_1: Some(99_000.0),
            negative_level_2: None,
        }
    }

    #[test]
    fn proxy_scheduler_is_independent_offline_bounded_and_reconnectable() {
        let now = UnixMs::new(1_800_000_000_000);
        let mut value = coordinator();
        assert!(value.due_proxy_fetches(now, true).is_empty());
        value.set_consumers([OptionsUnderlying::Btc]);
        assert!(value.due_proxy_fetches(now, false).is_empty());
        assert_eq!(value.due_proxy_fetches(now, true), [OptionsUnderlying::Btc]);
        assert!(value.due_proxy_fetches(now, true).is_empty());
        value.complete_proxy(
            OptionsUnderlying::Btc,
            Ok(GexProxyHistoryResponse {
                points: vec![proxy_point(now.as_u64() as i64, 1.0)],
                stale: false,
            }),
            now,
        );
        assert!(
            value
                .due_proxy_fetches(now.saturating_add(PROXY_REFRESH_MS - 1), true)
                .is_empty()
        );
        assert_eq!(
            value.due_proxy_fetches(now.saturating_add(PROXY_REFRESH_MS), true),
            [OptionsUnderlying::Btc]
        );
        value.complete_proxy(
            OptionsUnderlying::Btc,
            Ok(GexProxyHistoryResponse {
                points: vec![proxy_point(now.as_u64() as i64, 1.0)],
                stale: false,
            }),
            now.saturating_add(PROXY_REFRESH_MS),
        );
        value.reconnect();
        assert_eq!(
            value.due_proxy_fetches(now.saturating_add(PROXY_REFRESH_MS + 1), true),
            [OptionsUnderlying::Btc]
        );
    }

    #[test]
    fn proxy_error_does_not_change_deribit_freshness_or_error() {
        let now = UnixMs::new(1_800_000_000_000);
        let key = OptionsChainKey::deribit(OptionsUnderlying::Btc);
        let mut value = coordinator();
        value.set_consumers([OptionsUnderlying::Btc]);
        value.complete(
            GexFetchResult::Snapshot {
                key,
                result: Ok(snapshot(now)),
            },
            now,
        );
        let _ = value.due_proxy_fetches(now, true);
        value.complete_proxy(
            OptionsUnderlying::Btc,
            Err("remote unavailable".into()),
            now,
        );
        assert_eq!(
            value.freshness(OptionsUnderlying::Btc, now),
            GexFreshness::Fresh
        );
        assert!(value.last_error(OptionsUnderlying::Btc).is_none());
        assert_eq!(
            value.proxy_freshness(OptionsUnderlying::Btc, now),
            GexFreshness::Error
        );
    }

    #[test]
    fn stale_proxy_response_never_replaces_valid_cache() {
        let now = UnixMs::new(1_800_000_000_000);
        let mut value = coordinator();
        value.complete_proxy(
            OptionsUnderlying::Btc,
            Ok(GexProxyHistoryResponse {
                points: vec![proxy_point(now.as_u64() as i64, 10.0)],
                stale: false,
            }),
            now,
        );
        value.complete_proxy(
            OptionsUnderlying::Btc,
            Ok(GexProxyHistoryResponse {
                points: vec![proxy_point(now.as_u64() as i64 + 1, 999.0)],
                stale: true,
            }),
            now.saturating_add(1),
        );
        let history = value.proxy_history(OptionsUnderlying::Btc, now.saturating_add(1));
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].total_gex, 10.0);
        assert_eq!(
            value.proxy_freshness(OptionsUnderlying::Btc, now),
            GexFreshness::Stale
        );
    }

    fn heatmap_snapshot(observed_at: UnixMs, model: GexSignModel) -> Arc<GexSnapshot> {
        Arc::new(calculate_gex_at(
            &snapshot(observed_at),
            &Config {
                sign_model: model,
                ..Config::default()
            },
            observed_at,
        ))
    }

    #[test]
    fn history_is_ordered_deduplicated_and_pruned() {
        let now = UnixMs::new(1_800_100_000_000);
        let mut value = coordinator();
        let key = DerivedGexSeriesKey {
            chain: OptionsChainKey::deribit(OptionsUnderlying::Btc),
            model: GexSignModel::CallPutOiProxy,
            gamma_source: GexGammaSource::ProviderNativePreferred,
            expiry: GexExpiryFilter::SevenDays,
            scenario_resolution: GexScenarioResolution::Samples512,
            min_oi_bits: 0.0f64.to_bits(),
            min_gex_bits: 0.0f64.to_bits(),
        };
        let newer = now.saturating_sub(1_000);
        let older = now.saturating_sub(2_000);
        value.append_history(key, 2, heatmap_snapshot(newer, key.model), now);
        value.append_history(key, 1, heatmap_snapshot(older, key.model), now);
        value.append_history(key, 3, heatmap_snapshot(newer, key.model), now);
        assert_eq!(value.derived_history[&key].len(), 2);
        assert_eq!(value.derived_history[&key][0].value.observed_at, older);
        let expired = now.saturating_sub(24 * 60 * 60 * 1_000 + 1);
        value.append_history(key, 4, heatmap_snapshot(expired, key.model), now);
        assert!(
            value.derived_history[&key]
                .iter()
                .all(|entry| entry.value.observed_at != expired)
        );
    }

    #[test]
    fn histories_are_separated_by_dataset_configuration() {
        let now = UnixMs::new(1_800_100_000_000);
        let mut value = coordinator();
        let base = DerivedGexSeriesKey {
            chain: OptionsChainKey::deribit(OptionsUnderlying::Btc),
            model: GexSignModel::CallPutOiProxy,
            gamma_source: GexGammaSource::ProviderNativePreferred,
            expiry: GexExpiryFilter::SevenDays,
            scenario_resolution: GexScenarioResolution::Samples512,
            min_oi_bits: 0.0f64.to_bits(),
            min_gex_bits: 0.0f64.to_bits(),
        };
        let absolute = DerivedGexSeriesKey {
            model: GexSignModel::AbsoluteGamma,
            ..base
        };
        value.append_history(base, 1, heatmap_snapshot(now, base.model), now);
        value.append_history(absolute, 1, heatmap_snapshot(now, absolute.model), now);
        assert_eq!(value.derived_history.len(), 2);
        assert!(!Arc::ptr_eq(
            &value.derived_history[&base][0].value,
            &value.derived_history[&absolute][0].value
        ));
    }

    #[test]
    fn history_has_an_absolute_snapshot_cap() {
        let now = UnixMs::new(1_800_100_000_000);
        let mut value = coordinator();
        let key = DerivedGexSeriesKey {
            chain: OptionsChainKey::deribit(OptionsUnderlying::Btc),
            model: GexSignModel::CallPutOiProxy,
            gamma_source: GexGammaSource::ProviderNativePreferred,
            expiry: GexExpiryFilter::SevenDays,
            scenario_resolution: GexScenarioResolution::Samples512,
            min_oi_bits: 0.0f64.to_bits(),
            min_gex_bits: 0.0f64.to_bits(),
        };
        let template = heatmap_snapshot(now, key.model);
        for revision in 0..5_761u64 {
            let mut snapshot = (*template).clone();
            snapshot.observed_at = now.saturating_sub(5_761 - revision);
            value.append_history(key, revision + 1, Arc::new(snapshot), now);
        }
        assert_eq!(value.derived_history[&key].len(), 5_760);
    }

    #[test]
    fn derive_scheduling_is_offline_safe_overlapping_and_reconnect_forced() {
        let now = UnixMs::new(1_800_000_000_000);
        let mut value = coordinator();
        value.set_consumers([OptionsUnderlying::Btc]);
        assert!(value.due_derive_instrument_fetches(now, false).is_empty());
        assert!(value.due_derive_trade_fetches(now, false).is_empty());
        assert_eq!(
            value.due_derive_instrument_fetches(now, true),
            [OptionsUnderlying::Btc]
        );
        let contract = instrument();
        value.complete_derive_instruments(
            DeriveInstrumentsFetchResult {
                underlying: OptionsUnderlying::Btc,
                result: Ok(vec![DeriveOptionInstrument {
                    instrument_name: contract.instrument_name,
                    key: exchange::options::OptionContractMatchKey::new(
                        contract.underlying,
                        contract.expiration_timestamp,
                        contract.strike,
                        contract.right,
                    )
                    .expect("key"),
                    expiration_timestamp: contract.expiration_timestamp,
                }]),
            },
            now,
        );
        let request = value
            .due_derive_trade_fetches(now, true)
            .into_iter()
            .next()
            .expect("initial backfill");
        assert_eq!(request.from, now.saturating_sub(DERIVE_INITIAL_BACKFILL_MS));
        value.complete_derive_trades(
            DeriveTradesFetchResult {
                underlying: OptionsUnderlying::Btc,
                result: Ok(Vec::new()),
            },
            now,
        );
        assert!(
            value
                .due_derive_trade_fetches(now.saturating_add(DERIVE_TRADE_REFRESH_MS - 1), true)
                .is_empty()
        );
        value.reconnect();
        assert_eq!(
            value
                .due_derive_instrument_fetches(now.saturating_add(1), true)
                .len(),
            1
        );
        assert_eq!(
            value
                .due_derive_trade_fetches(now.saturating_add(1), true)
                .len(),
            1
        );
    }

    #[test]
    fn derive_backoff_and_errors_are_independent_from_deribit() {
        let now = UnixMs::new(1_800_000_000_000);
        let key = OptionsChainKey::deribit(OptionsUnderlying::Btc);
        let mut value = coordinator();
        value.set_consumers([OptionsUnderlying::Btc]);
        value.complete(
            GexFetchResult::Snapshot {
                key,
                result: Ok(snapshot(now)),
            },
            now,
        );
        let _ = value.due_derive_instrument_fetches(now, true);
        value.complete_derive_instruments(
            DeriveInstrumentsFetchResult {
                underlying: OptionsUnderlying::Btc,
                result: Err("derive unavailable".into()),
            },
            now,
        );
        assert!(
            value
                .due_derive_instrument_fetches(now.saturating_add(1), true)
                .is_empty()
        );
        assert_eq!(
            value.freshness(OptionsUnderlying::Btc, now),
            GexFreshness::Fresh
        );
        assert!(value.last_error(OptionsUnderlying::Btc).is_none());
        assert_eq!(
            value.derive_freshness(OptionsUnderlying::Btc, now),
            GexFreshness::Error
        );
    }
}
