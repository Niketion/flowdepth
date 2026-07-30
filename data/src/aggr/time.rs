use std::collections::BTreeMap;

use crate::chart::heatmap::HeatmapDataPoint;
use crate::chart::kline::{
    BubbleVolumeSummary, ClusterKind, KlineDataPoint, KlineTrades, NPoc, TradeCoverage,
};

use exchange::unit::{Price, PriceStep, Qty};
use exchange::{Kline, Timeframe, Trade, UnixMs, Volume};

pub trait DataPoint {
    fn add_trade(&mut self, trade: &Trade, step: PriceStep);

    fn clear_trades(&mut self);

    fn last_trade_time(&self) -> Option<UnixMs>;

    fn first_trade_time(&self) -> Option<UnixMs>;

    fn last_price(&self) -> Price;

    fn kline(&self) -> Option<&Kline>;

    fn value_high(&self) -> Price;

    fn value_low(&self) -> Price;
}

pub struct TimeSeries<D: DataPoint> {
    pub datapoints: BTreeMap<UnixMs, D>,
    pub interval: Timeframe,
    pub tick_size: PriceStep,
}

impl<D: DataPoint> TimeSeries<D> {
    pub fn base_price(&self) -> Price {
        self.datapoints
            .values()
            .last()
            .map_or(Price::from_f32(0.0), DataPoint::last_price)
    }

    pub fn latest_timestamp(&self) -> Option<UnixMs> {
        self.datapoints.keys().last().copied()
    }

    pub fn latest_kline(&self) -> Option<&Kline> {
        self.datapoints.values().last().and_then(|dp| dp.kline())
    }

    pub fn price_scale(&self, lookback: usize) -> (Price, Price) {
        let mut iter = self.datapoints.iter().rev().take(lookback);

        if let Some((_, first)) = iter.next() {
            let mut high = first.value_high();
            let mut low = first.value_low();

            for (_, dp) in iter {
                let value_high = dp.value_high();
                let value_low = dp.value_low();
                if value_high > high {
                    high = value_high;
                }
                if value_low < low {
                    low = value_low;
                }
            }

            (high, low)
        } else {
            (Price::from_f32(0.0), Price::from_f32(0.0))
        }
    }

    pub fn volume_data<'a>(&'a self) -> BTreeMap<UnixMs, exchange::Volume>
    where
        BTreeMap<UnixMs, exchange::Volume>: From<&'a TimeSeries<D>>,
    {
        self.into()
    }

    pub fn timerange(&self) -> (UnixMs, UnixMs) {
        let earliest = self
            .datapoints
            .keys()
            .next()
            .copied()
            .unwrap_or(UnixMs::ZERO);
        let latest = self
            .datapoints
            .keys()
            .last()
            .copied()
            .unwrap_or(UnixMs::ZERO);

        (earliest, latest)
    }

    pub fn min_max_price_in_range_prices(
        &self,
        earliest: UnixMs,
        latest: UnixMs,
    ) -> Option<(Price, Price)> {
        let mut it = self.datapoints.range(earliest..=latest);

        let (_, first) = it.next()?;
        let mut min_price = first.value_low();
        let mut max_price = first.value_high();

        for (_, dp) in it {
            let low = dp.value_low();
            let high = dp.value_high();
            if low < min_price {
                min_price = low;
            }
            if high > max_price {
                max_price = high;
            }
        }

        Some((min_price, max_price))
    }

    pub fn min_max_price_in_range(&self, earliest: UnixMs, latest: UnixMs) -> Option<(f32, f32)> {
        self.min_max_price_in_range_prices(earliest, latest)
            .map(|(min_p, max_p)| (min_p.to_f32_lossy(), max_p.to_f32_lossy()))
    }

    /// Ensures a datapoint bucket exists at `rounded_t` and ingests all trades into it.
    pub fn ingest_trades_bucket(&mut self, rounded_t: UnixMs, trades: &[Trade], step: PriceStep)
    where
        D: Default,
    {
        let bucket = self.datapoints.entry(rounded_t).or_default();

        for trade in trades {
            bucket.add_trade(trade, step);
        }
    }

    pub fn clear_trades(&mut self) {
        for data_point in self.datapoints.values_mut() {
            data_point.clear_trades();
        }
    }

    fn align_down_to_phase(time: UnixMs, phase: UnixMs, interval: u64) -> UnixMs {
        if time >= phase {
            let t = time.as_u64();
            let p = phase.as_u64();
            UnixMs::new(t.saturating_sub((t - p) % interval))
        } else {
            phase
        }
    }

    fn check_kline_integrity_range(
        &self,
        earliest: UnixMs,
        latest: UnixMs,
        interval: u64,
    ) -> Option<Vec<UnixMs>> {
        let mut time = earliest;
        let mut missing_count = 0;

        while time < latest {
            if !self.datapoints.contains_key(&time) {
                missing_count += 1;
                break;
            }
            time = time.saturating_add(interval);
        }

        if missing_count > 0 {
            let mut missing_keys =
                Vec::with_capacity(((latest.as_u64() - earliest.as_u64()) / interval) as usize);
            let mut time = earliest;

            while time < latest {
                if !self.datapoints.contains_key(&time) {
                    missing_keys.push(time);
                }
                time = time.saturating_add(interval);
            }

            log::debug!(
                "Integrity check failed: missing {} klines",
                missing_keys.len()
            );
            return Some(missing_keys);
        }

        None
    }

    pub fn check_kline_integrity(&self, earliest: UnixMs, latest: UnixMs) -> Option<Vec<UnixMs>> {
        if self.datapoints.is_empty() {
            return None;
        }

        let interval = self.interval.to_milliseconds();
        if interval == 0 {
            return None;
        }

        let (series_earliest, series_latest) = self.timerange();
        let phase = UnixMs::new(series_earliest.as_u64() % interval);

        let check_earliest =
            Self::align_down_to_phase(earliest.max(series_earliest), phase, interval)
                .max(series_earliest);
        let check_latest = Self::align_down_to_phase(latest.min(series_latest), phase, interval)
            .min(series_latest);

        if check_earliest < check_latest {
            self.check_kline_integrity_range(check_earliest, check_latest, interval)
        } else {
            None
        }
    }
}

