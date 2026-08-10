#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod chart;
mod connector;
mod layout;
mod logger;
mod market_service;
mod modal;
mod notify;
mod power_guard;
mod screen;
mod style;
mod version;
mod widget;
mod window;
mod windowing;

use connector::client::DataSources;
use data::config::theme::default_theme;
use data::{layout::WindowSpec, sidebar};
use layout::LayoutId;
use modal::{
    LayoutManager, ThemeEditor,
    audio::AudioStream,
    network_editor::{self, NetworkEditor},
};
use modal::{dashboard_modal, main_dialog_modal};
use notify::Notifications;
use screen::dashboard::{self, Dashboard};
use widget::{
    confirm_dialog_container,
    toast::{self, Toast},
    tooltip,
};

use iced::{
    Alignment, Element, Length, Subscription, Task, Theme, keyboard, padding,
    widget::{
        button, column, container, pick_list, progress_bar, row, scrollable, text, text_input,
        tooltip::Position as TooltipPosition,
    },
};
use std::{
    borrow::Cow,
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
    vec,
};
use windowing::WindowingMode;

/// Set to `true` to emit window focus/unfocus and tick diagnostic logs.
/// These are useful for debugging multi-window issues but noisy in normal use.
const DEBUG_WINDOW_DIAGNOSTICS: bool = false;

const DEBUG_TERMINAL_VSCROLL_ID: &str = "debug-terminal-vscroll";
const DEBUG_TERMINAL_HSCROLL_ID: &str = "debug-terminal-hscroll";
const STARTUP_MIN_VISIBLE: Duration = Duration::from_millis(900);
const STARTUP_READY_SETTLE: Duration = Duration::from_millis(650);
const STARTUP_WINDOW_WIDTH: f32 = 500.0;
const STARTUP_WINDOW_HEIGHT: f32 = 420.0;

fn template_file_name(name: &str) -> String {
    let stem = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_ascii_lowercase();
    format!(
        "{}.flowsurface-template.json",
        if stem.is_empty() { "dashboard" } else { &stem }
    )
}

fn main() {
    logger::install_panic_hook();

    if let Err(err) = logger::setup(cfg!(debug_assertions)) {
        logger::report_stderr(&format!("Failed to initialize logger: {err}"));
    }

    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    log::info!(
        "BUILD Info | git_sha={} branch={} profile={}",
        version::BUILD_GIT_SHA.unwrap_or("unknown"),
        version::BUILD_GIT_BRANCH.unwrap_or("unknown"),
        profile
    );

    std::thread::spawn(data::cleanup_old_market_data);

    let daemon = iced::daemon(Flowsurface::new, Flowsurface::update, Flowsurface::view)
        .settings(iced::Settings {
            antialiasing: true,
            fonts: vec![
                Cow::Borrowed(style::AZERET_MONO_BYTES),
                Cow::Borrowed(style::ICONS_BYTES),
            ],
            default_text_size: style::text_size::BODY.into(),
            ..Default::default()
        })
        .title(Flowsurface::title)
        .theme(Flowsurface::theme)
        .scale_factor(Flowsurface::scale_factor)
        .subscription(Flowsurface::subscription);

    if let Err(err) = daemon.run() {
        let message = format!("Runtime error: {err}");
        log::error!("{message}");
        logger::report_stderr(&message);
    }
}

struct Flowsurface {
    main_window: window::Window,
    sidebar: dashboard::Sidebar,
    layout_manager: LayoutManager,
    theme_editor: ThemeEditor,
    network_editor: NetworkEditor,
    network_config: data::Network,
    data_sources: Arc<DataSources>,
    audio_stream: AudioStream,
    confirm_dialog: Option<screen::ConfirmDialog<Message>>,
    startup_warning: Option<StartupWarning>,
    save_state_enabled: bool,
    volume_size_unit: exchange::SizeUnit,
    ui_scale_factor: data::ScaleFactor,
    timezone: data::UserTimezone,
    theme: data::Theme,
    notifications: Notifications,
    windowing_mode: WindowingMode,
    market_connectivity: market_service::MarketConnectivity,
    iceberg_detectors: connector::iceberg::IcebergDetectorRegistry,
    debug_terminal_enabled: bool,
    debug_terminal_window: Option<window::Id>,
    debug_terminal_embedded: bool,
    debug_terminal_logs: Vec<String>,
    debug_terminal_level_filter: DebugLevelFilter,
    debug_terminal_category_filter: DebugLogCategory,
    debug_terminal_search: String,
    debug_terminal_auto_scroll: bool,
    debug_terminal_app_only: bool,
    debug_terminal_compact_mode: bool,
    gex_coordinator: connector::gex::GexDataCoordinator,
    deribit_options_client: Option<exchange::options::deribit::DeribitOptionsClient>,
    quantwheel_gex_client: Option<exchange::options::quantwheel::QuantWheelGexClient>,
    derive_options_client: Option<exchange::options::derive::DeriveOptionsClient>,
    gex_monitor_client: Option<exchange::options::gex_monitor::GexMonitorClient>,
    startup_loading: StartupLoading,
    startup_main_window_target: StartupMainWindowTarget,
    closing_startup_window: Option<window::Id>,
}

#[derive(Debug, Clone, Copy)]
struct StartupMainWindowTarget {
    position: window::Position,
    size: iced::Size,
}

#[derive(Debug)]
struct StartupLoading {
    started_at: Instant,
    ready_since: Option<Instant>,
    finished: bool,
}

impl StartupLoading {
    fn new() -> Self {
        Self {
            started_at: Instant::now(),
            ready_since: None,
            finished: false,
        }
    }

    fn is_active(&self) -> bool {
        !self.finished
    }

    fn observe(&mut self, ready: bool, now: Instant) -> bool {
        if self.finished {
            return false;
        }
        if !ready {
            self.ready_since = None;
            return false;
        }

        let ready_since = *self.ready_since.get_or_insert(now);
        if now.duration_since(self.started_at) >= STARTUP_MIN_VISIBLE
            && now.duration_since(ready_since) >= STARTUP_READY_SETTLE
        {
            self.finished = true;
            return true;
        }

        false
    }
}

struct StartupViewState {
    progress: f32,
    detail: String,
}

fn startup_fun_message() -> &'static str {
    const MESSAGES: &[&str] = &[
        "Oiling the gears…",
        "Decoding market-maker intentions…",
        "Calling Wall Street…",
        "Teaching candles to behave…",
        "Convincing liquidity to show up…",
        "Counting invisible orders…",
        "Calibrating the crystal ball…",
        "Negotiating with the spread…",
        "Waking up the order book…",
        "Asking the whales to make room…",
        "Polishing the price ladder…",
        "Herding rogue candlesticks…",
        "Synchronizing with New York…",
        "Looking for the missing liquidity…",
        "Reading tea leaves in the tape…",
        "Turning coffee into alpha…",
        "Checking if the trend is still your friend…",
        "Summoning the opening bell…",
        "Feeding the data hamsters…",
        "Aligning bids and asks…",
        "Looking under the spread…",
        "Warming up the matching engine…",
        "Making volatility feel welcome…",
        "Asking the bears to wait outside…",
        "Convincing bulls to take the stairs…",
        "Counting ticks so you do not have to…",
        "Finding support in all the right places…",
        "Making resistance slightly less resistant…",
        "Checking whether the candles are sentient…",
        "Translating whale noises into charts…",
        "Rehearsing a very convincing breakout…",
        "Removing emotions from the order book…",
    ];
    let elapsed_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    MESSAGES[(elapsed_ms / 2_200) as usize % MESSAGES.len()]
}

#[derive(Debug, Clone)]
enum StartupWarning {
    SavedStateCorrupt {
        error: String,
        original_path: PathBuf,
        backup_path: Option<PathBuf>,
    },
    SavedStateRecovered {
        warnings: Vec<String>,
        backup_path: Option<PathBuf>,
    },
    SavedStateMigrated {
        from_version: u32,
        to_version: u32,
        backup_path: Option<PathBuf>,
    },
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
enum Message {
    Sidebar(dashboard::sidebar::Message),
    MarketWsEvent(exchange::Event),
    Dashboard {
        /// If `None`, the active layout is used for the event.
        layout_id: Option<uuid::Uuid>,
        event: dashboard::Message,
    },
    Tick(std::time::Instant),
    GexFetchCompleted(connector::gex::GexFetchResult),
    QuantWheelGexFetchCompleted(connector::gex::QuantWheelFetchCompletion),
    DeriveInstrumentsFetchCompleted(connector::gex::DeriveInstrumentsFetchResult),
    DeriveTradesFetchCompleted(connector::gex::DeriveTradesFetchResult),
    GexProxyFetchCompleted(
        (
            exchange::options::OptionsUnderlying,
            Result<exchange::options::gex_monitor::GexProxyHistoryResponse, Arc<str>>,
        ),
    ),
    WindowEvent(window::Event),
    ExitRequested(HashMap<window::Id, WindowSpec>),
    RestartRequested(Option<HashMap<window::Id, WindowSpec>>),
    SaveStateRequested(HashMap<window::Id, WindowSpec>),
    GoBack,
    DataFolderRequested,
    OpenUrlRequested(Cow<'static, str>),
    ThemeSelected(iced_core::Theme),
    ScaleFactorChanged(data::ScaleFactor),
    SetTimezone(data::UserTimezone),
    InvalidateMarketDataCache,
    ToggleDebugTerminal(bool),
    DebugTerminalOpened(window::Id),
    DebugTerminalRefresh,
    DebugTerminalClear,
    DebugTerminalCopyAll,
    DebugTerminalCopyVisible,
    DebugTerminalSearchChanged(String),
    DebugTerminalToggleLevel(DebugLogLevel, bool),
    DebugTerminalToggleAutoScroll(bool),
    DebugTerminalCategoryFilterChanged(DebugLogCategory),
    DebugTerminalToggleAppOnly(bool),
    DebugTerminalToggleCompactMode(bool),
    SetMetadataCaching(bool),
    RefreshMetadata,
    ApplyVolumeSizeUnit(exchange::SizeUnit),
    RemoveNotification(usize),
    StartupContinueWithDefault,
    StartupExitWithoutOverwrite,
    StartupWarningNoop,
    ConnectionOverlayNoop,
    ToggleDialogModal(Option<screen::ConfirmDialog<Message>>),
    ThemeEditor(modal::theme_editor::Message),
    NetworkEditor(modal::network_editor::Message),
    Layouts(modal::layout_manager::Message),
    TemplateImported(Result<Option<Vec<u8>>, String>),
    TemplateExported(Result<Option<String>, String>),
    AudioStream(modal::audio::Message),
}

/// Multi-level filter for the Debug Terminal.
/// Each level can be independently enabled/disabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DebugLevelFilter {
    error: bool,
    warn: bool,
    info: bool,
    debug: bool,
    trace: bool,
}

impl DebugLevelFilter {
    /// Default levels: ERROR, WARN, INFO enabled; DEBUG, TRACE disabled.
    const DEFAULT: Self = Self {
        error: true,
        warn: true,
        info: true,
        debug: false,
        trace: false,
    };

    fn matches(self, line: &str) -> bool {
        let level = debug_line_level(line);
        match level {
            Some(DebugLogLevel::Error) => self.error,
            Some(DebugLogLevel::Warn) => self.warn,
            Some(DebugLogLevel::Info) => self.info,
            Some(DebugLogLevel::Debug) => self.debug,
            Some(DebugLogLevel::Trace) => self.trace,
            // Unknown-level logs show when INFO is enabled (simpler than an extra toggle).
            None => self.info,
        }
    }

    fn toggle(&mut self, level: DebugLogLevel, enabled: bool) {
        match level {
            DebugLogLevel::Error => self.error = enabled,
            DebugLogLevel::Warn => self.warn = enabled,
            DebugLogLevel::Info => self.info = enabled,
            DebugLogLevel::Debug => self.debug = enabled,
            DebugLogLevel::Trace => self.trace = enabled,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DebugLogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

fn debug_line_level(line: &str) -> Option<DebugLogLevel> {
    let level_start = line.find("] [")? + 3;
    let level_end = line[level_start..].find(']')? + level_start;

    match line[level_start..level_end].trim() {
        "ERROR" | "FATAL" => Some(DebugLogLevel::Error),
        "WARN" => Some(DebugLogLevel::Warn),
        "INFO" => Some(DebugLogLevel::Info),
        "DEBUG" => Some(DebugLogLevel::Debug),
        "TRACE" => Some(DebugLogLevel::Trace),
        _ => None,
    }
}

fn debug_log_text_style(
    level: Option<DebugLogLevel>,
) -> impl Fn(&iced::Theme) -> iced::widget::text::Style {
    move |theme| {
        let palette = theme.extended_palette();
        let color = match level {
            Some(DebugLogLevel::Error) => Some(palette.danger.base.color),
            Some(DebugLogLevel::Warn) => Some(palette.primary.strong.color),
            Some(DebugLogLevel::Info) => None,
            Some(DebugLogLevel::Debug) => Some(palette.secondary.strong.color),
            Some(DebugLogLevel::Trace) => Some(palette.background.strongest.color),
            None => None,
        };

        iced::widget::text::Style { color }
    }
}

impl std::fmt::Display for DebugLogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Error => write!(f, "Error"),
            Self::Warn => write!(f, "Warn"),
            Self::Info => write!(f, "Info"),
            Self::Debug => write!(f, "Debug"),
            Self::Trace => write!(f, "Trace"),
        }
    }
}

// ── Debug log entry parsing ─────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DebugLogCategory {
    All,
    Fetch,
    Cache,
    Ws,
    Stream,
    Backfill,
    Chart,
    Bubbles,
    Footprint,
    Kline,
    Oi,
    Data,
    Ui,
    App,
    ThirdParty,
}

