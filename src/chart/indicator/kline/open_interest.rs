use crate::chart::{
    Basis, Caches, Message, ViewState,
    indicator::{
        indicator_row,
        kline::{AvailabilityCause, FetchCtx, IndicatorAvailability, KlineIndicatorImpl},
        plot::{
            AnySeries, PlotTooltip,
            line::{LineInterpolation, LinePlot},
        },
    },
};
use crate::connector::fetcher::FetchRange;

use data::chart::{PlotData, kline::KlineDataPoint};
use data::util::format_with_commas;
use exchange::adapter::Exchange;
use exchange::{Kline, Timeframe, Trade, UnixMs};

use iced::widget::{center, column, row, text};
use std::{collections::BTreeMap, ops::RangeInclusive};

pub struct OpenInterestIndicator {
    cache: Caches,
    pub data: BTreeMap<UnixMs, f64>,
    source_timeframe: Option<Timeframe>,
}

impl OpenInterestIndicator {
    pub fn new() -> Self {
        Self {
            cache: Caches::default(),
            data: BTreeMap::new(),
            source_timeframe: None,
        }
    }

    fn indicator_elem<'a>(
        &'a self,
        main_chart: &'a ViewState,
        data_labels_always_visible: bool,
        visible_range: RangeInclusive<u64>,
    ) -> iced::Element<'a, Message> {
        if let Some(message) = self.unavailable_message(main_chart, "Open Interest") {
            return center(text(message)).into();
        }

        let (earliest, latest) = visible_range.clone().into_inner();
        if latest < earliest {
            return row![].into();
        }

        let source = self.source_timeframe;
        let tooltip = move |value: &f64, next: Option<&f64>| {
            let value_text = format!("Open Interest: {}", format_with_commas(*value));
            let change_text = if let Some(next_value) = next {
                let delta = next_value - *value;
                let sign = if delta >= 0.0 { "+" } else { "" };
                format!("Change: {}{}", sign, format_with_commas(delta))
            } else {
                "Change: N/A".to_string()
            };
            let source_text =
                source.map_or(String::new(), |timeframe| format!("\nSource: {timeframe}"));
            PlotTooltip::new(format!("{value_text}\n{change_text}{source_text}"))
        };

        let value_fn = |v: &f64| *v as f32;

        let plot = LinePlot::new(value_fn)
            .stroke_width(1.0)
            .show_points(true)
            .point_radius_factor(0.2)
            .interpolation(LineInterpolation::StepAfter)
            .padding(0.08)
            .with_tooltip(tooltip);

        let plot = indicator_row(
            main_chart,
            &self.cache,
            data_labels_always_visible,
            plot,
            AnySeries::forward_unix_ms(&self.data),
            visible_range,
        );
        let label = self.source_timeframe.map_or_else(
            || "Open Interest".to_string(),
            |timeframe| format!("Open Interest source {timeframe}"),
        );
        column![text(label).size(crate::style::text_size::TINY), plot].into()
    }

    // helper to compute (earliest, latest) present OI keys
    fn oi_timerange(&self, latest_kline: UnixMs) -> (UnixMs, UnixMs) {
        let mut from_time = latest_kline;
        let mut to_time = UnixMs::ZERO;

        self.data.iter().for_each(|(time, _)| {
            from_time = from_time.min(*time);
            to_time = to_time.max(*time);
        });
        (from_time, to_time)
    }

    fn is_supported_exchange(exchange: Exchange) -> bool {
        exchange.is_perps()
            && exchange != Exchange::HyperliquidLinear
            && exchange != Exchange::MexcLinear
            && exchange != Exchange::MexcInverse
    }

    pub(crate) fn source_timeframe(timeframe: Timeframe) -> Option<Timeframe> {
        match timeframe {
            Timeframe::M1 | Timeframe::M3 | Timeframe::M5 => Some(Timeframe::M5),
            Timeframe::M15 => Some(Timeframe::M15),
            Timeframe::M30 => Some(Timeframe::M30),
            Timeframe::H1 => Some(Timeframe::H1),
            Timeframe::H4 => Some(Timeframe::H4),
            _ => None,
        }
    }

    fn availability_for(basis: Basis, exchange: Exchange) -> IndicatorAvailability {
        match basis {
            Basis::Tick(_) => IndicatorAvailability::Unavailable(AvailabilityCause::Basis(basis)),
            Basis::Time(timeframe) => {
                if !Self::is_supported_exchange(exchange) {
                    IndicatorAvailability::Unavailable(AvailabilityCause::Exchange(exchange))
                } else if Self::source_timeframe(timeframe).is_none() {
                    IndicatorAvailability::Unavailable(AvailabilityCause::Timeframe(timeframe))
                } else {
                    IndicatorAvailability::Available
                }
            }
        }
    }
}

