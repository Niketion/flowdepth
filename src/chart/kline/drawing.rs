use super::KlineChart;
use crate::chart::ticks::AxisOverlayLabel;
use crate::chart::{Chart, DrawingMessage, Interaction, Message};
use crate::widget::color_picker::color_picker;
use crate::widget::drag_handle;
use crate::widget::pick_list;
use data::chart::{
    Basis,
    kline::{SessionProfileMode, SessionProfilePlacement, drawing::*},
};
use exchange::{UnixMs, unit::Price};
use iced::widget::canvas::{self, Frame, Geometry, LineDash, Path, Stroke};
use iced::{
    Alignment, Color, Element, Length, Point, Rectangle, Renderer, Size, Theme, Vector, mouse,
    widget::{
        button, checkbox, column, container, mouse_area, opaque, row, scrollable, slider, space,
        svg, text, text_input,
    },
};
use iced_core::mouse::{Click, click};

#[derive(Debug, Clone)]
pub(super) enum Draft {
    TwoPoint {
        tool: DrawingTool,
        first: DrawingAnchor,
        preview: DrawingAnchor,
    },
    Freehand {
        points: Vec<DrawingAnchor>,
    },
    Text {
        anchor: DrawingAnchor,
        value: String,
    },
    Moving {
        id: u64,
        original: DrawingGeometry,
        start: DrawingAnchor,
    },
    Resizing {
        id: u64,
        original: DrawingGeometry,
        start_screen: Point,
        handle: u8,
        original_text_scale: f32,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FloatingPanel {
    Toolbar,
    DrawingList,
}

pub(super) struct DrawingState {
    pub active_tool: DrawingTool,
    pub drawings: Vec<Drawing>,
    pub selected: Option<u64>,
    pub draft: Option<Draft>,
    pub settings_open: bool,
    pub visible: bool,
    pub text_editor_position: Option<Point>,
    pub horizontal_line_price_input: Option<String>,
    pub toolbar_open: bool,
    pub toolbar_position: Point,
    pub drawing_list_open: bool,
    pub drawing_list_position: Point,
    dragging_panel: Option<FloatingPanel>,
    drag_grab_offset: Option<Point>,
    tool_styles: Vec<(DrawingTool, DrawingStyle)>,
    next_id: u64,
    pub text_input_id: iced::widget::Id,
}

impl Default for DrawingState {
    fn default() -> Self {
        Self {
            active_tool: DrawingTool::Select,
            drawings: Vec::new(),
            selected: None,
            draft: None,
            settings_open: false,
            visible: true,
            text_editor_position: None,
            horizontal_line_price_input: None,
            toolbar_open: true,
            toolbar_position: Point::new(8.0, 8.0),
            drawing_list_open: false,
            drawing_list_position: Point::new(48.0, 48.0),
            dragging_panel: None,
            drag_grab_offset: None,
            tool_styles: [
                DrawingTool::Pen,
                DrawingTool::HorizontalLine,
                DrawingTool::VerticalLine,
                DrawingTool::Rectangle,
                DrawingTool::Fibonacci,
                DrawingTool::TrendLine,
                DrawingTool::Text,
                DrawingTool::FixedRangeVolumeProfile,
            ]
            .into_iter()
            .map(|tool| (tool, DrawingStyle::default()))
            .collect(),
            next_id: 1,
            text_input_id: iced::widget::Id::unique(),
        }
    }
}

#[derive(Default)]
pub struct CanvasState {
    pub navigation: Interaction,
    pub previous_click: Option<Click>,
    pub previous_hit: Option<u64>,
}

/// Drawing coordinates are local to the canvas frame. `Rectangle::center` also
/// includes the widget's position in its parent, so using it here makes a
/// timestamp depend on where the chart is laid out and causes it to drift when
/// the scale changes.
fn drawing_canvas_center(bounds: Rectangle) -> Point {
    Point::new(bounds.width / 2.0, bounds.height / 2.0)
}

impl KlineChart {
    const MAX_FIXED_VOLUME_PROFILES: usize = 16;
    const MAX_FIXED_VOLUME_PROFILE_RANGE_MS: u64 = 7 * 24 * 60 * 60_000;
    const MAX_FIXED_VOLUME_PROFILE_CANDLES: usize = 10_000;

    pub(super) fn drawing_anchor(&self, point: Point, bounds: Rectangle) -> DrawingAnchor {
        let chart = self.state();
        let center = drawing_canvas_center(bounds);
        let world = (point - center) * (1.0 / chart.scaling) - chart.translation;
        let x = match chart.basis {
            Basis::Time(_) => DrawingX::Time(UnixMs::new(chart.x_to_interval(world.x))),
            Basis::Tick(_) => DrawingX::Tick(chart.x_to_interval(world.x)),
        };
        DrawingAnchor {
            x,
            price: chart.y_to_price(world.y),
        }
    }

    fn drawing_screen_point(&self, anchor: DrawingAnchor, bounds: Rectangle) -> Point {
        let chart = self.state();
        let x = match anchor.x {
            DrawingX::Time(value) => chart.interval_to_x(value.as_u64()),
            DrawingX::Tick(value) => chart.interval_to_x(value),
        };
        drawing_canvas_center(bounds)
            + (Vector::new(x, chart.price_to_y(anchor.price)) + chart.translation) * chart.scaling
    }

    fn volume_profile_screen_x(&self, time: UnixMs, bounds: Rectangle) -> f32 {
        let chart = self.state();
        drawing_canvas_center(bounds).x
            + (chart.interval_to_x(time.as_u64()) + chart.translation.x) * chart.scaling
    }

    fn snap_volume_profile_time(&self, x: DrawingX) -> Option<UnixMs> {
        let DrawingX::Time(requested) = x else {
            return None;
        };
        let data::chart::PlotData::TimeBased(timeseries) = &self.data_source else {
            return None;
        };
        let keys = timeseries.datapoints.keys().copied().collect::<Vec<_>>();
        snap_timestamp_to_candle(&keys, requested)
    }

    fn normalized_fixed_range(&self, first: UnixMs, second: UnixMs) -> Option<(UnixMs, UnixMs)> {
        let data::chart::PlotData::TimeBased(timeseries) = &self.data_source else {
            return None;
        };
        let keys = timeseries.datapoints.keys().copied().collect::<Vec<_>>();
        normalize_fixed_range(&keys, first, second)
    }

    fn normalize_fixed_range_drawings(&mut self) {
        let data::chart::PlotData::TimeBased(timeseries) = &self.data_source else {
            return;
        };
        let keys = timeseries.datapoints.keys().copied().collect::<Vec<_>>();
        for drawing in &mut self.drawings.drawings {
            if let DrawingGeometry::FixedRangeVolumeProfile { first, second } =
                &mut drawing.geometry
                && let Some((normalized_first, normalized_second)) =
                    normalize_fixed_range(&keys, *first, *second)
            {
                *first = normalized_first;
                *second = normalized_second;
            }
        }
    }

    pub(super) fn fixed_volume_profiles(
        &self,
    ) -> Vec<(UnixMs, UnixMs, FixedRangeVolumeProfileConfig)> {
        let Basis::Time(timeframe) = self.chart.basis else {
            return Vec::new();
        };
        if self.drawings.visible {
            self.drawings
                .drawings
                .iter()
                .filter(|drawing| drawing.visible)
                .filter_map(|drawing| {
                    let DrawingGeometry::FixedRangeVolumeProfile { first, second } =
                        drawing.geometry
                    else {
                        return None;
                    };
                    let (from, last) = if first <= second {
                        (first, second)
                    } else {
                        (second, first)
                    };
                    Some((
                        from,
                        last.saturating_add(timeframe.to_milliseconds()),
                        sanitize_volume_profile_config(drawing.style.fixed_range_volume_profile),
                    ))
                })
                .collect()
        } else {
            Vec::new()
        }
    }

    pub fn has_fixed_volume_profiles(&self) -> bool {
        self.drawings.drawings.iter().any(|drawing| {
            matches!(
                drawing.geometry,
                DrawingGeometry::FixedRangeVolumeProfile { .. }
            )
        })
    }

    pub(super) fn fixed_volume_profile_ready(&self, from: UnixMs, to: UnixMs) -> bool {
        let (Basis::Time(timeframe), data::chart::PlotData::TimeBased(timeseries)) =
            (self.chart.basis, &self.data_source)
        else {
            return false;
        };
        let (_, latest) = timeseries.timerange();
        let historical_to =
            super::historical_trade_target_to(latest, timeframe.to_milliseconds(), UnixMs::now());
        let required_to = to.min(historical_to);
        required_to <= from || self.is_trade_range_covered(from, required_to)
    }

    fn fixed_volume_profile_loading(&self, first: UnixMs, second: UnixMs) -> bool {
        let Basis::Time(timeframe) = self.chart.basis else {
            return true;
        };
        let from = first.min(second);
        let to = first
            .max(second)
            .saturating_add(timeframe.to_milliseconds());
        !self.fixed_volume_profile_ready(from, to)
    }

    fn style_for_tool(&self, tool: DrawingTool) -> DrawingStyle {
        self.drawings
            .tool_styles
            .iter()
            .find(|(candidate, _)| *candidate == tool)
            .map(|(_, style)| style.clone())
            .unwrap_or_default()
    }

    fn commit(&mut self, geometry: DrawingGeometry) {
        if matches!(geometry, DrawingGeometry::FixedRangeVolumeProfile { .. })
            && self
                .drawings
                .drawings
                .iter()
                .filter(|drawing| {
                    matches!(
                        drawing.geometry,
                        DrawingGeometry::FixedRangeVolumeProfile { .. }
                    )
                })
                .count()
                >= Self::MAX_FIXED_VOLUME_PROFILES
        {
            self.drawings.active_tool = DrawingTool::Select;
            return;
        }
        let id = self.drawings.next_id;
        self.drawings.next_id = self.drawings.next_id.saturating_add(1);
        let tool = geometry_tool(&geometry);
        self.drawings.drawings.push(Drawing {
            id,
            geometry,
            style: self.style_for_tool(tool),
            visible: true,
        });
        self.drawings.selected = Some(id);
        self.drawings.active_tool = DrawingTool::Select;
        self.chart.cache.clear_all();
    }

    pub fn handle_drawing(&mut self, message: &DrawingMessage) -> bool {
        if !matches!(self.kind, data::chart::KlineChartKind::Candles) {
            return false;
        }
        match message {
            DrawingMessage::ToolSelected(tool) => {
                self.drawings.active_tool = *tool;
                self.drawings.draft = None;
                self.drawings.selected = None;
                self.drawings.settings_open = false;
                self.drawings.horizontal_line_price_input = None;
            }
            DrawingMessage::PointerPressed(anchor, editor_position) => match self
                .drawings
                .active_tool
            {
                DrawingTool::Select => {
                    self.drawings.selected = None;
                    self.drawings.settings_open = false;
                }
                DrawingTool::HorizontalLine => self.commit(DrawingGeometry::HorizontalLine {
                    price: anchor.price,
                }),
                DrawingTool::VerticalLine => {
                    self.commit(DrawingGeometry::VerticalLine { x: anchor.x })
                }
                DrawingTool::Rectangle
                | DrawingTool::Fibonacci
                | DrawingTool::TrendLine
                | DrawingTool::FixedRangeVolumeProfile => {
                    let mut anchor = *anchor;
                    if self.drawings.active_tool == DrawingTool::FixedRangeVolumeProfile {
                        let Some(time) = self.snap_volume_profile_time(anchor.x) else {
                            return true;
                        };
                        anchor.x = DrawingX::Time(time);
                    }
                    if let Some(Draft::TwoPoint { tool, first, .. }) = self.drawings.draft.take() {
                        let geometry = match tool {
                            DrawingTool::Rectangle => DrawingGeometry::Rectangle {
                                first,
                                second: anchor,
                            },
                            DrawingTool::Fibonacci => DrawingGeometry::Fibonacci {
                                first,
                                second: anchor,
                            },
                            DrawingTool::FixedRangeVolumeProfile => {
                                let DrawingX::Time(first) = first.x else {
                                    return true;
                                };
                                let DrawingX::Time(second) = anchor.x else {
                                    return true;
                                };
                                let Some((first, second)) =
                                    self.normalized_fixed_range(first, second)
                                else {
                                    return true;
                                };
                                DrawingGeometry::FixedRangeVolumeProfile { first, second }
                            }
                            _ => DrawingGeometry::TrendLine {
                                first,
                                second: anchor,
                            },
                        };
                        self.commit(geometry);
                    } else {
                        self.drawings.draft = Some(Draft::TwoPoint {
                            tool: self.drawings.active_tool,
                            first: anchor,
                            preview: anchor,
                        });
                    }
                }
                DrawingTool::Pen => {
                    self.drawings.draft = Some(Draft::Freehand {
                        points: vec![*anchor],
                    });
                }
                DrawingTool::Text => {
                    self.drawings.draft = Some(Draft::Text {
                        anchor: *anchor,
                        value: String::new(),
                    });
                    self.drawings.text_editor_position = Some(*editor_position);
                }
            },
            DrawingMessage::PointerMoved(anchor, screen_point) => {
                let profile_time = self.snap_volume_profile_time(anchor.x);
                match &mut self.drawings.draft {
                    Some(Draft::TwoPoint { tool, preview, .. }) => {
                        *preview = *anchor;
                        if *tool == DrawingTool::FixedRangeVolumeProfile
                            && let Some(time) = profile_time
                        {
                            preview.x = DrawingX::Time(time);
                        }
                    }
                    Some(Draft::Freehand { points })
                        if points.last().is_none_or(|last| last != anchor) =>
                    {
                        points.push(*anchor);
                    }
                    Some(Draft::Moving {
                        original,
                        start,
                        id,
                    }) => {
                        if let Some(drawing) =
                            self.drawings.drawings.iter_mut().find(|d| d.id == *id)
                        {
                            drawing.geometry = translate_geometry(original, *start, *anchor);
                        }
                    }
                    Some(Draft::Resizing {
                        id,
                        original,
                        start_screen,
                        handle,
                        original_text_scale,
                    }) => {
                        if let Some(drawing) =
                            self.drawings.drawings.iter_mut().find(|d| d.id == *id)
                        {
                            drawing.geometry = resize_geometry(original, *anchor, *handle);
                            if matches!(original, DrawingGeometry::Text { .. }) {
                                let distance = (screen_point.x - start_screen.x)
                                    .hypot(screen_point.y - start_screen.y);
                                drawing.style.text_scale = (*original_text_scale
                                    * (1.0 + distance / 120.0))
                                    .clamp(0.4, 8.0);
                            }
                        }
                    }
                    _ => {}
                }
            }
            DrawingMessage::PointerReleased(_) => {
                if let Some(Draft::Freehand { points }) = self.drawings.draft.take() {
                    if points.len() > 1 {
                        self.commit(DrawingGeometry::Freehand { points });
                    } else {
                        self.drawings.active_tool = DrawingTool::Select;
                    }
                }
            }
            DrawingMessage::DoubleClicked(id) | DrawingMessage::OpenDrawingSettings(id) => {
                self.drawings.selected = Some(*id);
                self.drawings.settings_open = true;
                self.drawings.active_tool = DrawingTool::Select;
                self.drawings.horizontal_line_price_input = self
                    .drawings
                    .drawings
                    .iter()
                    .find(|drawing| drawing.id == *id)
                    .and_then(|drawing| match drawing.geometry {
                        DrawingGeometry::HorizontalLine { price } => {
                            Some(price_label(price, self.tick_size().decimal_places()))
                        }
                        _ => None,
                    });
            }
            DrawingMessage::MoveStarted(anchor) => {
                if let Some(id) = self.drawings.selected
                    && let Some(drawing) = self
                        .drawings
                        .drawings
                        .iter()
                        .find(|drawing| drawing.id == id)
                {
                    self.drawings.draft = Some(Draft::Moving {
                        id,
                        original: drawing.geometry.clone(),
                        start: *anchor,
                    });
                }
            }
            DrawingMessage::ResizeStarted(_anchor, screen_point, handle) => {
                if let Some(id) = self.drawings.selected
                    && let Some(drawing) = self
                        .drawings
                        .drawings
                        .iter()
                        .find(|drawing| drawing.id == id)
                {
                    self.drawings.draft = Some(Draft::Resizing {
                        id,
                        original: drawing.geometry.clone(),
                        start_screen: *screen_point,
                        handle: *handle,
                        original_text_scale: drawing.style.text_scale,
                    });
                }
            }
            DrawingMessage::MoveFinished => {
                if matches!(
                    self.drawings.draft,
                    Some(Draft::Moving { .. }) | Some(Draft::Resizing { .. })
                ) {
                    self.drawings.draft = None;
                    self.normalize_fixed_range_drawings();
                }
            }
            DrawingMessage::CancelOrCommit => match self.drawings.draft.take() {
                Some(Draft::Text { anchor, value }) if !value.trim().is_empty() => {
                    self.commit(DrawingGeometry::Text {
                        anchor,
                        content: value,
                    })
                }
                Some(Draft::Moving { .. })
                | Some(Draft::Resizing { .. })
                | Some(Draft::TwoPoint { .. })
                | Some(Draft::Freehand { .. })
                | Some(Draft::Text { .. }) => {}
                None => {
                    self.drawings.selected = None;
                    self.drawings.settings_open = false;
                }
            },
            DrawingMessage::TextChanged(value) => {
                if let Some(Draft::Text {
                    value: draft_value, ..
                }) = &mut self.drawings.draft
                {
                    *draft_value = value.clone();
                }
            }
            DrawingMessage::TextCommitted => {
                if let Some(Draft::Text { anchor, value }) = self.drawings.draft.take()
                    && !value.trim().is_empty()
                {
                    self.commit(DrawingGeometry::Text {
                        anchor,
                        content: value,
                    });
                }
                self.drawings.text_editor_position = None;
            }
            DrawingMessage::HorizontalLinePriceChanged(value) => {
                self.drawings.horizontal_line_price_input = Some(value.clone());
            }
            DrawingMessage::CommitHorizontalLinePrice => {
                let Some(input) = self.drawings.horizontal_line_price_input.as_deref() else {
                    return true;
                };
                let Some(value) = parse_horizontal_line_price(input) else {
                    return true;
                };
                let price = Price::from_f64(value).round_to_step(self.tick_size());
                let decimals = self.tick_size().decimal_places();
                if let Some(drawing) = self.drawings.drawings.iter_mut().find(|drawing| {
                    Some(drawing.id) == self.drawings.selected
                        && matches!(drawing.geometry, DrawingGeometry::HorizontalLine { .. })
                }) {
                    drawing.geometry = DrawingGeometry::HorizontalLine { price };
                    self.drawings.horizontal_line_price_input = Some(price_label(price, decimals));
                }
            }
            DrawingMessage::CloseSettings => {
                self.drawings.settings_open = false;
                self.drawings.horizontal_line_price_input = None;
            }
            DrawingMessage::DeleteSelected => {
                if let Some(id) = self.drawings.selected {
                    self.drawings.drawings.retain(|drawing| drawing.id != id);
                }
                self.drawings.selected = None;
                self.drawings.settings_open = false;
                self.drawings.horizontal_line_price_input = None;
            }
            DrawingMessage::DeleteDrawing(id) => {
                self.drawings.drawings.retain(|drawing| drawing.id != *id);
                if self.drawings.selected == Some(*id) {
                    self.drawings.selected = None;
                    self.drawings.settings_open = false;
                }
                if self.drawings.drawings.is_empty() {
                    self.drawings.drawing_list_open = false;
                }
            }
            DrawingMessage::ToggleDrawingsVisibility => {
                self.drawings.visible = !self.drawings.visible;
                self.drawings.selected = None;
                self.drawings.settings_open = false;
                self.drawings.draft = None;
                self.drawings.text_editor_position = None;
            }
            DrawingMessage::ClearAllDrawings => {
                self.drawings.drawings.clear();
                self.drawings.drawing_list_open = false;
                self.drawings.selected = None;
                self.drawings.settings_open = false;
                self.drawings.draft = None;
                self.drawings.active_tool = DrawingTool::Select;
                self.drawings.text_editor_position = None;
            }
            DrawingMessage::SetColor(color) => self.modify_selected_style(|style| {
                style.color = *color;
                style.fill_color = *color;
            }),
            DrawingMessage::SetStrokeWidth(value) => {
                self.modify_selected_style(|style| style.stroke_width = *value)
            }
            DrawingMessage::SetOpacity(value) => {
                self.modify_selected_style(|style| style.opacity = *value)
            }
            DrawingMessage::SetFillOpacity(value) => {
                self.modify_selected_style(|style| style.fill_opacity = *value)
            }
            DrawingMessage::SetTextSize(value) => {
                self.modify_selected_style(|style| style.text_size = *value)
            }
            DrawingMessage::ToggleLabels(value) => {
                self.modify_selected_style(|style| style.show_labels = *value)
            }
            DrawingMessage::ToggleFibonacciLevel(index) => self.modify_selected_style(|style| {
                if let Some(level) = style.fibonacci_levels.get_mut(*index) {
                    level.visible = !level.visible;
                }
            }),
            DrawingMessage::SetFixedRangeVolumeProfile(config) => {
                self.modify_selected_style(|style| {
                    style.fixed_range_volume_profile = sanitize_volume_profile_config(*config)
                })
            }
            DrawingMessage::ToggleToolbar => {
                self.drawings.toolbar_open = !self.drawings.toolbar_open
            }
            DrawingMessage::ToggleDrawingVisibility(id) => {
                if let Some(drawing) = self
                    .drawings
                    .drawings
                    .iter_mut()
                    .find(|drawing| drawing.id == *id)
                {
                    drawing.visible = !drawing.visible;
                }
            }
            DrawingMessage::FocusDrawing(id) => self.focus_drawing(*id),
            DrawingMessage::ToggleDrawingList => {
                self.drawings.drawing_list_open =
                    !self.drawings.drawings.is_empty() && !self.drawings.drawing_list_open;
            }
            DrawingMessage::ToolbarDragStarted(_) => {
                self.drawings.dragging_panel = Some(FloatingPanel::Toolbar);
                self.drawings.drag_grab_offset = None;
            }
            DrawingMessage::ToolbarDragged(_) => {}
            DrawingMessage::ToolbarDragEnded => self.stop_dragging_floating_panel(),
            DrawingMessage::DrawingListDragStarted(_) => {
                self.drawings.dragging_panel = Some(FloatingPanel::DrawingList);
                self.drawings.drag_grab_offset = None;
            }
            DrawingMessage::DrawingListDragged(_) => {}
            DrawingMessage::DrawingListDragEnded => self.stop_dragging_floating_panel(),
            DrawingMessage::FloatingPanelDragged(position) => {
                if let Some(panel) = self.drawings.dragging_panel {
                    self.drag_floating_panel(panel, *position);
                }
            }
            DrawingMessage::FloatingPanelDragEnded => self.stop_dragging_floating_panel(),
        }
        self.chart.cache.clear_all();
        if self.has_fixed_volume_profiles() {
            self.last_tick = std::time::Instant::now() - std::time::Duration::from_secs(1);
        }
        true
    }

    fn drag_floating_panel(&mut self, panel: FloatingPanel, pointer: Point) {
        if self.drawings.dragging_panel != Some(panel) {
            return;
        }
        let Some(last_pointer) = self.drawings.drag_grab_offset else {
            self.drawings.drag_grab_offset = Some(pointer);
            return;
        };
        let delta = pointer - last_pointer;
        self.drawings.drag_grab_offset = Some(pointer);
        let (width, height) = self.floating_panel_size(panel);
        let max_x = (self.chart.bounds.width - width).max(0.0);
        let max_y = (self.chart.bounds.height - height).max(0.0);
        let position = match panel {
            FloatingPanel::Toolbar => &mut self.drawings.toolbar_position,
            FloatingPanel::DrawingList => &mut self.drawings.drawing_list_position,
        };
        position.x = (position.x + delta.x).clamp(0.0, max_x);
        position.y = (position.y + delta.y).clamp(0.0, max_y);
    }

    fn floating_panel_position(&self, panel: FloatingPanel) -> Point {
        let position = match panel {
            FloatingPanel::Toolbar => self.drawings.toolbar_position,
            FloatingPanel::DrawingList => self.drawings.drawing_list_position,
        };
        let (width, height) = self.floating_panel_size(panel);
        Point::new(
            position
                .x
                .clamp(0.0, (self.chart.bounds.width - width).max(0.0)),
            position
                .y
                .clamp(0.0, (self.chart.bounds.height - height).max(0.0)),
        )
    }

    fn floating_panel_size(&self, panel: FloatingPanel) -> (f32, f32) {
        match panel {
            FloatingPanel::Toolbar => (
                44.0,
                toolbar_panel_height(
                    self.drawings.toolbar_open,
                    !self.drawings.drawings.is_empty(),
                )
                .min(self.chart.bounds.height),
            ),
            FloatingPanel::DrawingList => (
                288.0,
                36.0 + (self.drawings.drawings.len() as f32 * 30.0).clamp(30.0, 240.0),
            ),
        }
    }

    fn stop_dragging_floating_panel(&mut self) {
        self.drawings.dragging_panel = None;
        self.drawings.drag_grab_offset = None;
    }

    fn focus_drawing(&mut self, id: u64) {
        let Some(drawing) = self
            .drawings
            .drawings
            .iter()
            .find(|drawing| drawing.id == id)
        else {
            return;
        };
        if let DrawingGeometry::HorizontalLine { price } = drawing.geometry {
            self.chart.translation.y = -self.chart.price_to_y(price);
            self.drawings.selected = Some(id);
            self.drawings.active_tool = DrawingTool::Select;
            return;
        }
        if let DrawingGeometry::VerticalLine { x } = drawing.geometry {
            let x = match x {
                DrawingX::Time(value) => self.chart.interval_to_x(value.as_u64()),
                DrawingX::Tick(value) => self.chart.interval_to_x(value),
            };
            self.chart.translation.x = -x;
            self.drawings.selected = Some(id);
            self.drawings.active_tool = DrawingTool::Select;
            return;
        }
        if let DrawingGeometry::FixedRangeVolumeProfile { first, second } = drawing.geometry {
            let midpoint = UnixMs::new((first.as_u64() + second.as_u64()) / 2);
            self.chart.translation.x = -self.chart.interval_to_x(midpoint.as_u64());
            self.drawings.selected = Some(id);
            self.drawings.active_tool = DrawingTool::Select;
            return;
        }
        let Some(anchor) = drawing_focus_anchor(&drawing.geometry) else {
            return;
        };
        let x = match anchor.x {
            DrawingX::Time(value) => self.chart.interval_to_x(value.as_u64()),
            DrawingX::Tick(value) => self.chart.interval_to_x(value),
        };
        self.chart.translation = Vector::new(-x, -self.chart.price_to_y(anchor.price));
        self.drawings.selected = Some(id);
        self.drawings.active_tool = DrawingTool::Select;
    }

    fn modify_selected_style(&mut self, apply: impl FnOnce(&mut DrawingStyle)) {
        if let Some(id) = self.drawings.selected
            && let Some(index) = self
                .drawings
                .drawings
                .iter()
                .position(|drawing| drawing.id == id)
        {
            let drawing = &mut self.drawings.drawings[index];
            apply(&mut drawing.style);
            let tool = geometry_tool(&drawing.geometry);
            let style = drawing.style.clone();
            if let Some((_, default_style)) = self
                .drawings
                .tool_styles
                .iter_mut()
                .find(|(candidate, _)| *candidate == tool)
            {
                *default_style = style;
            }
        }
    }

    pub(super) fn drawing_hit_test(&self, point: Point, bounds: Rectangle) -> Option<u64> {
        self.drawings
            .drawings
            .iter()
            .rev()
            .filter(|drawing| drawing.visible)
            .find(|drawing| hit_test(self, drawing, point, bounds))
            .map(|drawing| drawing.id)
    }

    fn drawing_handle_hit(&self, id: u64, point: Point, bounds: Rectangle) -> Option<u8> {
        let drawing = self
            .drawings
            .drawings
            .iter()
            .find(|drawing| drawing.id == id)?;
        if let DrawingGeometry::FixedRangeVolumeProfile { first, second } = drawing.geometry {
            let first_x = self.volume_profile_screen_x(first, bounds);
            let second_x = self.volume_profile_screen_x(second, bounds);
            if (point.x - first_x).abs() <= 8.0 && point.y >= 0.0 && point.y <= bounds.height {
                return Some(0);
            }
            if (point.x - second_x).abs() <= 8.0 && point.y >= 0.0 && point.y <= bounds.height {
                return Some(1);
            }
        }
        drawing_handles(self, drawing, bounds)
            .iter()
            .enumerate()
            .filter(|(index, _)| {
                !matches!(drawing.geometry, DrawingGeometry::Text { .. }) || *index == 1
            })
            .find(|(_, handle)| (point.x - handle.x).hypot(point.y - handle.y) <= 8.0)
            .map(|(index, _)| index as u8)
    }

    pub(super) fn drawing_canvas_update(
        &self,
        state: &mut CanvasState,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let local = cursor.position_in(bounds);
        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let point = local?;
                let anchor = self.drawing_anchor(point, bounds);
                if self.drawings.active_tool != DrawingTool::Select {
                    return Some(
                        canvas::Action::publish(Message::Drawing(DrawingMessage::PointerPressed(
                            anchor, point,
                        )))
                        .and_capture(),
                    );
                }
                if let Some(id) = self.drawing_hit_test(point, bounds) {
                    let click = cursor.position().map(|global| {
                        Click::new(global, mouse::Button::Left, state.previous_click)
                    });
                    let is_double = click
                        .as_ref()
                        .is_some_and(|click| click.kind() == click::Kind::Double)
                        && state.previous_hit == Some(id);
                    state.previous_click = click;
                    state.previous_hit = Some(id);
                    let message = if is_double {
                        DrawingMessage::DoubleClicked(id)
                    } else if self.drawings.selected == Some(id)
                        && let Some(handle) = self.drawing_handle_hit(id, point, bounds)
                    {
                        DrawingMessage::ResizeStarted(anchor, point, handle)
                    } else if self.drawings.selected == Some(id) {
                        DrawingMessage::MoveStarted(anchor)
                    } else {
                        // The first click belongs to chart navigation; a double-click selects.
                        return super::super::canvas_interaction(
                            self,
                            &mut state.navigation,
                            event,
                            bounds,
                            cursor,
                        );
                    };
                    return Some(canvas::Action::publish(Message::Drawing(message)).and_capture());
                }
                state.previous_click = None;
                state.previous_hit = None;
                if self.drawings.selected.is_some() {
                    return Some(
                        canvas::Action::publish(Message::Drawing(DrawingMessage::CancelOrCommit))
                            .and_capture(),
                    );
                }
                super::super::canvas_interaction(self, &mut state.navigation, event, bounds, cursor)
            }
            canvas::Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if matches!(
                    self.drawings.draft,
                    Some(Draft::TwoPoint { .. })
                        | Some(Draft::Freehand { .. })
                        | Some(Draft::Moving { .. })
                        | Some(Draft::Resizing { .. })
                ) && let Some(point) = local
                {
                    return Some(
                        canvas::Action::publish(Message::Drawing(DrawingMessage::PointerMoved(
                            self.drawing_anchor(point, bounds),
                            point,
                        )))
                        .and_capture(),
                    );
                }
                super::super::canvas_interaction(self, &mut state.navigation, event, bounds, cursor)
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                match &self.drawings.draft {
                    Some(Draft::Freehand { .. }) => {
                        if let Some(point) = local {
                            return Some(
                                canvas::Action::publish(Message::Drawing(
                                    DrawingMessage::PointerReleased(
                                        self.drawing_anchor(point, bounds),
                                    ),
                                ))
                                .and_capture(),
                            );
                        }
                        return Some(canvas::Action::capture());
                    }
                    Some(Draft::Moving { .. }) | Some(Draft::Resizing { .. }) => {
                        return Some(
                            canvas::Action::publish(Message::Drawing(DrawingMessage::MoveFinished))
                                .and_capture(),
                        );
                    }
                    Some(Draft::TwoPoint { .. }) | Some(Draft::Text { .. }) => {
                        return Some(canvas::Action::capture());
                    }
                    None => {}
                }
                super::super::canvas_interaction(self, &mut state.navigation, event, bounds, cursor)
            }
            canvas::Event::Keyboard(iced::keyboard::Event::KeyPressed { key, .. })
                if matches!(
                    key.as_ref(),
                    iced::keyboard::Key::Named(
                        iced::keyboard::key::Named::Escape | iced::keyboard::key::Named::Enter
                    )
                ) =>
            {
                Some(
                    canvas::Action::publish(Message::Drawing(DrawingMessage::CancelOrCommit))
                        .and_capture(),
                )
            }
            canvas::Event::Keyboard(iced::keyboard::Event::KeyPressed { key, .. })
                if matches!(
                    key.as_ref(),
                    iced::keyboard::Key::Named(iced::keyboard::key::Named::Delete)
                ) =>
            {
                Some(
                    canvas::Action::publish(Message::Drawing(DrawingMessage::DeleteSelected))
                        .and_capture(),
                )
            }
            _ => {
                super::super::canvas_interaction(self, &mut state.navigation, event, bounds, cursor)
            }
        }
    }

    pub(super) fn draw_drawings(
        &self,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
    ) -> Geometry {
        let mut frame = Frame::new(renderer, bounds.size());
        if !self.drawings.visible {
            return frame.into_geometry();
        }
        for drawing in &self.drawings.drawings {
            if !drawing.visible {
                continue;
            }
            draw_one(
                self,
                &mut frame,
                drawing,
                bounds,
                Some(drawing.id) == self.drawings.selected,
            );
        }
        if let Some(draft) = &self.drawings.draft {
            draw_draft(self, &mut frame, draft, bounds, theme);
        }
        frame.into_geometry()
    }

    pub(super) fn drawing_overlay(&self) -> Element<'_, Message> {
        let toolbar = toolbar(
            self.drawings.active_tool,
            self.drawings.visible,
            self.drawings.toolbar_open,
            self.drawings.drawing_list_open,
            !self.drawings.drawings.is_empty(),
            self.chart.bounds.height,
        );
        let mut layers: Vec<Element<'_, Message>> = vec![
            container(toolbar)
                .padding(
                    iced::padding::top(self.floating_panel_position(FloatingPanel::Toolbar).y)
                        .left(self.floating_panel_position(FloatingPanel::Toolbar).x),
                )
                .align_x(Alignment::Start)
                .align_y(Alignment::Start)
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
        ];
        if self.drawings.visible
            && self.drawings.drawing_list_open
            && !self.drawings.drawings.is_empty()
        {
            layers.push(
                container(drawing_list(self))
                    .padding(
                        iced::padding::top(
                            self.floating_panel_position(FloatingPanel::DrawingList).y,
                        )
                        .left(self.floating_panel_position(FloatingPanel::DrawingList).x),
                    )
                    .align_x(Alignment::Start)
                    .align_y(Alignment::Start)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into(),
            );
        }
        if self.drawings.dragging_panel.is_some() {
            layers.push(
                mouse_area(
                    container(space::horizontal())
                        .width(Length::Fill)
                        .height(Length::Fill),
                )
                .on_move(|position| {
                    Message::Drawing(DrawingMessage::FloatingPanelDragged(position))
                })
                .on_release(Message::Drawing(DrawingMessage::FloatingPanelDragEnded))
                .into(),
            );
        }
        if self.drawings.visible && self.drawings.settings_open && self.drawings.selected.is_some()
        {
            layers.push(
                container(opaque(settings(self)))
                    .padding(8)
                    .align_x(Alignment::End)
                    .align_y(Alignment::Start)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into(),
            );
        }
        if self.drawings.visible
            && let Some(Draft::Text { anchor, value }) = &self.drawings.draft
        {
            let point = self.drawings.text_editor_position.unwrap_or_else(|| {
                self.drawing_screen_point(
                    *anchor,
                    Rectangle::with_size(Size::new(
                        self.chart.bounds.width,
                        self.chart.bounds.height,
                    )),
                )
            });
            let input = text_input("Text", value)
                .id(self.drawings.text_input_id.clone())
                .on_input(|value| Message::Drawing(DrawingMessage::TextChanged(value)))
                .on_submit(Message::Drawing(DrawingMessage::TextCommitted))
                .width(180)
                .size(self.style_for_tool(DrawingTool::Text).text_size);
            layers.push(
                container(opaque(input))
                    .padding(iced::padding::top(point.y.max(0.0)).left(point.x.max(0.0)))
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into(),
            );
        }
        iced::widget::stack(layers).into()
    }

    pub(super) fn axis_drawing_labels(&self) -> (Vec<AxisOverlayLabel>, Vec<AxisOverlayLabel>) {
        if !self.drawings.visible {
            return (Vec::new(), Vec::new());
        }

        let bounds = self.chart.bounds;
        let mut x_labels = Vec::new();
        let mut y_labels = Vec::new();
        for drawing in &self.drawings.drawings {
            if !drawing.visible {
                continue;
            }
            match &drawing.geometry {
                DrawingGeometry::HorizontalLine { price } => {
                    let y = self
                        .drawing_screen_point(
                            DrawingAnchor {
                                x: DrawingX::Tick(0),
                                price: *price,
                            },
                            bounds,
                        )
                        .y;
                    y_labels.push(AxisOverlayLabel {
                        position: y,
                        content: price_label(*price, self.tick_size().decimal_places()),
                        color: color(drawing.style.color, drawing.style.opacity),
                    });
                }
                DrawingGeometry::VerticalLine { x } => {
                    let x_position = self
                        .drawing_screen_point(
                            DrawingAnchor {
                                x: *x,
                                price: Price::from_units(0),
                            },
                            bounds,
                        )
                        .x;
                    x_labels.push(AxisOverlayLabel {
                        position: x_position,
                        content: x_label(*x),
                        color: color(drawing.style.color, drawing.style.opacity),
                    });
                }
                _ => {}
            }
        }
        (x_labels, y_labels)
    }

    pub fn drawings(&self) -> Vec<Drawing> {
        self.drawings.drawings.clone()
    }
    pub fn set_drawings(&mut self, drawings: Vec<Drawing>) {
        self.drawings.next_id = drawings
            .iter()
            .map(|drawing| drawing.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        self.drawings.drawings = drawings;
        self.drawings.selected = None;
        self.drawings.draft = None;
    }
    pub fn drawing_text_input_id(&self) -> Option<iced::widget::Id> {
        matches!(self.drawings.draft, Some(Draft::Text { .. }))
            .then(|| self.drawings.text_input_id.clone())
    }
}

