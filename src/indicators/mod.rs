//! Shared indicator browser: catalog-driven discovery, explicit activation and
//! one settings editor. Pane state owns navigation; persisted chart settings
//! remain in data::chart::kline::Config.
mod editors;
mod icons;
mod model;
use crate::{
    screen::dashboard::pane::{self, Message},
    style,
    widget::column_drag,
};
use data::chart::indicator::{
    UiIndicator,
    catalog::{Group, Icon},
};
use iced::{
    Alignment, Element, Length, Size,
    widget::{
        button, checkbox, column, container, pane_grid, responsive, row, rule, scrollable, space,
        text, text_input, tooltip,
    },
};
pub use model::{Action, State};
use model::{Filter, active, available, definition, has_advanced, matching};

fn message(pane: pane_grid::Pane, action: Action) -> Message {
    Message::PaneEvent(pane, pane::Event::IndicatorMenu(action))
}
pub fn toolbar_icon() -> iced::widget::svg::Svg<'static> {
    icons::indicator(Icon::Levels, 14.0)
}

pub fn view<'a>(pane: pane_grid::Pane, state: &'a pane::State) -> Element<'a, Message> {
    container(responsive(move |size| browser(pane, state, size)))
        .width(Length::Fill.max(880))
        .height(Length::Fill.max(620))
        .id(iced::widget::Id::new("indicator-dialog"))
        .padding(18)
        .style(style::chart_modal)
        .into()
}
fn browser<'a>(pane: pane_grid::Pane, state: &'a pane::State, size: Size) -> Element<'a, Message> {
    let menu = &state.indicator_menu;
    let market = state
        .stream_pair()
        .map_or(exchange::adapter::MarketKind::LinearPerps, |info| {
            info.market_type()
        });
    let available = available(&state.content, market);
    let active = active(&state.content);
    let focused = menu.selection(&available, &active);
    let narrow = size.width < 620.0;
    let close = tooltip(
        button(icons::glyph(icons::CLOSE, 16.0))
            .padding(7)
            .on_press(Message::PaneEvent(pane, pane::Event::HideModal))
            .style(button::text),
        "Close · Esc",
        tooltip::Position::Bottom,
    );
    let subtitle = state.stream_pair().map_or_else(
        || "Chart studies".to_string(),
        |info| info.ticker.display_symbol_and_type().0,
    );
    let heading = row![
        icons::indicator(Icon::Levels, 20.0),
        column![text("Indicators").size(18), text(subtitle).size(11)].spacing(3),
        space::horizontal(),
        container(close).id("indicator-close")
    ]
    .spacing(12)
    .align_y(Alignment::Center);
    let content: Element<_> = if narrow && menu.compact_detail {
        detail(pane, state, focused, &active, true)
    } else {
        let list = listing(pane, menu, &available, &active, focused);
        if narrow {
            list
        } else {
            row![
                container(list).width(270),
                rule::vertical(1),
                detail(pane, state, focused, &active, false)
            ]
            .spacing(18)
            .height(Length::Fill)
            .into()
        }
    };
    column![heading, rule::horizontal(1), content]
        .spacing(14)
        .height(Length::Fill)
        .into()
}
fn listing<'a>(
    pane: pane_grid::Pane,
    menu: &'a State,
    available: &[UiIndicator],
    active: &[UiIndicator],
    focused: Option<UiIndicator>,
) -> Element<'a, Message> {
    let search = row![
        icons::glyph(icons::SEARCH, 16.0),
        text_input("Search indicators", &menu.query)
            .id(menu.search_id.clone())
            .on_input(move |query| message(pane, Action::Query(query)))
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    let tab = |label: String, filter: Filter| {
        button(text(label).size(12))
            .width(Length::Fill)
            .padding(8)
            .on_press(message(pane, Action::Filter(filter)))
            .style(move |theme, status| {
                style::button::modifier(theme, status, menu.filter == filter)
            })
    };
    let tabs = row![
        tab("All indicators".into(), Filter::All),
        tab(format!("On chart ({})", active.len()), Filter::Active)
    ]
    .spacing(4);
    let matches = matching(available, active, menu);
    let mut groups = column![].spacing(8);
    if matches.is_empty() {
        groups = groups.push(
            text(if menu.query.trim().is_empty() {
                "No indicators on this chart."
            } else {
                "No matching indicators."
            })
            .size(12),
        );
    }
    if menu.filter == Filter::Active {
        let panels: Vec<_> = matches
            .iter()
            .copied()
            .filter(|id| !definition(*id).kind.is_overlay())
            .collect();
        let overlays: Vec<_> = matches
            .iter()
            .copied()
            .filter(|id| definition(*id).kind.is_overlay())
            .collect();
        let can_reorder = menu.can_reorder() && panels.len() > 1;
        if !panels.is_empty() {
            groups = groups.push(
                text(if can_reorder {
                    "Panels · drag to reorder"
                } else {
                    "Panels"
                })
                .size(11),
            );
            if can_reorder {
                let mut ordered = column_drag::Column::new()
                    .width(Length::Fill)
                    .spacing(4)
                    .on_drag(move |event| message(pane, Action::ReorderPanels(event)));
                for id in panels {
                    ordered = ordered.push(indicator_row(
                        pane,
                        id,
                        active.contains(&id),
                        focused == Some(id),
                        true,
                    ));
                }
                groups = groups.push(ordered);
            } else {
                for id in panels {
                    groups = groups.push(indicator_row(pane, id, true, focused == Some(id), false));
                }
            }
        }
        if !overlays.is_empty() {
            groups = groups.push(text("On price chart").size(11));
            for id in overlays {
                groups = groups.push(indicator_row(pane, id, true, focused == Some(id), false));
            }
        }
    } else {
        for group in Group::ALL {
            let entries: Vec<_> = matches
                .iter()
                .copied()
                .filter(|id| definition(*id).group == group)
                .collect();
            if entries.is_empty() {
                continue;
            }
            groups = groups.push(
                container(text(group.label()).size(11)).padding(iced::padding::top(8).bottom(2)),
            );
            for id in entries {
                groups = groups.push(indicator_row(
                    pane,
                    id,
                    active.contains(&id),
                    focused == Some(id),
                    false,
                ));
            }
        }
    }
    container(
        column![
            search,
            tabs,
            scrollable(container(groups).padding(iced::padding::right(14)))
                .id(menu.list_scroll_id.clone())
                .height(Length::Fill)
        ]
        .spacing(12)
        .height(Length::Fill),
    )
    .id("indicator-browser-list")
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}
fn indicator_row<'a>(
    pane: pane_grid::Pane,
    indicator: UiIndicator,
    enabled: bool,
    focused: bool,
    draggable: bool,
) -> Element<'a, Message> {
    let entry = definition(indicator);
    let select = button(
        row![
            icons::indicator(entry.icon, 16.0),
            text(entry.name).size(12)
        ]
        .spacing(9)
        .align_y(Alignment::Center),
    )
    .on_press(message(pane, Action::Select(indicator)))
    .padding(8)
    .width(Length::Fill)
    .style(move |theme, status| style::button::modifier(theme, status, focused));
    let enable = checkbox(enabled)
        .on_toggle(move |enabled| message(pane, Action::Enable { indicator, enabled }));
    let controls = row![
        container(select)
            .id(format!("indicator-select-{}", entry.name))
            .width(Length::Fill),
        container(enable).id(format!("indicator-enable-{}", entry.name))
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if draggable {
        row![
            container(icons::glyph(icons::DRAG, 12.0)).width(14),
            controls
        ]
        .align_y(Alignment::Center)
        .into()
    } else {
        controls.into()
    }
}
fn detail<'a>(
    pane: pane_grid::Pane,
    state: &'a pane::State,
    indicator: Option<UiIndicator>,
    active: &[UiIndicator],
    narrow: bool,
) -> Element<'a, Message> {
    let Some(indicator) = indicator else {
        return container(text("Choose an indicator to see its settings.").size(12))
            .width(Length::Fill)
            .into();
    };
    let entry = definition(indicator);
    let placement = if entry.kind.is_overlay() {
        "On price chart"
    } else {
        "Separate panel"
    };
    let status = if active.contains(&indicator) {
        "On chart"
    } else {
        "Not on chart"
    };
    let mut contents = column![].spacing(12);
    if narrow {
        contents = contents.push(
            button(
                row![
                    icons::glyph(icons::BACK, 14.0),
                    text("Back to indicators").size(12)
                ]
                .spacing(6),
            )
            .on_press(message(pane, Action::Back))
            .style(button::text),
        );
    }
    contents = contents
        .push(
            row![
                icons::indicator(entry.icon, 20.0),
                text(entry.name).size(18)
            ]
            .spacing(10)
            .align_y(Alignment::Center),
        )
        .push(text(format!("{status} · {placement}")).size(11))
        .push(text(entry.description).size(12));
    if has_advanced(indicator) {
        contents = contents.push(
            checkbox(state.indicator_menu.advanced)
                .label("Advanced settings")
                .on_toggle(move |advanced| message(pane, Action::Advanced(advanced))),
        );
    }
    let editor = match &state.content {
        pane::Content::Kline {
            chart: Some(chart), ..
        } => editors::view(
            entry.editor,
            pane,
            state,
            chart.visual_config(),
            chart.volume_bubble_qty_scale(),
        ),
        _ => text("This indicator has no additional settings.")
            .size(12)
            .into(),
    };
    contents = contents.push(rule::horizontal(1)).push(editor);
    container(
        scrollable(container(contents).padding(iced::padding::right(14)))
            .id(state.indicator_menu.settings_scroll_id.clone())
            .height(Length::Fill),
    )
    .id("indicator-browser-detail")
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// Render one modal for this window, outside individual pane bounds.
pub fn overlay<'a>(
    base: Element<'a, Message>,
    panes: &'a pane_grid::State<pane::State>,
) -> Element<'a, Message> {
    let Some((id, state)) = panes
        .iter()
        .find(|(_, state)| state.modal == Some(crate::modal::pane::Modal::Indicators))
    else {
        return base;
    };
    let close = Message::PaneEvent(*id, pane::Event::HideModal);
    let dialog = iced::widget::opaque(view(*id, state));
    let layer = iced::widget::mouse_area(
        container(dialog)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(20)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center)
            .style(|_theme| iced::widget::container::Style {
                background: Some(iced::Color::BLACK.scale_alpha(0.5).into()),
                ..Default::default()
            }),
    )
    .on_press(close);
    let base = container(base).width(Length::Fill).height(Length::Fill);
    iced::widget::stack![base, layer].into()
}
pub fn reorder_panels(
    indicators: &mut [data::chart::indicator::KlineIndicator],
    event: &column_drag::DragEvent,
) {
    model::reorder_panels(indicators, event)
}

pub fn supports(state: &pane::State, indicator: UiIndicator) -> bool {
    let market = state
        .stream_pair()
        .map_or(exchange::adapter::MarketKind::LinearPerps, |info| {
            info.market_type()
        });
    available(&state.content, market).contains(&indicator)
}

#[cfg(test)]
mod tests;