impl TimeSeries<KlineDataPoint> {
    pub fn new(interval: Timeframe, tick_size: PriceStep, klines: &[Kline]) -> Self {
        let mut timeseries = Self {
            datapoints: BTreeMap::new(),
            interval,
            tick_size,
        };

        timeseries.insert_klines(klines);
        timeseries
    }

    pub fn with_trades(&self, trades: &[Trade]) -> TimeSeries<KlineDataPoint> {
        let mut new_series = Self {
            datapoints: self.datapoints.clone(),
            interval: self.interval,
            tick_size: self.tick_size,
        };

        new_series.insert_trades_or_create_bucket(trades);
        new_series
    }

    pub fn insert_klines(&mut self, klines: &[Kline]) {
        for kline in klines {
            let entry = self
                .datapoints
                .entry(kline.time)
                .or_insert_with(|| KlineDataPoint {
                    kline: *kline,
                    footprint: KlineTrades::new(),
                    bubble_summary: BubbleVolumeSummary::default(),
                    trade_coverage: TradeCoverage::Unknown,
                    trade_sequence: Vec::new(),
                    trade_ids: Default::default(),
                });

            entry.kline = *kline;
        }

        self.update_poc_status();
    }

    pub fn insert_trades_or_create_bucket(&mut self, buffer: &[Trade]) {
        if buffer.is_empty() {
            return;
        }
        let mut updated_times = Vec::new();

        buffer.iter().for_each(|trade| {
            let rounded_time = trade.time.floor_to(self.interval);

            if !updated_times.contains(&rounded_time) {
                updated_times.push(rounded_time);
            }

            let entry = self
                .datapoints
                .entry(rounded_time)
                .or_insert_with(|| KlineDataPoint {
                    kline: Kline {
                        time: rounded_time,
                        open: trade.price,
                        high: trade.price,
                        low: trade.price,
                        close: trade.price,
                        volume: Volume::empty_buy_sell(),
                    },
                    footprint: KlineTrades::new(),
                    bubble_summary: BubbleVolumeSummary::default(),
                    trade_coverage: TradeCoverage::Unknown,
                    trade_sequence: Vec::new(),
                    trade_ids: Default::default(),
                });

            entry.add_trade(trade, self.tick_size);
        });

        for time in updated_times {
            if let Some(data_point) = self.datapoints.get_mut(&time) {
                data_point.calculate_poc();
            }
        }
    }

    pub fn insert_trades_existing_buckets(&mut self, buffer: &[Trade]) {
        if buffer.is_empty() {
            return;
        }
        let mut updated_times: Vec<UnixMs> = Vec::new();

        for trade in buffer {
            let rounded_time = trade.time.floor_to(self.interval);

            if let Some(entry) = self.datapoints.get_mut(&rounded_time) {
                if !updated_times.contains(&rounded_time) {
                    updated_times.push(rounded_time);
                }
                entry.add_trade(trade, self.tick_size);
            }
        }

        for time in updated_times {
            if let Some(data_point) = self.datapoints.get_mut(&time) {
                data_point.calculate_poc();
            }
        }
    }

