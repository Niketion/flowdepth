use data::chart::{
    PlotData,
    kline::{
        KlineDataPoint,
        tpo::{
            TpoConfig, TpoProfile, TpoProfileSettings, TpoProfileStatus,
            build_tpo_profile_with_source_timeframe,
        },
    },
};
use exchange::{
    UnixMs,
    unit::{Price, PriceStep},
};
use iced::{
    Point, Size,
    theme::Palette,
    widget::canvas::{self, Frame},
};

#[allow(clippy::too_many_arguments)]
pub(super) fn draw(
    source: &PlotData<KlineDataPoint>,
    frame: &mut Frame,
    earliest: u64,
    latest: u64,
    x: impl Fn(u64) -> f32,
    y: impl Fn(Price) -> f32,
    step: PriceStep,
    scaling: f32,
    config: &TpoConfig,
    palette: &Palette,
) {
    let PlotData::TimeBased(series) = source else {
        return;
    };
    // A candle spanning several letters cannot establish individual period ranges.
    if series.interval.to_milliseconds() > config.letter_period.to_milliseconds() {
        return;
    }
    let anchor = config.anchor;
    let end = anchor.offset(anchor.floor(UnixMs::new(latest)), 1);
    let mut start = anchor
        .floor(UnixMs::new(earliest))
        .max(anchor.offset(end, -32));
    let now = UnixMs::now();
    while start < end {
        let next = anchor.offset(start, 1);
        if next <= start {
            break;
        }
        let bars: Vec<_> = series
            .datapoints
            .range(start..next)
            .map(|(_, dp)| dp.kline)
            .collect();
        let (Some(low), Some(high)) = (
            bars.iter().map(|bar| bar.low).min(),
            bars.iter().map(|bar| bar.high).max(),
        ) else {
            start = next;
            continue;
        };
        // Bound both calculation and drawing work at the screen's useful resolution.
        let Some(price_step) = step.coarsened_to_fit(low, high, 256) else {
            start = next;
            continue;
        };
        let settings = TpoProfileSettings {
            anchor,
            letter_period: config.letter_period,
            price_step,
            value_area_percent: config.value_area_percent.clamp(5, 100),
        };
        if let Ok(profile) = build_tpo_profile_with_source_timeframe(
            &bars,
            start,
            next,
            settings,
            series.interval,
            now,
        ) {
            let left = x(start.as_u64()).max(x(earliest));
            let width = ((x(next.as_u64()) - left).abs() * 0.75).min(180.0 / scaling);
            let max_count = profile
                .rows
                .iter()
                .map(|row| row.count())
                .max()
                .unwrap_or(1)
                .max(1) as f32;
            let row_height = (y(low) - y(low.checked_add_steps(1, price_step).unwrap_or(low)))
                .abs()
                .max(1.0 / scaling);
            let text_size = (row_height * 0.85).min(11.0 / scaling);
            for row in &profile.rows {
                if row.periods.is_empty() {
                    continue;
                }
                let top = y(row.price) - row_height / 2.0;
                let in_area = profile
                    .val
                    .zip(profile.vah)
                    .is_some_and(|(val, vah)| row.price >= val && row.price <= vah);
                let color = if profile.poc == Some(row.price) {
                    palette.warning.base.color
                } else if in_area {
                    palette.primary.strong.color
                } else {
                    palette.secondary.base.color
                };
                frame.fill_rectangle(
                    Point::new(left, top),
                    Size::new(width * row.count() as f32 / max_count, row_height * 0.9),
                    color.scale_alpha(0.28),
                );
                if config.show_letters && text_size * scaling >= 7.0 {
                    // Fit the letters inside the session; never let a long profile cover adjacent panes.
                    let max_letters = (width / (text_size * 0.65)).floor().max(0.0) as usize;
                    let text = row
                        .periods
                        .iter()
                        .take(max_letters)
                        .map(|period| TpoProfile::letter_label(usize::from(*period)))
                        .collect::<String>();
                    frame.fill_text(canvas::Text {
                        content: text,
                        position: Point::new(left, top),
                        color,
                        size: text_size.into(),
                        ..Default::default()
                    });
                }
            }
            let status = match profile.status {
                TpoProfileStatus::Complete => "TPO",
                TpoProfileStatus::Developing => "TPO · developing",
                TpoProfileStatus::Incomplete => "TPO · partial",
            };
            frame.fill_text(canvas::Text {
                content: status.into(),
                position: Point::new(left, y(high) - 14.0 / scaling),
                color: palette.background.base.text,
                size: (10.0 / scaling).into(),
                ..Default::default()
            });
        }
        start = next;
    }
}
