//! Editors are independent of discovery and menu layout; one module per study.
mod bubbles;
mod cvd;
mod gex;
mod scripted;
mod session_profile;
mod tpo;
mod vwap;
use crate::screen::dashboard::pane::{self, Message};
use data::{
    chart::{
        indicator::catalog::Editor,
        kline::{Config, scripted::ScriptKind},
    },
    layout::pane::VisualConfig,
};
use iced::{
    Element,
    widget::{pane_grid, text},
};
fn config_message(pane: pane_grid::Pane, cfg: Config) -> Message {
    Message::VisualConfigChanged(pane, VisualConfig::Kline(cfg), false)
}
pub(super) fn view<'a>(
    editor: Editor,
    pane: pane_grid::Pane,
    state: &'a pane::State,
    cfg: Config,
    scale: crate::chart::kline::VolumeBubbleQtyScale,
) -> Element<'a, Message> {
    match editor {
        Editor::None => text("This indicator has no additional settings.")
            .size(12)
            .into(),
        Editor::Cvd => cvd::view(pane, cfg),
        Editor::Period(kind) => scripted::view(pane, cfg, kind),
        Editor::Macd => scripted::view(pane, cfg, ScriptKind::Macd),
        Editor::Tpo => tpo::view(pane, cfg),
        Editor::SessionProfile => session_profile::view(pane, cfg),
        Editor::Bubbles => bubbles::view(pane, cfg, scale, state.indicator_menu.advanced),
        Editor::Vwap => vwap::view(pane, cfg),
        Editor::Gex => gex::view(pane, cfg, state, state.indicator_menu.advanced),
    }
}
