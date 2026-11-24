//! Tests for visible docking targets and confirmation during a native drag.

use super::*;

const START: Point = Point(-20_000.0, -20_000.0);
const CENTER: Point = Point(400.0, 300.0);
const MINIMUM: Point = Point(200.0, 150.0);
const GRID: Rect = Rect {
    x: 0.0,
    y: 0.0,
    width: 800.0,
    height: 600.0,
};

impl Drag {
    fn sample(&mut self, pointer: Point, geometry: Option<Geometry>) -> Option<Guide> {
        self.update(pointer, geometry, Instant::now())
    }
}

fn geometry(scale: f64) -> Geometry {
    Geometry::from_client(Point(0.0, 0.0), scale, GRID, MINIMUM)
}

fn center(rect: Rect) -> Point {
    Point(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
}

fn target(guide: Guide, zone: Zone) -> Rect {
    guide.targets().find(|(side, _)| *side == zone).unwrap().1
}

#[test]
fn guide_appears_on_entry_without_arming_a_drop() {
    let geometry = geometry(1.0);
    let mut drag = Drag::new(START);
    let pointer = Point(1.0, 1.0);
    let guide = drag.sample(pointer, Some(geometry)).unwrap();

    assert_eq!(center(guide.bounds), CENTER);
    assert_eq!(guide.targets().count(), 4);
    assert_eq!(guide.preview, None);
    assert_eq!(drag.release(pointer, Some(geometry)), None);
}

#[test]
fn every_tile_previews_and_confirms_its_own_split() {
    let geometry = geometry(1.0);
    let guide = geometry.guide(CENTER).unwrap();
    let expected = [
        (Zone::Left, (0.0, 0.0, 397.0, 600.0)),
        (Zone::Right, (403.0, 0.0, 397.0, 600.0)),
        (Zone::Top, (0.0, 0.0, 800.0, 297.0)),
        (Zone::Bottom, (0.0, 303.0, 800.0, 297.0)),
    ];

    for (zone, (x, y, width, height)) in expected {
        let pointer = center(target(guide, zone));
        let mut drag = Drag::new(START);
        let preview = drag.sample(pointer, Some(geometry)).unwrap().preview;
        assert_eq!(
            preview,
            Some(Preview {
                zone,
                placement: Rect {
                    x,
                    y,
                    width,
                    height
                },
            })
        );
        assert_eq!(drag.release(pointer, Some(geometry)), Some(zone));
    }
}

#[test]
fn an_invalid_axis_has_no_tiles_and_the_remaining_row_is_centered() {
    let mut geometry = geometry(1.0);
    geometry.screen.width = 400.0;
    let pointer = center(geometry.screen);
    let guide = geometry.guide(pointer).unwrap();
    assert_eq!(center(guide.bounds), pointer);
    assert_eq!(
        guide.targets().map(|(zone, _)| zone).collect::<Vec<_>>(),
        [Zone::Top, Zone::Bottom]
    );

    geometry.screen.height = 350.0;
    assert!(geometry.guide(pointer).is_none());
}

#[test]
fn placement_leaves_room_for_the_existing_grid() {
    let mut geometry = geometry(1.0);
    geometry.minimum.0 = geometry.screen.width - GRID_GAP - PANE_MIN;
    let guide = geometry.guide(CENTER).unwrap();
    let pointer = center(target(guide, Zone::Right));
    let mut drag = Drag::new(START);
    let preview = drag
        .sample(pointer, Some(geometry))
        .unwrap()
        .preview
        .unwrap();
    assert_eq!(preview.placement.width, PANE_MIN);

    geometry.minimum.0 += 1.0;
    let guide = drag.sample(pointer, Some(geometry)).unwrap();
    assert!(guide.targets().all(|(zone, _)| zone != Zone::Right));
    assert_eq!(drag.release(pointer, Some(geometry)), None);
}

#[test]
fn release_requires_the_same_visible_tile_and_geometry() {
    let before = geometry(1.0);
    let guide = before.guide(CENTER).unwrap();
    let left = center(target(guide, Zone::Left));
    let right = center(target(guide, Zone::Right));
    let mut after = before;
    after.screen.x += 4.0;
    for (point, geometry) in [
        (right, Some(before)),
        (CENTER, Some(before)),
        (left, Some(after)),
        (left, None),
    ] {
        let mut drag = Drag::new(START);
        assert!(drag.sample(left, Some(before)).unwrap().preview.is_some());
        assert_eq!(drag.release(point, geometry), None);
    }
}

#[test]
fn switching_tiles_updates_the_active_preview() {
    let geometry = geometry(1.0);
    let guide = geometry.guide(CENTER).unwrap();
    let mut drag = Drag::new(START);
    for zone in [Zone::Left, Zone::Right] {
        let pointer = center(target(guide, zone));
        let preview = drag
            .sample(pointer, Some(geometry))
            .unwrap()
            .preview
            .unwrap();
        assert_eq!(preview.zone, zone);
    }
    assert_eq!(
        drag.release(center(target(guide, Zone::Right)), Some(geometry)),
        Some(Zone::Right)
    );
}

#[test]
fn leaving_the_dashboard_clears_the_guide_and_previous_drop() {
    let geometry = geometry(1.0);
    let tile = center(target(geometry.guide(CENTER).unwrap(), Zone::Top));
    for outside in [Point(-1.0, 300.0), Point(800.0, 300.0), Point(400.0, 600.0)] {
        let mut drag = Drag::new(START);
        assert!(drag.sample(tile, Some(geometry)).unwrap().preview.is_some());
        assert_eq!(drag.sample(outside, Some(geometry)), None);
        assert_eq!(drag.release(tile, Some(geometry)), None);
    }
}

#[test]
fn a_small_movement_inside_a_tile_does_not_show_a_guide_or_dock() {
    let geometry = geometry(1.0);
    let start = center(target(geometry.guide(CENTER).unwrap(), Zone::Left));
    let pointer = Point(start.0 + 8.0, start.1);
    let mut drag = Drag::new(start);
    assert_eq!(drag.sample(pointer, Some(geometry)), None);
    assert_eq!(drag.release(pointer, Some(geometry)), None);
}

#[test]
fn guide_draw_and_hit_bounds_agree_on_a_negative_origin_at_fractional_scale() {
    let geometry = Geometry::from_client(
        Point(-1920.0, -238.0),
        1.25,
        Rect {
            x: 11.0,
            y: 3.0,
            ..GRID
        },
        MINIMUM,
    );
    let guide = geometry.guide(center(geometry.screen)).unwrap();
    assert_eq!(
        geometry.screen,
        Rect {
            x: -1906.25,
            y: -234.25,
            width: 1000.0,
            height: 750.0,
        }
    );
    assert_eq!(center(guide.bounds), center(geometry.screen));

    let tile = target(guide, Zone::Left);
    assert!(tile.x < 0.0 && tile.x.fract() == 0.0 && tile.y.fract() == 0.0);
    assert_eq!(tile.width, 130.0);
    let mut drag = Drag::new(START);
    let edge = Point(tile.x + tile.width - 1.0, tile.y);
    assert_eq!(
        drag.sample(edge, Some(geometry))
            .unwrap()
            .preview
            .unwrap()
            .zone,
        Zone::Left
    );
    let gap = Point(edge.0 + 1.0, edge.1);
    assert_eq!(drag.release(gap, Some(geometry)), None);
}

#[test]
fn invalid_bounds_and_scale_hide_the_guide() {
    for scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(geometry(scale).guide(CENTER).is_none());
    }
    let mut geometry = geometry(1.0);
    for width in [0.0, -1.0, f64::INFINITY] {
        geometry.screen.width = width;
        assert!(geometry.guide(CENTER).is_none());
    }
}