fn color(value: DrawingColor, opacity: f32) -> Color {
    Color {
        a: (value.a * opacity).clamp(0.0, 1.0),
        r: value.r,
        g: value.g,
        b: value.b,
    }
}

fn snap_timestamp_to_candle(keys: &[UnixMs], requested: UnixMs) -> Option<UnixMs> {
    let first = *keys.first()?;
    let last = *keys.last()?;
    if requested <= first {
        return Some(first);
    }
    if requested >= last {
        return Some(last);
    }
    let index = keys.partition_point(|value| *value < requested);
    let after = keys.get(index).copied().unwrap_or(last);
    let before = keys.get(index.saturating_sub(1)).copied().unwrap_or(first);
    (requested.as_u64().saturating_sub(before.as_u64())
        <= after.as_u64().saturating_sub(requested.as_u64()))
    .then_some(before)
    .or(Some(after))
}

fn normalize_fixed_range(
    keys: &[UnixMs],
    first: UnixMs,
    second: UnixMs,
) -> Option<(UnixMs, UnixMs)> {
    let first = snap_timestamp_to_candle(keys, first)?;
    let mut second = snap_timestamp_to_candle(keys, second)?;
    let first_index = keys.binary_search(&first).ok()?;
    let second_index = keys.binary_search(&second).ok()?;
    let max_index_distance = KlineChart::MAX_FIXED_VOLUME_PROFILE_CANDLES.saturating_sub(1);
    second = if second >= first {
        let latest_time = UnixMs::new(
            first
                .as_u64()
                .saturating_add(KlineChart::MAX_FIXED_VOLUME_PROFILE_RANGE_MS),
        );
        let latest_time_index = keys.partition_point(|value| *value <= latest_time);
        let max_index = (first_index + max_index_distance)
            .min(latest_time_index.saturating_sub(1))
            .min(keys.len().saturating_sub(1));
        keys[second_index.min(max_index)]
    } else {
        let earliest_time = UnixMs::new(
            first
                .as_u64()
                .saturating_sub(KlineChart::MAX_FIXED_VOLUME_PROFILE_RANGE_MS),
        );
        let earliest_time_index = keys.partition_point(|value| *value < earliest_time);
        let min_index = first_index
            .saturating_sub(max_index_distance)
            .max(earliest_time_index);
        keys[second_index.max(min_index)]
    };
    Some((first, second))
}

