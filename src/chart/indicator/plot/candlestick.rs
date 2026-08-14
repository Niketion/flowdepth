use std::ops::RangeInclusive;

use iced::{
    Point, Size, Theme,
    widget::canvas::{self, Path, Stroke},
};

use crate::chart::{
    ViewState,
    indicator::plot::{Plot, PlotTooltip, Series, TooltipFn, YScale},
};

/// Numeric OHLC contract used by indicator candlesticks.
pub trait CandlestickValue {
    fn open(&self) -> f32;
    fn high(&self) -> f32;
    fn low(&self) -> f32;
    fn close(&self) -> f32;
}

type ValidFn<T> = Box<dyn Fn(&T) -> bool>;

pub struct CandlestickPlot<T> {
    body_width_factor: f32,
    padding: f32,
    show_wicks: bool,
    tooltip: Option<TooltipFn<T>>,
    is_valid: Option<ValidFn<T>>,
    invalid_point_message: Option<String>,
}

impl<T> CandlestickPlot<T> {
    pub fn new() -> Self {
        Self {
            body_width_factor: 0.7,
            padding: 0.08,
            show_wicks: true,
            tooltip: None,
            is_valid: None,
            invalid_point_message: None,
        }
    }

    pub fn body_width_factor(mut self, factor: f32) -> Self {
        self.body_width_factor = factor.clamp(0.1, 1.0);
        self
    }

    pub fn padding(mut self, padding: f32) -> Self {
        self.padding = padding.max(0.0);
        self
    }

    pub fn show_wicks(mut self, show: bool) -> Self {
        self.show_wicks = show;
        self
    }

    pub fn valid_when<F>(mut self, predicate: F) -> Self
    where
        F: Fn(&T) -> bool + 'static,
    {
        self.is_valid = Some(Box::new(predicate));
        self
    }

    pub fn invalid_point_message(mut self, message: impl Into<String>) -> Self {
        self.invalid_point_message = Some(message.into());
        self
    }

    pub fn with_tooltip<F>(mut self, tooltip: F) -> Self
    where
        F: Fn(&T, Option<&T>) -> PlotTooltip + 'static,
    {
        self.tooltip = Some(Box::new(tooltip));
        self
    }
}

impl<S> Plot<S> for CandlestickPlot<S::Y>
where
    S: Series,
    S::Y: CandlestickValue,
{
    fn y_extents(&self, series: &S, range: RangeInclusive<u64>) -> Option<(f32, f32)> {
        let mut low = f32::MAX;
        let mut high = f32::MIN;
        series.for_each_in(range, |_, point| {
            if self
                .is_valid
                .as_ref()
                .is_none_or(|is_valid| is_valid(point))
            {
                low = low.min(point.low());
                high = high.max(point.high());
            }
        });
        (low != f32::MAX).then_some((low, high))
    }

    fn adjust_extents(&self, min: f32, max: f32) -> (f32, f32) {
        if max > min {
            let pad = (max - min) * self.padding;
            (min - pad, max + pad)
        } else {
            (min - 1.0, max + 1.0)
        }
    }

    fn draw(
        &self,
        frame: &mut canvas::Frame,
        ctx: &ViewState,
        theme: &Theme,
        series: &S,
        range: RangeInclusive<u64>,
        scale: &YScale,
    ) {
        let palette = theme.palette();
        let body_width = ctx.cell_width * self.body_width_factor;

        series.for_each_in(range, |x, point| {
            if !self
                .is_valid
                .as_ref()
                .is_none_or(|is_valid| is_valid(point))
            {
                return;
            }

            let center_x = ctx.interval_to_x(x);
            let open_y = scale.to_y(point.open());
            let high_y = scale.to_y(point.high());
            let low_y = scale.to_y(point.low());
            let close_y = scale.to_y(point.close());
            let color = if point.close() > point.open() {
                palette.success.strong.color
            } else if point.close() < point.open() {
                palette.danger.strong.color
            } else {
                palette.secondary.strong.color
            };

            if self.show_wicks {
                frame.stroke(
                    &Path::line(Point::new(center_x, high_y), Point::new(center_x, low_y)),
                    Stroke::default().with_color(color).with_width(1.0),
                );
            }

            let top = open_y.min(close_y);
            let height = (open_y - close_y).abs().max(1.0);
            frame.fill_rectangle(
                Point::new(center_x - body_width / 2.0, top),
                Size::new(body_width.max(0.5), height),
                color,
            );
        });
    }

    fn tooltip_fn(&self) -> Option<&TooltipFn<S::Y>> {
        self.tooltip.as_ref()
    }

    fn is_point_valid(&self, point: &S::Y) -> bool {
        self.is_valid
            .as_ref()
            .is_none_or(|is_valid| is_valid(point))
    }

    fn invalid_point_message(&self) -> Option<&str> {
        self.invalid_point_message.as_deref()
    }
}
