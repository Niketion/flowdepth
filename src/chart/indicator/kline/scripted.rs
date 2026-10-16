use super::{AvailabilityCause, BasisSeriesExt, IndicatorAvailability, KlineIndicatorImpl};
use crate::chart::{
    Caches, Message, ViewState,
    indicator::{
        indicator_row,
        plot::{Plot, PlotTooltip, Series, TooltipFn, YScale},
    },
};
use data::chart::{
    PlotData,
    kline::{
        Config, KlineDataPoint,
        scripted::{ScriptKind, ScriptedSeries},
    },
};
use exchange::{Kline, Trade};
use iced::{
    Color, Point, Size, Theme,
    widget::canvas::{self, Path, Stroke},
};
use indicator_engine::{
    FrameOutput, PlotDescriptor, PlotScale, PlotStyle, PlotValueType, ReadoutBehavior,
};
use std::ops::RangeInclusive;

pub struct ScriptedIndicator {
    cache: Caches,
    series: ScriptedSeries,
}

impl ScriptedIndicator {
    pub fn new(kind: ScriptKind) -> Self {
        Self {
            cache: Caches::default(),
            series: ScriptedSeries::new(kind),
        }
    }
}

impl KlineIndicatorImpl for ScriptedIndicator {
    fn clear_all_caches(&mut self) {
        self.cache.clear_all();
    }
    fn clear_crosshair_caches(&mut self) {
        self.cache.clear_crosshair();
    }
    fn element<'a>(
        &'a self,
        chart: &'a ViewState,
        labels: bool,
        range: RangeInclusive<u64>,
    ) -> iced::Element<'a, Message> {
        if let Some(error) = &self.series.error {
            return iced::widget::center(iced::widget::text(format!(
                "Indicator calculation failed: {error}"
            )))
            .into();
        }
        indicator_row(
            chart,
            &self.cache,
            labels,
            ScriptPlot {
                plots: self.series.plots(),
            },
            self.series.output.as_plot_series(),
            range,
        )
    }
    fn availability(&self, _chart: &ViewState) -> IndicatorAvailability {
        if self.series.is_cvd() && !self.series.directional_volume_available {
            let empty = match &self.series.output {
                data::chart::BasisSeries::Time(v) => v.is_empty(),
                data::chart::BasisSeries::Tick(v) => v.is_empty(),
            };
            if empty {
                IndicatorAvailability::Unknown
            } else {
                IndicatorAvailability::Unavailable(AvailabilityCause::TradeData)
            }
        } else {
            IndicatorAvailability::Available
        }
    }
    fn unavailable_message(&self, chart: &ViewState, indicator: &str) -> Option<String> {
        self.series
            .error
            .as_ref()
            .map(|error| format!("Indicator calculation failed: {error}"))
            .or_else(|| self.availability(chart).unavailable_message(indicator))
    }
    fn rebuild_from_source(&mut self, source: &PlotData<KlineDataPoint>) {
        self.series.sync(source);
        self.clear_all_caches();
    }
    fn on_insert_klines(&mut self, _klines: &[Kline], source: &PlotData<KlineDataPoint>) {
        self.rebuild_from_source(source);
    }
    fn on_insert_trades(
        &mut self,
        _trades: &[Trade],
        _old_len: usize,
        source: &PlotData<KlineDataPoint>,
    ) {
        self.rebuild_from_source(source);
    }
    fn on_ticksize_change(&mut self, source: &PlotData<KlineDataPoint>) {
        self.rebuild_from_source(source);
    }
    fn on_basis_change(&mut self, source: &PlotData<KlineDataPoint>) {
        self.rebuild_from_source(source);
    }
    fn on_config_changed(&mut self, config: &Config) {
        self.series.configure(config.scripted);
        self.series.configure_cvd(config.cvd);
    }
}

struct ScriptPlot<'a> {
    plots: &'a [PlotDescriptor],
}

impl ScriptPlot<'_> {
    fn visible(&self) -> impl Iterator<Item = &PlotDescriptor> {
        self.plots.iter().filter(|plot| {
            plot.value_type == PlotValueType::Number && plot.readout != ReadoutBehavior::Hidden
        })
    }
    fn value(output: &FrameOutput, id: &str) -> Option<f32> {
        output
            .plots
            .iter()
            .find(|plot| plot.id == id)
            .and_then(|plot| plot.value.as_float())
            .map(|value| value as f32)
            .filter(|value| value.is_finite())
    }
    fn color(plot: &PlotDescriptor, theme: &Theme, index: usize) -> Color {
        let palette = theme.palette();
        match plot.color_slot.as_deref() {
            Some("plus" | "upper") => palette.success.base.color,
            Some("minus" | "lower") => palette.danger.base.color,
            Some("signal") => palette.warning.base.color,
            _ if index.is_multiple_of(2) => palette.primary.strong.color,
            _ => palette.secondary.strong.color,
        }
    }
}