    /// Mark all kline buckets in a time range as having complete raw-trade coverage.
    /// Used when a trade fetch completes with no/empty results to prevent
    /// re-requesting the same empty range.
    pub fn mark_range_trades_complete(&mut self, from: UnixMs, to: UnixMs) {
        let interval_ms = self.interval.to_milliseconds();
        for (&time, dp) in self.datapoints.range_mut(from..=to) {
            if time.saturating_add(interval_ms) <= to.saturating_add(1) {
                dp.trade_coverage = TradeCoverage::Complete;
            }
        }
    }

    pub fn insert_bubble_summaries(&mut self, summaries: Vec<BubbleVolumeSummary>) {
        for summary in summaries {
            if let Some(dp) = self.datapoints.get_mut(&summary.candle_time) {
                dp.set_bubble_summary(summary);
            }
        }
    }

    pub fn change_tick_size(&mut self, tick_size: PriceStep) {
        self.tick_size = tick_size;

        // Re-bin from every candle's retained sequence. The chart-level raw
        // buffer is capped and may no longer contain the full SVP session.
        for data_point in self.datapoints.values_mut() {
            let trades = std::mem::take(&mut data_point.trade_sequence);
            let coverage = data_point.trade_coverage;
            data_point.clear_trades();
            for trade in &trades {
                data_point.add_trade(trade, tick_size);
            }
            data_point.trade_coverage = coverage;
            data_point.calculate_poc();
        }
        self.update_poc_status();
    }

    pub fn update_poc_status(&mut self) {
        let updates = self
            .datapoints
            .iter()
            .filter_map(|(&time, dp)| dp.poc_price().map(|price| (time, price)))
            .collect::<Vec<_>>();

        for (current_time, poc_price) in updates {
            let mut npoc = NPoc::default();

            for (&next_time, next_dp) in self.datapoints.range(current_time.saturating_add(1)..) {
                let next_dp_low = next_dp.kline.low.round_to_side_step(true, self.tick_size);
                let next_dp_high = next_dp.kline.high.round_to_side_step(false, self.tick_size);

                if next_dp_low <= poc_price && next_dp_high >= poc_price {
                    npoc.filled(next_time.as_u64());
                    break;
                } else {
                    npoc.unfilled();
                }
            }

            if let Some(data_point) = self.datapoints.get_mut(&current_time) {
                data_point.set_poc_status(npoc);
            }
        }
    }

    pub fn min_max_footprint_price_in_range(
        &self,
        earliest: UnixMs,
        latest: UnixMs,
    ) -> Option<(Price, Price)> {
        if latest < earliest {
            return None;
        }

        let mut min_price: Option<Price> = None;
        let mut max_price: Option<Price> = None;

        let mut track_price = |price: Price| {
            min_price = Some(match min_price {
                Some(current) => current.min(price),
                None => price,
            });
            max_price = Some(match max_price {
                Some(current) => current.max(price),
                None => price,
            });
        };

        self.datapoints
            .range(earliest..=latest)
            .for_each(|(_, dp)| {
                track_price(dp.kline.low);
                track_price(dp.kline.high);

                for price in dp.footprint.trades.keys() {
                    track_price(*price);
                }
            });

        match (min_price, max_price) {
            (Some(low), Some(high)) => Some((low, high)),
            _ => None,
        }
    }

    pub fn suggest_trade_fetch_range(
        &self,
        visible_earliest: UnixMs,
        visible_latest: UnixMs,
    ) -> Option<(UnixMs, UnixMs)> {
        if self.datapoints.is_empty() {
            return None;
        }

        self.find_trade_gap()
            .and_then(|(last_t_before_gap, first_t_after_gap)| {
                if last_t_before_gap.is_none() && first_t_after_gap.is_none() {
                    return None;
                }
                let (data_earliest, data_latest) = self.timerange();

                let fetch_from = last_t_before_gap
                    .map_or(data_earliest, |t| t.saturating_add(1))
                    .max(visible_earliest);
                let fetch_to = first_t_after_gap
                    .map_or(data_latest, |t| t.saturating_sub(1))
                    .min(visible_latest);

                if fetch_from < fetch_to {
                    Some((fetch_from, fetch_to))
                } else {
                    None
                }
            })
    }

    fn find_trade_gap(&self) -> Option<(Option<UnixMs>, Option<UnixMs>)> {
        let empty_kline_time = self
            .datapoints
            .iter()
            .rev()
            .find(|(_, dp)| {
                dp.footprint.trades.is_empty() && dp.trade_coverage != TradeCoverage::Complete
            })
            .map(|(&time, _)| time);

        if let Some(target_time) = empty_kline_time {
            let last_t_before_gap = self
                .datapoints
                .range(..target_time)
                .rev()
                .find_map(|(_, dp)| dp.last_trade_time());

            let first_t_after_gap = self
                .datapoints
                .range(target_time.saturating_add(1)..)
                .find_map(|(_, dp)| dp.first_trade_time());

            Some((last_t_before_gap, first_t_after_gap))
        } else {
            None
        }
    }