fn sanitize_volume_profile_config(
    mut config: FixedRangeVolumeProfileConfig,
) -> FixedRangeVolumeProfileConfig {
    config.width_percent = if config.width_percent.is_finite() {
        config.width_percent.clamp(10.0, 90.0)
    } else {
        FixedRangeVolumeProfileConfig::default().width_percent
    };
    config.value_area_percent = if config.value_area_percent.is_finite() {
        config.value_area_percent.clamp(50.0, 95.0)
    } else {
        FixedRangeVolumeProfileConfig::default().value_area_percent
    };
    config.row_size_ticks = config.row_size_ticks.clamp(1, 50);
    config
}

fn geometry_tool(geometry: &DrawingGeometry) -> DrawingTool {
    match geometry {
        DrawingGeometry::Freehand { .. } => DrawingTool::Pen,
        DrawingGeometry::HorizontalLine { .. } => DrawingTool::HorizontalLine,
        DrawingGeometry::VerticalLine { .. } => DrawingTool::VerticalLine,
        DrawingGeometry::Rectangle { .. } => DrawingTool::Rectangle,
        DrawingGeometry::Fibonacci { .. } => DrawingTool::Fibonacci,
        DrawingGeometry::TrendLine { .. } => DrawingTool::TrendLine,
        DrawingGeometry::Text { .. } => DrawingTool::Text,
        DrawingGeometry::FixedRangeVolumeProfile { .. } => DrawingTool::FixedRangeVolumeProfile,
    }
}

