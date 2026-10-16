use super::config_message;
use crate::screen::dashboard::pane::Message;
use crate::widget::{labeled_slider, pick_list};
use data::chart::kline::{Config as KlineConfig, SessionProfileInterval};
use iced::{
    Element,
    widget::{checkbox, column, pane_grid},
};

pub(super) fn view<'a>(pane: pane_grid::Pane, cfg: KlineConfig) -> Element<'a, Message> {
    let mut sections = column![];

    let vwap = cfg.vwap;
    let anchor = pick_list(
        SessionProfileInterval::ALL,
        Some(vwap.anchor),
        move |anchor| {
            config_message(
                pane,
                KlineConfig {
                    vwap: data::chart::kline::VwapConfig { anchor, ..vwap },
                    ..cfg
                },
            )
        },
    );
    let width = labeled_slider(
        "Line width",
        0.5..=5.0,
        vwap.line_width,
        move |line_width| {
            config_message(
                pane,
                KlineConfig {
                    vwap: data::chart::kline::VwapConfig { line_width, ..vwap },
                    ..cfg
                },
            )
        },
        |v| format!("{v:.1}px"),
        Some(0.1),
    );
    let multiplier = labeled_slider(
        "Band multiplier",
        0.25..=3.0,
        vwap.band_multiplier,
        move |band_multiplier| {
            config_message(
                pane,
                KlineConfig {
                    vwap: data::chart::kline::VwapConfig {
                        band_multiplier,
                        ..vwap
                    },
                    ..cfg
                },
            )
        },
        |v| format!("{v:.2}σ"),
        Some(0.25),
    );
    let bands = checkbox(vwap.show_bands)
        .label("Standard-deviation bands")
        .on_toggle(move |show_bands| {
            config_message(
                pane,
                KlineConfig {
                    vwap: data::chart::kline::VwapConfig { show_bands, ..vwap },
                    ..cfg
                },
            )
        });
    let labels = checkbox(vwap.show_labels)
        .label("Labels")
        .on_toggle(move |show_labels| {
            config_message(
                pane,
                KlineConfig {
                    vwap: data::chart::kline::VwapConfig {
                        show_labels,
                        ..vwap
                    },
                    ..cfg
                },
            )
        });
    sections = sections.push(column![anchor, width, multiplier, bands, labels].spacing(6));

    sections.into()
}
