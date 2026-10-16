use super::config_message;
use crate::screen::dashboard::pane::Message;
use crate::style;
use crate::widget::{labeled_slider, pick_list};
use data::chart::kline::Config as KlineConfig;
use iced::{
    Element,
    widget::{checkbox, column, pane_grid, text},
};

pub(super) fn view<'a>(pane: pane_grid::Pane, cfg: KlineConfig) -> Element<'a, Message> {
    let mut sections = column![];

    use data::chart::kline::tpo::{TpoAnchor, TpoConfig};
    let tpo = cfg.tpo;
    let anchor = pick_list(TpoAnchor::ALL, Some(tpo.anchor), move |anchor| {
        config_message(
            pane,
            KlineConfig {
                tpo: TpoConfig { anchor, ..tpo },
                ..cfg
            },
        )
    });
    let period = pick_list(
        [
            exchange::Timeframe::M15,
            exchange::Timeframe::M30,
            exchange::Timeframe::H1,
            exchange::Timeframe::H2,
            exchange::Timeframe::H4,
        ],
        Some(tpo.letter_period),
        move |letter_period| {
            config_message(
                pane,
                KlineConfig {
                    tpo: TpoConfig {
                        letter_period,
                        ..tpo
                    },
                    ..cfg
                },
            )
        },
    );
    let value_area = labeled_slider(
        "Value area",
        5u16..=100,
        u16::from(tpo.value_area_percent).clamp(5, 100),
        move |percent| {
            config_message(
                pane,
                KlineConfig {
                    tpo: TpoConfig {
                        value_area_percent: percent as u8,
                        ..tpo
                    },
                    ..cfg
                },
            )
        },
        |value| format!("{value}%"),
        Some(1),
    );
    let letters = checkbox(tpo.show_letters)
        .label("Show letters")
        .on_toggle(move |show_letters| {
            config_message(
                pane,
                KlineConfig {
                    tpo: TpoConfig {
                        show_letters,
                        ..tpo
                    },
                    ..cfg
                },
            )
        });
    sections = sections.push(column![
            text("Session (UTC)"), anchor, text("Letter period"), period, value_area, letters,
            text("Uses loaded candle high/low ranges. Requires time candles no longer than the letter period. Partial sessions are marked.")
                .size(style::text_size::SMALL),
        ].spacing(6));

    sections.into()
}
