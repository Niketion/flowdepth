use crate::{screen::dashboard::pane::Content, widget::column_drag::DragEvent};
use data::chart::indicator::{
    Indicator, KlineIndicator, UiIndicator,
    catalog::{Definition, Editor},
};
use exchange::adapter::MarketKind;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Filter {
    #[default]
    All,
    Active,
}
#[derive(Debug, Clone)]
pub enum Action {
    Query(String),
    Filter(Filter),
    Select(UiIndicator),
    Back,
    Enable {
        indicator: UiIndicator,
        enabled: bool,
    },
    Advanced(bool),
    ReorderPanels(DragEvent),
}
#[derive(Debug)]
pub struct State {
    pub query: String,
    pub filter: Filter,
    pub focused: Option<UiIndicator>,
    pub advanced: bool,
    pub compact_detail: bool,
    pub search_id: iced::widget::Id,
    pub list_scroll_id: iced::widget::Id,
    pub settings_scroll_id: iced::widget::Id,
}
impl Default for State {
    fn default() -> Self {
        Self {
            query: String::new(),
            filter: Filter::All,
            focused: None,
            advanced: false,
            compact_detail: false,
            search_id: iced::widget::Id::unique(),
            list_scroll_id: iced::widget::Id::unique(),
            settings_scroll_id: iced::widget::Id::unique(),
        }
    }
}
impl State {
    pub fn can_reorder(&self) -> bool {
        self.filter == Filter::Active && self.query.trim().is_empty()
    }
    pub fn open(&mut self) {
        self.query.clear();
        self.filter = Filter::All;
        self.focused = None;
        self.advanced = false;
        self.compact_detail = false;
    }
    pub fn update(&mut self, action: &Action) {
        match action {
            Action::Query(query) => self.query = query.clone(),
            Action::Filter(filter) => self.filter = *filter,
            Action::Select(indicator) => {
                if self.focused != Some(*indicator) {
                    self.advanced = false;
                }
                self.focused = Some(*indicator);
                self.compact_detail = true;
            }
            Action::Back => self.compact_detail = false,
            Action::Advanced(value) => self.advanced = *value,
            Action::Enable { indicator, .. } => {
                if self.focused != Some(*indicator) {
                    self.advanced = false;
                }
                self.focused = Some(*indicator);
            }
            Action::ReorderPanels(_) => {}
        }
    }
    pub fn selection(
        &self,
        available: &[UiIndicator],
        active: &[UiIndicator],
    ) -> Option<UiIndicator> {
        self.focused
            .filter(|id| available.contains(id))
            .or_else(|| active.iter().copied().find(|id| available.contains(id)))
            .or_else(|| available.first().copied())
    }
}
pub fn definition(indicator: UiIndicator) -> &'static Definition {
    match indicator {
        UiIndicator::Kline(kind) => kind.definition(),
        UiIndicator::Heatmap(_) => KlineIndicator::Volume.definition(),
    }
}
pub fn has_advanced(indicator: UiIndicator) -> bool {
    matches!(definition(indicator).editor, Editor::Bubbles | Editor::Gex)
}
pub fn available(content: &Content, market: MarketKind) -> Vec<UiIndicator> {
    let candidates: Vec<_> = match content {
        Content::Kline { .. } => KlineIndicator::for_market(market)
            .iter()
            .copied()
            .map(UiIndicator::from)
            .collect(),
        Content::Heatmap { .. } | Content::ShaderHeatmap { .. } => vec![UiIndicator::Heatmap(
            data::chart::indicator::HeatmapIndicator::Volume,
        )],
        _ => vec![],
    };
    candidates
        .into_iter()
        .filter(|indicator| content.allows_indicator(*indicator))
        .collect()
}
pub fn active(content: &Content) -> Vec<UiIndicator> {
    match content {
        Content::Kline { indicators, .. } => {
            indicators.iter().copied().map(UiIndicator::from).collect()
        }
        Content::Heatmap { indicators, .. } | Content::ShaderHeatmap { indicators, .. } => {
            indicators.iter().copied().map(UiIndicator::from).collect()
        }
        _ => vec![],
    }
}
pub fn matches_query(indicator: UiIndicator, query: &str) -> bool {
    let entry = definition(indicator);
    let haystack = format!(
        "{} {} {}",
        entry.name,
        entry.description,
        entry.group.label()
    )
    .to_lowercase();
    query
        .split_whitespace()
        .all(|word| haystack.contains(&word.to_lowercase()))
}
pub fn matching(
    available: &[UiIndicator],
    active: &[UiIndicator],
    state: &State,
) -> Vec<UiIndicator> {
    let ordered = if state.filter == Filter::Active {
        active
    } else {
        available
    };
    ordered
        .iter()
        .copied()
        .filter(|id| available.contains(id) && matches_query(*id, &state.query))
        .collect()
}
/// Reorder panels while preserving overlays in their original positions.
/// Ignore stale drag indices; filtering a list must never panic or move an overlay.
pub fn reorder_panels(indicators: &mut [KlineIndicator], event: &DragEvent) {
    let DragEvent::Dropped {
        index,
        target_index,
    } = event
    else {
        return;
    };
    let mut panels: Vec<_> = indicators
        .iter()
        .copied()
        .filter(|id| !id.is_overlay())
        .collect();
    if *index >= panels.len() || *target_index > panels.len() || index == target_index {
        return;
    }
    let item = panels.remove(*index);
    panels.insert(
        if index < target_index {
            target_index - 1
        } else {
            *target_index
        },
        item,
    );
    let mut order = panels.into_iter();
    for id in indicators.iter_mut().filter(|id| !id.is_overlay()) {
        *id = order.next().unwrap();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn ids() -> Vec<UiIndicator> {
        KlineIndicator::for_market(MarketKind::LinearPerps)
            .iter()
            .copied()
            .map(UiIndicator::from)
            .collect()
    }
    #[test]
    fn search_matches_names_aliases_groups_and_multiple_words() {
        let available = ids();
        let mut menu = State::default();
        menu.update(&Action::Query(" relative STRENGTH ".into()));
        assert_eq!(
            matching(&available, &[], &menu),
            vec![KlineIndicator::Rsi.into()]
        );
        menu.update(&Action::Query("cumulative delta".into()));
        assert_eq!(
            matching(&available, &[], &menu),
            vec![KlineIndicator::CumulativeDelta.into()]
        );
        menu.update(&Action::Query("no matching indicator".into()));
        assert!(matching(&available, &[], &menu).is_empty());
    }
    #[test]
    fn browsing_never_changes_active_order_and_focused_settings_stay_put() {
        let available = ids();
        let active = vec![KlineIndicator::Macd.into(), KlineIndicator::Volume.into()];
        let mut menu = State::default();
        menu.update(&Action::Select(KlineIndicator::Rsi.into()));
        assert_eq!(
            menu.selection(&available, &active),
            Some(KlineIndicator::Rsi.into())
        );
        menu.update(&Action::Filter(Filter::Active));
        assert_eq!(matching(&available, &active, &menu), active);
        menu.update(&Action::Advanced(true));
        menu.update(&Action::Select(KlineIndicator::Vwap.into()));
        assert!(!menu.advanced);
        menu.open();
        assert!(menu.query.is_empty() && !menu.compact_detail);
        assert_eq!(menu.filter, Filter::All);
    }
    #[test]
    fn drag_reorders_only_panels_and_ignores_invalid_indices() {
        let mut order = vec![
            KlineIndicator::Volume,
            KlineIndicator::Vwap,
            KlineIndicator::CumulativeDelta,
            KlineIndicator::Tpo,
            KlineIndicator::Rsi,
        ];
        reorder_panels(
            &mut order,
            &DragEvent::Dropped {
                index: 2,
                target_index: 0,
            },
        );
        assert_eq!(
            order,
            vec![
                KlineIndicator::Rsi,
                KlineIndicator::Vwap,
                KlineIndicator::Volume,
                KlineIndicator::Tpo,
                KlineIndicator::CumulativeDelta
            ]
        );
        let original = order.clone();
        reorder_panels(
            &mut order,
            &DragEvent::Dropped {
                index: 20,
                target_index: 0,
            },
        );
        reorder_panels(
            &mut order,
            &DragEvent::Dropped {
                index: 0,
                target_index: 20,
            },
        );
        assert_eq!(order, original);
    }
}
