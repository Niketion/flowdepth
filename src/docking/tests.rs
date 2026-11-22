//! Tests for the native popout re-docking geometry.
//!
//! These exercise the public contract of the parent module and the internal
//! helpers behind it (`Geometry::local`, `Geometry::placement`, the `Zone`
//! target maths), which this child module can see through `super`.

use super::*;

#[test]
fn docking_preview_is_immediate_at_a_scaled_edge() {
    let geometry = geometry(1.5);
    let pointer = target(geometry, Zone::Left);
    let mut drag = Drag::new(center(geometry));
    let preview = drag.update(pointer, Some(geometry)).unwrap();

    assert!(preview.placement.contains(pointer));
    assert_eq!(drag.release(pointer, Some(geometry)), Some(Zone::Left));
}

#[test]
fn placement_leaves_room_for_the_existing_grid() {
    let mut geometry = geometry(1.0);
    geometry.minimum.0 = geometry.screen.width - GRID_GAP - PANE_MIN;
    let pointer = target(geometry, Zone::Right);
    let mut drag = Drag::new(center(geometry));
    let preview = drag.update(pointer, Some(geometry)).unwrap();
    assert_eq!(preview.placement.width, PANE_MIN);

    geometry.minimum.0 += 1.0;
    assert!(drag.update(pointer, Some(geometry)).is_none());
}

#[test]
fn zero_sized_and_negative_main_bounds_clear_the_preview() {
    let geometry = geometry(1.0);
    let pointer = target(geometry, Zone::Left);
    let mut drag = Drag::new(center(geometry));
    assert!(drag.update(pointer, Some(geometry)).is_some());
    for width in [0.0, -1.0] {
        let mut invalid = geometry;
        invalid.screen.width = width;
        assert!(drag.update(pointer, Some(invalid)).is_none());
    }
    assert!(drag.update(pointer, Some(geometry)).is_some());
}

const ORIGIN: Point = Point(0.0, 0.0);
const MINIMUM: Point = Point(200.0, 150.0);
const GRID_1X: Rect = Rect {
    x: 0.0,
    y: 0.0,
    width: 800.0,
    height: 600.0,
};

/// A plain 1x-scale 800x600 dashboard at the screen origin.
fn geometry(scale: f64) -> Geometry {
    Geometry::from_client(ORIGIN, scale, GRID_1X, MINIMUM)
}

/// Screen-space centre of the dashboard, used as a neutral drag start.
fn center(geometry: Geometry) -> Point {
    Point(
        geometry.screen.x + geometry.screen.width / 2.0,
        geometry.screen.y + geometry.screen.height / 2.0,
    )
}

/// Converts a dashboard-local point to screen space for `geometry`.
fn to_screen(geometry: Geometry, local: Point) -> Point {
    Point(
        geometry.screen.x + local.0 * geometry.scale,
        geometry.screen.y + local.1 * geometry.scale,
    )
}

/// The middle of a zone's compact edge target, in screen space.
fn target(geometry: Geometry, zone: Zone) -> Point {
    let width = geometry.screen.width / geometry.scale;
    let height = geometry.screen.height / geometry.scale;
    let local = match zone {
        Zone::Left => Point(EDGE_INSET, height / 2.0),
        Zone::Right => Point(width - EDGE_INSET, height / 2.0),
        Zone::Top => Point(width / 2.0, EDGE_INSET),
        Zone::Bottom => Point(width / 2.0, height - EDGE_INSET),
    };
    to_screen(geometry, local)
}

#[test]
fn from_client_maps_grid_through_scale() {
    let geometry = Geometry::from_client(
        Point(100.0, 50.0),
        1.5,
        Rect {
            x: 10.0,
            y: 20.0,
            width: 400.0,
            height: 300.0,
        },
        Point(200.0, 150.0),
    );
    assert_eq!(
        geometry.screen,
        Rect {
            x: 115.0,
            y: 80.0,
            width: 600.0,
            height: 450.0,
        }
    );
    assert_eq!(geometry.scale, 1.5);
    assert_eq!(geometry.minimum, Point(200.0, 150.0));
}

#[test]
fn local_maps_pointer_and_rejects_outside() {
    let geometry = Geometry::from_client(
        Point(100.0, 50.0),
        1.5,
        Rect {
            x: 10.0,
            y: 20.0,
            width: 400.0,
            height: 300.0,
        },
        MINIMUM,
    );
    // screen = (115, 80, 600, 450); the local rect is the grid size.
    let (area, local) = geometry.local(Point(115.0, 80.0)).unwrap();
    assert_eq!(
        area,
        Rect {
            x: 0.0,
            y: 0.0,
            width: 400.0,
            height: 300.0,
        }
    );
    assert_eq!(local, Point(0.0, 0.0));
    // Anything at or beyond the far edges is outside (the rect is exclusive).
    assert!(geometry.local(Point(114.0, 80.0)).is_none());
    assert!(geometry.local(Point(715.0, 80.0)).is_none());
    assert!(geometry.local(Point(115.0, 530.0)).is_none());
}