fn plot_extents<'a, S: Series<Y = FrameOutput>>(
    plots: impl Iterator<Item = &'a PlotDescriptor>,
    series: &S,
    range: RangeInclusive<u64>,
) -> Option<(f32, f32)> {
    let plots: Vec<_> = plots.collect();
    let (mut min, mut max) = (f32::INFINITY, f32::NEG_INFINITY);
    series.for_each_in(range, |_, output| {
        for plot in &plots {
            if let Some(value) = ScriptPlot::value(output, &plot.id) {
                min = min.min(value);
                max = max.max(value);
            }
        }
    });
    if !min.is_finite() {
        return None;
    }
    if plots.iter().any(|plot| plot.style == PlotStyle::Histogram) {
        min = min.min(0.0);
        max = max.max(0.0);
    }
    let padding = ((max - min) * 0.08).max(0.01);
    Some((min - padding, max + padding))
}

impl<S: Series<Y = FrameOutput>> Plot<S> for ScriptPlot<'_> {
    fn y_extents(&self, series: &S, range: RangeInclusive<u64>) -> Option<(f32, f32)> {
        let has_primary = self.visible().any(|plot| plot.scale == PlotScale::Price);
        plot_extents(
            self.visible()
                .filter(|plot| !has_primary || plot.scale == PlotScale::Price),
            series,
            range,
        )
    }

    fn draw(
        &self,
        frame: &mut canvas::Frame,
        chart: &ViewState,
        theme: &Theme,
        series: &S,
        range: RangeInclusive<u64>,
        scale: &YScale,
    ) {
        // Draw bars first so optional independent-scale bars cannot cover a line.
        for (index, plot) in self
            .visible()
            .enumerate()
            .filter(|(_, p)| p.style == PlotStyle::Histogram)
            .chain(
                self.visible()
                    .enumerate()
                    .filter(|(_, p)| p.style != PlotStyle::Histogram),
            )
        {
            let independent;
            let scale = if plot.scale == PlotScale::Independent {
                let Some((min, max)) = plot_extents(std::iter::once(plot), series, range.clone())
                else {
                    continue;
                };
                independent = YScale {
                    min,
                    max,
                    px_height: scale.px_height,
                };
                &independent
            } else {
                scale
            };
            let color = Self::color(plot, theme, index);
            let mut previous = None;
            series.for_each_in(range.clone(), |key, output| {
                let Some(value) = Self::value(output, &plot.id) else {
                    previous = None;
                    return;
                };
                let point = Point::new(chart.interval_to_x(key), scale.to_y(value));
                if plot.style == PlotStyle::Histogram {
                    let zero = scale.to_y(0.0);
                    let width = (chart.cell_width * 0.8).max(1.0);
                    let color = if value >= 0.0 {
                        theme.palette().success.base.color
                    } else {
                        theme.palette().danger.base.color
                    };
                    frame.fill_rectangle(
                        Point::new(point.x - width / 2.0, point.y.min(zero)),
                        Size::new(width, (point.y - zero).abs().max(1.0)),
                        color,
                    );
                } else if let Some(previous) = previous {
                    frame.stroke(
                        &Path::line(previous, point),
                        Stroke::default().with_color(color).with_width(plot.width),
                    );
                }
                previous = Some(point);
            });
        }
    }
    fn tooltip_fn(&self) -> Option<&TooltipFn<FrameOutput>> {
        None
    }
    fn tooltip(
        &self,
        output: &FrameOutput,
        _next: Option<&FrameOutput>,
        _theme: &Theme,
    ) -> Option<PlotTooltip> {
        let text = self
            .visible()
            .filter_map(|plot| {
                output
                    .plots
                    .iter()
                    .find(|value| value.id == plot.id)
                    .and_then(|p| p.value.as_float())
                    .map(|value| {
                        format!("{}: {}", plot.label, data::util::format_with_commas(value))
                    })
            })
            .collect::<Vec<_>>()
            .join("\n");
        (!text.is_empty()).then(|| PlotTooltip::new(text))
    }
    fn is_point_valid(&self, output: &FrameOutput) -> bool {
        self.visible()
            .any(|plot| Self::value(output, &plot.id).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indicator_engine::{PlotOutput, Value};
    use std::collections::BTreeMap;
    #[test]
    fn independent_change_bars_do_not_flatten_the_cumulative_line() {
        let series = ScriptedSeries::new(ScriptKind::Cvd);
        let plot = ScriptPlot {
            plots: series.plots(),
        };
        let output = |value, change| FrameOutput {
            plots: vec![
                PlotOutput {
                    id: "value".into(),
                    value: Value::Float(value),
                    color: None,
                },
                PlotOutput {
                    id: "change".into(),
                    value: Value::Float(change),
                    color: None,
                },
            ]
            .into(),
            ..Default::default()
        };
        let values: BTreeMap<exchange::UnixMs, FrameOutput> = [
            (0.into(), output(1000.0, 5.0)),
            (1.into(), output(1003.0, -2.0)),
        ]
        .into_iter()
        .collect();
        let (min, max) = plot.y_extents(&&values, 0..=1).unwrap();
        assert!(min > 999.0 && max < 1004.0);
        let descriptor = series.plots().iter().find(|p| p.id == "change").unwrap();
        let (min, max) = plot_extents(std::iter::once(descriptor), &&values, 0..=1).unwrap();
        assert!(min < -2.0 && max > 5.0);
    }
}
