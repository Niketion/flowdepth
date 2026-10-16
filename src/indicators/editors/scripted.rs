use super::config_message;
use crate::{screen::dashboard::pane::Message, widget::labeled_slider};
use data::chart::kline::{
    Config,
    scripted::{ScriptKind, ScriptedConfig},
};
use iced::{
    Element,
    widget::{column, pane_grid},
};
fn period(
    pane: pane_grid::Pane,
    cfg: Config,
    label: &'static str,
    range: std::ops::RangeInclusive<u16>,
    value: u16,
    set: fn(&mut ScriptedConfig, u16),
) -> Element<'static, Message> {
    labeled_slider(
        label,
        range.clone(),
        value.clamp(*range.start(), *range.end()),
        move |value| {
            let mut scripted = cfg.scripted;
            set(&mut scripted, value);
            config_message(pane, Config { scripted, ..cfg })
        },
        |value| value.to_string(),
        Some(1),
    )
}
pub(super) fn view(
    pane: pane_grid::Pane,
    cfg: Config,
    kind: ScriptKind,
) -> Element<'static, Message> {
    let settings = cfg.scripted;
    match kind {
        ScriptKind::Rsi => period(pane, cfg, "Period", 2..=200, settings.rsi_period, |s, v| {
            s.rsi_period = v
        }),
        ScriptKind::Atr => period(pane, cfg, "Period", 1..=200, settings.atr_period, |s, v| {
            s.atr_period = v
        }),
        ScriptKind::Adx => period(pane, cfg, "Period", 2..=200, settings.adx_period, |s, v| {
            s.adx_period = v
        }),
        ScriptKind::Macd => column![
            period(
                pane,
                cfg,
                "Fast period",
                1..=200,
                settings.macd_fast,
                |s, v| s.macd_fast = v
            ),
            period(
                pane,
                cfg,
                "Slow period",
                2..=500,
                settings.macd_slow,
                |s, v| s.macd_slow = v
            ),
            period(
                pane,
                cfg,
                "Signal period",
                1..=200,
                settings.macd_signal,
                |s, v| s.macd_signal = v
            )
        ]
        .spacing(12)
        .into(),
        ScriptKind::Cvd => super::cvd::view(pane, cfg),
    }
}