#[test]
fn invalid_scale_has_no_local_point() {
    for scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let geometry = Geometry::from_client(ORIGIN, scale, GRID_1X, MINIMUM);
        assert!(geometry.local(Point(10.0, 10.0)).is_none(), "scale {scale}");
    }
}

#[test]
fn leaving_a_preview_before_releasing_keeps_the_window_detached() {
    let geometry = geometry(1.0);
    let mut drag = Drag::new(center(geometry));
    let pointer = target(geometry, Zone::Left);
    assert!(drag.update(pointer, Some(geometry)).is_some());
    assert_eq!(drag.release(center(geometry), Some(geometry)), None);
}

#[test]
fn release_cannot_confirm_a_different_edge_than_the_visible_preview() {
    let geometry = geometry(1.0);
    let mut drag = Drag::new(center(geometry));
    let left = target(geometry, Zone::Left);
    let right = target(geometry, Zone::Right);
    assert!(drag.update(left, Some(geometry)).is_some());
    assert_eq!(drag.release(right, Some(geometry)), None);
}

#[test]
fn click_within_the_zone_does_not_dock() {
    let geometry = geometry(1.0);
    let start = target(geometry, Zone::Left);
    let mut drag = Drag::new(start);
    let pointer = Point(start.0 + 8.0, start.1);
    assert!(drag.update(pointer, Some(geometry)).is_none());
    assert_eq!(drag.release(pointer, Some(geometry)), None);
}

#[test]
fn centre_of_window_is_not_a_target() {
    let geometry = geometry(1.0);
    let mut drag = Drag::new(center(geometry));
    assert_eq!(drag.update(Point(400.0, 200.0), Some(geometry)), None);
}

#[test]
fn preview_placement_matches_zone_split() {
    let geometry = geometry(1.0);
    assert_eq!(
        geometry.placement(Zone::Left),
        Rect {
            x: 0.0,
            y: 0.0,
            width: 397.0,
            height: 600.0,
        }
    );
    assert_eq!(
        geometry.placement(Zone::Right),
        Rect {
            x: 403.0,
            y: 0.0,
            width: 397.0,
            height: 600.0,
        }
    );
    assert_eq!(
        geometry.placement(Zone::Top),
        Rect {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 297.0,
        }
    );
    assert_eq!(
        geometry.placement(Zone::Bottom),
        Rect {
            x: 0.0,
            y: 303.0,
            width: 800.0,
            height: 297.0,
        }
    );
}

#[test]
fn hysteresis_keeps_candidate_within_exit_margin() {
    let geometry = geometry(1.0);
    let mut drag = Drag::new(center(geometry));
    let pointer = target(geometry, Zone::Left);
    drag.update(pointer, Some(geometry));

    // Exactly on the outer edge of the hysteresis band.
    let drift = Point(EDGE_INSET + TARGET_DEPTH + EXIT_MARGIN, 300.0);
    let preview = drag.update(drift, Some(geometry)).unwrap();
    assert_eq!(preview.zone, Zone::Left);
    assert_eq!(drag.release(drift, Some(geometry)), Some(Zone::Left));
}

#[test]
fn leaving_edge_beyond_exit_margin_drops_candidate() {
    let geometry = geometry(1.0);
    let mut drag = Drag::new(center(geometry));
    let pointer = target(geometry, Zone::Left);
    drag.update(pointer, Some(geometry));

    // One pixel past the hysteresis band: the candidate is gone.
    let away = Point(EDGE_INSET + TARGET_DEPTH + EXIT_MARGIN + 1.0, 300.0);
    assert_eq!(drag.update(away, Some(geometry)), None);
    assert_eq!(drag.release(away, Some(geometry)), None);
}

#[test]
fn switching_edges_updates_the_preview_and_release_target() {
    let geometry = geometry(1.0);
    let left = target(geometry, Zone::Left);
    let right = target(geometry, Zone::Right);
    let mut drag = Drag::new(center(geometry));
    assert_eq!(drag.update(left, Some(geometry)).unwrap().zone, Zone::Left);
    assert_eq!(
        drag.update(right, Some(geometry)).unwrap().zone,
        Zone::Right
    );
    assert_eq!(drag.release(right, Some(geometry)), Some(Zone::Right));
}