fn drawing_focus_anchor(geometry: &DrawingGeometry) -> Option<DrawingAnchor> {
    match geometry {
        DrawingGeometry::HorizontalLine { price } => Some(DrawingAnchor {
            x: DrawingX::Tick(0),
            price: *price,
        }),
        DrawingGeometry::VerticalLine { x } => Some(DrawingAnchor {
            x: *x,
            price: Price::from_units(0),
        }),
        DrawingGeometry::TrendLine { first, second }
        | DrawingGeometry::Rectangle { first, second }
        | DrawingGeometry::Fibonacci { first, second } => Some(DrawingAnchor {
            x: midpoint_x(first.x, second.x),
            price: Price::from_units((first.price.units + second.price.units) / 2),
        }),
        DrawingGeometry::Freehand { points } => points.first().copied(),
        DrawingGeometry::Text { anchor, .. } => Some(*anchor),
        DrawingGeometry::FixedRangeVolumeProfile { first, second } => Some(DrawingAnchor {
            x: DrawingX::Time(UnixMs::new((first.as_u64() + second.as_u64()) / 2)),
            price: Price::from_units(0),
        }),
    }
}

fn midpoint_x(first: DrawingX, second: DrawingX) -> DrawingX {
    match (first, second) {
        (DrawingX::Time(first), DrawingX::Time(second)) => {
            DrawingX::Time(UnixMs::new((first.as_u64() + second.as_u64()) / 2))
        }
        (DrawingX::Tick(first), DrawingX::Tick(second)) => DrawingX::Tick((first + second) / 2),
        (first, _) => first,
    }
}