    pub fn max_qty_ts_range(
        &self,
        cluster_kind: ClusterKind,
        earliest: UnixMs,
        latest: UnixMs,
        highest: Price,
        lowest: Price,
    ) -> Qty {
        let mut max_cluster_qty: Qty = Qty::default();

        self.datapoints
            .range(earliest..=latest)
            .for_each(|(_, dp)| {
                max_cluster_qty =
                    max_cluster_qty.max(dp.max_cluster_qty(cluster_kind, highest, lowest));
            });

        max_cluster_qty
    }
}

impl TimeSeries<HeatmapDataPoint> {
    pub fn new(timeframe: Timeframe, tick_size: PriceStep) -> Self {
        Self {
            datapoints: BTreeMap::new(),
            interval: timeframe,
            tick_size,
        }
    }

    pub fn max_trade_qty_and_aggr_volume(&self, earliest: UnixMs, latest: UnixMs) -> (Qty, Qty) {
        let mut max_trade_qty = Qty::ZERO;
        let mut max_aggr_volume = Qty::ZERO;

        self.datapoints
            .range(earliest..=latest)
            .for_each(|(_, dp)| {
                let (mut buy_volume, mut sell_volume) = (Qty::ZERO, Qty::ZERO);

                dp.grouped_trades.iter().for_each(|trade| {
                    let trade_qty = trade.qty;
                    max_trade_qty = max_trade_qty.max(trade_qty);

                    if trade.is_sell {
                        sell_volume += trade_qty;
                    } else {
                        buy_volume += trade_qty;
                    }
                });

                max_aggr_volume = max_aggr_volume.max(buy_volume + sell_volume);
            });

        (max_trade_qty, max_aggr_volume)
    }

    pub fn max_trade_qty_in_range(
        &self,
        earliest: UnixMs,
        latest: UnixMs,
        highest: Price,
        lowest: Price,
    ) -> Qty {
        let mut max_trade_qty = Qty::default();

        self.datapoints
            .range(earliest..=latest)
            .for_each(|(_, dp)| {
                dp.grouped_trades.iter().for_each(|trade| {
                    if trade.price >= lowest && trade.price <= highest {
                        max_trade_qty = max_trade_qty.max(trade.qty);
                    }
                });
            });

        max_trade_qty
    }
}

impl From<&TimeSeries<KlineDataPoint>> for BTreeMap<UnixMs, exchange::Volume> {
    /// Converts datapoints into a map of timestamps and volume data
    fn from(timeseries: &TimeSeries<KlineDataPoint>) -> Self {
        timeseries
            .datapoints
            .iter()
            .map(|(time, dp)| (*time, dp.kline.volume))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kline_tick_size_change_preserves_full_trade_sequences() {
        let old_step = PriceStep {
            units: Price::from_f64(0.1).units,
        };
        let new_step = PriceStep {
            units: Price::from_f64(1.0).units,
        };
        let candle_time = UnixMs::new(60_000);
        let kline = Kline {
            time: candle_time,
            open: Price::from_f64(100.0),
            high: Price::from_f64(101.0),
            low: Price::from_f64(99.0),
            close: Price::from_f64(100.5),
            volume: Volume::empty_buy_sell(),
        };
        let trades = [
            Trade {
                id: Some(1),
                time: UnixMs::new(61_000),
                is_sell: false,
                price: Price::from_f64(100.1),
                qty: Qty::from_f64(2.0),
            },
            Trade {
                id: Some(2),
                time: UnixMs::new(62_000),
                is_sell: true,
                price: Price::from_f64(100.9),
                qty: Qty::from_f64(3.0),
            },
        ];
        let mut series = TimeSeries::<KlineDataPoint>::new(Timeframe::M1, old_step, &[kline]);
        series.insert_trades_existing_buckets(&trades);
        series.mark_range_trades_complete(candle_time, candle_time.saturating_add(60_000));

        series.change_tick_size(new_step);

        let data_point = series.datapoints.get(&candle_time).expect("candle exists");
        assert_eq!(data_point.trade_sequence.len(), trades.len());
        assert_eq!(data_point.trade_coverage, TradeCoverage::Complete);
        let total = data_point
            .footprint
            .trades
            .values()
            .fold(Qty::ZERO, |sum, group| sum + group.total_qty());
        assert_eq!(total, Qty::from_f64(5.0));
    }
}
