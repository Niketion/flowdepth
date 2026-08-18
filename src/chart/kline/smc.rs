// Smart Money Concepts (SMC) overlay — LuxAlgo spec translated to Rust.
//
// Detects swing pivots, BOS/CHoCH structure, order blocks, equal highs/lows,
// fair value gaps, premium/discount zones, and trailing strong/weak extremes.
//
// Renders directly on the main candle chart frame.

use data::chart::{
    PlotData,
    kline::{KlineDataPoint, SmcConfig},
};
use exchange::{UnixMs, unit::price::Price};

use iced::theme::Palette;
use iced::{
    Color, Point,
    widget::canvas::{self, LineDash, Path, Stroke},
};

// ── Constants ────────────────────────────────────────────────────────────────

const BULLISH: i8 = 1;
const BEARISH: i8 = -1;

// ── Types ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
struct SwingPoint {
    price: f64,
    time: UnixMs,
    bar_index: usize,
    last_price: f64,
}

#[derive(Debug, Clone, Copy)]
struct OrderBlock {
    high: f64,
    low: f64,
    time: UnixMs,
    bias: i8,
}

#[derive(Debug, Clone)]
struct SmcState {
    /// Swing high pivot
    swing_high: Option<SwingPoint>,
    /// Swing low pivot
    swing_low: Option<SwingPoint>,
    /// Current swing trend bias
    swing_trend: i8,
    /// Detected order blocks
    swing_order_blocks: Vec<OrderBlock>,
    /// Detected equal highs/lows
    equal_highs: Vec<(UnixMs, f64, f64)>,
    equal_lows: Vec<(UnixMs, f64, f64)>,
    /// Trailing extremes
    trailing_high: f64,
    trailing_low: f64,
    trailing_high_time: UnixMs,
    trailing_low_time: UnixMs,
    /// Leg detection state (0 = bearish, 1 = bullish)
    leg: usize,
}

impl Default for SmcState {
    fn default() -> Self {
        Self {
            swing_high: None,
            swing_low: None,
            swing_trend: 0,
            swing_order_blocks: Vec::new(),
            equal_highs: Vec::new(),
            equal_lows: Vec::new(),
            trailing_high: f64::MIN,
            trailing_low: f64::MAX,
            trailing_high_time: UnixMs::ZERO,
            trailing_low_time: UnixMs::ZERO,
            leg: 0,
        }
    }
}

// ── Data collection / state update ──────────────────────────────────────────

/// Collect OHLC data from the visible range into a time-ordered vec.
fn collect_klines(
    data_source: &PlotData<KlineDataPoint>,
    earliest: u64,
    latest: u64,
) -> Vec<(UnixMs, f64, f64, f64, f64)> {
    let mut result = Vec::new();
    let earliest_ms = UnixMs::new(earliest);
    let latest_ms = UnixMs::new(latest);

    match data_source {
        PlotData::TimeBased(timeseries) => {
            for (time, dp) in timeseries.datapoints.range(earliest_ms..=latest_ms) {
                result.push((
                    *time,
                    dp.kline.open.to_f64(),
                    dp.kline.high.to_f64(),
                    dp.kline.low.to_f64(),
                    dp.kline.close.to_f64(),
                ));
            }
        }
        PlotData::TickBased(_) => {}
    }
    result
}

/// Determine current leg direction at a given index using a lookback window.
fn detect_leg(klines: &[(UnixMs, f64, f64, f64, f64)], index: usize, size: usize) -> usize {
    if klines.len() < 2 || index < size {
        return 0;
    }
    let start = index.saturating_sub(size);
    let slice = &klines[start..=index];
    let highest = slice.iter().map(|k| k.2).fold(f64::NEG_INFINITY, f64::max);
    let lowest = slice.iter().map(|k| k.3).fold(f64::INFINITY, f64::min);

    let current_high = klines[index].2;
    let current_low = klines[index].3;

    if current_high >= highest {
        0 // bearish leg (pushing higher)
    } else if current_low <= lowest {
        1 // bullish leg (pushing lower)
    } else {
        0
    }
}

