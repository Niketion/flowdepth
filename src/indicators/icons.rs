use data::chart::indicator::catalog::Icon;
use iced::{Theme, widget::svg};
pub(super) fn glyph(bytes: &'static [u8], size: f32) -> svg::Svg<'static> {
    svg(svg::Handle::from_memory(bytes))
        .width(size)
        .height(size)
        .style(|theme: &Theme, _| svg::Style {
            color: Some(theme.palette().background.base.text),
        })
}
pub(super) fn indicator(icon: Icon, size: f32) -> svg::Svg<'static> {
    glyph(
        match icon {
            Icon::Volume => include_bytes!("../../assets/ui/indicators/volume.svg"),
            Icon::Delta => include_bytes!("../../assets/ui/indicators/delta.svg"),
            Icon::Momentum => include_bytes!("../../assets/ui/indicators/momentum.svg"),
            Icon::Profile => include_bytes!("../../assets/ui/indicators/profile.svg"),
            Icon::Trend => include_bytes!("../../assets/ui/indicators/trend.svg"),
            Icon::Levels => include_bytes!("../../assets/ui/indicators/levels.svg"),
            Icon::Bars => include_bytes!("../../assets/ui/indicators/bars.svg"),
        },
        size,
    )
}
pub(super) const SEARCH: &[u8] = include_bytes!("../../assets/ui/indicators/search.svg");
pub(super) const CLOSE: &[u8] = include_bytes!("../../assets/ui/indicators/close.svg");
pub(super) const DRAG: &[u8] = include_bytes!("../../assets/ui/indicators/drag.svg");
pub(super) const BACK: &[u8] = include_bytes!("../../assets/ui/indicators/back.svg");
