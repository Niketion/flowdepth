//! Bridge to the upstream indicator engine. The last bar is always a preview:
//! repeated live updates must not advance EMA/RSI state more than once per bar.
use super::KlineDataPoint;
use crate::chart::{BasisSeries, PlotData};
use exchange::Kline;
use indicator_engine::{
    ChartFrame, CompiledIndicator, FrameOutput, IndicatorConfig, IndicatorFrame, IndicatorInstance,
    InputValue, InstrumentContext, TimeOrTick,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{Arc, LazyLock},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptKind {
    Rsi,
    Atr,
    Macd,
    Adx,
}

static PROGRAMS: LazyLock<[Arc<CompiledIndicator>; 4]> = LazyLock::new(|| {
    [
        (
            "rsi.ind",
            include_str!("../../../indicators/sources/rsi.ind"),
        ),
        (
            "atr.ind",
            include_str!("../../../../indicators/sources/atr.ind"),
        ),
        (
            "macd.ind",
            include_str!("../../../../indicators/sources/macd.ind"),
        ),
        (
            "adx-dmi.ind",
            include_str!("../../../../indicators/sources/adx-dmi.ind"),
        ),
    ]
    .map(|(name, source)| {
        Arc::new(
            indicator_engine::compile(source, name).unwrap_or_else(|diagnostics| {
                panic!("Invalid bundled indicator {name}: {diagnostics:?}")
            }),
        )
    })
});

#[derive(Debug, Clone, Copy, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct ScriptedConfig {
    pub rsi_period: u16,
    pub atr_period: u16,
    pub adx_period: u16,
    pub macd_fast: u16,
    pub macd_slow: u16,
    pub macd_signal: u16,
}

impl Default for ScriptedConfig {
    fn default() -> Self {
        Self {
            rsi_period: 14,
            atr_period: 14,
            adx_period: 14,
            macd_fast: 12,
            macd_slow: 26,
            macd_signal: 9,
        }
    }
}

impl ScriptKind {
    fn program(self) -> Arc<CompiledIndicator> {
        Arc::clone(&PROGRAMS[self as usize])
    }

    fn config(self, config: ScriptedConfig) -> IndicatorConfig {
        let int = |n: u16| InputValue::Int(i64::from(n));
        match self {
            Self::Rsi => {
                IndicatorConfig::default().with("period", int(config.rsi_period.clamp(2, 200)))
            }
            Self::Atr => {
                IndicatorConfig::default().with("period", int(config.atr_period.clamp(1, 200)))
            }
            Self::Adx => {
                IndicatorConfig::default().with("period", int(config.adx_period.clamp(2, 200)))
            }
            Self::Macd => IndicatorConfig::default()
                .with("fast_period", int(config.macd_fast.clamp(1, 200)))
                .with("slow_period", int(config.macd_slow.clamp(2, 500)))
                .with("signal_period", int(config.macd_signal.clamp(1, 200))),
        }
    }
}

pub struct ScriptedSeries {
    kind: ScriptKind,
    config: ScriptedConfig,
    instance: IndicatorInstance,
    closed: Vec<(u64, Kline)>,
    basis: TimeOrTick,
    pub output: BasisSeries<FrameOutput>,
    pub error: Option<String>,
}

impl ScriptedSeries {
    pub fn plots(&self) -> &[indicator_engine::PlotDescriptor] {
        &PROGRAMS[self.kind as usize].descriptor.plots
    }
    pub fn new(kind: ScriptKind) -> Self {
        let config = ScriptedConfig::default();
        let instance = kind
            .program()
            .instantiate(kind.config(config))
            .expect("bundled indicator defaults are valid");
        Self {
            kind,
            config,
            instance,
            closed: Vec::new(),
            basis: TimeOrTick::Time,
            output: BasisSeries::default(),
            error: None,
        }
    }

    pub fn configure(&mut self, config: ScriptedConfig) {
        if self.config != config {
            self.config = config;
            self.reset();
        }
    }

    fn reset(&mut self) {
        self.instance
            .reset(self.kind.config(self.config))
            .expect("clamped bundled inputs are valid");
        self.closed.clear();
        self.output = match self.basis {
            TimeOrTick::Time => BasisSeries::Time(BTreeMap::new()),
            TimeOrTick::Tick => BasisSeries::Tick(BTreeMap::new()),
        };
        self.error = None;
    }

    pub fn sync(&mut self, source: &PlotData<KlineDataPoint>) {
        let (basis, bars): (_, Vec<_>) = match source {
            PlotData::TimeBased(series) => (
                TimeOrTick::Time,
                series
                    .datapoints
                    .iter()
                    .map(|(time, dp)| (time.as_u64(), dp.kline))
                    .collect(),
            ),
            PlotData::TickBased(series) => (
                TimeOrTick::Tick,
                series
                    .datapoints
                    .iter()
                    .enumerate()
                    .map(|(index, dp)| (index as u64, dp.kline))
                    .collect(),
            ),
        };
        self.sync_bars(basis, &bars);
    }