/// Process all klines in the visible range and build the SMC state.
fn build_smc_state(klines: &[(UnixMs, f64, f64, f64, f64)], config: &SmcConfig) -> SmcState {
    let mut state = SmcState::default();

    if klines.is_empty() {
        return state;
    }

    let swing_len = config.swing_length as usize;
    let atr = estimate_atr(klines) as f32;

    for i in 0..klines.len() {
        let (time, _open, high, low, close) = klines[i];

        // Update trailing extremes
        if state.trailing_high < high {
            state.trailing_high = high;
            state.trailing_high_time = time;
        }
        if state.trailing_low > low {
            state.trailing_low = low;
            state.trailing_low_time = time;
        }

        // ── Swing leg detection ──
        if i >= swing_len {
            let new_leg = detect_leg(klines, i, swing_len);

            if new_leg != state.leg {
                // Leg changed — we have a pivot
                let pivot_idx = i.saturating_sub(1);
                if pivot_idx < klines.len() {
                    if new_leg == 1 {
                        // Bearish → Bullish: we found a swing LOW at pivot_idx
                        let pivot_low = klines[pivot_idx].3;
                        if let Some(prev) = state.swing_low {
                            // Check equal lows
                            if config.show_equal_highs_lows
                                && (prev.price - pivot_low).abs()
                                    < (config.equal_highs_lows_threshold * atr) as f64
                                && pivot_idx >= config.equal_highs_lows_length as usize
                            {
                                state.equal_lows.push((prev.time, prev.price, pivot_low));
                            }

                            state.swing_low = Some(SwingPoint {
                                price: pivot_low,
                                time: klines[pivot_idx].0,
                                bar_index: pivot_idx,
                                last_price: prev.price,
                            });
                        } else {
                            state.swing_low = Some(SwingPoint {
                                price: pivot_low,
                                time: klines[pivot_idx].0,
                                bar_index: pivot_idx,
                                last_price: pivot_low,
                            });
                        }
                        state.trailing_low = klines[pivot_idx].3;
                        state.trailing_low_time = klines[pivot_idx].0;
                    } else {
                        // Bullish → Bearish: we found a swing HIGH at pivot_idx
                        let pivot_high = klines[pivot_idx].2;
                        if let Some(prev) = state.swing_high {
                            // Check equal highs
                            if config.show_equal_highs_lows
                                && (prev.price - pivot_high).abs()
                                    < (config.equal_highs_lows_threshold * atr) as f64
                                && pivot_idx >= config.equal_highs_lows_length as usize
                            {
                                state.equal_highs.push((prev.time, prev.price, pivot_high));
                            }

                            state.swing_high = Some(SwingPoint {
                                price: pivot_high,
                                time: klines[pivot_idx].0,
                                bar_index: pivot_idx,
                                last_price: prev.price,
                            });
                        } else {
                            state.swing_high = Some(SwingPoint {
                                price: pivot_high,
                                time: klines[pivot_idx].0,
                                bar_index: pivot_idx,
                                last_price: pivot_high,
                            });
                        }
                        state.trailing_high = klines[pivot_idx].2;
                        state.trailing_high_time = klines[pivot_idx].0;
                    }
                }

                // Trigger order block detection at pivot point
                if new_leg == 1 {
                    // Bullish pivot: order block is the last bearish candle before the move up
                    if i >= 2 {
                        let prev_bar = &klines[i - 2];
                        let ob = OrderBlock {
                            high: prev_bar.2,
                            low: prev_bar.3,
                            time: prev_bar.0,
                            bias: BULLISH,
                        };
                        state.swing_order_blocks.insert(0, ob);
                        if state.swing_order_blocks.len() > config.order_blocks_count as usize {
                            state.swing_order_blocks.pop();
                        }
                    }
                } else {
                    // Bearish pivot: order block is the last bullish candle before the move down
                    if i >= 2 {
                        let prev_bar = &klines[i - 2];
                        let ob = OrderBlock {
                            high: prev_bar.2,
                            low: prev_bar.3,
                            time: prev_bar.0,
                            bias: BEARISH,
                        };
                        state.swing_order_blocks.insert(0, ob);
                        if state.swing_order_blocks.len() > config.order_blocks_count as usize {
                            state.swing_order_blocks.pop();
                        }
                    }
                }

                state.leg = new_leg;
            }
        }

        // ── Structure breaks (BOS/CHoCH) detection ──
        if let Some(high) = state.swing_high
            && close > high.price
            && state.swing_trend == BEARISH
        {
            state.swing_trend = BULLISH;
        }
        if let Some(low) = state.swing_low
            && close < low.price
            && state.swing_trend == BULLISH
        {
            state.swing_trend = BEARISH;
        }
    }

    state
}

