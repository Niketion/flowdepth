//! Geometry and intent for a single native title-bar drag.

#[cfg(test)]
mod tests;

use std::time::{Duration, Instant};

const MIN_MOVEMENT: f64 = 18.0;
const EDGE_DWELL: Duration = Duration::from_millis(180);
const CORNER_TIE: f64 = 16.0;
const GUIDE_PADDING: f64 = 10.0;
const GUIDE_HEADER: f64 = 26.0;
const TARGET_WIDTH: f64 = 104.0;
const TARGET_HEIGHT: f64 = 40.0;
const TARGET_GAP: f64 = 6.0;
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
    pub fn contains(self, Point(x, y): Point) -> bool {
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

    fn guide(self, pointer: Point) -> Option<Guide> {
        if !self.scale.is_finite() || self.scale <= 0.0 || !self.screen.contains(pointer) {
            return None;
        }
        let width = self.screen.width / self.scale;
        let height = self.screen.height / self.scale;
        if !width.is_finite() || !height.is_finite() {
            return None;
        }
        let horizontal = self.can_split(Zone::Left);
        let vertical = self.can_split(Zone::Top);
        if !horizontal && !vertical {
            return None;
        }
        let guide_width = 2.0 * (GUIDE_PADDING + TARGET_WIDTH) + TARGET_GAP;
        let rows = if horizontal && vertical { 2.0 } else { 1.0 };
        let guide_height = GUIDE_HEADER + GUIDE_PADDING + rows * (TARGET_HEIGHT + TARGET_GAP);
        let bounds = to_screen(
            Point(self.screen.x, self.screen.y),
            self.scale,
            Rect {
                x: (width - guide_width) / 2.0,
                y: (height - guide_height) / 2.0,
                width: guide_width,
                height: guide_height,
            },
        );
        Some(Guide {
            bounds,
            scale: self.scale,
            horizontal,
            vertical,
            preview: None,
        })
    }

    pub fn can_split(self, zone: Zone) -> bool {
        let width = self.screen.width / self.scale;
        let height = self.screen.height / self.scale;
        let (extent, cross_extent, minimum, cross_minimum) = match zone {
            Zone::Left | Zone::Right => (width, height, self.minimum.0, self.minimum.1),
            Zone::Top | Zone::Bottom => (height, width, self.minimum.1, self.minimum.0),
        };
        extent >= minimum + PANE_MIN + GRID_GAP && cross_extent >= cross_minimum.max(PANE_MIN)
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

    fn edge(self, pointer: Point, guide: Guide) -> Option<Zone> {
        let x = (pointer.0 - self.screen.x) / self.scale;
        let y = (pointer.1 - self.screen.y) / self.scale;
        let width = self.screen.width / self.scale;
        let height = self.screen.height / self.scale;
        let (horizontal, vertical) = (guide.horizontal, guide.vertical);
        let nearest_x = x.min(width - x);
        let nearest_y = y.min(height - y);
        let near_x = horizontal && nearest_x <= edge_depth(width);
        let near_y = vertical && nearest_y <= edge_depth(height);
        match (near_x, near_y) {
            (false, false) => None,
            (true, true) if (nearest_x - nearest_y).abs() < CORNER_TIE => None,
            (true, false) => Some(horizontal_edge(x, width)),
            (true, true) if nearest_x < nearest_y => Some(horizontal_edge(x, width)),
            _ if y < height / 2.0 => Some(Zone::Top),
            _ => Some(Zone::Bottom),
        }
    }
}

fn edge_depth(extent: f64) -> f64 {
    (extent * 0.12).clamp(56.0, 112.0)
}

fn horizontal_edge(x: f64, width: f64) -> Zone {
    if x < width / 2.0 {
        Zone::Left
    } else {
        Zone::Right
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
#[cfg_attr(target_os = "windows", derive(serde::Serialize, serde::Deserialize))]
pub enum Zone {
    Left,
    Right,
    Top,
    Bottom,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Preview {
    pub zone: Zone,
    pub placement: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Guide {
    pub bounds: Rect,
    pub scale: f64,
    horizontal: bool,
    vertical: bool,
    pub preview: Option<Preview>,
}

impl Guide {
    pub fn targets(self) -> impl Iterator<Item = (Zone, Rect)> {
        self.horizontal
            .then_some([Zone::Left, Zone::Right])
            .into_iter()
            .chain(self.vertical.then_some([Zone::Top, Zone::Bottom]))
            .enumerate()
            .flat_map(move |(row, zones)| {
                zones.into_iter().enumerate().map(move |(column, zone)| {
                    let rect = to_screen(
                        Point(self.bounds.x, self.bounds.y),
                        self.scale,
                        Rect {
                            x: GUIDE_PADDING + column as f64 * (TARGET_WIDTH + TARGET_GAP),
                            y: GUIDE_HEADER + row as f64 * (TARGET_HEIGHT + TARGET_GAP),
                            width: TARGET_WIDTH,
                            height: TARGET_HEIGHT,
                        },
                    );
                    // Match the exact pixels used by the native overlay.
                    let x = rect.x.round();
                    let y = rect.y.round();
                    (
                        zone,
                        Rect {
                            x,
                            y,
                            width: (rect.x + rect.width).round() - x,
                            height: (rect.y + rect.height).round() - y,
                        },
                    )
                })
            })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Candidate {
    zone: Zone,
    geometry: Geometry,
    via_edge: bool,
}

pub struct Drag {
    start: Point,
    candidate: Option<Candidate>,
    pending_edge: Option<(Candidate, Instant)>,
    moved: bool,
}

impl Drag {
    pub fn new(start: Point) -> Self {
        Self {
            start,
            candidate: None,
            pending_edge: None,
            moved: false,
        }
    }

    pub fn update(
        &mut self,
        pointer: Point,
        geometry: Option<Geometry>,
        now: Instant,
    ) -> Option<Guide> {
        self.candidate = None;
        let pending = self.pending_edge.take();
        let (geometry, mut guide) =
            geometry.and_then(|geometry| geometry.guide(pointer).map(|guide| (geometry, guide)))?;
        self.moved |= (pointer.0 - self.start.0).hypot(pointer.1 - self.start.1)
            >= MIN_MOVEMENT * geometry.scale;
        if !self.moved {
            return None;
        }

        if let Some(candidate) = Self::at(pointer, geometry, guide) {
            if candidate.via_edge {
                let since = pending
                    .filter(|(previous, _)| *previous == candidate)
                    .map_or(now, |(_, since)| since);
                self.pending_edge = Some((candidate, since));
                if now.duration_since(since) < EDGE_DWELL {
                    return Some(guide);
                }
            }
            self.candidate = Some(candidate);
            guide.preview = Some(Preview {
                zone: candidate.zone,
                placement: geometry.placement(candidate.zone),
            });
        }
        Some(guide)
    }

    #[cfg(target_os = "windows")]
    pub fn waiting_for_edge(&self) -> bool {
        self.pending_edge.is_some() && self.candidate.is_none()
    }

    #[cfg(target_os = "windows")]
    pub fn reset_target(&mut self) {
        self.pending_edge = None;
        self.candidate = None;
    }

    fn at(pointer: Point, geometry: Geometry, guide: Guide) -> Option<Candidate> {
        if let Some((zone, _)) = guide.targets().find(|(_, rect)| rect.contains(pointer)) {
            return Some(Candidate {
                zone,
                geometry,
                via_edge: false,
            });
        }
        if guide.bounds.contains(pointer) {
            return None;
        }
        geometry.edge(pointer, guide).map(|zone| Candidate {
            zone,
            geometry,
            via_edge: true,
        })
    }

    /// Confirm only the preview that was active before release.
    pub fn release(self, pointer: Point, geometry: Option<Geometry>) -> Option<Zone> {
        let candidate = self.candidate?;
        let geometry = geometry?;
        let current = Self::at(pointer, geometry, geometry.guide(pointer)?)?;
        (candidate == current).then_some(current.zone)
    }
}
