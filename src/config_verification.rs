//! Opt-in checks against local, copied settings. No private fixtures are committed.
use crate::{connector::ResolvedStream, layout, screen::dashboard::pane::Content};
use data::stream::PersistStreamKind;
use iced::advanced::{
    Layout, layout as widget_layout,
    renderer::{Headless, Renderer as _},
    widget::Tree,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

#[derive(Debug, Default, PartialEq, Eq)]
struct Inventory {
    panes: BTreeMap<String, usize>,
    tickers: BTreeMap<String, usize>,
    indicators: BTreeMap<String, usize>,
    drawings: usize,
}
fn inventory(value: &Value) -> Inventory {
    fn walk(value: &Value, out: &mut Inventory) {
        match value {
            Value::Object(fields) => {
                for (key, value) in fields {
                    if matches!(
                        key.as_str(),
                        "Starter"
                            | "HeatmapChart"
                            | "ShaderHeatmap"
                            | "KlineChart"
                            | "GexChart"
                            | "NewsTracker"
                            | "ComparisonChart"
                            | "TimeAndSales"
                            | "Ladder"
                    ) {
                        *out.panes.entry(key.clone()).or_default() += 1;
                    }
                    if key == "ticker"
                        && let Some(ticker) = value.as_str()
                    {
                        *out.tickers.entry(ticker.to_owned()).or_default() += 1;
                    }
                    if key == "indicators"
                        && let Some(values) = value.as_array()
                    {
                        for value in values {
                            if let Some(name) = value.as_str() {
                                *out.indicators.entry(name.to_owned()).or_default() += 1;
                            }
                        }
                    }
                    if key == "drawings" {
                        out.drawings += value.as_array().map_or(0, Vec::len);
                    }
                    walk(value, out);
                }
            }
            Value::Array(values) => {
                for value in values {
                    walk(value, out);
                }
            }
            _ => {}
        }
    }
    let mut out = Inventory::default();
    walk(value, &mut out);
    out
}

// Expected legacy recovery excludes unsupported sources while keeping chart shells,
// drawings, indicators and all supported streams. User files are never used directly.
fn without_unsupported_sources(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                if let Some(entries) = value.as_array_mut() {
                    match key.as_str() {
                        "stream_type" => entries.retain(|entry| {
                            serde_json::from_value::<PersistStreamKind>(entry.clone()).is_ok()
                        }),
                        "selected_exchanges" => entries.retain(|entry| {
                            serde_json::from_value::<exchange::adapter::Venue>(entry.clone())
                                .is_ok()
                        }),
                        "favorited_tickers" => entries.retain(|entry| {
                            serde_json::from_value::<exchange::Ticker>(entry.clone()).is_ok()
                        }),
                        _ => {}
                    }
                }
                without_unsupported_sources(value);
            }
        }
        Value::Array(values) => values.iter_mut().for_each(without_unsupported_sources),
        _ => {}
    }
}