impl DebugLogCategory {
    const ALL: [Self; 15] = [
        Self::All,
        Self::Fetch,
        Self::Cache,
        Self::Ws,
        Self::Stream,
        Self::Backfill,
        Self::Chart,
        Self::Bubbles,
        Self::Footprint,
        Self::Kline,
        Self::Oi,
        Self::Data,
        Self::Ui,
        Self::App,
        Self::ThirdParty,
    ];
}

impl std::fmt::Display for DebugLogCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::All => write!(f, "All"),
            Self::Fetch => write!(f, "Fetch"),
            Self::Cache => write!(f, "Cache"),
            Self::Ws => write!(f, "WS"),
            Self::Stream => write!(f, "Stream"),
            Self::Backfill => write!(f, "Backfill"),
            Self::Chart => write!(f, "Chart"),
            Self::Bubbles => write!(f, "Bubbles"),
            Self::Footprint => write!(f, "Footprint"),
            Self::Kline => write!(f, "Kline"),
            Self::Oi => write!(f, "OI"),
            Self::Data => write!(f, "Data"),
            Self::Ui => write!(f, "UI"),
            Self::App => write!(f, "App"),
            Self::ThirdParty => write!(f, "Third-party"),
        }
    }
}

#[derive(Debug, Clone)]
struct DebugLogEntry {
    raw: String,
    timestamp: Option<String>,
    level: Option<DebugLogLevel>,
    target: Option<String>,
    category: DebugLogCategory,
    event: String,
    summary: String,
}

fn parse_debug_log_entry(line: &str) -> DebugLogEntry {
    let raw = line.to_string();
    let mut timestamp = None;
    let mut level = None;
    let mut target = None;

    // Parse format: [timestamp] [LEVEL] [target] message
    let mut remaining = line;

    // Extract timestamp
    if let Some(start) = remaining.find('[')
        && let Some(end) = remaining[start + 1..].find(']')
    {
        timestamp = Some(remaining[start + 1..start + 1 + end].to_string());
        remaining = &remaining[start + 1 + end + 1..];
    }

    // Extract level
    if let Some(start) = remaining.find('[')
        && let Some(end) = remaining[start + 1..].find(']')
    {
        let level_str = remaining[start + 1..start + 1 + end].trim();
        level = match level_str {
            "ERROR" | "FATAL" => Some(DebugLogLevel::Error),
            "WARN" => Some(DebugLogLevel::Warn),
            "INFO" => Some(DebugLogLevel::Info),
            "DEBUG" => Some(DebugLogLevel::Debug),
            "TRACE" => Some(DebugLogLevel::Trace),
            _ => None,
        };
        remaining = &remaining[start + 1 + end + 1..];
    }

    // Extract target
    if let Some(start) = remaining.find('[')
        && let Some(end) = remaining[start + 1..].find(']')
    {
        target = Some(remaining[start + 1..start + 1 + end].to_string());
        remaining = &remaining[start + 1 + end + 1..];
    }

    let message = remaining.trim();
    let (category, event, summary) = classify_log_message(message, target.as_deref());

    DebugLogEntry {
        raw,
        timestamp,
        level,
        target,
        category,
        event,
        summary,
    }
}

fn classify_log_message(message: &str, target: Option<&str>) -> (DebugLogCategory, String, String) {
    // Check for our structured log format: CATEGORY Event | key=value ...
    if let Some(pipe_pos) = message.find('|') {
        let prefix = message[..pipe_pos].trim();
        let details = message[pipe_pos + 1..].trim();

        let parts: Vec<&str> = prefix.split_whitespace().collect();
        if parts.len() >= 2 {
            let cat_str = parts[0];
            let event = parts[1..].join(" ");

            let category = match cat_str {
                "FETCH" | "TRADE" => DebugLogCategory::Fetch,
                "KLINE" => DebugLogCategory::Kline,
                "OI" => DebugLogCategory::Oi,
                "CACHE" => DebugLogCategory::Cache,
                "WS" if event.contains("Backfill") => DebugLogCategory::Backfill,
                "WS" => DebugLogCategory::Ws,
                "STREAM" => DebugLogCategory::Stream,
                "BACKFILL" => DebugLogCategory::Backfill,
                "CHART" if event.contains("Bubbles") => DebugLogCategory::Bubbles,
                "CHART" if event.contains("Footprint") => DebugLogCategory::Footprint,
                "CHART" => DebugLogCategory::Chart,
                "DATA" => DebugLogCategory::Data,
                _ => DebugLogCategory::App,
            };

            // Extract key info for summary
            let summary = extract_summary(details, cat_str);
            return (category, event, summary);
        }
    }

    // Fallback: classify by target
    let category = match target {
        Some(t) if t.starts_with("flowsurface") || t.starts_with("flowsurface_") => {
            if t.contains("exchange") {
                DebugLogCategory::Fetch
            } else {
                DebugLogCategory::App
            }
        }
        Some(t) if t == "iced_wgpu" || t.contains("wgpu") || t.contains("winit") => {
            DebugLogCategory::ThirdParty
        }
        Some("panic") => DebugLogCategory::App,
        Some(_) => DebugLogCategory::ThirdParty,
        None => DebugLogCategory::App,
    };

    (category, String::new(), message.to_string())
}

fn extract_summary(details: &str, cat_str: &str) -> String {
    let mut summary_parts = Vec::new();

    for part in details.split_whitespace() {
        if let Some((key, value)) = part.split_once('=') {
            match key {
                "symbol" | "venue" | "stream" | "range" | "records" | "raw_records"
                | "retained_records" | "trades" | "duration" | "requests" | "session"
                | "reason" | "error" | "req" | "pane" | "panes" | "gap_ms" => {
                    summary_parts.push(format!("{key}={value}"));
                }
                _ => {}
            }
        }
    }

    if summary_parts.is_empty() {
        // For TRADE/KLINE/OI, try to extract symbol and venue from details
        if matches!(cat_str, "TRADE" | "KLINE" | "OI") {
            for part in details.split_whitespace() {
                if let Some(("venue" | "symbol" | "records" | "duration", value)) =
                    part.split_once('=')
                {
                    summary_parts.push(value.to_string());
                }
            }
        }

        if summary_parts.is_empty() {
            return details.to_string();
        }
    }

    summary_parts.join(" ")
}

fn is_app_target(target: Option<&str>) -> bool {
    match target {
        Some(t) => t.starts_with("flowsurface") || t.starts_with("flowsurface_") || t == "panic",
        None => true,
    }
}