#[test]
fn release_rejects_geometry_changed_since_the_preview() {
    let before = geometry(1.0);
    let mut after = before;
    after.screen.x += 4.0;
    let pointer = target(before, Zone::Left);
    let mut drag = Drag::new(center(before));
    assert!(drag.update(pointer, Some(before)).is_some());
    assert_eq!(drag.release(pointer, Some(after)), None);
}

#[test]
fn missing_geometry_cancels_the_candidate() {
    let geometry = geometry(1.0);
    let pointer = target(geometry, Zone::Left);
    let mut drag = Drag::new(center(geometry));
    assert!(drag.update(pointer, Some(geometry)).is_some());
    assert_eq!(drag.update(pointer, None), None);
    assert_eq!(drag.release(pointer, Some(geometry)), None);
}

#[test]
fn pointer_outside_window_has_no_candidate() {
    let geometry = geometry(1.0);
    let start = center(geometry);
    for pointer in [
        Point(-1.0, 300.0),
        Point(801.0, 300.0),
        Point(400.0, -1.0),
        Point(400.0, 601.0),
    ] {
        let mut drag = Drag::new(start);
        assert_eq!(drag.update(pointer, Some(geometry)), None, "{pointer:?}");
    }
}

#[test]
fn panes_respect_minimum_window_size() {
    // A 400px-wide dashboard is too narrow for left/right panes: they need
    // minimum (200) + PANE_MIN (240) + GRID_GAP (6) = 446px.
    let narrow = Geometry::from_client(
        ORIGIN,
        1.0,
        Rect {
            x: 0.0,
            y: 0.0,
            width: 400.0,
            height: 600.0,
        },
        MINIMUM,
    );

    let mut drag = Drag::new(center(narrow));
    assert_eq!(drag.update(Point(EDGE_INSET, 300.0), Some(narrow)), None);

    // Top/bottom still fit because the height is large enough.
    let mut drag = Drag::new(center(narrow));
    assert_eq!(
        drag.update(Point(200.0, EDGE_INSET), Some(narrow))
            .map(|preview| preview.zone),
        Some(Zone::Top)
    );
}

#[test]
fn negative_monitor_origin_works() {
    let geometry = Geometry::from_client(
        Point(-1920.0, -240.0),
        1.0,
        Rect {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 700.0,
        },
        MINIMUM,
    );
    let mut drag = Drag::new(center(geometry));
    let pointer = target(geometry, Zone::Left);
    assert_eq!(pointer, Point(-1884.0, 110.0));

    drag.update(pointer, Some(geometry));
    assert_eq!(drag.release(pointer, Some(geometry)), Some(Zone::Left));
}

#[test]
fn mixed_dpi_scales_targets_and_placement() {
    let geometry = geometry(2.0);
    assert_eq!(
        geometry.screen,
        Rect {
            x: 0.0,
            y: 0.0,
            width: 1600.0,
            height: 1200.0,
        }
    );

    let pointer = target(geometry, Zone::Left);
    assert_eq!(pointer, Point(72.0, 600.0));

    let mut drag = Drag::new(center(geometry));
    drag.update(pointer, Some(geometry));
    let preview = drag.update(pointer, Some(geometry)).unwrap();
    assert_eq!(preview.zone, Zone::Left);
    assert_eq!(
        preview.placement,
        Rect {
            x: 0.0,
            y: 0.0,
            width: 794.0,
            height: 1200.0,
        }
    );
    assert_eq!(drag.release(pointer, Some(geometry)), Some(Zone::Left));
}

#[test]
fn same_physical_offset_differs_by_scale() {
    // At 1x, 72px from the left is 72 logical: outside the target.
    let geometry_1x = geometry(1.0);
    let mut drag_1x = Drag::new(center(geometry_1x));
    assert_eq!(drag_1x.update(Point(72.0, 300.0), Some(geometry_1x)), None);

    // At 2x the same 72px is 36 logical: inside the target.
    let geometry_2x = geometry(2.0);
    let mut drag_2x = Drag::new(center(geometry_2x));
    assert!(
        drag_2x
            .update(Point(72.0, 600.0), Some(geometry_2x))
            .is_some()
    );
}

#[test]
fn release_confirms_once_per_drag() {
    let geometry = geometry(1.0);
    let start = center(geometry);
    let pointer = target(geometry, Zone::Left);

    // `release` consumes the drag, so a second confirmation is a compile error;
    // a fresh drag is independent and can confirm on its own.
    let mut drag = Drag::new(start);
    drag.update(pointer, Some(geometry));
    assert_eq!(drag.release(pointer, Some(geometry)), Some(Zone::Left));

    let fresh = Drag::new(start);
    assert_eq!(fresh.release(pointer, Some(geometry)), None);
}
