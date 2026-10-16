use super::config_message;
use crate::screen::dashboard::pane::Message;
use crate::widget::{labeled_slider, pick_list};
use data::chart::kline::{
    Config as KlineConfig, SessionProfileInterval, SessionProfileMode, SessionProfilePlacement,
};
use iced::{
    Element,
    widget::{checkbox, column, pane_grid, row},
};

pub(super) fn view<'a>(pane: pane_grid::Pane, cfg: KlineConfig) -> Element<'a, Message> {
    let mut sections = column![];

    let svp = cfg.session_volume_profile;
    let interval = pick_list(
        SessionProfileInterval::ALL,
        Some(svp.interval),
        move |interval| {
            config_message(
                pane,
                KlineConfig {
                    session_volume_profile: data::chart::kline::SessionVolumeProfileConfig {
                        interval,
                        ..svp
                    },
                    ..cfg
                },
            )
        },
    );
    let placement = pick_list(
        SessionProfilePlacement::ALL,
        Some(svp.placement),
        move |placement| {
            config_message(
                pane,
                KlineConfig {
                    session_volume_profile: data::chart::kline::SessionVolumeProfileConfig {
                        placement,
                        ..svp
                    },
                    ..cfg
                },
            )
        },
    );
    let mode = pick_list(SessionProfileMode::ALL, Some(svp.mode), move |mode| {
        config_message(
            pane,
            KlineConfig {
                session_volume_profile: data::chart::kline::SessionVolumeProfileConfig {
                    mode,
                    ..svp
                },
                ..cfg
            },
        )
    });
    let width = labeled_slider(
        "Width",
        10.0..=90.0,
        svp.width_percent,
        move |width_percent| {
            config_message(
                pane,
                KlineConfig {
                    session_volume_profile: data::chart::kline::SessionVolumeProfileConfig {
                        width_percent,
                        ..svp
                    },
                    ..cfg
                },
            )
        },
        |v| format!("{v:.0}%"),
        Some(1.0),
    );
    let value_area = labeled_slider(
        "Value area",
        50.0..=95.0,
        svp.value_area_percent,
        move |value_area_percent| {
            config_message(
                pane,
                KlineConfig {
                    session_volume_profile: data::chart::kline::SessionVolumeProfileConfig {
                        value_area_percent,
                        ..svp
                    },
                    ..cfg
                },
            )
        },
        |v| format!("{v:.0}%"),
        Some(1.0),
    );
    let rows = labeled_slider(
        "Ticks / row",
        1.0..=50.0,
        svp.row_size_ticks as f32,
        move |v| {
            config_message(
                pane,
                KlineConfig {
                    session_volume_profile: data::chart::kline::SessionVolumeProfileConfig {
                        row_size_ticks: v as u16,
                        ..svp
                    },
                    ..cfg
                },
            )
        },
        |v| format!("{v:.0}"),
        Some(1.0),
    );
    let poc = checkbox(svp.show_poc)
        .label("POC")
        .on_toggle(move |show_poc| {
            config_message(
                pane,
                KlineConfig {
                    session_volume_profile: data::chart::kline::SessionVolumeProfileConfig {
                        show_poc,
                        ..svp
                    },
                    ..cfg
                },
            )
        });
    let va = checkbox(svp.show_value_area)
        .label("VAH / VAL")
        .on_toggle(move |show_value_area| {
            config_message(
                pane,
                KlineConfig {
                    session_volume_profile: data::chart::kline::SessionVolumeProfileConfig {
                        show_value_area,
                        ..svp
                    },
                    ..cfg
                },
            )
        });
    let vwap = checkbox(svp.show_vwap)
        .label("Session VWAP level")
        .on_toggle(move |show_vwap| {
            config_message(
                pane,
                KlineConfig {
                    session_volume_profile: data::chart::kline::SessionVolumeProfileConfig {
                        show_vwap,
                        ..svp
                    },
                    ..cfg
                },
            )
        });
    let hi_lo = checkbox(svp.show_session_high_low)
        .label("Session high / low")
        .on_toggle(move |show_session_high_low| {
            config_message(
                pane,
                KlineConfig {
                    session_volume_profile: data::chart::kline::SessionVolumeProfileConfig {
                        show_session_high_low,
                        ..svp
                    },
                    ..cfg
                },
            )
        });
    sections = sections.push(
        column![
            interval,
            placement,
            mode,
            width,
            value_area,
            rows,
            row![poc, va].spacing(8),
            vwap,
            hi_lo
        ]
        .spacing(6),
    );

    sections.into()
}