fn line(style: &DrawingStyle) -> Stroke<'static> {
    Stroke::with_color(
        Stroke::default().with_width(style.stroke_width),
        color(style.color, style.opacity),
    )
}

fn price_label(price: Price, decimals: usize) -> String {
    let sign = if price.units < 0 { "-" } else { "" };
    let absolute = price.units.unsigned_abs();
    let scale = 100_000_000_000_u64;
    let integer = absolute / scale;
    if decimals == 0 {
        return format!("{sign}{integer}");
    }
    let fraction = absolute % scale;
    let divisor = 10_u64.pow((11_usize.saturating_sub(decimals)) as u32);
    format!(
        "{sign}{integer}.{:0width$}",
        fraction / divisor,
        width = decimals
    )
}

/// Parses prices pasted from common market formats. A single separator followed
/// by three digits is treated as a thousands separator, so both `63,000` and
/// `63.000` mean 63000. The final separator is otherwise the decimal mark.
fn parse_horizontal_line_price(input: &str) -> Option<f64> {
    let compact = input
        .trim()
        .chars()
        .filter(|character| !matches!(character, ' ' | '\u{a0}' | '_' | '\''))
        .collect::<String>();
    let (negative, body) = if let Some(value) = compact.strip_prefix('-') {
        (true, value)
    } else if let Some(value) = compact.strip_prefix('+') {
        (false, value)
    } else {
        (false, compact.as_str())
    };
    if body.is_empty()
        || body
            .chars()
            .any(|character| !character.is_ascii_digit() && character != '.' && character != ',')
    {
        return None;
    }

    let separators = body
        .char_indices()
        .filter(|(_, character)| matches!(character, '.' | ','))
        .collect::<Vec<_>>();
    let decimal_position = separators.last().and_then(|(position, _)| {
        let decimals = body[position + 1..]
            .chars()
            .take_while(|character| character.is_ascii_digit())
            .count();
        if decimals == 0 {
            return None;
        }
        let has_both_separator_kinds = separators.iter().any(|(_, character)| *character == '.')
            && separators.iter().any(|(_, character)| *character == ',');
        if has_both_separator_kinds || decimals != 3 {
            Some(*position)
        } else {
            None
        }
    });

    if !separators.is_empty() && decimal_position.is_none() {
        let groups = body.split(['.', ',']).collect::<Vec<_>>();
        if groups.iter().any(|group| group.is_empty())
            || groups.iter().skip(1).any(|group| group.len() != 3)
        {
            return None;
        }
    }

    let mut normalized = String::with_capacity(body.len() + usize::from(negative));
    if negative {
        normalized.push('-');
    }
    for (position, character) in body.char_indices() {
        if character.is_ascii_digit() {
            normalized.push(character);
        } else if Some(position) == decimal_position {
            normalized.push('.');
        }
    }
    normalized
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

fn x_label(x: DrawingX) -> String {
    match x {
        DrawingX::Time(value) => value
            .format_utc("%H:%M:%S")
            .unwrap_or_else(|| value.as_u64().to_string()),
        DrawingX::Tick(value) => value.to_string(),
    }
}

fn fixed_range_guide_stroke() -> Stroke<'static> {
    Stroke {
        line_dash: LineDash {
            segments: &[6.0, 4.0],
            offset: 0,
        },
        ..Stroke::default()
            .with_width(1.0)
            .with_color(Color::from_rgb(0.55, 0.57, 0.60))
    }
}

fn draw_fixed_range_volume_profile_guides(
    frame: &mut Frame,
    first_x: f32,
    second_x: f32,
    bounds: Rectangle,
    stroke: Stroke<'_>,
) {
    frame.stroke(
        &Path::line(Point::new(first_x, 0.0), Point::new(first_x, bounds.height)),
        stroke,
    );
    frame.stroke(
        &Path::line(
            Point::new(second_x, 0.0),
            Point::new(second_x, bounds.height),
        ),
        stroke,
    );
    frame.stroke(
        &Path::line(Point::new(first_x, 8.0), Point::new(second_x, 8.0)),
        stroke,
    );
}

fn draw_one(
    chart: &KlineChart,
    frame: &mut Frame,
    drawing: &Drawing,
    bounds: Rectangle,
    selected: bool,
) {
    let style = &drawing.style;
    match &drawing.geometry {
        DrawingGeometry::HorizontalLine { price } => {
            let y = chart
                .drawing_screen_point(
                    DrawingAnchor {
                        x: DrawingX::Tick(0),
                        price: *price,
                    },
                    bounds,
                )
                .y;
            frame.stroke(
                &Path::line(Point::new(0.0, y), Point::new(bounds.width, y)),
                line(style),
            );
        }
        DrawingGeometry::VerticalLine { x } => {
            let x_position = chart
                .drawing_screen_point(
                    DrawingAnchor {
                        x: *x,
                        price: Price::from_units(0),
                    },
                    bounds,
                )
                .x;
            frame.stroke(
                &Path::line(
                    Point::new(x_position, 0.0),
                    Point::new(x_position, bounds.height),
                ),
                line(style),
            );
        }
        DrawingGeometry::TrendLine { first, second } => frame.stroke(
            &Path::line(
                chart.drawing_screen_point(*first, bounds),
                chart.drawing_screen_point(*second, bounds),
            ),
            line(style),
        ),
        DrawingGeometry::Rectangle { first, second } => {
            let a = chart.drawing_screen_point(*first, bounds);
            let b = chart.drawing_screen_point(*second, bounds);
            let rect = Rectangle::new(
                Point::new(a.x.min(b.x), a.y.min(b.y)),
                Size::new((a.x - b.x).abs(), (a.y - b.y).abs()),
            );
            frame.fill_rectangle(
                rect.position(),
                rect.size(),
                color(style.fill_color, style.fill_opacity),
            );
            frame.stroke(&Path::rectangle(rect.position(), rect.size()), line(style));
        }
        DrawingGeometry::Fibonacci { first, second } => {
            draw_fibonacci(chart, frame, *first, *second, style, bounds)
        }
        DrawingGeometry::Freehand { points } => {
            if points.len() > 1 {
                frame.stroke(
                    &Path::new(|builder| {
                        builder.move_to(chart.drawing_screen_point(points[0], bounds));
                        for point in &points[1..] {
                            builder.line_to(chart.drawing_screen_point(*point, bounds));
                        }
                    }),
                    line(style),
                );
            }
        }
        DrawingGeometry::Text { anchor, content } => frame.fill_text(canvas::Text {
            content: content.clone(),
            position: chart.drawing_screen_point(*anchor, bounds),
            color: color(style.color, style.opacity),
            size: iced::Pixels(style.text_size * style.text_scale),
            ..Default::default()
        }),
        DrawingGeometry::FixedRangeVolumeProfile { first, second } => {
            let first_x = chart.volume_profile_screen_x(*first, bounds);
            let second_x = chart.volume_profile_screen_x(*second, bounds);
            if selected {
                draw_fixed_range_volume_profile_guides(
                    frame,
                    first_x,
                    second_x,
                    bounds,
                    fixed_range_guide_stroke(),
                );
            }
            if selected && chart.fixed_volume_profile_loading(*first, *second) {
                frame.fill_text(canvas::Text {
                    content: "Loading VP…".to_string(),
                    position: Point::new(first_x.min(second_x) + 3.0, 20.0),
                    color: Color::from_rgb(0.66, 0.68, 0.71),
                    size: iced::Pixels(10.0),
                    ..Default::default()
                });
            }
        }
    }
    if selected {
        draw_selection(chart, frame, drawing, bounds);
    }
}

fn draw_draft(
    chart: &KlineChart,
    frame: &mut Frame,
    draft: &Draft,
    bounds: Rectangle,
    _theme: &Theme,
) {
    let tool = match draft {
        Draft::TwoPoint { tool, .. } => *tool,
        Draft::Freehand { .. } => DrawingTool::Pen,
        Draft::Text { .. } => DrawingTool::Text,
        Draft::Moving { .. } | Draft::Resizing { .. } => return,
    };
    let mut style = chart.style_for_tool(tool);
    style.opacity *= 0.65;
    match draft {
        Draft::TwoPoint {
            tool,
            first,
            preview,
        } => {
            if *tool == DrawingTool::FixedRangeVolumeProfile {
                let (DrawingX::Time(first), DrawingX::Time(second)) = (first.x, preview.x) else {
                    return;
                };
                draw_fixed_range_volume_profile_guides(
                    frame,
                    chart.volume_profile_screen_x(first, bounds),
                    chart.volume_profile_screen_x(second, bounds),
                    bounds,
                    fixed_range_guide_stroke(),
                );
                return;
            }
            let geometry = match tool {
                DrawingTool::Rectangle => DrawingGeometry::Rectangle {
                    first: *first,
                    second: *preview,
                },
                DrawingTool::Fibonacci => DrawingGeometry::Fibonacci {
                    first: *first,
                    second: *preview,
                },
                _ => DrawingGeometry::TrendLine {
                    first: *first,
                    second: *preview,
                },
            };
            draw_one(
                chart,
                frame,
                &Drawing {
                    id: 0,
                    geometry,
                    style,
                    visible: true,
                },
                bounds,
                false,
            );
        }
        Draft::Freehand { points } => draw_one(
            chart,
            frame,
            &Drawing {
                id: 0,
                geometry: DrawingGeometry::Freehand {
                    points: points.clone(),
                },
                style,
                visible: true,
            },
            bounds,
            false,
        ),
        Draft::Text { anchor, value } => draw_one(
            chart,
            frame,
            &Drawing {
                id: 0,
                geometry: DrawingGeometry::Text {
                    anchor: *anchor,
                    content: value.clone(),
                },
                style,
                visible: true,
            },
            bounds,
            false,
        ),
        Draft::Moving { .. } | Draft::Resizing { .. } => {}
    }
}

