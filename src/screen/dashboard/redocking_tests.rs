use super::{Dashboard, PopoutPlacement, pane};
use crate::{chart, connector::ResolvedStream, window};
use data::layout::{WindowSpec, pane::LinkGroup};
use exchange::{TickerInfo, adapter::StreamKind};
use iced::widget::pane_grid::{self, Axis, Edge, Node};

#[cfg(target_os = "windows")]
#[test]
fn docking_peer_import_keeps_source_until_receipt_and_preserves_configuration() {
    let main = window::Id::unique();
    let receiving_main = window::Id::unique();
    let mut source = Dashboard::empty(uuid::Uuid::new_v4());
    let mut destination = Dashboard::empty(uuid::Uuid::new_v4());
    let mut pane = pane::State::new();
    pane.link_group = Some(LinkGroup::C);
    let unrelated = add_popout(&mut source, pane::State::new()).0;
    let (popup, _) = add_popout(&mut source, pane);

    let (pane_id, offer) = source.export_docking_pane(popup).unwrap();
    assert!(
        destination
            .import_docking_pane(receiving_main, crate::docking::Zone::Right, offer)
            .is_some()
    );
    assert!(source.popout.contains_key(&popup));
    let (focused_window, focused_pane) = destination.focus.unwrap();
    assert_eq!(focused_window, receiving_main);
    assert_eq!(
        destination.panes.get(focused_pane).unwrap().link_group,
        Some(LinkGroup::C)
    );
    assert!(matches!(
        destination.panes.layout(),
        Node::Split { axis: Axis::Vertical, b, .. } if matches!(**b, Node::Pane(pane) if pane == focused_pane)
    ));

    let _close = source.close_exported_pane(main, pane_id);
    assert!(!source.popout.contains_key(&popup));
    assert!(source.popout.contains_key(&unrelated));
}

fn add_popout(dashboard: &mut Dashboard, state: pane::State) -> (window::Id, pane_grid::Pane) {
    let window = window::Id::unique();
    let (grid, pane) = pane_grid::State::new(state);
    dashboard
        .popout
        .insert(window, (grid, WindowSpec::default()));
    (window, pane)
}

#[test]
fn docking_moves_the_original_state_and_leaves_the_other_popout_open() {
    let main = window::Id::unique();
    let mut dashboard = Dashboard::empty(uuid::Uuid::new_v4());
    let mut pane = pane::State::new();
    let id = pane.unique_id();
    let basis = data::chart::Basis::Time(exchange::Timeframe::M1);
    pane.settings.selected_basis = Some(basis);
    pane.link_group = Some(LinkGroup::C);
    let stream = StreamKind::Trades {
        ticker_info: TickerInfo::new(
            exchange::Ticker::new("BTCUSDT", exchange::adapter::Exchange::BinanceLinear),
            0.01,
            0.001,
            None,
        ),
    };
    pane.streams = ResolvedStream::Ready(vec![stream]);
    let (source, _) = add_popout(&mut dashboard, pane);
    let unrelated = pane::State::new();
    let unrelated_id = unrelated.unique_id();
    let (other, other_pane) = add_popout(&mut dashboard, unrelated);
    dashboard.focus = Some((other, other_pane));

    let placement = PopoutPlacement::Edge(Edge::Left);
    let destination = dashboard.transfer_popout(main, source, placement).unwrap();
    let docked = dashboard.panes.get(destination).unwrap();
    assert_eq!(docked.unique_id(), id);
    assert_eq!(docked.settings.selected_basis, Some(basis));
    assert_eq!(docked.link_group, Some(LinkGroup::C));
    assert_eq!(
        docked
            .streams
            .ready_iter()
            .unwrap()
            .copied()
            .collect::<Vec<_>>(),
        [stream]
    );
    assert_eq!(dashboard.focus, Some((main, destination)));
    assert_eq!(dashboard.popout.len(), 1);
    assert_eq!(
        dashboard
            .get_pane(main, other, other_pane)
            .unwrap()
            .unique_id(),
        unrelated_id
    );
    assert!(matches!(
        dashboard.panes.layout(),
        Node::Split { axis: Axis::Vertical, a, .. } if matches!(**a, Node::Pane(pane) if pane == destination)
    ));
    assert!(
        dashboard
            .transfer_popout(main, source, PopoutPlacement::Edge(Edge::Left))
            .is_none()
    );
    assert_eq!(dashboard.panes.len(), 2);
}