impl Flowsurface {
    fn new() -> (Self, Task<Message>) {
        let load_outcome = layout::load_saved_state();
        let (mut saved_state, startup_warning, save_state_enabled) = match load_outcome {
            layout::SavedStateLoadOutcome::Loaded(state)
            | layout::SavedStateLoadOutcome::MissingDefault(state) => (state, None, true),
            layout::SavedStateLoadOutcome::Migrated {
                state,
                from_version,
                to_version,
                backup_path,
            } => (
                state,
                Some(StartupWarning::SavedStateMigrated {
                    from_version,
                    to_version,
                    backup_path,
                }),
                true,
            ),
            layout::SavedStateLoadOutcome::Recovered {
                state,
                warnings,
                backup_path,
            } => (
                state,
                Some(StartupWarning::SavedStateRecovered {
                    warnings,
                    backup_path,
                }),
                true,
            ),
            layout::SavedStateLoadOutcome::Corrupt {
                error,
                original_path,
                backup_path,
            } => (
                layout::SavedState::default(),
                Some(StartupWarning::SavedStateCorrupt {
                    error,
                    original_path,
                    backup_path,
                }),
                false,
            ),
        };

        if window::recover_offscreen_position(&mut saved_state.main_window) {
            log::warn!(
                "WINDOW SavedPositionRecovered | reason=no_reachable_title_bar action=open_centered"
            );
        }

        let (main_window_id, open_main_window, startup_main_window_target) = {
            let (position, size) = saved_state.window();
            let config = window::startup_popup_settings(iced::Size::new(
                STARTUP_WINDOW_WIDTH,
                STARTUP_WINDOW_HEIGHT,
            ));
            let (id, open) = window::open(config);
            (id, open, StartupMainWindowTarget { position, size })
        };

        let data_sources = Arc::new(DataSources::new(&saved_state.network));
        let deribit_options_client = exchange::options::deribit::DeribitOptionsClient::new(
            saved_state.network.proxy.as_ref(),
        )
        .map_err(|error| {
            log::error!("GEX client initialization failed: {error}");
            error
        })
        .ok();
        let quantwheel_gex_client = exchange::options::quantwheel::QuantWheelGexClient::new(
            saved_state.network.proxy.as_ref(),
        )
        .map_err(|error| {
            log::error!("QuantWheel GEX client initialization failed: {error}");
            error
        })
        .ok();
        let gex_monitor_client = exchange::options::gex_monitor::GexMonitorClient::new(
            saved_state.network.proxy.as_ref(),
        )
        .map_err(|error| {
            log::error!("GEX Monitor client initialization failed: {error}");
            error
        })
        .ok();
        let derive_options_client =
            exchange::options::derive::DeriveOptionsClient::new(saved_state.network.proxy.as_ref())
                .map_err(|error| {
                    log::error!("Derive options client initialization failed: {error}");
                    error
                })
                .ok();

        let (sidebar, launch_sidebar) =
            dashboard::Sidebar::new(&saved_state, data_sources.exchange.clone());

        let (audio_stream, audio_init_err) = AudioStream::new(saved_state.audio_cfg);

        let windowing_mode = WindowingMode::platform_default();
        log::info!(
            "WINDOW Mode | mode={windowing_mode} reason={reason}",
            reason = windowing_mode.reason()
        );

        // Keep Windows awake while the application is running.
        #[cfg(target_os = "windows")]
        {
            power_guard::windows_power::init();
        }

        let mut state = Self {
            main_window: window::Window::new(main_window_id),
            layout_manager: saved_state.layout_manager,
            theme_editor: ThemeEditor::new(saved_state.custom_theme),
            audio_stream,
            sidebar,
            data_sources,
            confirm_dialog: None,
            startup_warning,
            save_state_enabled,
            timezone: saved_state.timezone,
            ui_scale_factor: saved_state.scale_factor,
            volume_size_unit: saved_state.volume_size_unit,
            theme: saved_state.theme,
            notifications: Notifications::new(),
            network_config: saved_state.network.clone(),
            network_editor: NetworkEditor::new(&saved_state.network, None),
            windowing_mode,
            market_connectivity: market_service::MarketConnectivity::new(),
            iceberg_detectors: connector::iceberg::IcebergDetectorRegistry::default(),
            debug_terminal_enabled: saved_state.debug_terminal_enabled,
            debug_terminal_window: None,
            debug_terminal_embedded: false,
            debug_terminal_logs: logger::debug_terminal_snapshot(),
            debug_terminal_level_filter: DebugLevelFilter::DEFAULT,
            debug_terminal_category_filter: DebugLogCategory::All,
            debug_terminal_search: String::new(),
            debug_terminal_auto_scroll: true,
            debug_terminal_app_only: true,
            debug_terminal_compact_mode: true,
            gex_coordinator: connector::gex::GexDataCoordinator::default(),
            deribit_options_client,
            quantwheel_gex_client,
            derive_options_client,
            gex_monitor_client,
            startup_loading: StartupLoading::new(),
            startup_main_window_target,
            closing_startup_window: None,
        };

        if let Some(err) = audio_init_err {
            state
                .notifications
                .push(Toast::error(format!("Audio disabled: {err}")));
        }

        match &state.startup_warning {
            Some(StartupWarning::SavedStateMigrated {
                from_version,
                to_version,
                backup_path,
            }) => state.notifications.push(Toast::info(format!(
                "Saved layout migrated from version {from_version} to {to_version}. Backup: {}",
                backup_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "none".to_string())
            ))),
            Some(StartupWarning::SavedStateRecovered {
                warnings,
                backup_path,
            }) => state.notifications.push(Toast::warn(format!(
                "Saved layout was repaired: {} Backup: {}",
                warnings.join("; "),
                backup_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "none".to_string())
            ))),
            Some(StartupWarning::SavedStateCorrupt { .. }) | None => {}
        }

        if state.layout_manager.layouts.is_empty() {
            log::error!("No layouts available after loading state; creating a default layout");
            state.layout_manager = LayoutManager::new();
        }

        let active_layout_id = state
            .layout_manager
            .active_layout_id()
            .or_else(|| {
                state
                    .layout_manager
                    .layouts
                    .first()
                    .map(|layout| &layout.id)
            })
            .map(|layout| layout.unique);

        let load_layout = active_layout_id
            .map(|uid| state.load_layout(uid, main_window_id))
            .unwrap_or_else(|| {
                log::error!("No active layout could be selected at startup");
                Task::none()
            });

        (
            state,
            open_main_window
                .discard()
                .chain(load_layout)
                .chain(launch_sidebar.map(Message::Sidebar)),
        )
    }

    fn apply_connectivity_transition(
        &mut self,
        transition: market_service::ConnectivityTransition,
    ) -> Task<Message> {
        match transition {
            market_service::ConnectivityTransition::None => Task::none(),
            market_service::ConnectivityTransition::WentOffline => {
                log::warn!(
                    "MARKET Offline | connected_streams={} expected_streams={} reason={:?}",
                    self.market_connectivity.connected_count(),
                    self.market_connectivity.expected_count(),
                    self.market_connectivity.last_reason()
                );
                Task::none()
            }
            market_service::ConnectivityTransition::Restored => {
                log::info!(
                    "MARKET Restored | connected_streams={} expected_streams={} action=resume_and_backfill",
                    self.market_connectivity.connected_count(),
                    self.market_connectivity.expected_count()
                );
                self.gex_coordinator.reconnect();

                let data_sources = Arc::clone(&self.data_sources);
                let main_window_id = self.main_window.id;
                let reconnect_time = exchange::UnixMs::now();
                self.active_dashboard_mut()
                    .execute_reconnect_backfill(&data_sources, main_window_id, reconnect_time)
                    .map(move |msg| Message::Dashboard {
                        layout_id: None,
                        event: msg,
                    })
            }
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::MarketWsEvent(event) => {
                let main_window_id = self.main_window.id;

                if let exchange::Event::Connected(streams) = &event {
                    log::info!("WS Connected | streams={}", streams.len());
                    for (idx, stream) in streams.iter().enumerate() {
                        log::debug!(
                            "WS ConnectedStream | idx={idx} stream={}",
                            crate::connector::fetcher::format_stream(stream)
                        );
                    }

                    let transition = self
                        .market_connectivity
                        .record_connected(streams, std::time::Instant::now());
                    return self.apply_connectivity_transition(transition);
                }

                if let exchange::Event::Disconnected(streams, reason) = &event {
                    let now = exchange::UnixMs::now();
                    log::info!(
                        "WS Disconnected | reason={reason:?} streams={} now={}",
                        streams.len(),
                        crate::connector::fetcher::format_time_short(now)
                    );
                    for (idx, stream) in streams.iter().enumerate() {
                        log::debug!(
                            "WS DisconnectedStream | idx={idx} stream={}",
                            crate::connector::fetcher::format_stream(stream)
                        );
                    }

                    // Accumulate every independently disconnected WS group.
                    // Backfill starts only when the aggregate connection state
                    // reports that all required streams have recovered.
                    self.active_dashboard_mut()
                        .record_pending_disconnect_gaps(streams, now);
                    let transition = self.market_connectivity.record_disconnected(
                        streams,
                        reason.clone(),
                        std::time::Instant::now(),
                    );
                    return self.apply_connectivity_transition(transition);
                }

                if let exchange::Event::OrderFlow(orderflow) = &event {
                    let requirements = self.active_dashboard().iceberg_requirements(main_window_id);
                    self.iceberg_detectors
                        .sync_requirements(requirements, exchange::UnixMs::now());
                    let visible_updates = self.iceberg_detectors.ingest(orderflow.clone());
                    if !visible_updates.is_empty() {
                        self.active_dashboard_mut()
                            .ingest_iceberg_events(&visible_updates, main_window_id);
                    }
                    return Task::none();
                }

                if let exchange::Event::OrderFlowTrades(trades) = &event {
                    // Raw Binance trades are batched only for transport. Their original
                    // timestamps and IDs remain intact for deterministic detector ordering.
                    // Syncing pane requirements once per batch also avoids traversing the
                    // dashboard for every market trade on busy symbols.
                    let requirements = self.active_dashboard().iceberg_requirements(main_window_id);
                    self.iceberg_detectors
                        .sync_requirements(requirements, exchange::UnixMs::now());
                    let visible_updates = trades
                        .iter()
                        .flat_map(|trade| {
                            self.iceberg_detectors
                                .ingest(exchange::orderflow::OrderFlowEvent::Trade(*trade))
                        })
                        .collect::<Vec<_>>();
                    if !visible_updates.is_empty() {
                        self.active_dashboard_mut()
                            .ingest_iceberg_events(&visible_updates, main_window_id);
                    }
                    return Task::none();
                }

                let dashboard = self.active_dashboard_mut();

                match event {
                    exchange::Event::Connected(..)
                    | exchange::Event::Disconnected(..)
                    | exchange::Event::OrderFlow(..)
                    | exchange::Event::OrderFlowTrades(..) => {
                        unreachable!("connection events are handled before dashboard routing")
                    }
                    exchange::Event::DepthReceived(stream, update_t, depth) => {
                        log::trace!(
                            "WS DepthReceived | stream={} update_t={} routed=true",
                            crate::connector::fetcher::format_stream(&stream),
                            crate::connector::fetcher::format_time_short(update_t)
                        );
                        let task = dashboard
                            .ingest_depth(&stream, update_t, &depth, main_window_id)
                            .map(move |msg| Message::Dashboard {
                                layout_id: None,
                                event: msg,
                            });

                        return task;
                    }
                    exchange::Event::TradesReceived(stream, update_t, buffer) => {
                        let now = exchange::UnixMs::now();
                        let first_trade_t = buffer.first().map(|trade| trade.time);
                        let last_trade_t = buffer.last().map(|trade| trade.time);
                        log::trace!(
                            "WS TradesReceived | stream={} update_t={} batch_len={} first_trade_t={} last_trade_t={} lag_ms={}",
                            crate::connector::fetcher::format_stream(&stream),
                            crate::connector::fetcher::format_time_short(update_t),
                            buffer.len(),
                            crate::connector::fetcher::format_optional_time(first_trade_t),
                            crate::connector::fetcher::format_optional_time(last_trade_t),
                            now.saturating_diff(last_trade_t.unwrap_or(update_t))
                        );
                        let task = dashboard
                            .ingest_trades(&stream, &buffer, update_t, main_window_id)
                            .map(move |msg| Message::Dashboard {
                                layout_id: None,
                                event: msg,
                            });

                        if let Some(msg) = self.audio_stream.try_play_sound(&stream, &buffer) {
                            self.notifications.push(Toast::error(msg));
                        }

                        return task;
                    }
                    exchange::Event::KlineReceived(stream, kline) => {
                        let now = exchange::UnixMs::now();
                        log::trace!(
                            "WS KlineReceived | stream={} kline_t={} open={:?} high={:?} low={:?} close={:?} volume={:?} lag_ms={}",
                            crate::connector::fetcher::format_stream(&stream),
                            crate::connector::fetcher::format_time_short(kline.time),
                            kline.open,
                            kline.high,
                            kline.low,
                            kline.close,
                            kline.volume,
                            now.saturating_diff(kline.time)
                        );
                        return dashboard
                            .update_latest_klines(&stream, &kline, main_window_id)
                            .map(move |msg| Message::Dashboard {
                                layout_id: None,
                                event: msg,
                            });
                    }
                }
            }
            Message::Tick(now) => {
                self.iceberg_detectors
                    .collect_garbage(exchange::UnixMs::now());
                // Throttled tick debug logging (once every 2 seconds)
                if DEBUG_WINDOW_DIAGNOSTICS {
                    static LAST_TICK_LOG: std::sync::Mutex<Option<std::time::Instant>> =
                        std::sync::Mutex::new(None);
                    if let Ok(mut last) = LAST_TICK_LOG.lock()
                        && last.is_none_or(|t| t.elapsed() > Duration::from_secs(2))
                    {
                        let popout_count = self.active_dashboard().popout.len();
                        log::trace!(
                            "[tick] main={:?}, debug_term={:?}, popouts={}",
                            self.main_window.id,
                            self.debug_terminal_window,
                            popout_count
                        );
                        *last = Some(now);
                    }
                }

                let expected_streams = self.active_dashboard().configured_market_streams();
                let sync_transition = self
                    .market_connectivity
                    .sync_expected(&expected_streams, now);
                let transition = if sync_transition != market_service::ConnectivityTransition::None
                {
                    sync_transition
                } else {
                    self.market_connectivity.tick(now)
                };
                let connectivity_task = self.apply_connectivity_transition(transition);

                let consumers = self
                    .layout_manager
                    .iter_dashboards()
                    .flat_map(Dashboard::gex_consumers)
                    .collect::<Vec<_>>();
                self.gex_coordinator.set_consumers(consumers);
                self.sync_gex_dashboard(exchange::UnixMs::now());

                let startup_ready = self.startup_dependencies_ready();
                let startup_completed = self.startup_loading.observe(startup_ready, now);
                let startup_task = if startup_completed {
                    log::info!(
                        "STARTUP Ready | elapsed_ms={} panes={} streams={}",
                        now.duration_since(self.startup_loading.started_at)
                            .as_millis(),
                        self.active_dashboard()
                            .startup_load_status(self.main_window.id)
                            .pane_count,
                        expected_streams.len()
                    );
                    let dashboard_window = self.open_main_dashboard_window();
                    let popouts = self.open_startup_popouts();
                    let debug_terminal = if self.debug_terminal_enabled {
                        self.open_debug_terminal()
                    } else {
                        Task::none()
                    };
                    Task::batch([dashboard_window, popouts, debug_terminal])
                } else {
                    Task::none()
                };

                // Keep WS subscriptions alive so their reconnect loop can run,
                // but freeze chart timers/fetch scheduling while offline. The
                // reconnect transition performs the missing-range backfill.
                if self.market_connectivity.overlay_visible() {
                    return Task::batch([connectivity_task, startup_task]);
                }

                let gex_now = exchange::UnixMs::now();
                let gex_tasks = if let Some(client) = self.deribit_options_client.clone() {
                    self.gex_coordinator
                        .due_fetches(gex_now, true)
                        .into_iter()
                        .map(|request| {
                            let instruments = self.gex_coordinator.instruments_for(request.key());
                            Task::perform(
                                connector::gex::execute_fetch(client.clone(), request, instruments),
                                Message::GexFetchCompleted,
                            )
                        })
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                let proxy_tasks = if let Some(client) = self.gex_monitor_client.clone() {
                    self.gex_coordinator
                        .due_proxy_fetches(gex_now, true)
                        .into_iter()
                        .map(|underlying| {
                            Task::perform(
                                connector::gex::execute_proxy_fetch(client.clone(), underlying),
                                Message::GexProxyFetchCompleted,
                            )
                        })
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                let quantwheel_tasks = if let Some(client) = self.quantwheel_gex_client.clone()
                    && self.gex_coordinator.due_quantwheel_fetch(gex_now, true)
                {
                    vec![Task::perform(
                        connector::gex::execute_quantwheel_fetch(client),
                        Message::QuantWheelGexFetchCompleted,
                    )]
                } else {
                    Vec::new()
                };
                let derive_instrument_tasks =
                    if let Some(client) = self.derive_options_client.clone() {
                        self.gex_coordinator
                            .due_derive_instrument_fetches(gex_now, true)
                            .into_iter()
                            .map(|underlying| {
                                Task::perform(
                                    connector::gex::execute_derive_instruments_fetch(
                                        client.clone(),
                                        underlying,
                                    ),
                                    Message::DeriveInstrumentsFetchCompleted,
                                )
                            })
                            .collect::<Vec<_>>()
                    } else {
                        Vec::new()
                    };
                let derive_trade_tasks = if let Some(client) = self.derive_options_client.clone() {
                    self.gex_coordinator
                        .due_derive_trade_fetches(gex_now, true)
                        .into_iter()
                        .map(|request| {
                            let instruments = self
                                .gex_coordinator
                                .derive_instruments_for(request.underlying);
                            Task::perform(
                                connector::gex::execute_derive_trades_fetch(
                                    client.clone(),
                                    request,
                                    instruments,
                                ),
                                Message::DeriveTradesFetchCompleted,
                            )
                        })
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                };

                let main_window_id = self.main_window.id;
                let data_sources = Arc::clone(&self.data_sources);

                let chart_tick = self
                    .active_dashboard_mut()
                    .tick(now, &data_sources, main_window_id)
                    .map(move |msg| Message::Dashboard {
                        layout_id: None,
                        event: msg,
                    });

                return Task::batch(
                    [connectivity_task, chart_tick, startup_task]
                        .into_iter()
                        .chain(gex_tasks)
                        .chain(proxy_tasks)
                        .chain(quantwheel_tasks)
                        .chain(derive_instrument_tasks)
                        .chain(derive_trade_tasks)
                        .collect::<Vec<_>>(),
                );
            }
            Message::GexFetchCompleted(completion) => {
                let now = exchange::UnixMs::now();
                self.gex_coordinator.complete(completion, now);
                self.sync_gex_dashboard(now);
                return Task::none();
            }
            Message::QuantWheelGexFetchCompleted(result) => {
                let now = exchange::UnixMs::now();
                self.gex_coordinator.complete_quantwheel_fetch(result, now);
                self.sync_gex_dashboard(now);
                return Task::none();
            }
            Message::GexProxyFetchCompleted((underlying, result)) => {
                let now = exchange::UnixMs::now();
                self.gex_coordinator.complete_proxy(underlying, result, now);
                self.sync_gex_dashboard(now);
                return Task::none();
            }
            Message::DeriveInstrumentsFetchCompleted(completion) => {
                let now = exchange::UnixMs::now();
                self.gex_coordinator
                    .complete_derive_instruments(completion, now);
                self.sync_gex_dashboard(now);
                return Task::none();
            }
            Message::DeriveTradesFetchCompleted(completion) => {
                let now = exchange::UnixMs::now();
                self.gex_coordinator.complete_derive_trades(completion, now);
                self.sync_gex_dashboard(now);
                return Task::none();
            }
            Message::WindowEvent(event) => match event {
                window::Event::CloseRequested(window) => {
                    if self.debug_terminal_window == Some(window) {
                        self.debug_terminal_window = None;
                        self.debug_terminal_enabled = false;
                        return window::close(window);
                    }

                    let main_window = self.main_window.id;
                    let startup_active = self.startup_loading.is_active();
                    if startup_active {
                        let windows = match self.startup_main_window_target.position {
                            window::Position::Specific(position) => HashMap::from([(
                                main_window,
                                WindowSpec::from((
                                    &position,
                                    &self.startup_main_window_target.size,
                                )),
                            )]),
                            window::Position::Centered
                            | window::Position::Default
                            | window::Position::SpecificWith(_) => HashMap::new(),
                        };
                        return Task::done(Message::ExitRequested(windows));
                    }
                    let dashboard = self.active_dashboard_mut();

                    if window != main_window {
                        dashboard.popout.remove(&window);
                        return window::close(window);
                    }

                    let mut active_windows = dashboard
                        .popout
                        .keys()
                        .copied()
                        .collect::<Vec<window::Id>>();
                    active_windows.push(main_window);

                    return window::collect_window_specs(active_windows, Message::ExitRequested);
                }
                window::Event::Focused(id) => {
                    if DEBUG_WINDOW_DIAGNOSTICS {
                        log::debug!(
                            "[window] Focused: id={:?} ({})",
                            id,
                            self.debug_window_label(id)
                        );
                    }
                }
                window::Event::Unfocused(id) => {
                    if DEBUG_WINDOW_DIAGNOSTICS {
                        log::debug!(
                            "[window] Unfocused: id={:?} ({})",
                            id,
                            self.debug_window_label(id)
                        );
                    }
                }
            },
            Message::ExitRequested(windows) => {
                if self.save_state_enabled {
                    self.save_state_to_disk(&windows);
                } else {
                    log::warn!(
                        "SAVED_STATE SaveSkipped | reason=awaiting_corrupt_state_confirmation"
                    );
                }
                power_guard::windows_power::cleanup();
                return iced::exit();
            }
            Message::SaveStateRequested(windows) => {
                if self.save_state_enabled {
                    self.save_state_to_disk(&windows);
                } else {
                    log::warn!(
                        "SAVED_STATE SaveSkipped | reason=awaiting_corrupt_state_confirmation"
                    );
                }
            }
            Message::RestartRequested(Some(windows)) => {
                if self.save_state_enabled {
                    self.save_state_to_disk(&windows);
                } else {
                    log::warn!(
                        "SAVED_STATE SaveSkipped | reason=awaiting_corrupt_state_confirmation"
                    );
                }
                return self.restart();
            }
            Message::RestartRequested(None) => {
                self.confirm_dialog = None;

                let mut active_windows = self
                    .active_dashboard()
                    .popout
                    .keys()
                    .copied()
                    .collect::<Vec<window::Id>>();
                active_windows.push(self.main_window.id);

                return window::collect_window_specs(active_windows, |windows| {
                    Message::RestartRequested(Some(windows))
                });
            }
            Message::GoBack => {
                let main_window = self.main_window.id;

                if self.confirm_dialog.is_some() {
                    self.confirm_dialog = None;
                } else if self.sidebar.active_menu().is_some() {
                    self.sidebar.set_menu(None);
                } else {
                    let dashboard = self.active_dashboard_mut();

                    if dashboard.go_back(main_window) {
                        return Task::none();
                    } else if dashboard.focus.is_some() {
                        dashboard.focus = None;
                    } else {
                        self.sidebar.hide_tickers_table();
                    }
                }
            }
            Message::ThemeSelected(theme) => {
                self.theme = data::Theme(theme.clone());

                let main_window = self.main_window.id;
                self.active_dashboard_mut()
                    .theme_updated(main_window, &theme);
            }
            Message::Dashboard {
                layout_id: id,
                event: msg,
            } => {
                let Some(active_layout) = self.layout_manager.active_layout_id() else {
                    log::error!("No active layout to handle dashboard message");
                    return Task::none();
                };

                let main_window = self.main_window;
                let layout_id = id.unwrap_or(active_layout.unique);
                if let Some(dashboard) = self.layout_manager.mut_dashboard(layout_id) {
                    let (main_task, event) = dashboard.update(
                        msg,
                        &main_window,
                        &layout_id,
                        &self.data_sources,
                        self.windowing_mode,
                    );

                    let additional_task = match event {
                        Some(dashboard::Event::DistributeFetchedData {
                            layout_id,
                            pane_id,
                            data,
                            stream,
                        }) => dashboard
                            .distribute_fetched_data(main_window.id, pane_id, data, stream, false)
                            .map(move |msg| Message::Dashboard {
                                layout_id: Some(layout_id),
                                event: msg,
                            }),
                        Some(dashboard::Event::Notification(toast)) => {
                            self.notifications.push(toast);
                            Task::none()
                        }
                        Some(dashboard::Event::ResolveStreams { pane_id, streams }) => {
                            let tickers_info = self.sidebar.tickers_info();

                            let resolved_streams =
                                streams.into_iter().try_fold(vec![], |mut acc, persist| {
                                    let resolver = |t: &exchange::Ticker| {
                                        tickers_info.get(t).and_then(|opt| *opt)
                                    };

                                    match persist.into_stream_kinds(resolver) {
                                        Ok(mut resolved) => {
                                            acc.append(&mut resolved);
                                            Ok(acc)
                                        }
                                        Err(err) => Err(err),
                                    }
                                });

                            match resolved_streams {
                                Ok(resolved) => {
                                    if resolved.is_empty() {
                                        Task::none()
                                    } else {
                                        dashboard
                                            .resolve_streams(main_window.id, pane_id, resolved)
                                            .map(move |msg| Message::Dashboard {
                                                layout_id: None,
                                                event: msg,
                                            })
                                    }
                                }
                                Err(err) => {
                                    if self.sidebar.is_metadata_loading() {
                                        // Metadata fetches are still in flight
                                        log::debug!(
                                            "Deferring stream resolution for pane {pane_id}: metadata still loading ({err})"
                                        );
                                    } else {
                                        log::debug!("Blocking streams for pane {pane_id}: {err}");
                                        dashboard.block_streams(
                                            main_window.id,
                                            pane_id,
                                            format!("Metadata not available: {err}"),
                                        );
                                    }
                                    Task::none()
                                }
                            }
                        }
                        Some(dashboard::Event::RequestPalette) => {
                            let theme = self.theme.0.clone();

                            let main_window = self.main_window.id;
                            self.active_dashboard_mut()
                                .theme_updated(main_window, &theme);

                            Task::none()
                        }
                        None => Task::none(),
                    };

                    return main_task
                        .map(move |msg| Message::Dashboard {
                            layout_id: Some(layout_id),
                            event: msg,
                        })
                        .chain(additional_task);
                }
            }
            Message::RemoveNotification(index) => {
                self.notifications.remove(index);
            }
            Message::StartupContinueWithDefault => {
                self.save_state_enabled = true;
                self.startup_warning = None;
                self.notifications.push(Toast::warn(
                    "Default layout is active. The next save will overwrite saved-state.json; the backup remains available.",
                ));
            }
            Message::StartupExitWithoutOverwrite => {
                self.save_state_enabled = false;
                power_guard::windows_power::cleanup();
                return iced::exit();
            }
            Message::StartupWarningNoop => {}
            Message::ConnectionOverlayNoop => {}
            Message::SetTimezone(tz) => {
                self.timezone = tz;
            }
            Message::SetMetadataCaching(enabled) => {
                self.sidebar.set_cache_enabled(enabled);
            }
            Message::RefreshMetadata => {
                if let Some(task) = self.sidebar.force_refresh_metadata() {
                    return task.map(Message::Sidebar);
                }
            }
            Message::ScaleFactorChanged(value) => {
                self.ui_scale_factor = value;
            }
            Message::InvalidateMarketDataCache => {
                self.confirm_dialog = None;

                let result = connector::persistent_cache::market_cache()
                    .ok_or_else(|| "Market-data cache is unavailable".to_string())
                    .and_then(|cache| cache.clear_all());
                let gex_result = self
                    .gex_coordinator
                    .invalidate_persistent()
                    .map_err(|error| error.to_string());

                match result.and(gex_result) {
                    Ok(()) => {
                        self.layout_manager
                            .iter_dashboards_mut()
                            .for_each(|dashboard| {
                                dashboard.invalidate_market_data_cache(&self.main_window);
                            });
                        self.notifications.push(Toast::info(
                            "Market-data cache cleared. Charts are rebuilding from newest to oldest."
                                .to_string(),
                        ));
                    }
                    Err(error) => {
                        log::error!("CACHE Clear Error | error={error}");
                        self.notifications.push(Toast::error(format!(
                            "Could not clear market-data cache: {error}"
                        )));
                    }
                }
            }
            Message::ToggleDebugTerminal(enabled) => {
                self.debug_terminal_enabled = enabled;

                if enabled {
                    self.debug_terminal_logs = logger::debug_terminal_snapshot();
                    return self.open_debug_terminal();
                } else {
                    if let Some(window) = self.debug_terminal_window.take() {
                        return window::close(window);
                    }
                    self.debug_terminal_embedded = false;
                }
            }
            Message::DebugTerminalOpened(window) => {
                self.debug_terminal_window = Some(window);
                self.debug_terminal_logs = logger::debug_terminal_snapshot();
                if self.debug_terminal_auto_scroll {
                    return self.scroll_debug_terminal_to_bottom();
                }
            }
            Message::DebugTerminalRefresh => {
                if self.debug_terminal_enabled || self.debug_terminal_window.is_some() {
                    self.debug_terminal_logs = logger::debug_terminal_snapshot();
                    if self.debug_terminal_auto_scroll {
                        return self.scroll_debug_terminal_to_bottom();
                    }
                }
            }
            Message::DebugTerminalClear => {
                logger::clear_debug_terminal();
                self.debug_terminal_logs.clear();
            }
            Message::DebugTerminalCopyAll => {
                return iced::clipboard::write(self.debug_terminal_logs.join("\n"));
            }
            Message::DebugTerminalCopyVisible => {
                let visible: Vec<String> = self
                    .filtered_debug_terminal_entries()
                    .into_iter()
                    .map(|e| e.raw)
                    .collect();
                return iced::clipboard::write(visible.join("\n"));
            }
            Message::DebugTerminalSearchChanged(value) => {
                self.debug_terminal_search = value;
            }
            Message::DebugTerminalToggleLevel(level, enabled) => {
                self.debug_terminal_level_filter.toggle(level, enabled);
            }
            Message::DebugTerminalToggleAutoScroll(enabled) => {
                self.debug_terminal_auto_scroll = enabled;
                if enabled {
                    return self.scroll_debug_terminal_to_bottom();
                }
            }
            Message::DebugTerminalCategoryFilterChanged(category) => {
                self.debug_terminal_category_filter = category;
            }
            Message::DebugTerminalToggleAppOnly(app_only) => {
                self.debug_terminal_app_only = app_only;
            }
            Message::DebugTerminalToggleCompactMode(compact) => {
                self.debug_terminal_compact_mode = compact;
            }
            Message::ToggleDialogModal(dialog) => {
                self.confirm_dialog = dialog;
            }
            Message::Layouts(message) => {
                let action = self.layout_manager.update(message);

                match action {
                    Some(modal::layout_manager::Action::Select(layout)) => {
                        let active_popout_keys = self
                            .active_dashboard()
                            .popout
                            .keys()
                            .copied()
                            .collect::<Vec<_>>();

                        let window_tasks = Task::batch(
                            active_popout_keys
                                .iter()
                                .map(|&popout_id| window::close::<window::Id>(popout_id))
                                .collect::<Vec<_>>(),
                        )
                        .discard();

                        let old_layout_id = self
                            .layout_manager
                            .active_layout_id()
                            .as_ref()
                            .map(|layout| layout.unique);

                        return window::collect_window_specs(
                            active_popout_keys,
                            dashboard::Message::SavePopoutSpecs,
                        )
                        .map(move |msg| Message::Dashboard {
                            layout_id: old_layout_id,
                            event: msg,
                        })
                        .chain(window_tasks)
                        .chain(self.load_layout(layout, self.main_window.id));
                    }
                    Some(modal::layout_manager::Action::Clone(id)) => {
                        let manager = &mut self.layout_manager;

                        let source_data = manager.get(id).map(|layout| {
                            (
                                layout.id.name.clone(),
                                data::Dashboard::from(&layout.dashboard),
                            )
                        });

                        if let Some((name, ser_dashboard)) = source_data {
                            let new_uid = uuid::Uuid::new_v4();
                            let new_layout = LayoutId {
                                unique: new_uid,
                                name: manager.ensure_unique_name(&name, new_uid),
                            };
                            let dashboard = layout::dashboard_from_data(ser_dashboard, new_uid);
                            manager.insert_layout(new_layout, dashboard);
                            self.notifications.push(Toast::info(
                                "Current dashboard saved as a new template".to_string(),
                            ));
                        }
                    }
                    Some(modal::layout_manager::Action::Overwrite { source, target }) => {
                        let dashboard = self
                            .layout_manager
                            .get(source)
                            .map(|layout| data::Dashboard::from(&layout.dashboard));
                        if let Some(dashboard) = dashboard
                            && let Some(target_layout) = self.layout_manager.get_mut(target)
                        {
                            target_layout.dashboard =
                                layout::dashboard_from_data(dashboard, target);
                            self.notifications.push(Toast::info(format!(
                                "Saved current dashboard to {}",
                                target_layout.id.name
                            )));
                        }
                    }
                    Some(modal::layout_manager::Action::Export(id)) => {
                        let result = self
                            .layout_manager
                            .get(id)
                            .ok_or_else(|| "Template not found".to_string())
                            .and_then(layout::export_template);
                        let bytes = match result {
                            Ok(bytes) => bytes,
                            Err(error) => {
                                self.notifications.push(Toast::error(error));
                                return Task::none();
                            }
                        };
                        let suggested_name = self
                            .layout_manager
                            .get(id)
                            .map(|layout| template_file_name(&layout.id.name))
                            .unwrap_or_else(|| "dashboard-template.json".to_string());
                        return Task::perform(
                            async move {
                                let Some(file) = rfd::AsyncFileDialog::new()
                                    .add_filter("FlowSurface template", &["json"])
                                    .set_file_name(suggested_name)
                                    .save_file()
                                    .await
                                else {
                                    return Ok(None);
                                };
                                file.write(&bytes)
                                    .await
                                    .map_err(|error| error.to_string())?;
                                Ok(Some(file.path().display().to_string()))
                            },
                            Message::TemplateExported,
                        );
                    }
                    Some(modal::layout_manager::Action::Import) => {
                        return Task::perform(
                            async {
                                let Some(file) = rfd::AsyncFileDialog::new()
                                    .add_filter("FlowSurface template", &["json"])
                                    .pick_file()
                                    .await
                                else {
                                    return Ok(None);
                                };
                                Ok(Some(file.read().await))
                            },
                            Message::TemplateImported,
                        );
                    }
                    None => {}
                }
            }
            Message::TemplateImported(result) => match result {
                Ok(Some(bytes)) => match layout::import_template(&bytes) {
                    Ok(imported) => {
                        let id = uuid::Uuid::new_v4();
                        let name = self.layout_manager.ensure_unique_name(&imported.name, id);
                        let dashboard = layout::dashboard_from_data(imported.dashboard, id);
                        self.layout_manager.insert_layout(
                            LayoutId {
                                unique: id,
                                name: name.clone(),
                            },
                            dashboard,
                        );
                        self.notifications
                            .push(Toast::info(format!("Imported template {name}")));
                    }
                    Err(error) => self.notifications.push(Toast::error(error)),
                },
                Ok(None) => {}
                Err(error) => self
                    .notifications
                    .push(Toast::error(format!("Could not import template: {error}"))),
            },
            Message::TemplateExported(result) => match result {
                Ok(Some(path)) => self
                    .notifications
                    .push(Toast::info(format!("Template exported to {path}"))),
                Ok(None) => {}
                Err(error) => self
                    .notifications
                    .push(Toast::error(format!("Could not export template: {error}"))),
            },
            Message::AudioStream(message) => {
                if let Some(event) = self.audio_stream.update(message) {
                    match event {
                        modal::audio::UpdateEvent::RetryFailed(err) => {
                            self.notifications
                                .push(Toast::error(format!("Audio still unavailable: {err}")));
                        }
                        modal::audio::UpdateEvent::RetrySucceeded => {
                            self.notifications.push(Toast::info(
                                "Audio output re-initialized successfully".to_string(),
                            ));
                        }
                    }
                }
            }
            Message::DataFolderRequested => {
                if let Err(err) = data::open_data_folder() {
                    self.notifications
                        .push(Toast::error(format!("Failed to open data folder: {err}")));
                }
            }
            Message::OpenUrlRequested(url) => {
                if let Err(err) = data::open_url(url.as_ref()) {
                    self.notifications
                        .push(Toast::error(format!("Failed to open link: {err}")));
                }
            }
            Message::ThemeEditor(msg) => {
                let action = self.theme_editor.update(msg, &self.theme.clone().into());

                match action {
                    Some(modal::theme_editor::Action::Exit) => {
                        self.sidebar.set_menu(Some(sidebar::Menu::Settings));
                    }
                    Some(modal::theme_editor::Action::UpdateTheme(theme)) => {
                        self.theme = data::Theme(theme.clone());

                        let main_window = self.main_window.id;
                        self.active_dashboard_mut()
                            .theme_updated(main_window, &theme);
                    }
                    None => {}
                }
            }
            Message::NetworkEditor(msg) => {
                let action = self.network_editor.update(msg);

                match action {
                    Some(network_editor::Action::ApplyProxy(ref proxy)) => {
                        if let Some(proxy) = proxy {
                            data::config::auth::save_proxy_auth(proxy);
                        } else if let Some(ref old_proxy) = self.network_config.proxy {
                            data::config::auth::delete_proxy_auth(old_proxy);
                        }
                        self.network_config.proxy = proxy.clone();

                        self.confirm_dialog = Some(
                            screen::ConfirmDialog::new(
                                "Proxy changes saved. Restart now to apply?".to_string(),
                                Box::new(Message::RestartRequested(None)),
                            )
                            .with_confirm_btn_text("Restart now".to_string()),
                        );

                        let main_window = self.main_window.id;
                        let dashboard = self.active_dashboard_mut();

                        let mut active_windows = dashboard
                            .popout
                            .keys()
                            .copied()
                            .collect::<Vec<window::Id>>();
                        active_windows.push(main_window);

                        return window::collect_window_specs(
                            active_windows,
                            Message::SaveStateRequested,
                        );
                    }
                    Some(network_editor::Action::ApplyServerConfig {
                        mode,
                        url,
                        auth_token,
                    }) => {
                        if let Some(ref url) = url
                            && let Some(ref token) = auth_token
                        {
                            data::config::auth::save_server_token(url, token);
                        } else if let Some(ref old_url) = self.network_config.server_url {
                            data::config::auth::delete_server_token(old_url);
                        }
                        self.network_config.server_url = url;
                        self.network_config.server_auth_token = auth_token;
                        self.network_config.trade_fetch_mode = mode;

                        self.confirm_dialog = Some(
                            screen::ConfirmDialog::new(
                                "Trade fetch mode changed. Restart now to apply?".to_string(),
                                Box::new(Message::RestartRequested(None)),
                            )
                            .with_confirm_btn_text("Restart now".to_string()),
                        );

                        let main_window = self.main_window.id;
                        let mut active_windows = self
                            .active_dashboard()
                            .popout
                            .keys()
                            .copied()
                            .collect::<Vec<window::Id>>();
                        active_windows.push(main_window);

                        return window::collect_window_specs(
                            active_windows,
                            Message::SaveStateRequested,
                        );
                    }
                    Some(network_editor::Action::Exit) => {
                        self.sidebar.set_menu(Some(sidebar::Menu::Settings));
                    }
                    None => {}
                }
            }
            Message::Sidebar(message) => {
                let (task, action) = self.sidebar.update(message);

                match action {
                    Some(dashboard::sidebar::Action::MenuChanged(Some(sidebar::Menu::Network))) => {
                        self.network_editor = NetworkEditor::new(
                            &self.network_config,
                            self.network_editor.pending_apply().cloned(),
                        );
                    }
                    Some(dashboard::sidebar::Action::MenuChanged(_)) => {}
                    Some(dashboard::sidebar::Action::AddViewSelected(kind)) => {
                        let handles = self.data_sources.exchange.clone();
                        let main_window = self.main_window.id;
                        return self
                            .active_dashboard_mut()
                            .add_view(&handles, main_window, kind)
                            .map(|event| Message::Dashboard {
                                layout_id: None,
                                event,
                            });
                    }
                    Some(dashboard::sidebar::Action::TickerSelected(ticker_info, content)) => {
                        let main_window_id = self.main_window.id;
                        let handles = self.data_sources.exchange.clone();

                        let task = {
                            if let Some(kind) = content {
                                self.active_dashboard_mut().init_focused_pane(
                                    &handles,
                                    main_window_id,
                                    ticker_info,
                                    kind,
                                )
                            } else {
                                self.active_dashboard_mut().switch_tickers_in_group(
                                    &handles,
                                    main_window_id,
                                    ticker_info,
                                )
                            }
                        };

                        return task.map(move |msg| Message::Dashboard {
                            layout_id: None,
                            event: msg,
                        });
                    }
                    Some(dashboard::sidebar::Action::ErrorOccurred(err)) => {
                        self.notifications.push(Toast::error(err.to_string()));
                    }
                    None => {}
                }

                return task.map(Message::Sidebar);
            }
            Message::ApplyVolumeSizeUnit(pref) => {
                self.volume_size_unit = pref;
                self.confirm_dialog = None;

                let mut active_windows: Vec<window::Id> =
                    self.active_dashboard().popout.keys().copied().collect();
                active_windows.push(self.main_window.id);

                return window::collect_window_specs(active_windows, |windows| {
                    Message::RestartRequested(Some(windows))
                });
            }
        }
        Task::none()
    }

    fn startup_dependencies_ready(&self) -> bool {
        let mut dashboard = self
            .active_dashboard()
            .startup_load_status(self.main_window.id);
        if self.deribit_options_client.is_none() {
            dashboard.loading_gex = 0;
        }

        !self.sidebar.is_metadata_loading()
            && dashboard.is_ready()
            && self.market_connectivity.is_online()
    }

    fn startup_view_state(&self) -> StartupViewState {
        let (metadata_loaded, metadata_total) = self.sidebar.metadata_loading_progress();
        let mut dashboard = self
            .active_dashboard()
            .startup_load_status(self.main_window.id);
        if self.deribit_options_client.is_none() {
            dashboard.loading_gex = 0;
        }

        if self.sidebar.is_metadata_loading() {
            let ratio = if metadata_total == 0 {
                0.0
            } else {
                metadata_loaded as f32 / metadata_total as f32
            };
            return StartupViewState {
                progress: 0.08 + ratio * 0.24,
                detail: format!("Loading market metadata ({metadata_loaded}/{metadata_total})…"),
            };
        }
        if dashboard.unresolved_streams > 0 {
            return StartupViewState {
                progress: 0.42,
                detail: format!(
                    "Resolving data sources for {} chart{}…",
                    dashboard.unresolved_streams,
                    if dashboard.unresolved_streams == 1 {
                        ""
                    } else {
                        "s"
                    }
                ),
            };
        }
        if dashboard.initializing_panes > 0 {
            return StartupViewState {
                progress: 0.56,
                detail: format!(
                    "Preparing {} chart{}…",
                    dashboard.initializing_panes,
                    if dashboard.initializing_panes == 1 {
                        ""
                    } else {
                        "s"
                    }
                ),
            };
        }
        if !self.market_connectivity.is_online() {
            let connected = self.market_connectivity.connected_count();
            let expected = self.market_connectivity.expected_count();
            let ratio = if expected == 0 {
                0.0
            } else {
                connected as f32 / expected as f32
            };
            return StartupViewState {
                progress: 0.62 + ratio * 0.14,
                detail: if expected == 0 {
                    "Starting market services…".to_string()
                } else {
                    format!("Connecting market streams ({connected}/{expected})…")
                },
            };
        }
        if dashboard.loading_panes > 0 {
            return StartupViewState {
                progress: 0.82,
                detail: format!(
                    "Loading initial data for {} chart{}…",
                    dashboard.loading_panes,
                    if dashboard.loading_panes == 1 {
                        ""
                    } else {
                        "s"
                    }
                ),
            };
        }
        if dashboard.loading_gex > 0 {
            return StartupViewState {
                progress: 0.92,
                detail: "Loading options and GEX data…".to_string(),
            };
        }

        StartupViewState {
            progress: 0.98,
            detail: if dashboard.pane_count == 0 {
                "Preparing workspace…".to_string()
            } else {
                "Opening workspace…".to_string()
            },
        }
    }

    fn startup_loading_view(&self) -> Element<'_, Message> {
        let state = self.startup_view_state();
        let modal = container(
            column![
                widget::startup_loading_animation(),
                text("Starting FlowSurface")
                    .size(crate::style::text_size::TITLE)
                    .font(iced::Font {
                        weight: iced::font::Weight::Bold,
                        ..Default::default()
                    }),
                progress_bar(0.0..=1.0, state.progress).girth(Length::Fixed(8.0)),
                text(state.detail)
                    .size(crate::style::text_size::BODY)
                    .style(|_| iced::widget::text::Style {
                        color: Some(iced::Color::from_rgb8(148, 155, 164)),
                    }),
                text(startup_fun_message())
                    .size(crate::style::text_size::SMALL)
                    .style(|_| iced::widget::text::Style {
                        color: Some(iced::Color::from_rgb8(114, 120, 128)),
                    }),
            ]
            .width(Length::Fill)
            .align_x(Alignment::Center)
            .spacing(18),
        )
        .width(Length::Fixed(440.0))
        .padding([28, 34])
        .style(style::startup_modal);

        container(modal)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Alignment::Center)
            .align_y(Alignment::Center)
            .style(style::startup_backdrop)
            .into()
    }

    fn view(&self, id: window::Id) -> Element<'_, Message> {
        if self.closing_startup_window == Some(id) {
            return self.startup_loading_view();
        }

        if self.startup_loading.is_active() {
            if id == self.main_window.id {
                return self.startup_loading_view();
            }

            return container(column![])
                .width(Length::Fill)
                .height(Length::Fill)
                .style(style::startup_backdrop)
                .into();
        }

        if self.debug_terminal_window == Some(id) {
            let content = self.debug_terminal_view();
            return self.with_connection_overlay(content);
        }

        let dashboard = self.active_dashboard();
        let sidebar_pos = self.sidebar.position();

        let tickers_table = &self.sidebar.tickers_table;

        let content = if id == self.main_window.id {
            let sidebar_view = self
                .sidebar
                .view(
                    self.audio_stream.volume(),
                    self.market_connectivity.phase(),
                    self.market_connectivity.connected_count(),
                    self.market_connectivity.expected_count(),
                )
                .map(Message::Sidebar);

            let dashboard_view = dashboard
                .view(
                    &self.main_window,
                    tickers_table,
                    self.timezone,
                    self.windowing_mode.allows_native_popout(),
                )
                .map(move |msg| Message::Dashboard {
                    layout_id: None,
                    event: msg,
                });

            let header_title = {
                #[cfg(target_os = "macos")]
                {
                    iced::widget::center(
                        text("FLOWSURFACE")
                            .font(iced::Font {
                                weight: iced::font::Weight::Bold,
                                ..Default::default()
                            })
                            .size(crate::style::text_size::TITLE)
                            .style(style::title_text),
                    )
                    .height(20)
                    .align_y(Alignment::Center)
                    .padding(padding::top(4))
                }
                #[cfg(not(target_os = "macos"))]
                {
                    column![]
                }
            };

            let base = column![
                header_title,
                match sidebar_pos {
                    sidebar::Position::Left => row![sidebar_view, dashboard_view,],
                    sidebar::Position::Right => row![dashboard_view, sidebar_view],
                }
                .spacing(4)
                .padding(8),
            ];

            // In embedded mode, show debug terminal as a docked bottom panel
            let base_with_debug = if self.debug_terminal_embedded
                && self.debug_terminal_enabled
                && self.debug_terminal_window.is_none()
            {
                let debug_panel = container(self.debug_terminal_view())
                    .height(Length::FillPortion(2))
                    .width(Length::Fill);
                column![
                    container(base).height(Length::FillPortion(5)),
                    iced::widget::rule::horizontal(2).style(style::split_ruler),
                    debug_panel,
                ]
                .into()
            } else {
                base.into()
            };

            if let Some(menu) = self.sidebar.active_menu() {
                self.view_with_modal(base_with_debug, dashboard, menu)
            } else {
                base_with_debug
            }
        } else {
            container(
                dashboard
                    .view_window(
                        id,
                        &self.main_window,
                        tickers_table,
                        self.timezone,
                        self.windowing_mode.allows_native_popout(),
                    )
                    .map(move |msg| Message::Dashboard {
                        layout_id: None,
                        event: msg,
                    }),
            )
            .padding(padding::top(style::TITLE_PADDING_TOP))
            .into()
        };

        let content = if let Some(StartupWarning::SavedStateCorrupt { .. }) = &self.startup_warning
        {
            main_dialog_modal(
                content,
                self.startup_warning_modal(),
                Message::StartupWarningNoop,
            )
        } else {
            content
        };

        let content = toast::Manager::new(
            content,
            self.notifications.toasts(),
            match sidebar_pos {
                sidebar::Position::Left => Alignment::Start,
                sidebar::Position::Right => Alignment::End,
            },
            Message::RemoveNotification,
        )
        .into();

        self.with_connection_overlay(content)
    }

    fn with_connection_overlay<'a>(
        &'a self,
        content: Element<'a, Message>,
    ) -> Element<'a, Message> {
        if self.market_connectivity.overlay_visible() {
            main_dialog_modal(
                content,
                self.connection_lost_modal(),
                Message::ConnectionOverlayNoop,
            )
        } else {
            content
        }
    }

    fn connection_lost_modal(&self) -> Element<'_, Message> {
        let connected = self.market_connectivity.connected_count();
        let expected = self.market_connectivity.expected_count();

        let status_icon = container(text("!").size(36))
            .width(Length::Fixed(64.0))
            .height(Length::Fixed(64.0))
            .align_x(Alignment::Center)
            .align_y(Alignment::Center)
            .style(|theme: &Theme| {
                let palette = theme.extended_palette();
                container::Style {
                    text_color: Some(palette.danger.base.color),
                    background: Some(palette.danger.weak.color.into()),
                    ..container::Style::default()
                }
            });

        container(
            column![
                status_icon,
                text("No connection").size(crate::style::text_size::TITLE),
                text("Market data is temporarily unavailable. Charts are paused to prevent incomplete or misleading updates.")
                    .wrapping(iced::widget::text::Wrapping::Word)
                    .width(Length::Fill),
                text(format!(
                    "Reconnecting automatically… ({connected}/{expected} streams restored)"
                ))
                .size(crate::style::text_size::BODY),
                text("As soon as the connection is restored, missing trades and candles will be fetched and the charts will resume.")
                    .wrapping(iced::widget::text::Wrapping::Word)
                    .width(Length::Fill),
            ]
            .spacing(14)
            .align_x(Alignment::Center)
            .width(Length::Fill),
        )
        .width(Length::Fixed(480.0))
        .padding(28)
        .style(style::dashboard_modal)
        .into()
    }

    fn startup_warning_modal(&self) -> Element<'_, Message> {
        let Some(StartupWarning::SavedStateCorrupt {
            error,
            original_path,
            backup_path,
        }) = &self.startup_warning
        else {
            return container(column![]).into();
        };

        let backup_text = backup_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "Backup could not be created.".to_string());

        let body = format!(
            "FlowSurface could not load your saved layout.\n\nOriginal file:\n{}\n\nBackup:\n{}\n\nError:\n{}\n\nYou can continue with a default layout. If you continue, the next save will overwrite saved-state.json. Your backup will remain available.",
            original_path.display(),
            backup_text,
            error
        );

        container(
            column![
                text("Saved layout corrupted").size(crate::style::text_size::TITLE),
                text(body)
                    .wrapping(iced::widget::text::Wrapping::Word)
                    .width(Length::Fill),
                row![
                    button(text("Open backup folder")).on_press(Message::DataFolderRequested),
                    button(text("Exit without overwriting"))
                        .style(|theme, status| style::button::transparent(theme, status, false))
                        .on_press(Message::StartupExitWithoutOverwrite),
                    button(text("Continue with default layout"))
                        .on_press(Message::StartupContinueWithDefault),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            ]
            .spacing(16)
            .width(Length::Fill),
        )
        .width(Length::Fixed(620.0))
        .padding(24)
        .style(style::dashboard_modal)
        .into()
    }

    fn theme(&self, _window: window::Id) -> iced_core::Theme {
        self.theme.clone().into()
    }

    fn title(&self, _window: window::Id) -> String {
        if self.debug_terminal_window == Some(_window) {
            return "Flowsurface Debug Terminal".to_string();
        }

        if let Some(id) = self.layout_manager.active_layout_id() {
            format!("Flowsurface [{}]", id.name)
        } else {
            "Flowsurface".to_string()
        }
    }

    fn scale_factor(&self, _window: window::Id) -> f32 {
        self.ui_scale_factor.into()
    }

    fn subscription(&self) -> Subscription<Message> {
        let window_events = window::events().map(Message::WindowEvent);
        let sidebar = self.sidebar.subscription().map(Message::Sidebar);

        let exchange_streams = self
            .active_dashboard()
            .market_subscriptions(&self.data_sources.exchange)
            .map(Message::MarketWsEvent);

        let tick = iced::time::every(Duration::from_millis(16)).map(Message::Tick);
        let debug_terminal = if self.debug_terminal_enabled || self.debug_terminal_window.is_some()
        {
            iced::time::every(Duration::from_millis(500)).map(|_| Message::DebugTerminalRefresh)
        } else {
            Subscription::none()
        };

        let hotkeys = keyboard::listen().filter_map(|event| {
            let keyboard::Event::KeyPressed { key, modifiers, .. } = event else {
                return None;
            };
            match key {
                keyboard::Key::Named(keyboard::key::Named::Escape) => Some(Message::GoBack),
                keyboard::Key::Character(value)
                    if modifiers.control()
                        && !modifiers.shift()
                        && value.eq_ignore_ascii_case("k") =>
                {
                    Some(Message::Sidebar(dashboard::sidebar::Message::TickersTable(
                        dashboard::tickers_table::Message::ToggleTable,
                    )))
                }
                keyboard::Key::Character(value)
                    if modifiers.control()
                        && modifiers.shift()
                        && value.eq_ignore_ascii_case("a") =>
                {
                    Some(Message::Sidebar(
                        dashboard::sidebar::Message::ToggleSidebarMenu(Some(
                            sidebar::Menu::AddView,
                        )),
                    ))
                }
                _ => None,
            }
        });

        Subscription::batch(vec![
            exchange_streams,
            sidebar,
            window_events,
            tick,
            debug_terminal,
            hotkeys,
        ])
    }

    fn debug_window_label(&self, id: window::Id) -> &'static str {
        if id == self.main_window.id {
            "main"
        } else if self.debug_terminal_window == Some(id) {
            "debug_terminal"
        } else if self.active_dashboard().popout.contains_key(&id) {
            "popout"
        } else {
            "unknown"
        }
    }

    fn open_debug_terminal(&mut self) -> Task<Message> {
        if self.debug_terminal_window.is_some() || self.debug_terminal_embedded {
            return Task::none();
        }

        if self.windowing_mode.allows_native_popout() {
            let config = window::Settings {
                size: iced::Size::new(920.0, 520.0),
                position: window::Position::Centered,
                exit_on_close_request: false,
                min_size: Some(iced::Size::new(560.0, 320.0)),
                ..Default::default()
            };

            let (id, open) = window::open(config);
            open.map(move |_| Message::DebugTerminalOpened(id))
        } else {
            log::info!(
                "WINDOW DebugTerminalEmbedded | reason={reason}",
                reason = self.windowing_mode.reason()
            );
            self.debug_terminal_embedded = true;
            self.debug_terminal_logs = logger::debug_terminal_snapshot();
            if self.debug_terminal_auto_scroll {
                return self.scroll_debug_terminal_to_bottom();
            }
            Task::none()
        }
    }

    fn debug_terminal_view(&self) -> Element<'_, Message> {
        let filtered = self.filtered_debug_terminal_entries();
        let total = self.debug_terminal_logs.len();
        let visible = filtered.len();
        let error_count = filtered
            .iter()
            .filter(|e| e.level == Some(DebugLogLevel::Error))
            .count();
        let warn_count = filtered
            .iter()
            .filter(|e| e.level == Some(DebugLogLevel::Warn))
            .count();

        // Top row: title + stats
        let header = row![
            text("Debug terminal")
                .size(crate::style::text_size::SECTION)
                .width(Length::Fill),
            text(format!("{visible} visible / {total} total")).size(crate::style::text_size::SMALL),
            if error_count > 0 {
                text(format!(" {error_count} errors"))
                    .size(crate::style::text_size::SMALL)
                    .style(|theme: &iced::Theme| iced::widget::text::Style {
                        color: Some(theme.extended_palette().danger.base.color),
                    })
            } else {
                text("")
            },
            if warn_count > 0 {
                text(format!(" {warn_count} warnings"))
                    .size(crate::style::text_size::SMALL)
                    .style(|theme: &iced::Theme| iced::widget::text::Style {
                        color: Some(theme.extended_palette().primary.strong.color),
                    })
            } else {
                text("")
            },
        ]
        .align_y(Alignment::Center)
        .spacing(12);

        // Toolbar row
        let toolbar = row![
            button(text("Clear")).on_press(Message::DebugTerminalClear),
            button(text("Refresh")).on_press(Message::DebugTerminalRefresh),
            button(text("Copy all")).on_press(Message::DebugTerminalCopyAll),
            button(text("Copy visible")).on_press(Message::DebugTerminalCopyVisible),
            button(text("Open data folder")).on_press(Message::DataFolderRequested),
            iced::widget::checkbox(self.debug_terminal_auto_scroll)
                .label("Auto-scroll")
                .on_toggle(Message::DebugTerminalToggleAutoScroll),
            iced::widget::checkbox(self.debug_terminal_app_only)
                .label("App only")
                .on_toggle(Message::DebugTerminalToggleAppOnly),
            iced::widget::checkbox(self.debug_terminal_compact_mode)
                .label("Compact")
                .on_toggle(Message::DebugTerminalToggleCompactMode),
        ]
        .align_y(Alignment::Center)
        .spacing(8);

        // Filter row
        let level_checkboxes = row![
            iced::widget::checkbox(self.debug_terminal_level_filter.error)
                .label("Error")
                .on_toggle(|on| Message::DebugTerminalToggleLevel(DebugLogLevel::Error, on)),
            iced::widget::checkbox(self.debug_terminal_level_filter.warn)
                .label("Warn")
                .on_toggle(|on| Message::DebugTerminalToggleLevel(DebugLogLevel::Warn, on)),
            iced::widget::checkbox(self.debug_terminal_level_filter.info)
                .label("Info")
                .on_toggle(|on| Message::DebugTerminalToggleLevel(DebugLogLevel::Info, on)),
            iced::widget::checkbox(self.debug_terminal_level_filter.debug)
                .label("Debug")
                .on_toggle(|on| Message::DebugTerminalToggleLevel(DebugLogLevel::Debug, on)),
            iced::widget::checkbox(self.debug_terminal_level_filter.trace)
                .label("Trace")
                .on_toggle(|on| Message::DebugTerminalToggleLevel(DebugLogLevel::Trace, on)),
        ]
        .align_y(Alignment::Center)
        .spacing(8);

        let filters = row![
            text_input("Search logs...", &self.debug_terminal_search)
                .on_input(Message::DebugTerminalSearchChanged)
                .width(Length::Fill),
            level_checkboxes,
            pick_list(
                DebugLogCategory::ALL,
                Some(self.debug_terminal_category_filter),
                Message::DebugTerminalCategoryFilterChanged,
            )
            .width(110),
        ]
        .align_y(Alignment::Center)
        .spacing(8);

        // Log body
        let log_body: Element<'static, Message> = if filtered.is_empty() {
            text("No logs captured yet")
                .size(crate::style::text_size::SMALL)
                .font(iced::Font::MONOSPACE)
                .into()
        } else if self.debug_terminal_compact_mode {
            // Compact mode: structured rows
            let mut log_rows = column![].spacing(1);
            for entry in filtered {
                log_rows = log_rows.push(compact_log_row(entry));
            }
            log_rows.into()
        } else {
            // Raw mode: full lines
            let mut log_lines = column![].spacing(1);
            for entry in filtered {
                log_lines = log_lines.push(
                    text(entry.raw)
                        .size(crate::style::text_size::SMALL)
                        .font(iced::Font::MONOSPACE)
                        .wrapping(iced::widget::text::Wrapping::None)
                        .style(debug_log_text_style(entry.level)),
                );
            }
            log_lines.into()
        };

        // Horizontal scrollable wraps the log body
        let h_scroll = scrollable::Scrollable::with_direction(
            container(log_body).width(Length::Shrink).padding(12),
            scrollable::Direction::Horizontal(
                scrollable::Scrollbar::new().width(8).scroller_width(6),
            ),
        )
        .id(DEBUG_TERMINAL_HSCROLL_ID);

        // Vertical scrollable wraps the horizontal one
        let v_scroll = scrollable::Scrollable::with_direction(
            h_scroll,
            scrollable::Direction::Vertical(
                scrollable::Scrollbar::new().width(8).scroller_width(6),
            ),
        )
        .id(DEBUG_TERMINAL_VSCROLL_ID);

        container(column![header, toolbar, filters, v_scroll].spacing(8))
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(16)
            .style(style::dashboard_modal)
            .into()
    }

    fn filtered_debug_terminal_entries(&self) -> Vec<DebugLogEntry> {
        let search = self.debug_terminal_search.trim().to_lowercase();

        self.debug_terminal_logs
            .iter()
            .filter(|line| self.debug_terminal_level_filter.matches(line))
            .filter(|line| {
                if self.debug_terminal_app_only {
                    let entry = parse_debug_log_entry(line);
                    is_app_target(entry.target.as_deref())
                } else {
                    true
                }
            })
            .filter(|line| {
                if self.debug_terminal_category_filter != DebugLogCategory::All {
                    let entry = parse_debug_log_entry(line);
                    entry.category == self.debug_terminal_category_filter
                } else {
                    true
                }
            })
            .filter(|line| {
                if search.is_empty() {
                    true
                } else {
                    let entry = parse_debug_log_entry(line);
                    entry.raw.to_lowercase().contains(&search)
                        || entry.summary.to_lowercase().contains(&search)
                        || entry.event.to_lowercase().contains(&search)
                        || entry
                            .target
                            .as_deref()
                            .unwrap_or("")
                            .to_lowercase()
                            .contains(&search)
                        || format!("{}", entry.category)
                            .to_lowercase()
                            .contains(&search)
                }
            })
            .map(|line| parse_debug_log_entry(line))
            .collect()
    }

    fn scroll_debug_terminal_to_bottom(&self) -> Task<Message> {
        iced::widget::operation::snap_to(
            DEBUG_TERMINAL_VSCROLL_ID,
            iced::widget::scrollable::RelativeOffset { x: 0.0, y: 1.0 },
        )
    }

    fn active_dashboard(&self) -> &Dashboard {
        let active_layout = self
            .layout_manager
            .active_layout_id()
            .expect("No active layout");
        self.layout_manager
            .get(active_layout.unique)
            .map(|layout| &layout.dashboard)
            .expect("No active dashboard")
    }

    fn active_dashboard_mut(&mut self) -> &mut Dashboard {
        let active_layout = self
            .layout_manager
            .active_layout_id()
            .expect("No active layout");
        self.layout_manager
            .get_mut(active_layout.unique)
            .map(|layout| &mut layout.dashboard)
            .expect("No active dashboard")
    }

    fn sync_gex_dashboard(&mut self, now: exchange::UnixMs) {
        let coordinator = &mut self.gex_coordinator;
        for dashboard in self.layout_manager.iter_dashboards_mut() {
            dashboard.sync_gex(coordinator, now);
        }
    }

    fn load_layout(&mut self, layout_uid: uuid::Uuid, main_window: window::Id) -> Task<Message> {
        if let Err(err) = self.layout_manager.set_active_layout(layout_uid) {
            log::error!("Failed to set active layout: {}", err);
            return Task::none();
        }

        self.layout_manager
            .park_inactive_layouts(layout_uid, main_window);

        let windowing_mode = self.windowing_mode;
        let open_native_popouts = !self.startup_loading.is_active();
        self.layout_manager
            .get_mut(layout_uid)
            .map(|layout| {
                layout
                    .dashboard
                    .load_layout(main_window, windowing_mode, open_native_popouts)
                    .map(move |msg| Message::Dashboard {
                        layout_id: Some(layout_uid),
                        event: msg,
                    })
            })
            .unwrap_or_else(|| {
                log::error!("Active layout missing after selection: {}", layout_uid);
                Task::none()
            })
    }

    fn open_startup_popouts(&mut self) -> Task<Message> {
        if !self.windowing_mode.allows_native_popout() {
            return Task::none();
        }
        let Some(layout_id) = self.layout_manager.active_layout_id().map(|id| id.unique) else {
            return Task::none();
        };

        self.layout_manager
            .get_mut(layout_id)
            .map(|layout| {
                layout
                    .dashboard
                    .open_popout_windows()
                    .map(move |event| Message::Dashboard {
                        layout_id: Some(layout_id),
                        event,
                    })
            })
            .unwrap_or_else(Task::none)
    }

    fn open_main_dashboard_window(&mut self) -> Task<Message> {
        let old_window = self.main_window.id;
        let target = self.startup_main_window_target;
        let config = window::Settings {
            size: target.size,
            position: target.position,
            exit_on_close_request: false,
            ..window::settings()
        };
        let (new_window, open) = window::open(config);
        self.closing_startup_window = Some(old_window);
        self.main_window = window::Window::new(new_window);

        Task::batch([window::close(old_window), open.then(|_| Task::none())])
    }

    fn view_with_modal<'a>(
        &'a self,
        base: Element<'a, Message>,
        dashboard: &'a Dashboard,
        menu: sidebar::Menu,
    ) -> Element<'a, Message> {
        let sidebar_pos = self.sidebar.position();

        match menu {
            sidebar::Menu::Settings => {
                let settings_modal = {
                    let theme_picklist = {
                        let mut themes: Vec<iced::Theme> = iced_core::Theme::ALL.to_vec();

                        let default_theme = iced_core::Theme::Custom(default_theme().into());
                        themes.push(default_theme);

                        if let Some(custom_theme) = &self.theme_editor.custom_theme {
                            themes.push(custom_theme.clone());
                        }

                        pick_list(themes, Some(self.theme.0.clone()), |theme| {
                            Message::ThemeSelected(theme)
                        })
                    };

                    let toggle_theme_editor = button(text("Theme editor")).on_press(
                        Message::Sidebar(dashboard::sidebar::Message::ToggleSidebarMenu(Some(
                            sidebar::Menu::ThemeEditor,
                        ))),
                    );

                    let timezone_picklist = pick_list(
                        [data::UserTimezone::Utc, data::UserTimezone::Local],
                        Some(self.timezone),
                        Message::SetTimezone,
                    );

                    let size_in_quote_ccy_checkbox = {
                        let is_active = match self.volume_size_unit {
                            exchange::SizeUnit::Quote => true,
                            exchange::SizeUnit::Base => false,
                        };

                        let checkbox = iced::widget::checkbox(is_active)
                            .label("Size in quote currency")
                            .on_toggle(|checked| {
                                let on_dialog_confirm = Message::ApplyVolumeSizeUnit(if checked {
                                    exchange::SizeUnit::Quote
                                } else {
                                    exchange::SizeUnit::Base
                                });

                                let confirm_dialog = screen::ConfirmDialog::new(
                                    "Changing size display currency requires application restart"
                                        .to_string(),
                                    Box::new(on_dialog_confirm.clone()),
                                )
                                .with_confirm_btn_text("Restart now".to_string());

                                Message::ToggleDialogModal(Some(confirm_dialog))
                            });

                        tooltip(
                            checkbox,
                            Some(
                                "Display sizes/volumes in quote currency (USD).\n- Has no effect on inverse perps or open interest",
                            ),
                            TooltipPosition::Top,
                        )
                    };

                    let cache_metadata_checkbox = {
                        let cache_enabled = self.sidebar.cache_enabled();

                        let checkbox = tooltip(
                            iced::widget::checkbox(cache_enabled)
                                .label("Cache ticker metadata")
                                .on_toggle(Message::SetMetadataCaching),
                            Some("Cache ticker metadata for faster startup."),
                            TooltipPosition::Top,
                        );

                        if !cache_enabled {
                            column![row![checkbox].height(24).align_y(iced::Alignment::Center)]
                        } else {
                            let loading = self.sidebar.is_metadata_loading();

                            let refresh_btn = {
                                let btn = button(crate::style::icon_text(
                                    crate::style::Icon::Refresh,
                                    12,
                                ))
                                .style(move |theme, status| {
                                    style::button::modifier(theme, status, loading)
                                });
                                let btn = if loading {
                                    btn
                                } else {
                                    btn.on_press(Message::RefreshMetadata)
                                };

                                tooltip(
                                    btn,
                                    if loading { None } else { Some("Refresh now") },
                                    TooltipPosition::Top,
                                )
                            };

                            let last_updated = {
                                let last_update = self.sidebar.last_metadata_update();

                                let label = if loading {
                                    "Refreshing metadata…".to_string()
                                } else {
                                    match last_update {
                                        Some(at) => {
                                            format!(
                                                "Metadata last update: {}",
                                                data::util::relative_time_label(at)
                                            )
                                        }
                                        None => "Metadata not fetched yet".to_string(),
                                    }
                                };

                                text(label)
                                    .size(crate::style::text_size::SMALL)
                                    .style(style::secondary_text)
                            };

                            column![
                                row![checkbox, refresh_btn]
                                    .align_y(iced::Alignment::Center)
                                    .spacing(8),
                                last_updated,
                            ]
                            .spacing(4)
                        }
                    };

                    let sidebar_pos_picklist = pick_list(
                        [sidebar::Position::Left, sidebar::Position::Right],
                        Some(sidebar_pos),
                        |pos| {
                            Message::Sidebar(dashboard::sidebar::Message::SetSidebarPosition(pos))
                        },
                    );

                    let scale_factor = {
                        let current_value: f32 = self.ui_scale_factor.into();

                        let decrease_btn = if current_value > data::config::MIN_SCALE {
                            button(text("-"))
                                .on_press(Message::ScaleFactorChanged((current_value - 0.1).into()))
                        } else {
                            button(text("-"))
                        };

                        let increase_btn = if current_value < data::config::MAX_SCALE {
                            button(text("+"))
                                .on_press(Message::ScaleFactorChanged((current_value + 0.1).into()))
                        } else {
                            button(text("+"))
                        };

                        container(
                            row![
                                decrease_btn,
                                text(format!("{:.0}%", current_value * 100.0))
                                    .size(crate::style::text_size::SECTION),
                                increase_btn,
                            ]
                            .align_y(Alignment::Center)
                            .spacing(8)
                            .padding(4),
                        )
                        .style(style::modal_container)
                    };

                    let debug_terminal_checkbox = {
                        let checkbox = iced::widget::checkbox(self.debug_terminal_enabled)
                            .label("Debug terminal")
                            .on_toggle(Message::ToggleDebugTerminal);

                        tooltip(
                            checkbox,
                            Some("Open a popup terminal with detailed application logs"),
                            TooltipPosition::Top,
                        )
                    };

                    let open_data_folder = {
                        let button =
                            button(text("Open data folder")).on_press(Message::DataFolderRequested);

                        tooltip(
                            button,
                            Some("Open the folder where the data & config is stored"),
                            TooltipPosition::Top,
                        )
                    };

                    let invalidate_market_data_cache = {
                        let clear_button = button(text("Invalidate market-data cache")).on_press(
                            Message::ToggleDialogModal(Some(
                                screen::ConfirmDialog::new(
                                    "This deletes all cached market data and restarts indicator analysis. Continue?"
                                        .to_string(),
                                    Box::new(Message::InvalidateMarketDataCache),
                                )
                                .with_confirm_btn_text("Invalidate cache".to_string()),
                            )),
                        );

                        tooltip(
                            clear_button,
                            Some(
                                "Delete cached klines, trades, open interest and bubble summaries, then fetch them again",
                            ),
                            TooltipPosition::Top,
                        )
                    };

                    let version_info = {
                        let (version_label, commit_label) = version::app_build_version_parts();

                        let github_link_button =
                            button(text(version_label).size(crate::style::text_size::EMPHASIS))
                                .padding(0)
                                .style(style::button::text_link)
                                .on_press(Message::OpenUrlRequested(Cow::Borrowed(
                                    version::GITHUB_REPOSITORY_URL,
                                )));

                        let github_button: Element<'_, Message> = iced::widget::tooltip(
                            github_link_button,
                            container(
                                row![
                                    text("GitHub"),
                                    style::icon_text(style::Icon::ExternalLink, 12),
                                ]
                                .spacing(4)
                                .align_y(Alignment::Center),
                            )
                            .style(style::tooltip)
                            .padding(8),
                            TooltipPosition::Top,
                        )
                        .into();

                        if let (Some(commit_label), Some(commit_url)) =
                            (commit_label, version::build_commit_url())
                        {
                            let commit_button =
                                button(text(commit_label).size(crate::style::text_size::SMALL))
                                    .padding(0)
                                    .style(style::button::text_link_secondary)
                                    .on_press(Message::OpenUrlRequested(Cow::Owned(commit_url)));

                            column![github_button, commit_button]
                                .spacing(2)
                                .align_x(Alignment::End)
                                .into()
                        } else {
                            github_button
                        }
                    };

                    let footer = column![
                        container(version_info)
                            .width(iced::Length::Fill)
                            .align_x(Alignment::End),
                    ]
                    .spacing(8);

                    let column_content = split_column![
                        column![open_data_folder,].spacing(8),
                        column![text("Sidebar position").size(crate::style::text_size::SECTION), sidebar_pos_picklist,].spacing(12),
                        column![text("Time zone").size(crate::style::text_size::SECTION), timezone_picklist,].spacing(12),
                        column![
                            text("Market data").size(crate::style::text_size::SECTION),
                            size_in_quote_ccy_checkbox,
                            cache_metadata_checkbox,
                            invalidate_market_data_cache,
                        ].spacing(12),
                        column![text("Theme").size(crate::style::text_size::SECTION), theme_picklist,].spacing(12),
                        column![text("Interface scale").size(crate::style::text_size::SECTION), scale_factor,].spacing(12),
                        column![
                            text("Experimental").size(crate::style::text_size::SECTION),
                            column![
                                debug_terminal_checkbox,
                                toggle_theme_editor
                            ]
                            .spacing(8),
                        ]
                        .spacing(12),
                        footer,
                        ; spacing = 16, align_x = Alignment::Start
                    ];

                    let content = scrollable::Scrollable::with_direction(
                        column_content,
                        scrollable::Direction::Vertical(
                            scrollable::Scrollbar::new().width(8).scroller_width(6),
                        ),
                    );

                    container(content)
                        .align_x(Alignment::Start)
                        .max_width(240)
                        .padding(24)
                        .style(style::dashboard_modal)
                };

                let (align_x, padding) = match sidebar_pos {
                    sidebar::Position::Left => (Alignment::Start, padding::left(44).bottom(4)),
                    sidebar::Position::Right => (Alignment::End, padding::right(44).bottom(4)),
                };

                let base_content = dashboard_modal(
                    base,
                    settings_modal,
                    Message::Sidebar(dashboard::sidebar::Message::ToggleSidebarMenu(None)),
                    padding,
                    Alignment::End,
                    align_x,
                );

                if let Some(dialog) = &self.confirm_dialog {
                    let dialog_content =
                        confirm_dialog_container(dialog.clone(), Message::ToggleDialogModal(None));

                    main_dialog_modal(
                        base_content,
                        dialog_content,
                        Message::ToggleDialogModal(None),
                    )
                } else {
                    base_content
                }
            }
            sidebar::Menu::Layout => {
                let manage_layout_modal = {
                    container(self.layout_manager.view().map(Message::Layouts))
                        .width(260)
                        .padding(24)
                        .style(style::dashboard_modal)
                };

                let (align_x, padding) = match sidebar_pos {
                    sidebar::Position::Left => (Alignment::Start, padding::left(48).top(84)),
                    sidebar::Position::Right => (Alignment::End, padding::right(48).top(84)),
                };

                dashboard_modal(
                    base,
                    manage_layout_modal,
                    Message::Sidebar(dashboard::sidebar::Message::ToggleSidebarMenu(None)),
                    padding,
                    Alignment::Start,
                    align_x,
                )
            }
            sidebar::Menu::AddView => {
                let add_view = container(
                    column![
                        text("Add view").size(crate::style::text_size::TITLE),
                        widget::add_view::selector(2, |kind| Message::Sidebar(
                            dashboard::sidebar::Message::AddViewSelected(kind)
                        )),
                    ]
                    .spacing(12),
                )
                .width(316)
                .padding(16)
                .style(style::dashboard_modal);

                let (align_x, padding) = match sidebar_pos {
                    sidebar::Position::Left => (Alignment::Start, padding::left(48).top(44)),
                    sidebar::Position::Right => (Alignment::End, padding::right(48).top(44)),
                };
                dashboard_modal(
                    base,
                    add_view,
                    Message::Sidebar(dashboard::sidebar::Message::ToggleSidebarMenu(None)),
                    padding,
                    Alignment::Start,
                    align_x,
                )
            }
            sidebar::Menu::Audio => {
                let (align_x, padding) = match sidebar_pos {
                    sidebar::Position::Left => (Alignment::Start, padding::left(48).bottom(84)),
                    sidebar::Position::Right => (Alignment::End, padding::right(48).bottom(84)),
                };

                let trade_streams_list = dashboard.streams.trade_streams(None);

                dashboard_modal(
                    base,
                    self.audio_stream
                        .view(trade_streams_list)
                        .map(Message::AudioStream),
                    Message::Sidebar(dashboard::sidebar::Message::ToggleSidebarMenu(None)),
                    padding,
                    Alignment::End,
                    align_x,
                )
            }
            sidebar::Menu::ThemeEditor => {
                let (align_x, padding) = match sidebar_pos {
                    sidebar::Position::Left => (Alignment::Start, padding::left(44).bottom(4)),
                    sidebar::Position::Right => (Alignment::End, padding::right(44).bottom(4)),
                };

                dashboard_modal(
                    base,
                    self.theme_editor
                        .view(&self.theme.0)
                        .map(Message::ThemeEditor),
                    Message::Sidebar(dashboard::sidebar::Message::ToggleSidebarMenu(None)),
                    padding,
                    Alignment::End,
                    align_x,
                )
            }
            sidebar::Menu::Network => {
                let (align_x, padding) = match sidebar_pos {
                    sidebar::Position::Left => (Alignment::Start, padding::left(48).bottom(44)),
                    sidebar::Position::Right => (Alignment::End, padding::right(48).bottom(44)),
                };

                let base_content = dashboard_modal(
                    base,
                    self.network_editor
                        .view(&self.network_config)
                        .map(Message::NetworkEditor),
                    Message::Sidebar(dashboard::sidebar::Message::ToggleSidebarMenu(None)),
                    padding,
                    Alignment::End,
                    align_x,
                );

                if let Some(dialog) = &self.confirm_dialog {
                    let dialog_content =
                        confirm_dialog_container(dialog.clone(), Message::ToggleDialogModal(None));

                    main_dialog_modal(
                        base_content,
                        dialog_content,
                        Message::ToggleDialogModal(None),
                    )
                } else {
                    base_content
                }
            }
        }
    }

    fn save_state_to_disk(&mut self, windows: &HashMap<window::Id, WindowSpec>) {
        self.active_dashboard_mut()
            .popout
            .iter_mut()
            .for_each(|(id, (_, window_spec))| {
                if let Some(new_window_spec) = windows.get(id) {
                    *window_spec = *new_window_spec;
                }
            });

        self.sidebar.sync_tickers_table_settings();

        let mut ser_layouts = vec![];
        for layout in &self.layout_manager.layouts {
            if let Some(layout) = self.layout_manager.get(layout.id.unique) {
                let serialized_dashboard = data::Dashboard::from(&layout.dashboard);
                ser_layouts.push(data::Layout {
                    name: layout.id.name.clone(),
                    dashboard: serialized_dashboard,
                });
            }
        }

        let layouts = data::Layouts {
            layouts: ser_layouts,
            active_layout: self
                .layout_manager
                .active_layout_id()
                .map(|layout| layout.name.to_string())
                .clone(),
        };

        let main_window_spec = windows
            .iter()
            .find(|(id, _)| **id == self.main_window.id)
            .map(|(_, spec)| *spec);

        let audio_cfg = data::AudioStream::from(&self.audio_stream);

        let state = data::State::from_parts(
            layouts,
            self.theme.clone(),
            self.theme_editor.custom_theme.clone().map(data::Theme),
            main_window_spec,
            self.timezone,
            self.sidebar.state.clone(),
            self.ui_scale_factor,
            audio_cfg,
            self.network_config.for_persistence(),
            self.volume_size_unit,
            self.debug_terminal_enabled,
            self.sidebar.cache_enabled(),
        );

        match data::save_saved_state_atomic(&state) {
            Ok(()) => {
                log::info!("Persisted state to {}", data::SAVED_STATE_PATH);
            }
            Err(e) => {
                log::error!("SAVED_STATE SaveFailed | error={e}");
            }
        }

        self.sidebar.persist_metadata_cache();
    }

    fn restart(&mut self) -> Task<Message> {
        let mut windows_to_close: Vec<window::Id> = if self.startup_loading.is_active() {
            Vec::new()
        } else {
            self.active_dashboard().popout.keys().copied().collect()
        };
        windows_to_close.push(self.main_window.id);

        let close_windows = Task::batch(
            windows_to_close
                .into_iter()
                .map(window::close)
                .collect::<Vec<_>>(),
        );

        let (new_state, init_task) = Flowsurface::new();
        *self = new_state;

        close_windows.chain(init_task)
    }
}