fn draw_fibonacci(
    chart: &KlineChart,
    frame: &mut Frame,
    first: DrawingAnchor,
    second: DrawingAnchor,
    style: &DrawingStyle,
    bounds: Rectangle,
) {
    let a = chart.drawing_screen_point(first, bounds);
    let b = chart.drawing_screen_point(second, bounds);
    for level in style.fibonacci_levels.iter().filter(|level| level.visible) {
        let y = a.y + (b.y - a.y) * level.value;
        frame.stroke(
            &Path::line(Point::new(a.x, y), Point::new(b.x, y)),
            line(style),
        );
        if style.show_labels {
            frame.fill_text(canvas::Text {
                content: format!("{:.3}", level.value),
                position: Point::new(b.x + 4.0, y),
                color: color(style.color, style.opacity),
                size: iced::Pixels(style.text_size.min(13.0)),
                ..Default::default()
            });
        }
    }
}

fn draw_selection(chart: &KlineChart, frame: &mut Frame, drawing: &Drawing, bounds: Rectangle) {
    let accent = Color::from_rgb(0.95, 0.78, 0.18);
    for point in drawing_handles(chart, drawing, bounds) {
        frame.fill(&Path::circle(point, 4.0), accent);
    }
}

fn drawing_handles(chart: &KlineChart, drawing: &Drawing, bounds: Rectangle) -> Vec<Point> {
    match &drawing.geometry {
        DrawingGeometry::Rectangle { first, second } => {
            let top_right = DrawingAnchor {
                x: second.x,
                price: first.price,
            };
            let bottom_left = DrawingAnchor {
                x: first.x,
                price: second.price,
            };
            [*first, top_right, bottom_left, *second]
                .into_iter()
                .map(|anchor| chart.drawing_screen_point(anchor, bounds))
                .collect()
        }
        DrawingGeometry::Fibonacci { first, second }
        | DrawingGeometry::TrendLine { first, second } => {
            vec![
                chart.drawing_screen_point(*first, bounds),
                chart.drawing_screen_point(*second, bounds),
            ]
        }
        DrawingGeometry::Text { anchor, content } => {
            let point = chart.drawing_screen_point(*anchor, bounds);
            vec![
                point,
                Point::new(
                    point.x
                        + content.len() as f32
                            * drawing.style.text_size
                            * drawing.style.text_scale
                            * 0.65,
                    point.y + drawing.style.text_size * drawing.style.text_scale * 1.4,
                ),
            ]
        }
        DrawingGeometry::Freehand { points } => points
            .first()
            .copied()
            .into_iter()
            .chain(points.last().copied())
            .map(|anchor| chart.drawing_screen_point(anchor, bounds))
            .collect(),
        DrawingGeometry::FixedRangeVolumeProfile { first, second } => vec![
            Point::new(chart.volume_profile_screen_x(*first, bounds), 8.0),
            Point::new(chart.volume_profile_screen_x(*second, bounds), 8.0),
        ],
        _ => vec![],
    }
}

fn hit_test(chart: &KlineChart, drawing: &Drawing, point: Point, bounds: Rectangle) -> bool {
    const T: f32 = 8.0;
    match &drawing.geometry {
        DrawingGeometry::HorizontalLine { price } => {
            (point.y
                - chart
                    .drawing_screen_point(
                        DrawingAnchor {
                            x: DrawingX::Tick(0),
                            price: *price,
                        },
                        bounds,
                    )
                    .y)
                .abs()
                <= T
        }
        DrawingGeometry::VerticalLine { x } => {
            (point.x
                - chart
                    .drawing_screen_point(
                        DrawingAnchor {
                            x: *x,
                            price: Price::from_units(0),
                        },
                        bounds,
                    )
                    .x)
                .abs()
                <= T
        }
        DrawingGeometry::Text { anchor, content } => {
            let p = chart.drawing_screen_point(*anchor, bounds);
            Rectangle::new(
                p,
                Size::new(
                    content.len() as f32
                        * drawing.style.text_size
                        * drawing.style.text_scale
                        * 0.65,
                    drawing.style.text_size * drawing.style.text_scale * 1.4,
                ),
            )
            .contains(point)
        }
        DrawingGeometry::Rectangle { first, second } => {
            let a = chart.drawing_screen_point(*first, bounds);
            let b = chart.drawing_screen_point(*second, bounds);
            Rectangle::new(
                Point::new(a.x.min(b.x) - T, a.y.min(b.y) - T),
                Size::new((a.x - b.x).abs() + 2.0 * T, (a.y - b.y).abs() + 2.0 * T),
            )
            .contains(point)
        }
        DrawingGeometry::Fibonacci { first, second } => {
            let a = chart.drawing_screen_point(*first, bounds);
            let b = chart.drawing_screen_point(*second, bounds);
            point.x >= a.x.min(b.x) - T
                && point.x <= a.x.max(b.x) + T
                && drawing
                    .style
                    .fibonacci_levels
                    .iter()
                    .filter(|level| level.visible)
                    .any(|level| (point.y - (a.y + (b.y - a.y) * level.value)).abs() <= T)
        }
        DrawingGeometry::TrendLine { first, second } => {
            point_segment_distance(
                point,
                chart.drawing_screen_point(*first, bounds),
                chart.drawing_screen_point(*second, bounds),
            ) <= T
        }
        DrawingGeometry::Freehand { points } => points.windows(2).any(|pair| {
            point_segment_distance(
                point,
                chart.drawing_screen_point(pair[0], bounds),
                chart.drawing_screen_point(pair[1], bounds),
            ) <= T
        }),
        DrawingGeometry::FixedRangeVolumeProfile { first, second } => {
            let first_x = chart.volume_profile_screen_x(*first, bounds);
            let second_x = chart.volume_profile_screen_x(*second, bounds);
            // The dashed range boundaries are the handles. They must be
            // interactive for their full height, not just at the top bar.
            (point.x - first_x).abs() <= T
                || (point.x - second_x).abs() <= T
                || (point.y <= 24.0
                    && point.x >= first_x.min(second_x) - T
                    && point.x <= first_x.max(second_x) + T)
        }
    }
}

fn point_segment_distance(p: Point, a: Point, b: Point) -> f32 {
    let ab = b - a;
    let denom = ab.x * ab.x + ab.y * ab.y;
    if denom <= f32::EPSILON {
        let d = p - a;
        return d.x.hypot(d.y);
    }
    let t = (((p - a).x * ab.x + (p - a).y * ab.y) / denom).clamp(0.0, 1.0);
    let d = p - (a + ab * t);
    d.x.hypot(d.y)
}

fn translate_anchor(
    anchor: DrawingAnchor,
    start: DrawingAnchor,
    end: DrawingAnchor,
) -> DrawingAnchor {
    let x = match (anchor.x, start.x, end.x) {
        (DrawingX::Time(value), DrawingX::Time(from), DrawingX::Time(to)) => {
            DrawingX::Time(UnixMs::new(
                value
                    .as_u64()
                    .saturating_add_signed(to.as_u64() as i64 - from.as_u64() as i64),
            ))
        }
        (DrawingX::Tick(value), DrawingX::Tick(from), DrawingX::Tick(to)) => {
            DrawingX::Tick(value.saturating_add_signed(to as i64 - from as i64))
        }
        (x, _, _) => x,
    };
    DrawingAnchor {
        x,
        price: Price::from_units(
            anchor
                .price
                .units
                .saturating_add(end.price.units - start.price.units),
        ),
    }
}
fn translate_geometry(
    geometry: &DrawingGeometry,
    start: DrawingAnchor,
    end: DrawingAnchor,
) -> DrawingGeometry {
    match geometry {
        DrawingGeometry::Freehand { points } => DrawingGeometry::Freehand {
            points: points
                .iter()
                .map(|p| translate_anchor(*p, start, end))
                .collect(),
        },
        DrawingGeometry::HorizontalLine { price } => DrawingGeometry::HorizontalLine {
            price: Price::from_units(
                price
                    .units
                    .saturating_add(end.price.units - start.price.units),
            ),
        },
        DrawingGeometry::VerticalLine { x } => DrawingGeometry::VerticalLine {
            x: translate_anchor(
                DrawingAnchor {
                    x: *x,
                    price: start.price,
                },
                start,
                end,
            )
            .x,
        },
        DrawingGeometry::Rectangle { first, second } => DrawingGeometry::Rectangle {
            first: translate_anchor(*first, start, end),
            second: translate_anchor(*second, start, end),
        },
        DrawingGeometry::Fibonacci { first, second } => DrawingGeometry::Fibonacci {
            first: translate_anchor(*first, start, end),
            second: translate_anchor(*second, start, end),
        },
        DrawingGeometry::TrendLine { first, second } => DrawingGeometry::TrendLine {
            first: translate_anchor(*first, start, end),
            second: translate_anchor(*second, start, end),
        },
        DrawingGeometry::Text { anchor, content } => DrawingGeometry::Text {
            anchor: translate_anchor(*anchor, start, end),
            content: content.clone(),
        },
        DrawingGeometry::FixedRangeVolumeProfile { first, second } => {
            let delta = match (start.x, end.x) {
                (DrawingX::Time(from), DrawingX::Time(to)) => {
                    to.as_u64() as i64 - from.as_u64() as i64
                }
                _ => 0,
            };
            DrawingGeometry::FixedRangeVolumeProfile {
                first: UnixMs::new(first.as_u64().saturating_add_signed(delta)),
                second: UnixMs::new(second.as_u64().saturating_add_signed(delta)),
            }
        }
    }
}