/// Simple ATR estimate from visible klines.
fn estimate_atr(klines: &[(UnixMs, f64, f64, f64, f64)]) -> f64 {
    if klines.len() < 2 {
        return 0.0;
    }
    let mut sum = 0.0;
    let n = (klines.len() - 1).min(200);
    for i in 1..=n {
        let high = klines[i].2;
        let low = klines[i].3;
        let prev_close = klines[i - 1].4;
        let tr = (high - low)
            .max((high - prev_close).abs())
            .max((low - prev_close).abs());
        sum += tr;
    }
    sum / n as f64
}

// ── Rendering ────────────────────────────────────────────────────────────────

/// Main entry point: draw all SMC elements onto the chart frame.
#[allow(clippy::too_many_arguments)]
pub fn draw_smc_overlay(
    data_source: &PlotData<KlineDataPoint>,
    frame: &mut canvas::Frame,
    earliest: u64,
    latest: u64,
    interval_to_x: impl Fn(u64) -> f32,
    price_to_y: impl Fn(Price) -> f32,
    config: &SmcConfig,
    palette: &Palette,
) {
    let klines = collect_klines(data_source, earliest, latest);
    if klines.is_empty() {
        return;
    }

    let state = build_smc_state(&klines, config);
    let last_bar_time = if let Some((t, ..)) = klines.last() {
        *t
    } else {
        return;
    };

    // ── Colors ──
    let bullish_color = palette.success.strong.color;
    let bearish_color = palette.danger.strong.color;
    let neutral_color = palette.secondary.strong.color;
    let ob_bullish_color = Color::from_rgba(0.19, 0.35, 0.96, 0.20);
    let ob_bearish_color = Color::from_rgba(0.95, 0.49, 0.50, 0.20);
    let fvg_bullish_color = Color::from_rgba(0.00, 0.96, 0.41, 0.25);
    let fvg_bearish_color = Color::from_rgba(1.00, 0.00, 0.05, 0.25);
    let eq_color = Color::from_rgba(0.53, 0.54, 0.57, 0.8);

    let last_bar_x = interval_to_x(last_bar_time.as_u64());

    // ── 1. Premium / Discount Zones ──
    if config.show_premium_discount_zones && state.trailing_high > state.trailing_low {
        let equilibrium = (state.trailing_high + state.trailing_low) * 0.5;
        let first_bar_x = interval_to_x(klines.first().unwrap().0.as_u64());

        // Premium zone (top half)
        let pre_top_y = price_to_y(Price::from_f64(state.trailing_high));
        let pre_bot_y = price_to_y(Price::from_f64(equilibrium));
        frame.fill_rectangle(
            Point::new(first_bar_x, pre_top_y),
            iced::Size::new(last_bar_x - first_bar_x, pre_bot_y - pre_top_y),
            Color::from_rgba(bearish_color.r, bearish_color.g, bearish_color.b, 0.08),
        );

        // Discount zone (bottom half)
        let dis_top_y = price_to_y(Price::from_f64(equilibrium));
        let dis_bot_y = price_to_y(Price::from_f64(state.trailing_low));
        frame.fill_rectangle(
            Point::new(first_bar_x, dis_top_y),
            iced::Size::new(last_bar_x - first_bar_x, dis_bot_y - dis_top_y),
            Color::from_rgba(bullish_color.r, bullish_color.g, bullish_color.b, 0.08),
        );

        // Equilibrium line
        let eq_y = price_to_y(Price::from_f64(equilibrium));
        frame.stroke(
            &Path::line(Point::new(first_bar_x, eq_y), Point::new(last_bar_x, eq_y)),
            Stroke::default()
                .with_color(Color::from_rgba(
                    neutral_color.r,
                    neutral_color.g,
                    neutral_color.b,
                    0.5,
                ))
                .with_width(1.0),
        );
    }

    // ── 2. Trailing Strong/Weak High/Low ──
    if config.show_strong_weak_high_low && state.trailing_high > state.trailing_low {
        // Trailing high line
        let high_y = price_to_y(Price::from_f64(state.trailing_high));
        frame.stroke(
            &Path::line(
                Point::new(interval_to_x(state.trailing_high_time.as_u64()), high_y),
                Point::new(last_bar_x + 30.0, high_y),
            ),
            Stroke::default().with_color(bearish_color).with_width(1.0),
        );

        // Trailing low line
        let low_y = price_to_y(Price::from_f64(state.trailing_low));
        frame.stroke(
            &Path::line(
                Point::new(interval_to_x(state.trailing_low_time.as_u64()), low_y),
                Point::new(last_bar_x + 30.0, low_y),
            ),
            Stroke::default().with_color(bullish_color).with_width(1.0),
        );
    }

    // ── 3. Swing Order Blocks ──
    if config.show_swing_order_blocks {
        for ob in &state.swing_order_blocks {
            let ob_color = if ob.bias == BULLISH {
                ob_bullish_color
            } else {
                ob_bearish_color
            };
            let top_y = price_to_y(Price::from_f64(ob.high));
            let bottom_y = price_to_y(Price::from_f64(ob.low));
            let x = interval_to_x(ob.time.as_u64());

            frame.fill_rectangle(
                Point::new(x, top_y.min(bottom_y)),
                iced::Size::new(last_bar_x - x, (top_y - bottom_y).abs()),
                ob_color,
            );
        }
    }

    // ── 4. Swing Structure (pivot lines) ──
    if config.show_swing_structure {
        // Line from last swing high
        if let Some(high) = state.swing_high {
            let y = price_to_y(Price::from_f64(high.price));
            frame.stroke(
                &Path::line(
                    Point::new(interval_to_x(high.time.as_u64()), y),
                    Point::new(last_bar_x, y),
                ),
                Stroke::default().with_color(bearish_color).with_width(1.0),
            );
        }

        // Line from last swing low
        if let Some(low) = state.swing_low {
            let y = price_to_y(Price::from_f64(low.price));
            frame.stroke(
                &Path::line(
                    Point::new(interval_to_x(low.time.as_u64()), y),
                    Point::new(last_bar_x, y),
                ),
                Stroke::default().with_color(bullish_color).with_width(1.0),
            );
        }
    }

    // ── 5. Equal Highs/Lows ──
    if config.show_equal_highs_lows {
        for (time, price1, _price2) in &state.equal_highs {
            let y = price_to_y(Price::from_f64(*price1));
            frame.stroke(
                &Path::line(
                    Point::new(interval_to_x(time.as_u64()), y),
                    Point::new(last_bar_x, y),
                ),
                Stroke::with_color(
                    Stroke {
                        width: 1.0,
                        line_dash: LineDash {
                            segments: &[4.0, 4.0],
                            offset: 0,
                        },
                        ..Stroke::default()
                    },
                    eq_color,
                ),
            );
        }
        for (time, price1, _price2) in &state.equal_lows {
            let y = price_to_y(Price::from_f64(*price1));
            frame.stroke(
                &Path::line(
                    Point::new(interval_to_x(time.as_u64()), y),
                    Point::new(last_bar_x, y),
                ),
                Stroke::with_color(
                    Stroke {
                        width: 1.0,
                        line_dash: LineDash {
                            segments: &[4.0, 4.0],
                            offset: 0,
                        },
                        ..Stroke::default()
                    },
                    eq_color,
                ),
            );
        }
    }

    // ── 6. Fair Value Gaps ──
    if config.show_fair_value_gaps {
        for i in 2..klines.len() {
            // Bullish FVG: current low > previous (2-bars-ago) high
            let prev_high = klines[i.saturating_sub(2)].2;
            let curr_low = klines[i].3;
            if curr_low > prev_high {
                let top_y = price_to_y(Price::from_f64(curr_low));
                let bottom_y = price_to_y(Price::from_f64(prev_high));
                let x_left = interval_to_x(klines[i.saturating_sub(2)].0.as_u64());
                let x_right = interval_to_x(klines[i].0.as_u64());
                frame.fill_rectangle(
                    Point::new(x_left, top_y.min(bottom_y)),
                    iced::Size::new(x_right - x_left, (top_y - bottom_y).abs()),
                    fvg_bullish_color,
                );
            }

            // Bearish FVG: current high < previous (2-bars-ago) low
            let curr_high = klines[i].2;
            let prev2_low = klines[i.saturating_sub(2)].3;
            if curr_high < prev2_low {
                let top_y = price_to_y(Price::from_f64(prev2_low));
                let bottom_y = price_to_y(Price::from_f64(curr_high));
                let x_left = interval_to_x(klines[i.saturating_sub(2)].0.as_u64());
                let x_right = interval_to_x(klines[i].0.as_u64());
                frame.fill_rectangle(
                    Point::new(x_left, top_y.min(bottom_y)),
                    iced::Size::new(x_right - x_left, (top_y - bottom_y).abs()),
                    fvg_bearish_color,
                );
            }
        }
    }
}
