//! Tests for the native popout re-docking geometry.
//!
//! These exercise the public contract of the parent module and the internal
//! helpers behind it (`Geometry::local`, `Geometry::placement`, the `Zone`
//! target maths), which this child module can see through `super`.

use super::*;

#[test]
fn marker_and_placement_share_the_same_scaled_geometry() {
    let geometry = geometry(1.5);
    let pointer = target(geometry, Zone::Left);
    let mut drag = Drag::new(center(geometry));
    let marker = drag
        .update(pointer, Some(geometry), Duration::ZERO)
        .unwrap();
    assert!(!marker.armed);
    assert!(marker.activation.contains(pointer));
    assert_eq!(marker.activation.width, 2.0 * TARGET_DEPTH * 1.5);
    let armed = drag.update(pointer, Some(geometry), ARM_DELAY).unwrap();
    assert!(armed.armed);
    assert_eq!(marker.activation, armed.activation);
    assert!(armed.placement.contains(pointer));
}

#[test]
fn placement_leaves_room_for_the_existing_grid() {
    let mut geometry = geometry(1.0);
    geometry.minimum.0 = geometry.screen.width - GRID_GAP - PANE_MIN;
    let pointer = target(geometry, Zone::Right);
    let mut drag = Drag::new(center(geometry));
    let preview = drag
        .update(pointer, Some(geometry), Duration::ZERO)
        .unwrap();
    assert_eq!(preview.placement.width, PANE_MIN);

    geometry.minimum.0 += 1.0;
    assert!(drag.update(pointer, Some(geometry), ARM_DELAY).is_none());
}