fn load(path: &Path) -> data::State {
    // Recovery prunes old backups by filename. Give each fixture its own directory
    // so that neither recovery nor backup retention can alter another fixture.
    let scratch = path
        .parent()
        .unwrap()
        .join(format!(".verification-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&scratch).unwrap();
    let copy = scratch.join("saved-state.json");
    std::fs::copy(path, &copy).unwrap();
    let outcome = data::load_saved_state_from_path(&copy);
    std::fs::remove_dir_all(scratch).unwrap();
    match outcome {
        data::StateLoadOutcome::Loaded(state) => state,
        data::StateLoadOutcome::Migrated { state, .. } => state,
        data::StateLoadOutcome::Recovered {
            state, warnings, ..
        } => {
            assert!(
                !warnings.iter().any(|warning| !matches!(
                    warning.as_str(),
                    "dropped unsupported stream"
                        | "dropped unsupported favorite ticker"
                        | "dropped unsupported exchange"
                        | "main window position was invalid or off-screen; opening centered"
                )),
                "unexpected fixture recovery: {warnings:?}"
            );
            state
        }
        _ => panic!("fixture could not load as a saved state"),
    }
}

fn prime_charts(dashboard: &mut crate::screen::dashboard::Dashboard) {
    for (_, pane) in dashboard.panes.iter_mut() {
        let persisted = match &pane.streams {
            ResolvedStream::Waiting { streams, .. } | ResolvedStream::Blocked { streams, .. } => {
                streams.clone()
            }
            ResolvedStream::Ready(_) => continue,
        };
        let ticker = persisted.first().map(|stream| match stream {
            PersistStreamKind::Kline { ticker, .. } | PersistStreamKind::Trades { ticker } => {
                *ticker
            }
            PersistStreamKind::Depth(depth) | PersistStreamKind::DepthAndTrades(depth) => {
                depth.ticker
            }
        });
        let Some(ticker) = ticker else {
            continue;
        };
        let info = exchange::TickerInfo::new(ticker, 0.01, 0.0001, None);
        let kind = pane.content.kind();
        let streams = pane.set_content_and_streams(vec![info], kind);
        pane.streams = ResolvedStream::Ready(streams);
        if let Content::Kline {
            chart: Some(chart), ..
        } = &mut pane.content
        {
            let timeframe = match chart.basis() {
                data::chart::Basis::Time(tf) => tf,
                _ => continue,
            };
            let interval = timeframe.to_milliseconds();
            let end = exchange::UnixMs::now().floor_to(timeframe);
            let bars: Vec<_> = (0..500)
                .map(|index| {
                    let close = 100.0 + (index % 30) as f64 * 0.1 + index as f64 * 0.01;
                    exchange::Kline::new(
                        end.saturating_sub((499 - index) * interval),
                        close - 0.1,
                        close + 0.3,
                        close - 0.3,
                        close,
                        exchange::Volume::BuySell(
                            exchange::unit::Qty::from_f64(8.0),
                            exchange::unit::Qty::from_f64(5.0),
                        ),
                        info.min_ticksize,
                    )
                })
                .collect();
            pane.insert_hist_klines(None, timeframe, info, &bars);
            if let Content::Kline {
                chart: Some(chart), ..
            } = &mut pane.content
            {
                crate::chart::update(
                    chart,
                    &crate::chart::Message::BoundsChanged(iced::Rectangle::with_size(
                        iced::Size::new(750.0, 400.0),
                    )),
                );
            }
        }
    }
}

fn render_dashboard(
    dashboard: &crate::screen::dashboard::Dashboard,
    state: &data::State,
    renderer: &mut iced::Renderer,
    path: &Path,
) {
    let window = crate::window::Window::new(iced::window::Id::unique());
    let client = reqwest::Client::new();
    let handles = exchange::adapter::AdapterHandles::spawn_venues(&client, [], None);
    let (table, _) = crate::screen::dashboard::tickers_table::TickersTable::new(
        handles,
        data::MarketMetadata::default(),
    );
    let theme = &state.selected_theme.0;
    let bounds = iced::Rectangle::with_size(iced::Size::new(1600.0, 1000.0));
    renderer.reset(bounds);
    let mut element = dashboard.view(&window, &table, state.timezone, true);
    let mut tree = Tree::new(element.as_widget());
    tree.diff(element.as_widget_mut());
    let node = element.as_widget_mut().layout(
        &mut tree,
        renderer,
        &widget_layout::Limits::new(iced::Size::ZERO, bounds.size()),
    );
    let headless = iced_core::window::Headless;
    let mut bus = iced_core::shell::Bus::new();
    let mut shell = iced_core::Shell::new(&headless, iced_core::shell::Waker::noop(), &mut bus);
    element.as_widget_mut().update(
        &mut tree,
        &iced::Event::Window(iced::window::Event::RedrawRequested(
            std::time::Instant::now(),
        )),
        Layout::new(&node),
        iced::mouse::Cursor::Unavailable,
        renderer,
        &mut shell,
        &bounds,
    );
    element.as_widget().draw(
        &tree,
        renderer,
        theme,
        &iced::advanced::renderer::Style {
            text_color: theme.palette().background.base.text,
        },
        Layout::new(&node),
        iced::mouse::Cursor::Unavailable,
        &bounds,
    );
    let bytes = Headless::screenshot(
        renderer,
        iced::Size::new(1600, 1000),
        1.0,
        theme.palette().background.base.color,
    );
    assert_eq!(bytes.len(), 1600 * 1000 * 4);
    assert!(
        bytes.chunks_exact(4).any(|pixel| pixel != &bytes[..4]),
        "render must contain visible content"
    );
    std::fs::write(path, bytes).unwrap();
}

#[test]
#[ignore = "requires copied local configurations; set FLOWSURFACE_CONFIG_FIXTURES"]
fn saved_configurations_preserve_layouts_markets_and_settings() {
    let directory = std::path::PathBuf::from(
        std::env::var_os("FLOWSURFACE_CONFIG_FIXTURES").expect("isolated fixture directory"),
    );
    let output = directory.parent().unwrap();
    let mut files: Vec<_> = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().ends_with(".fixture.json"))
        })
        .collect();
    files.sort();
    assert!(!files.is_empty());
    let mut results = Vec::new();
    for path in files {
        let input: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let mut supported_input = input.clone();
        without_unsupported_sources(&mut supported_input);
        let excluded_stream_tickers = inventory(&input).tickers.values().sum::<usize>()
            - inventory(&supported_input).tickers.values().sum::<usize>();
        let state = load(&path);
        let normalized = serde_json::to_value(&state).unwrap();
        assert_eq!(
            inventory(&supported_input),
            inventory(&normalized),
            "saved-state inventory changed"
        );
        assert_eq!(
            state.layout_manager.layouts.len(),
            input["layout_manager"]["layouts"].as_array().unwrap().len()
        );
        let roundtrip: data::State = serde_json::from_value(normalized.clone()).unwrap();
        assert_eq!(serde_json::to_value(roundtrip).unwrap(), normalized);
        for layout in &state.layout_manager.layouts {
            let dashboard =
                layout::dashboard_from_data(layout.dashboard.clone(), uuid::Uuid::new_v4());
            let restored = data::Dashboard::from(&dashboard);
            assert_eq!(
                inventory(&serde_json::to_value(&layout.dashboard).unwrap()),
                inventory(&serde_json::to_value(restored).unwrap()),
                "runtime layout reconstruction changed inventory"
            );
        }
        results.push(json!({"fixture":path.file_name().unwrap().to_string_lossy(),"layouts":state.layout_manager.layouts.len(),"panes":inventory(&normalized).panes,"excluded_stream_tickers":excluded_stream_tickers,"ok":true}));
    }
    std::fs::write(
        output.join("config-results.json"),
        serde_json::to_vec_pretty(&results).unwrap(),
    )
    .unwrap();
    println!(
        "Validated {} copied saved-state configurations",
        results.len()
    );
}

