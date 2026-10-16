use super::config_message;
use crate::screen::dashboard::pane::Message;
use crate::widget::{labeled_slider, pick_list};
use data::chart::kline::{
    BubbleColorMode, BubbleLabelMode, BubbleThresholdMode, Config as KlineConfig,
    VolumeBubblePreset, VolumeBubbleSession,
};
use data::util::format_with_commas;
use iced::{
    Element,
    widget::{checkbox, column, pane_grid, text},
};

pub(super) fn view<'a>(
    pane: pane_grid::Pane,
    cfg: KlineConfig,
    bubble_scale: crate::chart::kline::VolumeBubbleQtyScale,
    advanced: bool,
) -> Element<'a, Message> {
    let mut sections = column![];

    let bubbles = cfg.volume_bubbles;
    let preset = pick_list(
        VolumeBubblePreset::ALL,
        Some(bubbles.preset),
        move |preset| {
            let mut volume_bubbles = data::chart::kline::VolumeBubbleConfig::for_preset(preset);
            volume_bubbles.enabled = bubbles.enabled;
            volume_bubbles.three_dimensional = bubbles.three_dimensional;
            config_message(
                pane,
                KlineConfig {
                    volume_bubbles,
                    ..cfg
                },
            )
        },
    );
    let threshold_mode = pick_list(
        BubbleThresholdMode::ALL,
        Some(bubbles.threshold_mode),
        move |threshold_mode| {
            config_message(
                pane,
                KlineConfig {
                    volume_bubbles: data::chart::kline::VolumeBubbleConfig {
                        threshold_mode,
                        ..bubbles
                    }
                    .customized(),
                    ..cfg
                },
            )
        },
    );
    let session = pick_list(
        VolumeBubbleSession::ALL,
        Some(bubbles.session),
        move |session| {
            config_message(
                pane,
                KlineConfig {
                    volume_bubbles: data::chart::kline::VolumeBubbleConfig { session, ..bubbles },
                    ..cfg
                },
            )
        },
    );
    let mode = pick_list(
        BubbleColorMode::ALL,
        Some(bubbles.color_mode),
        move |color_mode| {
            config_message(
                pane,
                KlineConfig {
                    volume_bubbles: data::chart::kline::VolumeBubbleConfig {
                        color_mode,
                        ..bubbles
                    },
                    ..cfg
                },
            )
        },
    );
    let count = labeled_slider(
        "Max / candle",
        1.0..=10.0,
        bubbles.max_bubbles_per_bar as f32,
        move |v| {
            config_message(
                pane,
                KlineConfig {
                    volume_bubbles: data::chart::kline::VolumeBubbleConfig {
                        max_bubbles_per_bar: v as usize,
                        ..bubbles
                    },
                    ..cfg
                },
            )
        },
        |v| format!("{v:.0}"),
        Some(1.0),
    );
    let viewport_count = labeled_slider(
        "Max / viewport",
        5.0..=100.0,
        bubbles.max_bubbles_in_view as f32,
        move |v| {
            config_message(
                pane,
                KlineConfig {
                    volume_bubbles: data::chart::kline::VolumeBubbleConfig {
                        max_bubbles_in_view: v as usize,
                        ..bubbles
                    }
                    .customized(),
                    ..cfg
                },
            )
        },
        |v| format!("{v:.0}"),
        Some(1.0),
    );
    let cluster_time = labeled_slider(
        "Cluster time",
        100.0..=1_500.0,
        bubbles.cluster_window_ms as f32,
        move |v| {
            config_message(
                pane,
                KlineConfig {
                    volume_bubbles: data::chart::kline::VolumeBubbleConfig {
                        cluster_window_ms: v as u32,
                        ..bubbles
                    }
                    .customized(),
                    ..cfg
                },
            )
        },
        |v| format!("{v:.0}ms"),
        Some(50.0),
    );
    let display_percentile = labeled_slider(
        "Display percentile",
        80.0..=99.9,
        bubbles.display_percentile,
        move |v| {
            config_message(
                pane,
                KlineConfig {
                    volume_bubbles: data::chart::kline::VolumeBubbleConfig {
                        display_percentile: v,
                        ..bubbles
                    }
                    .customized(),
                    ..cfg
                },
            )
        },
        |v| format!("{v:.1}"),
        Some(0.5),
    );
    let age_fading =
        checkbox(bubbles.age_fading)
            .label("Age fading")
            .on_toggle(move |age_fading| {
                config_message(
                    pane,
                    KlineConfig {
                        volume_bubbles: data::chart::kline::VolumeBubbleConfig {
                            age_fading,
                            ..bubbles
                        }
                        .customized(),
                        ..cfg
                    },
                )
            });
    let three_dimensional = checkbox(bubbles.three_dimensional)
        .label("3D bubbles")
        .on_toggle(move |three_dimensional| {
            config_message(
                pane,
                KlineConfig {
                    volume_bubbles: data::chart::kline::VolumeBubbleConfig {
                        three_dimensional,
                        ..bubbles
                    }
                    .customized(),
                    ..cfg
                },
            )
        });
    let price_response = checkbox(bubbles.price_response_enabled)
        .label("Price response analysis")
        .on_toggle(move |price_response_enabled| {
            config_message(
                pane,
                KlineConfig {
                    volume_bubbles: data::chart::kline::VolumeBubbleConfig {
                        price_response_enabled,
                        ..bubbles
                    }
                    .customized(),
                    ..cfg
                },
            )
        });
    let candidates = labeled_slider(
        "Historical candidates",
        1.0..=20.0,
        bubbles.max_candidates_per_candle as f32,
        move |v| {
            config_message(
                pane,
                KlineConfig {
                    volume_bubbles: data::chart::kline::VolumeBubbleConfig {
                        max_candidates_per_candle: v as usize,
                        ..bubbles
                    },
                    ..cfg
                },
            )
        },
        |v| format!("{v:.0}"),
        Some(1.0),
    );
    let history = labeled_slider(
        "History window",
        1.0..=120.0,
        bubbles.history_window_minutes as f32,
        move |v| {
            config_message(
                pane,
                KlineConfig {
                    volume_bubbles: data::chart::kline::VolumeBubbleConfig {
                        history_window_minutes: v as u64,
                        ..bubbles
                    },
                    ..cfg
                },
            )
        },
        |v| format!("{v:.0}m"),
        Some(1.0),
    );
    let min_qty = labeled_slider(
        "Minimum volume",
        bubble_scale.min..=bubble_scale.max,
        bubbles.min_qty.clamp(bubble_scale.min, bubble_scale.max),
        move |min_qty| {
            config_message(
                pane,
                KlineConfig {
                    volume_bubbles: data::chart::kline::VolumeBubbleConfig { min_qty, ..bubbles },
                    ..cfg
                },
            )
        },
        |v| format_with_commas(*v),
        Some(bubble_scale.step),
    );
    let min_radius = labeled_slider(
        "Minimum radius",
        1.0..=20.0,
        bubbles.min_radius_px,
        move |min_radius_px| {
            config_message(
                pane,
                KlineConfig {
                    volume_bubbles: data::chart::kline::VolumeBubbleConfig {
                        min_radius_px,
                        ..bubbles
                    },
                    ..cfg
                },
            )
        },
        |v| format!("{v:.0}px"),
        Some(1.0),
    );
    let max_radius = labeled_slider(
        "Maximum radius",
        4.0..=40.0,
        bubbles.max_radius_px,
        move |max_radius_px| {
            config_message(
                pane,
                KlineConfig {
                    volume_bubbles: data::chart::kline::VolumeBubbleConfig {
                        max_radius_px,
                        ..bubbles
                    },
                    ..cfg
                },
            )
        },
        |v| format!("{v:.0}px"),
        Some(1.0),
    );
    let labels = pick_list(
        BubbleLabelMode::ALL,
        Some(bubbles.label_mode),
        move |label_mode| {
            config_message(
                pane,
                KlineConfig {
                    volume_bubbles: data::chart::kline::VolumeBubbleConfig {
                        label_mode,
                        ..bubbles
                    }
                    .customized(),
                    ..cfg
                },
            )
        },
    );
    let reuse = checkbox(bubbles.use_raw_trades_when_available)
        .label("Reuse shared raw trades")
        .on_toggle(move |use_raw_trades_when_available| {
            config_message(
                pane,
                KlineConfig {
                    volume_bubbles: data::chart::kline::VolumeBubbleConfig {
                        use_raw_trades_when_available,
                        ..bubbles
                    },
                    ..cfg
                },
            )
        });
    if advanced {
        sections = sections.push(
            column![
                preset,
                session,
                mode,
                labels,
                threshold_mode,
                display_percentile,
                min_qty,
                cluster_time,
                count,
                viewport_count,
                min_radius,
                max_radius,
                three_dimensional,
                age_fading,
                price_response,
                history,
                candidates,
                reuse
            ]
            .spacing(6),
        );
    } else {
        let mut basic = column![
            text("Preset"),
            preset,
            text("Session"),
            session,
            text("Colour by"),
            mode,
            text("Threshold"),
            threshold_mode
        ]
        .spacing(8);
        if bubbles.threshold_mode != BubbleThresholdMode::Fixed {
            basic = basic.push(display_percentile);
        }
        if bubbles.threshold_mode != BubbleThresholdMode::AdaptivePercentile {
            basic = basic.push(min_qty);
        }
        sections = sections.push(basic.push(three_dimensional));
    }

    sections.into()
}
