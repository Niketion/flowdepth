//! Geometry and intent for a single native title-bar drag.

use std::time::Duration;

#[cfg(test)]
mod tests;

pub const ARM_DELAY: Duration = Duration::from_millis(240);
const MIN_MOVEMENT: f64 = 18.0;
const EDGE_INSET: f64 = 36.0;
const TARGET_DEPTH: f64 = 22.0;
const TARGET_SPAN: f64 = 60.0;
const EXIT_MARGIN: f64 = 8.0;
pub const GRID_GAP: f64 = 6.0;
pub const PANE_MIN: f64 = 240.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point(pub f64, pub f64);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    fn contains(self, Point(x, y): Point) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
}

/// A dashboard rectangle in screen pixels and the scale of the receiving window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub screen: Rect,
    pub scale: f64,
    pub minimum: Point,
}

impl Geometry {
    pub fn from_client(origin: Point, scale: f64, grid: Rect, minimum: Point) -> Self {
        Self {
            screen: to_screen(origin, scale, grid),
            scale,
            minimum,
        }
    }

    fn local(self, point: Point) -> Option<(Rect, Point)> {
        let screen = self.screen;
        if !self.scale.is_finite() || self.scale <= 0.0 || !screen.contains(point) {
            return None;
        }
        let width = screen.width / self.scale;
        let height = screen.height / self.scale;
        if !width.is_finite() || !height.is_finite() {
            return None;
        }
        Some((
            Rect {
                x: 0.0,
                y: 0.0,
                width,
                height,
            },
            Point(
                (point.0 - screen.x) / self.scale,
                (point.1 - screen.y) / self.scale,
            ),
        ))
    }

    fn placement(self, zone: Zone) -> Rect {
        let mut area = self.screen;
        let (origin, length, minimum) = match zone {
            Zone::Left | Zone::Right => (&mut area.x, &mut area.width, self.minimum.0),
            Zone::Top | Zone::Bottom => (&mut area.y, &mut area.height, self.minimum.1),
        };
        let extent = *length / self.scale;
        let proposed = ((extent - GRID_GAP) / 2.0).round();
        let available = extent - GRID_GAP;
        let first = matches!(zone, Zone::Left | Zone::Top);
        let (min_a, min_b) = if first {
            (PANE_MIN, minimum)
        } else {
            (minimum, PANE_MIN)
        };
        let leading = proposed.max(min_a).min(available - min_b);
        *length = if first {
            leading * self.scale
        } else {
            *origin += (leading + GRID_GAP) * self.scale;
            (available - leading) * self.scale
        };
        area
    }
}

fn to_screen(origin: Point, scale: f64, rect: Rect) -> Rect {
    Rect {
        x: origin.0 + rect.x * scale,
        y: origin.1 + rect.y * scale,
        width: rect.width * scale,
        height: rect.height * scale,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
    Left,
    Right,
    Top,
    Bottom,
}

impl Zone {
    const ALL: [Self; 4] = [Self::Left, Self::Right, Self::Top, Self::Bottom];

    fn band(self, area: Rect, margin: f64) -> Rect {
        let depth = TARGET_DEPTH + margin;
        let span = TARGET_SPAN + margin;
        match self {
            Self::Left | Self::Right => Rect {
                x: if self == Self::Left {
                    EDGE_INSET
                } else {
                    area.width - EDGE_INSET
                } - depth,
                y: area.height / 2.0 - span,
                width: 2.0 * depth,
                height: 2.0 * span,
            },
            Self::Top | Self::Bottom => Rect {
                x: area.width / 2.0 - span,
                y: if self == Self::Top {
                    EDGE_INSET
                } else {
                    area.height - EDGE_INSET
                } - depth,
                width: 2.0 * span,
                height: 2.0 * depth,
            },
        }
    }

    fn contains(self, area: Rect, point: Point, minimum: Point, margin: f64) -> bool {
        let (extent, cross_extent, min, cross_min) = match self {
            Self::Left | Self::Right => (area.width, area.height, minimum.0, minimum.1),
            Self::Top | Self::Bottom => (area.height, area.width, minimum.1, minimum.0),
        };
        let band = self.band(area, margin);
        extent >= min + PANE_MIN + GRID_GAP
            && cross_extent >= cross_min.max(PANE_MIN)
            && point.0 >= band.x
            && point.0 <= band.x + band.width
            && point.1 >= band.y
            && point.1 <= band.y + band.height
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Preview {
    pub zone: Zone,
    pub placement: Rect,
    pub activation: Rect,
    pub armed: bool,
}

struct Candidate {
    zone: Zone,
    geometry: Geometry,
    since: Duration,
}

pub struct Drag {
    start: Point,
    candidate: Option<Candidate>,
    moved: bool,
}

impl Drag {
    pub fn new(start: Point) -> Self {
        Self {
            start,
            candidate: None,
            moved: false,
        }
    }

    pub fn update(
        &mut self,
        pointer: Point,
        geometry: Option<Geometry>,
        elapsed: Duration,
    ) -> Option<Preview> {
        let Some((geometry, area, local)) =
            geometry.and_then(|g| g.local(pointer).map(|(area, point)| (g, area, point)))
        else {
            self.candidate = None;
            return None;
        };
        self.moved |= (pointer.0 - self.start.0).hypot(pointer.1 - self.start.1)
            >= MIN_MOVEMENT * geometry.scale;
        if !self.moved {
            return None;
        }

        if self.candidate.as_ref().is_none_or(|candidate| {
            candidate.geometry != geometry
                || !candidate
                    .zone
                    .contains(area, local, geometry.minimum, EXIT_MARGIN)
        }) {
            self.candidate = Zone::ALL
                .into_iter()
                .find(|zone| zone.contains(area, local, geometry.minimum, 0.0))
                .map(|zone| Candidate {
                    zone,
                    geometry,
                    since: elapsed,
                });
        }

        self.candidate.as_ref().map(|candidate| Preview {
            zone: candidate.zone,
            placement: geometry.placement(candidate.zone),
            activation: to_screen(
                Point(geometry.screen.x, geometry.screen.y),
                geometry.scale,
                candidate.zone.band(area, 0.0),
            ),
            armed: elapsed.saturating_sub(candidate.since) >= ARM_DELAY,
        })
    }

    /// Consuming the drag guarantees that its release can confirm at most once.
    pub fn release(
        mut self,
        pointer: Point,
        geometry: Option<Geometry>,
        elapsed: Duration,
    ) -> Option<Zone> {
        self.update(pointer, geometry, elapsed)
            .filter(|preview| preview.armed)
            .map(|preview| preview.zone)
    }
}
