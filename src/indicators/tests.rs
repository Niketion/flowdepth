use super::*;
use iced::advanced::{
    Layout, layout,
    renderer::{Headless, Renderer as _},
    widget::{Operation, Tree},
};
use std::{collections::HashMap, path::Path};

#[derive(Default)]
struct Regions {
    bounds: HashMap<iced::widget::Id, iced::Rectangle>,
}
impl Operation for Regions {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
        operate(self)
    }
    fn container(&mut self, id: Option<&iced::widget::Id>, bounds: iced::Rectangle) {
        if let Some(id) = id {
            self.bounds.insert(id.clone(), bounds);
        }
    }
}
struct Ui<'a> {
    element: Element<'a, Message>,
    tree: Tree,
    node: layout::Node,
    bounds: iced::Rectangle,
}
impl<'a> Ui<'a> {
    fn new(element: Element<'a, Message>, renderer: &iced::Renderer, size: Size) -> Self {
        let mut element = element;
        let mut tree = Tree::new(element.as_widget());
        tree.diff(element.as_widget_mut());
        let node = element.as_widget_mut().layout(
            &mut tree,
            renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        Self {
            element,
            tree,
            node,
            bounds: iced::Rectangle::with_size(size),
        }
    }
    fn event(
        &mut self,
        renderer: &iced::Renderer,
        event: iced::Event,
        cursor: iced::mouse::Cursor,
    ) -> Vec<Message> {
        let window = iced_core::window::Headless;
        let mut bus = iced_core::shell::Bus::new();
        let mut shell = iced_core::Shell::new(&window, iced_core::shell::Waker::noop(), &mut bus);
        self.element.as_widget_mut().update(
            &mut self.tree,
            &event,
            Layout::new(&self.node),
            cursor,
            renderer,
            &mut shell,
            &self.bounds,
        );
        bus.drain().collect()
    }
    fn regions(&mut self, renderer: &iced::Renderer) -> Regions {
        let mut regions = Regions::default();
        self.element.as_widget_mut().operate(
            &mut self.tree,
            Layout::new(&self.node),
            renderer,
            &mut regions,
        );
        regions
    }
    fn click(&mut self, renderer: &iced::Renderer, region: &str) -> Vec<Message> {
        let bounds = *self
            .regions(renderer)
            .bounds
            .get(&iced::widget::Id::from(region.to_string()))
            .expect("click target must exist");
        let position = bounds.center();
        let cursor = iced::mouse::Cursor::Available(position);
        let mut messages = self.event(
            renderer,
            iced::Event::Mouse(iced::mouse::Event::CursorMoved { position }),
            cursor,
        );
        for event in [
            iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left),
            iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left),
        ] {
            messages.extend(self.event(renderer, iced::Event::Mouse(event), cursor));
        }
        messages
    }
    fn screenshot(&mut self, renderer: &mut iced::Renderer, path: &Path) {
        let theme = data::State::default().selected_theme.0;
        renderer.reset(self.bounds);
        self.event(
            renderer,
            iced::Event::Window(iced::window::Event::RedrawRequested(
                std::time::Instant::now(),
            )),
            iced::mouse::Cursor::Unavailable,
        );
        self.element.as_widget().draw(
            &self.tree,
            renderer,
            &theme,
            &iced::advanced::renderer::Style {
                text_color: theme.palette().background.base.text,
            },
            Layout::new(&self.node),
            iced::mouse::Cursor::Unavailable,
            &self.bounds,
        );
        let size = iced::Size::new(self.bounds.width as u32, self.bounds.height as u32);
        let bytes =
            Headless::screenshot(renderer, size, 1.0, theme.palette().background.base.color);
        assert_eq!(bytes.len(), size.width as usize * size.height as usize * 4);
        assert!(bytes.chunks_exact(4).any(|pixel| pixel != &bytes[..4]));
        image::save_buffer(
            path,
            &bytes,
            size.width,
            size.height,
            image::ColorType::Rgba8,
        )
        .unwrap();
    }
}
fn synthetic_pane() -> pane::State {
    let mut state = pane::State::default();
    let info = exchange::TickerInfo::new(
        exchange::Ticker::new("BTCUSDT", exchange::adapter::Exchange::BinanceLinear),
        0.01,
        0.001,
        None,
    );
    let streams = state.set_content_and_streams(
        vec![info],
        data::layout::pane::ContentKind::CandlestickChart,
    );
    state.streams = crate::connector::ResolvedStream::Ready(streams);
    let timeframe = match &state.content {
        pane::Content::Kline {
            chart: Some(chart), ..
        } => match chart.basis() {
            data::chart::Basis::Time(tf) => tf,
            _ => panic!("time chart"),
        },
        _ => panic!("chart"),
    };
    let end = exchange::UnixMs::now().floor_to(timeframe);
    let interval = timeframe.to_milliseconds();
    let bars: Vec<_> = (0..200)
        .map(|index| {
            exchange::Kline::new(
                end.saturating_sub((199 - index) * interval),
                100.0,
                102.0,
                99.0,
                101.0,
                exchange::Volume::BuySell(
                    exchange::unit::Qty::from_f64(8.0),
                    exchange::unit::Qty::from_f64(5.0),
                ),
                info.min_ticksize,
            )
        })
        .collect();
    state.insert_hist_klines(None, timeframe, info, &bars);
    state.update(pane::Event::ShowModal(
        crate::modal::pane::Modal::Indicators,
    ));
    state
}
fn assert_inside(bounds: iced::Rectangle, size: Size, name: &str) {
    assert!(
        bounds.x >= -0.1
            && bounds.y >= -0.1
            && bounds.x + bounds.width <= size.width + 0.1
            && bounds.y + bounds.height <= size.height + 0.1,
        "{name} outside viewport: {bounds:?} / {size:?}"
    );
    assert!(
        bounds.width > 0.0 && bounds.height > 0.0,
        "{name} collapsed"
    );
}