impl KlineIndicatorImpl for OpenInterestIndicator {
    fn clear_all_caches(&mut self) {
        self.cache.clear_all();
    }

    fn clear_crosshair_caches(&mut self) {
        self.cache.clear_crosshair();
    }

    fn element<'a>(
        &'a self,
        chart: &'a ViewState,
        data_labels_always_visible: bool,
        visible_range: RangeInclusive<u64>,
    ) -> iced::Element<'a, Message> {
        self.indicator_elem(chart, data_labels_always_visible, visible_range)
    }

    fn availability(&self, chart: &ViewState) -> IndicatorAvailability {
        Self::availability_for(chart.basis, chart.ticker_info.exchange())
    }

    fn fetch_range(&mut self, ctx: &FetchCtx) -> Option<FetchRange> {
        let availability = Self::availability_for(
            Basis::Time(ctx.timeframe),
            ctx.main_chart.ticker_info.exchange(),
        );
        if !matches!(availability, IndicatorAvailability::Available) {
            return None;
        }
        let source_timeframe = Self::source_timeframe(ctx.timeframe)?;
        if self.source_timeframe != Some(source_timeframe) {
            self.source_timeframe = Some(source_timeframe);
            self.data.clear();
        }

        let (oi_earliest, oi_latest) = self.oi_timerange(ctx.kline_latest);

        if ctx.visible_earliest < oi_earliest {
            return Some(FetchRange::OpenInterest {
                from: ctx.prefetch_earliest,
                to: oi_earliest,
                timeframe: source_timeframe,
            });
        }

        if oi_latest.saturating_add(source_timeframe.to_milliseconds()) <= ctx.kline_latest {
            return Some(FetchRange::OpenInterest {
                from: oi_latest.max(ctx.prefetch_earliest),
                to: ctx.kline_latest,
                timeframe: source_timeframe,
            });
        }

        None
    }

    fn rebuild_from_source(&mut self, _source: &PlotData<KlineDataPoint>) {
        // OI comes from network via external fetches(trade-fetch alike)
        self.clear_all_caches();
    }

    fn on_insert_klines(&mut self, _klines: &[Kline], _source: &PlotData<KlineDataPoint>) {}

    fn on_insert_trades(
        &mut self,
        _trades: &[Trade],
        _old_dp_len: usize,
        _source: &PlotData<KlineDataPoint>,
    ) {
    }

    fn on_ticksize_change(&mut self, _source: &PlotData<KlineDataPoint>) {}

    fn on_basis_change(&mut self, _source: &PlotData<KlineDataPoint>) {}

    fn on_open_interest(&mut self, data: &[exchange::OpenInterest]) {
        self.data.extend(data.iter().map(|oi| (oi.time, oi.value)));
        self.clear_all_caches();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_chart_timeframes_to_supported_oi_sources() {
        assert_eq!(
            OpenInterestIndicator::source_timeframe(Timeframe::M1),
            Some(Timeframe::M5)
        );
        assert_eq!(
            OpenInterestIndicator::source_timeframe(Timeframe::M3),
            Some(Timeframe::M5)
        );
        assert_eq!(
            OpenInterestIndicator::source_timeframe(Timeframe::M5),
            Some(Timeframe::M5)
        );
        assert_eq!(OpenInterestIndicator::source_timeframe(Timeframe::H2), None);
    }

    #[test]
    fn m5_timestamp_is_not_shifted_on_m1_chart() {
        let mut indicator = OpenInterestIndicator::new();
        let timestamp = UnixMs::new(300_000);
        indicator.on_open_interest(&[exchange::OpenInterest {
            time: timestamp,
            value: 42.0,
        }]);
        assert_eq!(indicator.data.get(&timestamp), Some(&42.0));
    }
}
