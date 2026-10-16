use super::config_message;
use crate::screen::dashboard::pane::Message;
use data::chart::kline::{Config, CvdConfig};
use iced::{
    Element,
    widget::{checkbox, column, pane_grid, text},
};
pub(super) fn view(pane: pane_grid::Pane, cfg: Config) -> Element<'static, Message> {
    column![checkbox(cfg.cvd.show_change_bars).label("Show change bars").on_toggle(move |show_change_bars|
        config_message(pane,Config{cvd:CvdConfig{show_change_bars},..cfg})),
        text("Continuous cumulative delta across the loaded history. Change bars use an independent scale.").size(12),
        text("Directional buy and sell volume is required. Missing directional data creates gaps.").size(12),
    ].spacing(12).into()
}