#[test]
#[ignore = "requires local GPU; writes only synthetic UI previews"]
fn indicator_browser_gpu_layouts_and_pointer_interactions() {
    use data::chart::indicator::{Indicator, KlineIndicator};
    let output = std::env::var_os("FLOWSURFACE_INDICATOR_QA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("flowdepth-indicator-ui-qa"));
    std::fs::create_dir_all(&output).unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _guard = runtime.enter();
    let mut renderer = runtime
        .block_on(<iced::Renderer as Headless>::new(
            iced::advanced::renderer::Settings {
                default_text_size: iced::Pixels(12.0),
                ..Default::default()
            },
            Some("wgpu"),
        ))
        .expect("GPU renderer");
    {
        let mut fonts = iced_wgpu::graphics::text::font_system().write().unwrap();
        fonts.load_font(std::borrow::Cow::Borrowed(crate::style::AZERET_MONO_BYTES));
        fonts.load_font(std::borrow::Cow::Borrowed(crate::style::ICONS_BYTES));
    }
    let state = synthetic_pane();
    let (mut panes, id) = pane_grid::State::new(state);
    for &kind in KlineIndicator::for_market(exchange::adapter::MarketKind::LinearPerps) {
        if !supports(panes.get(id).unwrap(), kind.into()) {
            continue;
        }
        for advanced in [false, true] {
            if advanced && !has_advanced(kind.into()) {
                continue;
            }
            let state = panes.get_mut(id).unwrap();
            state.indicator_menu.update(&Action::Select(kind.into()));
            state.indicator_menu.update(&Action::Advanced(advanced));
            let size = Size::new(1100.0, 760.0);
            let mut ui = Ui::new(
                overlay(text("Underlying chart").into(), &panes),
                &renderer,
                size,
            );
            let regions = ui.regions(&renderer);
            for name in [
                "indicator-dialog",
                "indicator-close",
                "indicator-browser-list",
                "indicator-browser-detail",
            ] {
                assert_inside(regions.bounds[&iced::widget::Id::new(name)], size, name);
            }
            let list = regions.bounds[&iced::widget::Id::new("indicator-browser-list")];
            let toggle =
                regions.bounds[&iced::widget::Id::from("indicator-enable-Volume".to_string())];
            assert!(
                toggle.x + toggle.width <= list.x + list.width - 12.0,
                "scrollbar must leave the toggle clear"
            );
            let dialog = regions.bounds[&iced::widget::Id::new("indicator-dialog")];
            assert!(dialog.width > 700.0 && dialog.width <= 880.0);
            ui.screenshot(
                &mut renderer,
                &output.join(format!(
                    "{kind:?}-{}.png",
                    if advanced { "advanced" } else { "basic" }
                )),
            );
        }
    }
    // The active list has its own ordering controls; overlays stay outside that list.
    {
        let state = panes.get_mut(id).unwrap();
        for kind in [
            KlineIndicator::CumulativeDelta,
            KlineIndicator::Macd,
            KlineIndicator::Vwap,
        ] {
            state.update(pane::Event::IndicatorMenu(Action::Enable {
                indicator: kind.into(),
                enabled: true,
            }));
        }
        state.indicator_menu.update(&Action::Filter(Filter::Active));
    }
    {
        let mut ui = Ui::new(
            overlay(text("Underlying chart").into(), &panes),
            &renderer,
            Size::new(1100.0, 760.0),
        );
        let regions = ui.regions(&renderer);
        let list = regions.bounds[&iced::widget::Id::new("indicator-browser-list")];
        for name in ["Volume", "CVD", "MACD"] {
            let row = regions.bounds[&iced::widget::Id::from(format!("indicator-select-{name}"))];
            assert!(
                row.width > 100.0,
                "active indicator row must be visible: {name}"
            );
            let toggle =
                regions.bounds[&iced::widget::Id::from(format!("indicator-enable-{name}"))];
            assert!(
                toggle.x + toggle.width <= list.x + list.width - 12.0,
                "scrollbar must not cover activation: {name}"
            );
        }
        ui.screenshot(&mut renderer, &output.join("active-order.png"));
    }
    // Use the actual checkbox and selection widgets inside the modal overlay:
    // neither click may bubble into the outside-click close action.
    {
        let state = panes.get_mut(id).unwrap();
        state.indicator_menu.open();
        state.indicator_menu.update(&Action::Query("RSI".into()));
    }
    let messages = {
        let mut ui = Ui::new(
            overlay(text("Underlying chart").into(), &panes),
            &renderer,
            Size::new(1100.0, 760.0),
        );
        ui.click(&renderer, "indicator-enable-RSI")
    };
    assert_eq!(
        messages.len(),
        1,
        "checkbox must emit exactly one activation, without closing the modal"
    );
    assert!(matches!(
        &messages[0],
        Message::PaneEvent(
            _,
            pane::Event::IndicatorMenu(Action::Enable { enabled: true, .. })
        )
    ));
    for message in messages {
        if let Message::PaneEvent(_, event) = message {
            panes.get_mut(id).unwrap().update(event);
        }
    }
    assert!(
        panes
            .get(id)
            .unwrap()
            .content
            .indicator_enabled(KlineIndicator::Rsi.into())
    );
    let messages = {
        let mut ui = Ui::new(
            overlay(text("Underlying chart").into(), &panes),
            &renderer,
            Size::new(1100.0, 760.0),
        );
        ui.click(&renderer, "indicator-select-RSI")
    };
    assert_eq!(messages.len(), 1);
    assert!(matches!(
        &messages[0],
        Message::PaneEvent(_, pane::Event::IndicatorMenu(Action::Select(_)))
    ));
    // Small windows use a list then a detail view, with both regions scrollable.
    for size in [Size::new(360.0, 480.0), Size::new(560.0, 360.0)] {
        for detail in [false, true] {
            let state = panes.get_mut(id).unwrap();
            state.indicator_menu.open();
            if detail {
                state
                    .indicator_menu
                    .update(&Action::Select(KlineIndicator::CumulativeDelta.into()));
            }
            let mut ui = Ui::new(
                overlay(text("Underlying chart").into(), &panes),
                &renderer,
                size,
            );
            let regions = ui.regions(&renderer);
            let name = if detail {
                "indicator-browser-detail"
            } else {
                "indicator-browser-list"
            };
            assert_inside(
                regions.bounds[&iced::widget::Id::new("indicator-dialog")],
                size,
                "dialog",
            );
            assert_inside(regions.bounds[&iced::widget::Id::new(name)], size, name);
            ui.screenshot(
                &mut renderer,
                &output.join(format!(
                    "compact-{}-{}.png",
                    size.width,
                    if detail { "detail" } else { "list" }
                )),
            );
        }
    }
    let messages = {
        let mut ui = Ui::new(
            overlay(text("Underlying chart").into(), &panes),
            &renderer,
            Size::new(1100.0, 760.0),
        );
        ui.click(&renderer, "indicator-close")
    };
    assert_eq!(messages.len(), 1);
    assert!(matches!(
        &messages[0],
        Message::PaneEvent(_, pane::Event::HideModal)
    ));
    println!(
        "Rendered every registered editor, advanced settings, compact views; pointer activation, selection and close passed. Output: {}",
        output.display()
    );
}