fn compact_log_row(entry: DebugLogEntry) -> Element<'static, Message> {
    let time_text = entry
        .timestamp
        .as_deref()
        .and_then(|ts| ts.split_whitespace().last())
        .unwrap_or("")
        .to_string();

    let level_str = match entry.level {
        Some(DebugLogLevel::Error) => "ERR",
        Some(DebugLogLevel::Warn) => "WRN",
        Some(DebugLogLevel::Info) => "INF",
        Some(DebugLogLevel::Debug) => "DBG",
        Some(DebugLogLevel::Trace) => "TRC",
        None => "---",
    };

    let level = entry.level;
    let category = entry.category;
    let cat_str = format!("{}", category);
    let event_str = if entry.event.is_empty() {
        "-".to_string()
    } else {
        entry.event
    };
    let summary_str = entry.summary;

    row![
        text(time_text)
            .size(crate::style::text_size::SMALL)
            .font(iced::Font::MONOSPACE)
            .width(Length::Fixed(100.0)),
        text(level_str)
            .size(crate::style::text_size::SMALL)
            .font(iced::Font::MONOSPACE)
            .width(Length::Fixed(32.0))
            .style(debug_log_text_style(level)),
        text(cat_str)
            .size(crate::style::text_size::SMALL)
            .font(iced::Font::MONOSPACE)
            .width(Length::Fixed(72.0))
            .style(move |theme: &iced::Theme| {
                let palette = theme.extended_palette();
                let color = match category {
                    DebugLogCategory::Fetch => Some(palette.primary.strong.color),
                    DebugLogCategory::Cache => Some(palette.secondary.strong.color),
                    DebugLogCategory::Ws => Some(palette.warning.strong.color),
                    DebugLogCategory::Chart => Some(palette.success.strong.color),
                    DebugLogCategory::Data => Some(palette.primary.base.color),
                    DebugLogCategory::ThirdParty => Some(palette.background.strongest.color),
                    _ => None,
                };
                iced::widget::text::Style { color }
            }),
        text(event_str)
            .size(crate::style::text_size::SMALL)
            .font(iced::Font::MONOSPACE)
            .width(Length::Fixed(80.0)),
        text(summary_str)
            .size(crate::style::text_size::SMALL)
            .font(iced::Font::MONOSPACE)
            .wrapping(iced::widget::text::Wrapping::None),
    ]
    .align_y(Alignment::Center)
    .spacing(8)
    .into()
}

#[cfg(test)]
mod startup_tests {
    use super::{STARTUP_MIN_VISIBLE, STARTUP_READY_SETTLE, StartupLoading};
    use std::time::{Duration, Instant};

    #[test]
    fn startup_finishes_only_after_dependencies_stay_ready() {
        let now = Instant::now();
        let mut startup = StartupLoading {
            started_at: now - STARTUP_MIN_VISIBLE,
            ready_since: None,
            finished: false,
        };

        assert!(!startup.observe(true, now));
        assert!(!startup.observe(false, now + Duration::from_millis(100)));
        assert!(!startup.observe(true, now + Duration::from_millis(200)));
        assert!(startup.observe(
            true,
            now + Duration::from_millis(200) + STARTUP_READY_SETTLE
        ));
        assert!(!startup.is_active());
    }
}