#[test]
fn docking_with_an_invalid_destination_preserves_focus_and_source() {
    let main = window::Id::unique();
    let mut dashboard = Dashboard::empty(uuid::Uuid::new_v4());
    let target = *dashboard.panes.iter().next().unwrap().0;
    dashboard.panes.maximize(target);
    let (source, pane) = add_popout(&mut dashboard, pane::State::new());
    let original_id = dashboard.get_pane(main, source, pane).unwrap().unique_id();
    dashboard.focus = Some((source, pane));

    assert!(
        dashboard
            .transfer_popout(main, source, PopoutPlacement::Edge(Edge::Bottom))
            .is_none()
    );
    assert_eq!(dashboard.panes.maximized(), Some(target));
    assert_eq!(dashboard.focus, Some((source, pane)));
    assert_eq!(
        dashboard.get_pane(main, source, pane).unwrap().unique_id(),
        original_id
    );
}

#[test]
fn docking_serializes_the_pane_once_in_the_receiving_grid() {
    let main = window::Id::unique();
    let mut dashboard = Dashboard::empty(uuid::Uuid::new_v4());
    let mut pane = pane::State::new();
    pane.link_group = Some(LinkGroup::B);
    let (source, _) = add_popout(&mut dashboard, pane);
    assert!(
        dashboard
            .transfer_popout(main, source, PopoutPlacement::Edge(Edge::Bottom))
            .is_some()
    );

    let json = serde_json::to_vec(&data::Dashboard::from(&dashboard)).unwrap();
    let saved: data::Dashboard = serde_json::from_slice(&json).unwrap();
    use data::layout::pane::{Axis as SavedAxis, Pane as SavedPane};
    assert!(saved.popout.is_empty());
    assert!(matches!(saved.pane, SavedPane::Split {
        axis: SavedAxis::Horizontal, a, b, ..
    } if matches!(*a, SavedPane::Starter { link_group: None })
        && matches!(*b, SavedPane::Starter { link_group: Some(LinkGroup::B) })));
}

#[test]
fn docking_manual_merge_uses_clicked_pane_and_preserves_legacy_siblings() {
    let main = window::Id::unique();
    let mut dashboard = Dashboard::empty(uuid::Uuid::new_v4());
    let sibling = pane::State::new();
    let sibling_id = sibling.unique_id();
    let (source, first) = add_popout(&mut dashboard, sibling);
    let clicked = pane::State::new();
    let clicked_id = clicked.unique_id();
    let second = dashboard
        .popout
        .get_mut(&source)
        .unwrap()
        .0
        .split(Axis::Horizontal, first, clicked)
        .unwrap()
        .0;
    dashboard.focus = Some((source, first));

    assert!(
        dashboard
            .transfer_popout(main, source, PopoutPlacement::Edge(Edge::Bottom))
            .is_none()
    );
    let destination = dashboard
        .transfer_popout(main, source, PopoutPlacement::Manual(second))
        .unwrap();
    assert_eq!(
        dashboard.panes.get(destination).unwrap().unique_id(),
        clicked_id
    );
    assert_eq!(dashboard.focus, Some((main, destination)));
    assert_eq!(
        dashboard.get_pane(main, source, first).unwrap().unique_id(),
        sibling_id
    );
    assert!(
        dashboard
            .transfer_popout(main, source, PopoutPlacement::Manual(second))
            .is_none()
    );
    assert!(
        dashboard
            .transfer_popout(main, source, PopoutPlacement::Manual(first))
            .is_some()
    );
    assert!(dashboard.popout.is_empty());
}

#[test]
fn docking_keeps_news_subscription_identity() {
    use iced::advanced::subscription::{Hasher, into_recipes};
    use std::hash::Hasher as _;

    let main = window::Id::unique();
    let mut dashboard = Dashboard::empty(uuid::Uuid::new_v4());
    let mut pane = pane::State::new();
    pane.content =
        pane::Content::News(chart::news::NewsTracker::new(data::news::Config::default()));
    let (source, _) = add_popout(&mut dashboard, pane);
    let subscriptions = |dashboard: &Dashboard| {
        into_recipes(dashboard.news_subscriptions(main, None))
            .into_iter()
            .map(|recipe| {
                let mut hash = Hasher::default();
                recipe.hash(&mut hash);
                hash.finish()
            })
            .collect::<Vec<_>>()
    };
    let before = subscriptions(&dashboard);
    assert!(!before.is_empty());
    assert!(
        dashboard
            .transfer_popout(main, source, PopoutPlacement::Edge(Edge::Right))
            .is_some()
    );
    assert_eq!(subscriptions(&dashboard), before);
}