#[test]
fn zero_sized_and_negative_main_bounds_clear_the_preview() {
    let geometry = geometry(1.0);
    let pointer = target(geometry, Zone::Left);
    let mut drag = Drag::new(center(geometry));
    assert!(
        drag.update(pointer, Some(geometry), Duration::ZERO)
            .is_some()
    );
    for width in [0.0, -1.0] {
        let mut invalid = geometry;
        invalid.screen.width = width;
        assert!(drag.update(pointer, Some(invalid), ARM_DELAY).is_none());
    }
    let fresh = drag.update(pointer, Some(geometry), ARM_DELAY).unwrap();
    assert!(!fresh.armed);
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
fn left_edge_arms_after_delay_and_release_confirms() {
    let geometry = geometry(1.0);
    let pointer = target(geometry, Zone::Left);
    let mut drag = Drag::new(center(geometry));

    let preview = drag
        .update(pointer, Some(geometry), Duration::ZERO)
        .unwrap();
    assert_eq!(preview.zone, Zone::Left);
    assert!(!preview.armed);
    assert_eq!(
        preview.placement,
        Rect {
            x: 0.0,
            y: 0.0,
            width: 397.0,
            height: 600.0,
        }
    );

    assert!(
        drag.update(pointer, Some(geometry), ARM_DELAY)
            .unwrap()
            .armed
    );
    assert_eq!(
        drag.release(pointer, Some(geometry), ARM_DELAY),
        Some(Zone::Left)
    );
}

#[test]
fn release_before_dwell_does_not_confirm() {
    let geometry = geometry(1.0);
    let pointer = target(geometry, Zone::Left);
    let mut drag = Drag::new(center(geometry));
    drag.update(pointer, Some(geometry), Duration::ZERO);
    let too_early = ARM_DELAY - Duration::from_millis(1);
    assert_eq!(drag.release(pointer, Some(geometry), too_early), None);
}

#[test]
fn requires_minimum_movement_before_candidating() {
    let geometry = geometry(1.0);
    let start = Point(EDGE_INSET, 300.0);
    let mut drag = Drag::new(start);

    // Eight pixels of travel is below the movement threshold: still no candidate.
    assert_eq!(
        drag.update(
            Point(start.0 + 8.0, start.1),
            Some(geometry),
            Duration::ZERO
        ),
        None
    );

    // Crossing the threshold inside the left target arms the edge.
    let pointer = Point(start.0 + MIN_MOVEMENT, start.1);
    let preview = drag
        .update(pointer, Some(geometry), Duration::ZERO)
        .unwrap();
    assert_eq!(preview.zone, Zone::Left);
    assert!(!preview.armed);

    assert!(
        drag.update(pointer, Some(geometry), ARM_DELAY)
            .unwrap()
            .armed
    );
    assert_eq!(
        drag.release(pointer, Some(geometry), ARM_DELAY),
        Some(Zone::Left)
    );
}

#[test]
fn centre_of_window_is_not_a_target() {
    let geometry = geometry(1.0);
    let mut drag = Drag::new(center(geometry));
    assert_eq!(
        drag.update(Point(400.0, 200.0), Some(geometry), Duration::ZERO),
        None
    );
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
    drag.update(pointer, Some(geometry), Duration::ZERO);
    drag.update(pointer, Some(geometry), ARM_DELAY);

    // Exactly on the outer edge of the hysteresis band (36 + 22 + 8 = 66).
    let drift = Point(EDGE_INSET + TARGET_DEPTH + EXIT_MARGIN, 300.0);
    let preview = drag.update(drift, Some(geometry), ARM_DELAY).unwrap();
    assert_eq!(preview.zone, Zone::Left);
    assert!(preview.armed);
    assert_eq!(
        drag.release(drift, Some(geometry), ARM_DELAY),
        Some(Zone::Left)
    );
}

#[test]
fn leaving_edge_beyond_exit_margin_drops_candidate() {
    let geometry = geometry(1.0);
    let mut drag = Drag::new(center(geometry));
    let pointer = target(geometry, Zone::Left);
    drag.update(pointer, Some(geometry), Duration::ZERO);
    drag.update(pointer, Some(geometry), ARM_DELAY);

    // One pixel past the hysteresis band: the candidate is gone.
    let away = Point(EDGE_INSET + TARGET_DEPTH + EXIT_MARGIN + 1.0, 300.0);
    assert_eq!(drag.update(away, Some(geometry), ARM_DELAY), None);
    assert_eq!(drag.release(away, Some(geometry), ARM_DELAY), None);
}

#[test]
fn switching_edges_restarts_dwell() {
    let geometry = geometry(1.0);
    let mut drag = Drag::new(center(geometry));
    let left = target(geometry, Zone::Left);
    drag.update(left, Some(geometry), Duration::ZERO);
    drag.update(left, Some(geometry), ARM_DELAY);

    let right = target(geometry, Zone::Right);
    let preview = drag.update(right, Some(geometry), ARM_DELAY).unwrap();
    assert_eq!(preview.zone, Zone::Right);
    assert!(!preview.armed, "switching edges must restart the dwell");

    assert!(
        drag.update(right, Some(geometry), ARM_DELAY + ARM_DELAY)
            .unwrap()
            .armed
    );
    assert_eq!(
        drag.release(right, Some(geometry), ARM_DELAY + ARM_DELAY),
        Some(Zone::Right)
    );
}

#[test]
fn geometry_change_restarts_dwell() {
    let geometry_1x = geometry(1.0);
    let mut drag = Drag::new(center(geometry_1x));
    let pointer_1x = target(geometry_1x, Zone::Left);
    drag.update(pointer_1x, Some(geometry_1x), Duration::ZERO);
    drag.update(pointer_1x, Some(geometry_1x), ARM_DELAY);
    assert!(
        drag.update(pointer_1x, Some(geometry_1x), ARM_DELAY)
            .unwrap()
            .armed
    );

    // A different geometry invalidates the candidate and restarts the dwell.
    let geometry_2x = geometry(2.0);
    let pointer_2x = target(geometry_2x, Zone::Left);
    let preview = drag
        .update(pointer_2x, Some(geometry_2x), ARM_DELAY)
        .unwrap();
    assert_eq!(preview.zone, Zone::Left);
    assert!(!preview.armed);
}

#[test]
fn missing_geometry_cancels_the_candidate() {
    let geometry = geometry(1.0);
    let mut drag = Drag::new(center(geometry));
    let pointer = target(geometry, Zone::Left);
    drag.update(pointer, Some(geometry), Duration::ZERO);
    drag.update(pointer, Some(geometry), ARM_DELAY);

    // No geometry (the pointer left the window) clears the candidate.
    let after_leave = ARM_DELAY + Duration::from_millis(1);
    assert_eq!(drag.update(pointer, None, after_leave), None);

    // Re-entering restarts the dwell rather than resuming the old one.
    let reentry = ARM_DELAY + Duration::from_millis(2);
    let preview = drag.update(pointer, Some(geometry), reentry).unwrap();
    assert_eq!(preview.zone, Zone::Left);
    assert!(!preview.armed);

    let settled = ARM_DELAY + ARM_DELAY + Duration::from_millis(2);
    assert!(drag.update(pointer, Some(geometry), settled).unwrap().armed);
    assert_eq!(
        drag.release(pointer, Some(geometry), settled),
        Some(Zone::Left)
    );
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
        assert_eq!(
            drag.update(pointer, Some(geometry), Duration::ZERO),
            None,
            "{pointer:?}"
        );
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
    assert_eq!(
        drag.update(Point(EDGE_INSET, 300.0), Some(narrow), Duration::ZERO),
        None
    );

    // Top/bottom still fit because the height is large enough.
    let mut drag = Drag::new(center(narrow));
    assert_eq!(
        drag.update(Point(200.0, EDGE_INSET), Some(narrow), Duration::ZERO)
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

    drag.update(pointer, Some(geometry), Duration::ZERO);
    drag.update(pointer, Some(geometry), ARM_DELAY);
    assert_eq!(
        drag.release(pointer, Some(geometry), ARM_DELAY),
        Some(Zone::Left)
    );
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
    drag.update(pointer, Some(geometry), Duration::ZERO);
    let preview = drag.update(pointer, Some(geometry), ARM_DELAY).unwrap();
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
    assert_eq!(
        drag.release(pointer, Some(geometry), ARM_DELAY),
        Some(Zone::Left)
    );
}

#[test]
fn same_physical_offset_differs_by_scale() {
    // At 1x, 72px from the left is 72 logical: outside the 36±22 target.
    let geometry_1x = geometry(1.0);
    let mut drag_1x = Drag::new(center(geometry_1x));
    assert_eq!(
        drag_1x.update(Point(72.0, 300.0), Some(geometry_1x), Duration::ZERO),
        None
    );

    // At 2x the same 72px is 36 logical: inside the target.
    let geometry_2x = geometry(2.0);
    let mut drag_2x = Drag::new(center(geometry_2x));
    assert!(
        drag_2x
            .update(Point(72.0, 600.0), Some(geometry_2x), Duration::ZERO)
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
    drag.update(pointer, Some(geometry), Duration::ZERO);
    drag.update(pointer, Some(geometry), ARM_DELAY);
    assert_eq!(
        drag.release(pointer, Some(geometry), ARM_DELAY),
        Some(Zone::Left)
    );

    let fresh = Drag::new(start);
    assert_eq!(fresh.release(pointer, Some(geometry), Duration::ZERO), None);
}