#[test]
fn quick_passage_near_an_edge_keeps_the_popup_detached() {
    let geometry = Some(geometry(1.0));
    let start = Instant::now();
    let edge = Point(28.0, 175.0);
    let mut drag = Drag::new(START);

    assert_eq!(drag.update(edge, geometry, start).unwrap().preview, None);
    assert_eq!(
        drag.update(
            edge,
            geometry,
            start + EDGE_DWELL - Duration::from_millis(1)
        )
        .unwrap()
        .preview,
        None
    );
    assert_eq!(drag.release(edge, geometry), None);
}

#[test]
fn a_short_dwell_recommends_each_edge_across_its_length() {
    let geometry = Some(geometry(1.0));
    let start = Instant::now();
    for (edge, zone) in [
        (Point(60.0, 175.0), Zone::Left),
        (Point(750.0, 450.0), Zone::Right),
        (Point(630.0, 45.0), Zone::Top),
        (Point(210.0, 550.0), Zone::Bottom),
    ] {
        let mut drag = Drag::new(START);
        assert_eq!(drag.update(edge, geometry, start).unwrap().preview, None);
        let preview = drag
            .update(edge, geometry, start + EDGE_DWELL)
            .unwrap()
            .preview;
        assert_eq!(preview.map(|preview| preview.zone), Some(zone));
        assert_eq!(drag.release(edge, geometry), Some(zone));
    }
}

#[test]
fn an_ambiguous_corner_resets_the_suggestion_and_rejects_release() {
    let geometry = Some(geometry(1.0));
    let start = Instant::now();
    let left = Point(10.0, 48.0);
    let corner = Point(12.0, 16.0);
    let top = Point(64.0, 15.0);
    let mut drag = Drag::new(START);

    drag.update(left, geometry, start);
    assert!(
        drag.update(left, geometry, start + EDGE_DWELL)
            .unwrap()
            .preview
            .is_some()
    );
    assert_eq!(
        drag.update(corner, geometry, start + EDGE_DWELL)
            .unwrap()
            .preview,
        None
    );
    assert_eq!(
        drag.update(top, geometry, start + EDGE_DWELL)
            .unwrap()
            .preview,
        None
    );
    let preview = drag
        .update(top, geometry, start + 2 * EDGE_DWELL)
        .unwrap()
        .preview;
    assert_eq!(preview.map(|preview| preview.zone), Some(Zone::Top));
    assert_eq!(drag.release(corner, geometry), None);
}

#[test]
fn edge_suggestions_respect_dpi_and_available_splits() {
    let geometry = Geometry::from_client(
        Point(-1920.0, -240.0),
        1.5,
        Rect {
            width: 400.0,
            ..GRID
        },
        MINIMUM,
    );
    let guide = geometry.guide(center(geometry.screen)).unwrap();
    assert_eq!(geometry.edge(Point(-1900.0, 200.0), guide), None);
    assert_eq!(
        geometry.edge(Point(-1500.0, -140.0), guide),
        Some(Zone::Top)
    );
    assert_eq!(geometry.edge(Point(-1500.0, -120.0), guide), None);
}

#[test]
fn release_cannot_move_from_an_edge_suggestion_to_the_matching_central_tile() {
    let geometry = geometry(1.0);
    let guide = geometry.guide(CENTER).unwrap();
    let edge = Point(15.0, 300.0);
    let start = Instant::now();
    let mut drag = Drag::new(START);
    drag.update(edge, Some(geometry), start);
    drag.update(edge, Some(geometry), start + EDGE_DWELL);
    assert_eq!(
        drag.release(center(target(guide, Zone::Left)), Some(geometry)),
        None
    );
}