    fn sync_bars(&mut self, basis: TimeOrTick, bars: &[(u64, Kline)]) {
        let closed_len = bars.len().saturating_sub(1);
        // Rebuild after backfill, correction, pruning, basis changes or errors.
        if self.basis != basis
            || self.error.is_some()
            || self.closed.len() > closed_len
            || !self.closed.iter().zip(bars).all(|(old, new)| old == new)
        {
            self.basis = basis;
            self.reset();
        }
        if bars.is_empty() {
            self.reset();
            return;
        }
        for (index, &(key, bar)) in bars.iter().enumerate().skip(self.closed.len()) {
            let (buy_qty, sell_qty) = bar
                .volume
                .buy_sell()
                .map_or((0.0, 0.0), |(buy, sell)| (buy.to_f64(), sell.to_f64()));
            let frame = IndicatorFrame {
                index,
                time: i64::try_from(bar.time.as_u64()).unwrap_or(i64::MAX),
                basis,
                instrument: InstrumentContext::default(),
                chart: ChartFrame {
                    open: Some(bar.open.to_f64()),
                    high: Some(bar.high.to_f64()),
                    low: Some(bar.low.to_f64()),
                    close: Some(bar.close.to_f64()),
                    volume: Some(bar.volume.total().to_f64()),
                    directional_volume_available: bar.volume.buy_sell().is_some(),
                    buy_qty,
                    sell_qty,
                    trade: None,
                },
                open_interest: None,
                series_inputs: Arc::from([]),
            };
            let result = if index < closed_len {
                self.instance.evaluate_closed(frame)
            } else {
                self.instance.preview_open(frame)
            };
            match result {
                Ok(output) => {
                    match &mut self.output {
                        BasisSeries::Time(values) => {
                            values.insert(key.into(), output);
                        }
                        BasisSeries::Tick(values) => {
                            values.insert(key, output);
                        }
                    }
                    if index < closed_len {
                        self.closed.push((key, bar));
                    }
                }
                Err(error) => {
                    self.error = Some(error.to_string());
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use exchange::{Volume, unit::Qty};
    fn bars() -> Vec<(u64, Kline)> {
        (0..90)
            .map(|i| {
                let close = 100.0 + (i % 9) as f64 + i as f64 / 10.0;
                (
                    i * 60_000,
                    Kline::new(
                        i * 60_000,
                        close - 1.0,
                        close + 2.0,
                        close - 2.0,
                        close,
                        Volume::TotalOnly(Qty::from_f64(50.0)),
                        exchange::unit::MinTicksize::from(0.01),
                    ),
                )
            })
            .collect()
    }
    fn assert_matches_replay(series: &ScriptedSeries, bars: &[(u64, Kline)]) {
        let mut replay = ScriptedSeries::new(series.kind);
        replay.configure(series.config);
        replay.sync_bars(series.basis, bars);
        assert!(series.error.is_none(), "{:?}", series.error);
        match (&series.output, &replay.output) {
            (BasisSeries::Time(a), BasisSeries::Time(b)) => assert_eq!(a, b),
            (BasisSeries::Tick(a), BasisSeries::Tick(b)) => assert_eq!(a, b),
            _ => panic!("basis mismatch"),
        }
    }
    #[test]
    fn bundled_indicators_match_replay_after_live_updates_and_new_bars() {
        for kind in [
            ScriptKind::Rsi,
            ScriptKind::Atr,
            ScriptKind::Macd,
            ScriptKind::Adx,
        ] {
            let mut series = ScriptedSeries::new(kind);
            let mut bars = bars();
            series.sync_bars(TimeOrTick::Time, &bars[..89]);
            for _ in 0..3 {
                series.sync_bars(TimeOrTick::Time, &bars[..89]);
            }
            series.sync_bars(TimeOrTick::Time, &bars);
            bars[89].1.close = exchange::unit::Price::from_f64(102.0);
            series.sync_bars(TimeOrTick::Time, &bars);
            assert_matches_replay(&series, &bars);
            assert!(match &series.output {
                BasisSeries::Time(v) => v
                    .values()
                    .last()
                    .unwrap()
                    .plots
                    .iter()
                    .any(|p| p.value.as_float().is_some_and(f64::is_finite)),
                _ => false,
            });
        }
    }
    #[test]
    fn corrections_pruning_configuration_and_tick_basis_rebuild_state() {
        let mut series = ScriptedSeries::new(ScriptKind::Rsi);
        let mut bars = bars();
        series.sync_bars(TimeOrTick::Time, &bars);
        bars[30].1.close = exchange::unit::Price::from_f64(75.0);
        series.sync_bars(TimeOrTick::Time, &bars);
        assert_matches_replay(&series, &bars);
        series.sync_bars(TimeOrTick::Time, &bars[20..]);
        assert_matches_replay(&series, &bars[20..]);
        series.configure(ScriptedConfig {
            rsi_period: 7,
            ..Default::default()
        });
        series.sync_bars(TimeOrTick::Tick, &bars);
        assert_matches_replay(&series, &bars);
        series.sync_bars(TimeOrTick::Tick, &[]);
        assert!(matches!(&series.output, BasisSeries::Tick(v) if v.is_empty()));
    }
}