#[test]
#[ignore = "requires local configuration copies and GPU; set FLOWSURFACE_CONFIG_FIXTURES"]
fn render_saved_layouts_and_upstream_indicator_configuration() {
    let directory = std::path::PathBuf::from(
        std::env::var_os("FLOWSURFACE_CONFIG_FIXTURES").expect("isolated fixture directory"),
    );
    let output = directory.parent().unwrap();
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
        .expect("GPU headless renderer");
    {
        let mut fonts = iced_wgpu::graphics::text::font_system().write().unwrap();
        fonts.load_font(std::borrow::Cow::Borrowed(crate::style::AZERET_MONO_BYTES));
        fonts.load_font(std::borrow::Cow::Borrowed(crate::style::ICONS_BYTES));
    }
    for (name, prefix) in [
        ("saved-state.json.fixture.json", "existing"),
        ("new-indicators.fixture.json", "indicators"),
    ] {
        let state = load(&directory.join(name));
        for (index, layout) in state.layout_manager.layouts.iter().enumerate() {
            let mut dashboard =
                layout::dashboard_from_data(layout.dashboard.clone(), uuid::Uuid::new_v4());
            prime_charts(&mut dashboard);
            render_dashboard(
                &dashboard,
                &state,
                &mut renderer,
                &output.join(format!("{prefix}-{index}-1600x1000.rgba")),
            );
            println!("Rendered {prefix} layout {index}");
        }
    }
}