fn resize_geometry(
    geometry: &DrawingGeometry,
    anchor: DrawingAnchor,
    handle: u8,
) -> DrawingGeometry {
    match geometry {
        DrawingGeometry::Rectangle { first, second } => {
            let mut first = *first;
            let mut second = *second;
            match handle {
                0 => first = anchor,
                1 => {
                    first.price = anchor.price;
                    second.x = anchor.x;
                }
                2 => {
                    first.x = anchor.x;
                    second.price = anchor.price;
                }
                _ => second = anchor,
            }
            DrawingGeometry::Rectangle { first, second }
        }
        DrawingGeometry::Fibonacci { first, second } => DrawingGeometry::Fibonacci {
            first: if handle == 0 { anchor } else { *first },
            second: if handle == 0 { *second } else { anchor },
        },
        DrawingGeometry::TrendLine { first, second } => DrawingGeometry::TrendLine {
            first: if handle == 0 { anchor } else { *first },
            second: if handle == 0 { *second } else { anchor },
        },
        DrawingGeometry::FixedRangeVolumeProfile { first, second } => {
            let DrawingX::Time(time) = anchor.x else {
                return geometry.clone();
            };
            DrawingGeometry::FixedRangeVolumeProfile {
                first: if handle == 0 { time } else { *first },
                second: if handle == 0 { *second } else { time },
            }
        }
        DrawingGeometry::Text { .. } | DrawingGeometry::Freehand { .. } => geometry.clone(),
        DrawingGeometry::HorizontalLine { .. } | DrawingGeometry::VerticalLine { .. } => {
            geometry.clone()
        }
    }
}

fn toolbar(
    active: DrawingTool,
    visible: bool,
    open: bool,
    drawing_list_open: bool,
    has_drawings: bool,
    max_height: f32,
) -> Element<'static, Message> {
    let tools = [
        (DrawingTool::Select, "drawing-select.svg", "Select"),
        (DrawingTool::Pen, "drawing-pen.svg", "Pen"),
        (
            DrawingTool::HorizontalLine,
            "drawing-horizontal-line.svg",
            "Horizontal",
        ),
        (
            DrawingTool::VerticalLine,
            "drawing-vertical-line.svg",
            "Vertical",
        ),
        (DrawingTool::Rectangle, "drawing-rectangle.svg", "Rectangle"),
        (DrawingTool::Fibonacci, "drawing-fibonacci.svg", "Fibonacci"),
        (
            DrawingTool::TrendLine,
            "drawing-trend-line.svg",
            "Trendline",
        ),
        (DrawingTool::Text, "drawing-text.svg", "Text"),
        (
            DrawingTool::FixedRangeVolumeProfile,
            "drawing-volume-profile.svg",
            "Volume profile",
        ),
    ];
    let mut items = column![].spacing(2);
    let arrow = if open {
        include_bytes!("../../../assets/ui/drawing-toolbar-collapse.svg") as &'static [u8]
    } else {
        include_bytes!("../../../assets/ui/drawing-toolbar-expand.svg") as &'static [u8]
    };
    items = items.push(drag_handle::drag_handle(
        container(
            svg(svg::Handle::from_memory(include_bytes!(
                "../../../assets/ui/drawing-drag-handle.svg"
            )))
            .width(16)
            .height(16)
            .style(|theme: &Theme, _| svg::Style {
                color: Some(theme.palette().background.base.text),
            }),
        )
        .width(28)
        .height(20)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center),
        |position| Message::Drawing(DrawingMessage::ToolbarDragStarted(position)),
        |position| Message::Drawing(DrawingMessage::ToolbarDragged(position)),
        Message::Drawing(DrawingMessage::ToolbarDragEnded),
    ));
    if has_drawings {
        items = items.push(
            button(
                svg(svg::Handle::from_memory(include_bytes!(
                    "../../../assets/ui/drawing-list.svg"
                )))
                .width(16)
                .height(16)
                .style(|theme: &Theme, _| svg::Style {
                    color: Some(theme.palette().background.base.text),
                }),
            )
            .width(28)
            .height(28)
            .padding(5)
            .on_press(Message::Drawing(DrawingMessage::ToggleDrawingList))
            .style(move |theme, status| {
                crate::style::button::transparent(theme, status, drawing_list_open)
            }),
        );
    }
    items = items.push(
        button(
            svg(svg::Handle::from_memory(arrow))
                .width(16)
                .height(16)
                .style(|theme: &Theme, _| svg::Style {
                    color: Some(theme.palette().background.base.text),
                }),
        )
        .width(28)
        .height(28)
        .padding(5)
        .on_press(Message::Drawing(DrawingMessage::ToggleToolbar))
        .style(|theme, status| crate::style::button::transparent(theme, status, false)),
    );
    if !open {
        return container(
            scrollable(container(items).padding(iced::padding::right(10))).height(Length::Fixed(
                toolbar_panel_height(false, has_drawings).min(max_height),
            )),
        )
        .padding(3)
        .style(crate::style::chart_modal)
        .into();
    }
    for (tool, asset, _label) in tools {
        let bytes: &'static [u8] = match asset {
            "drawing-select.svg" => include_bytes!("../../../assets/ui/drawing-select.svg"),
            "drawing-pen.svg" => include_bytes!("../../../assets/ui/drawing-pen.svg"),
            "drawing-horizontal-line.svg" => {
                include_bytes!("../../../assets/ui/drawing-horizontal-line.svg")
            }
            "drawing-vertical-line.svg" => {
                include_bytes!("../../../assets/ui/drawing-vertical-line.svg")
            }
            "drawing-rectangle.svg" => include_bytes!("../../../assets/ui/drawing-rectangle.svg"),
            "drawing-fibonacci.svg" => include_bytes!("../../../assets/ui/drawing-fibonacci.svg"),
            "drawing-trend-line.svg" => include_bytes!("../../../assets/ui/drawing-trend-line.svg"),
            "drawing-volume-profile.svg" => {
                include_bytes!("../../../assets/ui/drawing-volume-profile.svg")
            }
            _ => include_bytes!("../../../assets/ui/drawing-text.svg"),
        };
        let selected = tool == active;
        items = items.push(
            button(
                svg(svg::Handle::from_memory(bytes))
                    .width(16)
                    .height(16)
                    .style(|theme: &Theme, _| svg::Style {
                        color: Some(theme.palette().background.base.text),
                    }),
            )
            .width(28)
            .height(28)
            .padding(5)
            .on_press(Message::Drawing(DrawingMessage::ToolSelected(tool)))
            .style(move |theme, status| crate::style::button::transparent(theme, status, selected)),
        );
    }
    let eye = if visible {
        include_bytes!("../../../assets/ui/drawing-eye.svg") as &'static [u8]
    } else {
        include_bytes!("../../../assets/ui/drawing-eye-off.svg") as &'static [u8]
    };
    items = items.push(
        button(
            svg(svg::Handle::from_memory(eye))
                .width(16)
                .height(16)
                .style(|theme: &Theme, _| svg::Style {
                    color: Some(theme.palette().background.base.text),
                }),
        )
        .width(28)
        .height(28)
        .padding(5)
        .on_press(Message::Drawing(DrawingMessage::ToggleDrawingsVisibility))
        .style(move |theme, status| crate::style::button::transparent(theme, status, visible)),
    );
    items = items.push(
        button(
            svg(svg::Handle::from_memory(include_bytes!(
                "../../../assets/ui/drawing-trash.svg"
            )))
            .width(16)
            .height(16)
            .style(|theme: &Theme, _| svg::Style {
                color: Some(theme.palette().background.base.text),
            }),
        )
        .width(28)
        .height(28)
        .padding(5)
        .on_press(Message::Drawing(DrawingMessage::ClearAllDrawings))
        .style(|theme, status| crate::style::button::transparent(theme, status, false)),
    );
    container(
        scrollable(container(items).padding(iced::padding::right(10))).height(Length::Fixed(
            toolbar_panel_height(true, has_drawings).min(max_height),
        )),
    )
    .padding(3)
    .style(crate::style::chart_modal)
    .into()
}

fn toolbar_panel_height(open: bool, has_drawings: bool) -> f32 {
    let buttons = if open { 12 } else { 1 } + usize::from(has_drawings);
    let items = buttons + 1; // drag handle
    20.0 + buttons as f32 * 28.0 + (items.saturating_sub(1) as f32) * 2.0 + 6.0
}

fn drawing_list(chart: &KlineChart) -> Element<'_, Message> {
    let header = drag_handle::drag_handle(
        container(
            row![
                svg(svg::Handle::from_memory(include_bytes!(
                    "../../../assets/ui/drawing-drag-handle.svg"
                )))
                .width(14)
                .height(14),
                text("Drawings list")
                    .size(13)
                    .color(Color::from_rgb(0.68, 0.70, 0.73)),
            ]
            .spacing(5),
        )
        .width(Length::Fill)
        .padding(5),
        |position| Message::Drawing(DrawingMessage::DrawingListDragStarted(position)),
        |position| Message::Drawing(DrawingMessage::DrawingListDragged(position)),
        Message::Drawing(DrawingMessage::DrawingListDragEnded),
    );

    let mut items = column![].spacing(3);
    for drawing in &chart.drawings.drawings {
        let title = drawing_name(&drawing.geometry);
        let position = drawing_position(drawing, chart.tick_size().decimal_places());
        let focus = button(
            text(ellipsize(&format!("{title} - {position}"), 24))
                .size(11)
                .color(Color::from_rgb(0.62, 0.64, 0.67)),
        )
        .padding(4)
        .width(Length::Fill)
        .on_press(Message::Drawing(DrawingMessage::FocusDrawing(drawing.id)))
        .style(|theme, status| crate::style::button::transparent(theme, status, false));
        let action_icon = |bytes: &'static [u8], message| {
            button(
                svg(svg::Handle::from_memory(bytes))
                    .width(14)
                    .height(14)
                    .style(|_: &Theme, _| svg::Style {
                        color: Some(Color::from_rgb(0.58, 0.60, 0.63)),
                    }),
            )
            .width(24)
            .height(24)
            .padding(4)
            .on_press(Message::Drawing(message))
            .style(|theme, status| crate::style::button::transparent(theme, status, false))
        };
        items = items.push(
            container(
                row![
                    focus,
                    action_icon(
                        include_bytes!("../../../assets/ui/drawing-settings.svg"),
                        DrawingMessage::OpenDrawingSettings(drawing.id),
                    ),
                    action_icon(
                        if drawing.visible {
                            include_bytes!("../../../assets/ui/drawing-eye.svg")
                        } else {
                            include_bytes!("../../../assets/ui/drawing-eye-off.svg")
                        },
                        DrawingMessage::ToggleDrawingVisibility(drawing.id),
                    ),
                    container(action_icon(
                        include_bytes!("../../../assets/ui/drawing-trash.svg"),
                        DrawingMessage::DeleteDrawing(drawing.id),
                    ))
                    .padding(iced::padding::right(10)),
                ]
                .spacing(2),
            )
            .padding(2)
            .style(crate::style::chart_modal),
        );
    }
    container(
        column![
            header,
            scrollable(items).height(Length::Fixed(
                (chart.drawings.drawings.len() as f32 * 30.0).clamp(30.0, 240.0),
            )),
        ]
        .spacing(3),
    )
    .padding(4)
    .width(280)
    .style(crate::style::chart_modal)
    .into()
}

fn drawing_name(geometry: &DrawingGeometry) -> &'static str {
    match geometry {
        DrawingGeometry::Freehand { .. } => "Pen",
        DrawingGeometry::HorizontalLine { .. } => "Horizontal line",
        DrawingGeometry::VerticalLine { .. } => "Vertical line",
        DrawingGeometry::Rectangle { .. } => "Rectangle",
        DrawingGeometry::Fibonacci { .. } => "Fibonacci",
        DrawingGeometry::TrendLine { .. } => "Trend line",
        DrawingGeometry::Text { .. } => "Text",
        DrawingGeometry::FixedRangeVolumeProfile { .. } => "Fixed Range VP",
    }
}

fn drawing_position(drawing: &Drawing, decimals: usize) -> String {
    match &drawing.geometry {
        DrawingGeometry::HorizontalLine { price } => price_label(*price, decimals),
        DrawingGeometry::VerticalLine { x } => x_label(*x),
        DrawingGeometry::Rectangle { first, second }
        | DrawingGeometry::Fibonacci { first, second }
        | DrawingGeometry::TrendLine { first, second } => format!(
            "{} @ {} → {} @ {}",
            x_label(first.x),
            price_label(first.price, decimals),
            x_label(second.x),
            price_label(second.price, decimals),
        ),
        DrawingGeometry::Freehand { points } => points
            .first()
            .map(|point| {
                format!(
                    "{} @ {}",
                    x_label(point.x),
                    price_label(point.price, decimals)
                )
            })
            .unwrap_or_else(|| "Empty".to_string()),
        DrawingGeometry::Text { anchor, .. } => {
            format!(
                "{} @ {}",
                x_label(anchor.x),
                price_label(anchor.price, decimals)
            )
        }
        DrawingGeometry::FixedRangeVolumeProfile { first, second } => {
            format!(
                "{} → {}",
                x_label(DrawingX::Time(*first)),
                x_label(DrawingX::Time(*second))
            )
        }
    }
}

fn ellipsize(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value.to_string()
    } else {
        format!(
            "{}...",
            value
                .chars()
                .take(max_chars.saturating_sub(3))
                .collect::<String>()
        )
    }
}

fn settings(chart: &KlineChart) -> Element<'_, Message> {
    let Some(drawing) = chart
        .drawings
        .drawings
        .iter()
        .find(|drawing| Some(drawing.id) == chart.drawings.selected)
    else {
        return column![].into();
    };
    if matches!(
        drawing.geometry,
        DrawingGeometry::FixedRangeVolumeProfile { .. }
    ) {
        return fixed_range_volume_profile_settings(drawing);
    }
    let style = &drawing.style;
    let selected_color = Color {
        r: style.color.r,
        g: style.color.g,
        b: style.color.b,
        a: style.color.a,
    };
    let mut controls = column![
        row![
            text("Drawing settings").size(14),
            button(text("×")).on_press(Message::Drawing(DrawingMessage::CloseSettings))
        ]
        .spacing(12),
        text("Color"),
        color_picker(data::config::theme::to_hsva(selected_color), |hsva| {
            let color = data::config::theme::from_hsva(hsva);
            Message::Drawing(DrawingMessage::SetColor(DrawingColor {
                r: color.r,
                g: color.g,
                b: color.b,
                a: color.a,
            }))
        }),
        text("Stroke"),
        slider(1.0..=8.0, style.stroke_width, |v| Message::Drawing(
            DrawingMessage::SetStrokeWidth(v)
        )),
        text(format!("Opacity: {:.0}%", style.opacity * 100.0)),
        slider(0.0..=100.0, style.opacity * 100.0, |v| Message::Drawing(
            DrawingMessage::SetOpacity(v / 100.0)
        )),
    ]
    .spacing(6);

    match &drawing.geometry {
        DrawingGeometry::HorizontalLine { .. } => {
            let input = chart
                .drawings
                .horizontal_line_price_input
                .as_deref()
                .unwrap_or_default();
            controls = controls.push(text("Price")).push(
                text_input("e.g. 63,000", input)
                    .on_input(|value| {
                        Message::Drawing(DrawingMessage::HorizontalLinePriceChanged(value))
                    })
                    .on_submit(Message::Drawing(DrawingMessage::CommitHorizontalLinePrice)),
            );
            controls = controls.push(
                button(text("Apply price"))
                    .on_press(Message::Drawing(DrawingMessage::CommitHorizontalLinePrice)),
            );
        }
        DrawingGeometry::Rectangle { .. } => {
            controls = controls
                .push(text(format!(
                    "Fill opacity: {:.0}%",
                    style.fill_opacity * 100.0
                )))
                .push(slider(0.0..=100.0, style.fill_opacity * 100.0, |v| {
                    Message::Drawing(DrawingMessage::SetFillOpacity(v / 100.0))
                }));
        }
        DrawingGeometry::Text { .. } => {}
        DrawingGeometry::Fibonacci { .. } => {
            controls = controls.push(
                button(text(if style.show_labels {
                    "Hide Fibonacci labels"
                } else {
                    "Show Fibonacci labels"
                }))
                .on_press(Message::Drawing(DrawingMessage::ToggleLabels(
                    !style.show_labels,
                ))),
            );
            controls = controls.push(text("Levels"));
            for (index, level) in style.fibonacci_levels.iter().enumerate() {
                controls = controls.push(
                    button(text(format!(
                        "{} {:.3}",
                        if level.visible { "✓" } else { "○" },
                        level.value
                    )))
                    .on_press(Message::Drawing(
                        DrawingMessage::ToggleFibonacciLevel(index),
                    )),
                );
            }
        }
        _ => {}
    }

    controls = controls.push(
        button(text("Delete drawing")).on_press(Message::Drawing(DrawingMessage::DeleteSelected)),
    );

    container(controls)
        .padding(10)
        .width(iced::Length::Fit.max(240))
        .style(crate::style::chart_modal)
        .into()
}

fn fixed_range_volume_profile_settings(drawing: &Drawing) -> Element<'_, Message> {
    let config = sanitize_volume_profile_config(drawing.style.fixed_range_volume_profile);
    let update = |config| Message::Drawing(DrawingMessage::SetFixedRangeVolumeProfile(config));
    let placement = pick_list(
        SessionProfilePlacement::ALL,
        Some(config.placement),
        move |placement| {
            update(FixedRangeVolumeProfileConfig {
                placement,
                ..config
            })
        },
    );
    let mode = pick_list(SessionProfileMode::ALL, Some(config.mode), move |mode| {
        Message::Drawing(DrawingMessage::SetFixedRangeVolumeProfile(
            FixedRangeVolumeProfileConfig { mode, ..config },
        ))
    });
    let width = slider(10.0..=90.0, config.width_percent, move |width_percent| {
        Message::Drawing(DrawingMessage::SetFixedRangeVolumeProfile(
            FixedRangeVolumeProfileConfig {
                width_percent,
                ..config
            },
        ))
    });
    let value_area = slider(
        50.0..=95.0,
        config.value_area_percent,
        move |value_area_percent| {
            Message::Drawing(DrawingMessage::SetFixedRangeVolumeProfile(
                FixedRangeVolumeProfileConfig {
                    value_area_percent,
                    ..config
                },
            ))
        },
    );
    let rows = slider(1.0..=50.0, config.row_size_ticks as f32, move |value| {
        Message::Drawing(DrawingMessage::SetFixedRangeVolumeProfile(
            FixedRangeVolumeProfileConfig {
                row_size_ticks: value as u16,
                ..config
            },
        ))
    });
    let toggle =
        |label,
         enabled,
         apply: fn(FixedRangeVolumeProfileConfig, bool) -> FixedRangeVolumeProfileConfig| {
            checkbox(enabled).label(label).on_toggle(move |value| {
                Message::Drawing(DrawingMessage::SetFixedRangeVolumeProfile(apply(
                    config, value,
                )))
            })
        };
    let poc = toggle("POC", config.show_poc, |mut value, enabled| {
        value.show_poc = enabled;
        value
    });
    let va = toggle("VAH / VAL", config.show_value_area, |mut value, enabled| {
        value.show_value_area = enabled;
        value
    });
    let vwap = toggle("Range VWAP", config.show_vwap, |mut value, enabled| {
        value.show_vwap = enabled;
        value
    });
    let high_low = toggle(
        "Range high / low",
        config.show_range_high_low,
        |mut value, enabled| {
            value.show_range_high_low = enabled;
            value
        },
    );

    container(
        column![
            row![
                text("Fixed Range Volume Profile").size(14),
                button(text("×")).on_press(Message::Drawing(DrawingMessage::CloseSettings))
            ]
            .spacing(12),
            text("Placement"),
            placement,
            text("Mode"),
            mode,
            text(format!("Width: {:.0}%", config.width_percent)),
            width,
            text(format!("Value area: {:.0}%", config.value_area_percent)),
            value_area,
            text(format!("Ticks / row: {}", config.row_size_ticks)),
            rows,
            row![poc, va].spacing(8),
            vwap,
            high_low,
            button(text("Delete drawing"))
                .on_press(Message::Drawing(DrawingMessage::DeleteSelected)),
        ]
        .spacing(6),
    )
    .padding(10)
    .width(iced::Length::Fit.max(240))
    .style(crate::style::chart_modal)
    .into()
}

#[cfg(test)]
mod tests {
    use super::{drawing_canvas_center, parse_horizontal_line_price};
    use iced::{Point, Rectangle, Size};

    #[test]
    fn drawing_center_uses_canvas_local_coordinates() {
        let bounds = Rectangle::new(Point::new(240.0, 90.0), Size::new(800.0, 500.0));

        assert_eq!(drawing_canvas_center(bounds), Point::new(400.0, 250.0));
    }

    #[test]
    fn horizontal_line_price_parser_accepts_grouped_separators() {
        for input in ["63,000", "63.000"] {
            assert_eq!(parse_horizontal_line_price(input), Some(63_000.0));
        }
        for input in ["63,000.5", "63.000,5"] {
            assert_eq!(parse_horizontal_line_price(input), Some(63_000.5));
        }
    }
}
