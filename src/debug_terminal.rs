//! Debug terminal: captures, filters, and renders application logs.
//!
//! Owns the runtime state, the log-parsing/classification logic, and the UI
//! for the popup (or embedded) debug terminal window. Window lifecycle and
//! top-level routing stay in `main.rs`; this module exposes a self-contained
//! [`Message`] stream that the app maps through its own `Message` enum.

use crate::logger;
use crate::style;
use crate::widget::pick_list;
use crate::window;
use crate::windowing::WindowingMode;

use iced::widget::{button, checkbox, column, container, row, scrollable, text, text_input};
use iced::{Alignment, Element, Length, Task};

const VSCROLL_ID: &str = "debug-terminal-vscroll";
const HSCROLL_ID: &str = "debug-terminal-hscroll";

/// Messages produced by the debug terminal UI.
#[derive(Debug, Clone)]
pub enum Message {
    Opened(window::Id),
    Refresh,
    Clear,
    CopyAll,
    CopyVisible,
    SearchChanged(String),
    ToggleLevel(LogLevel, bool),
    ToggleAutoScroll(bool),
    CategoryFilterChanged(LogCategory),
    ToggleAppOnly(bool),
    ToggleCompactMode(bool),
    /// Forwarded to the application, which opens the data folder.
    OpenDataFolder,
}

/// Multi-level filter for the Debug Terminal.
/// Each level can be independently enabled/disabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LevelFilter {
    error: bool,
    warn: bool,
    info: bool,
    debug: bool,
    trace: bool,
}

impl LevelFilter {
    /// Default levels: ERROR, WARN, INFO enabled; DEBUG, TRACE disabled.
    const DEFAULT: Self = Self {
        error: true,
        warn: true,
        info: true,
        debug: false,
        trace: false,
    };

    fn matches(self, line: &str) -> bool {
        match line_level(line) {
            Some(LogLevel::Error) => self.error,
            Some(LogLevel::Warn) => self.warn,
            Some(LogLevel::Info) => self.info,
            Some(LogLevel::Debug) => self.debug,
            Some(LogLevel::Trace) => self.trace,
            // Unknown-level logs show when INFO is enabled (simpler than an extra toggle).
            None => self.info,
        }
    }

    fn toggle(&mut self, level: LogLevel, enabled: bool) {
        match level {
            LogLevel::Error => self.error = enabled,
            LogLevel::Warn => self.warn = enabled,
            LogLevel::Info => self.info = enabled,
            LogLevel::Debug => self.debug = enabled,
            LogLevel::Trace => self.trace = enabled,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

fn line_level(line: &str) -> Option<LogLevel> {
    let level_start = line.find("] [")? + 3;
    let level_end = line[level_start..].find(']')? + level_start;

    match line[level_start..level_end].trim() {
        "ERROR" | "FATAL" => Some(LogLevel::Error),
        "WARN" => Some(LogLevel::Warn),
        "INFO" => Some(LogLevel::Info),
        "DEBUG" => Some(LogLevel::Debug),
        "TRACE" => Some(LogLevel::Trace),
        _ => None,
    }
}

fn text_style(level: Option<LogLevel>) -> impl Fn(&iced::Theme) -> iced::widget::text::Style {
    move |theme| {
        let palette = theme.palette();
        let color = match level {
            Some(LogLevel::Error) => Some(palette.danger.base.color),
            Some(LogLevel::Warn) => Some(palette.primary.strong.color),
            Some(LogLevel::Info) => None,
            Some(LogLevel::Debug) => Some(palette.secondary.strong.color),
            Some(LogLevel::Trace) => Some(palette.background.strongest.color),
            None => None,
        };

        iced::widget::text::Style { color }
    }
}

impl std::fmt::Display for LogLevel {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogCategory {
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

impl LogCategory {
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

impl std::fmt::Display for LogCategory {
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
pub struct LogEntry {
    raw: String,
    timestamp: Option<String>,
    level: Option<LogLevel>,
    target: Option<String>,
    category: LogCategory,
    event: String,
    summary: String,
}

fn parse_entry(line: &str) -> LogEntry {
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
            "ERROR" | "FATAL" => Some(LogLevel::Error),
            "WARN" => Some(LogLevel::Warn),
            "INFO" => Some(LogLevel::Info),
            "DEBUG" => Some(LogLevel::Debug),
            "TRACE" => Some(LogLevel::Trace),
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
    let (category, event, summary) = classify_message(message, target.as_deref());

    LogEntry {
        raw,
        timestamp,
        level,
        target,
        category,
        event,
        summary,
    }
}

fn classify_message(message: &str, target: Option<&str>) -> (LogCategory, String, String) {
    // Check for our structured log format: CATEGORY Event | key=value ...
    if let Some(pipe_pos) = message.find('|') {
        let prefix = message[..pipe_pos].trim();
        let details = message[pipe_pos + 1..].trim();

        let parts: Vec<&str> = prefix.split_whitespace().collect();
        if parts.len() >= 2 {
            let cat_str = parts[0];
            let event = parts[1..].join(" ");

            let category = match cat_str {
                "FETCH" | "TRADE" => LogCategory::Fetch,
                "KLINE" => LogCategory::Kline,
                "OI" => LogCategory::Oi,
                "CACHE" => LogCategory::Cache,
                "WS" if event.contains("Backfill") => LogCategory::Backfill,
                "WS" => LogCategory::Ws,
                "STREAM" => LogCategory::Stream,
                "BACKFILL" => LogCategory::Backfill,
                "CHART" if event.contains("Bubbles") => LogCategory::Bubbles,
                "CHART" if event.contains("Footprint") => LogCategory::Footprint,
                "CHART" => LogCategory::Chart,
                "DATA" => LogCategory::Data,
                _ => LogCategory::App,
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
                LogCategory::Fetch
            } else {
                LogCategory::App
            }
        }
        Some(t) if t == "iced_wgpu" || t.contains("wgpu") || t.contains("winit") => {
            LogCategory::ThirdParty
        }
        Some("panic") => LogCategory::App,
        Some(_) => LogCategory::ThirdParty,
        None => LogCategory::App,
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

/// Runtime state for the debug terminal, including its window lifecycle.
pub struct State {
    enabled: bool,
    window: Option<window::Id>,
    embedded: bool,
    logs: Vec<String>,
    level_filter: LevelFilter,
    category_filter: LogCategory,
    search: String,
    auto_scroll: bool,
    app_only: bool,
    compact_mode: bool,
}

impl State {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            window: None,
            embedded: false,
            logs: logger::debug_terminal_snapshot(),
            level_filter: LevelFilter::DEFAULT,
            category_filter: LogCategory::All,
            search: String::new(),
            auto_scroll: true,
            app_only: true,
            compact_mode: true,
        }
    }

    /// Whether the terminal is persisted as "enabled" (auto-opens at startup).
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// The id of the terminal's native window, if open.
    pub fn window(&self) -> Option<window::Id> {
        self.window
    }

    /// Whether `id` is the debug terminal's native window.
    pub fn is_window(&self, id: window::Id) -> bool {
        self.window == Some(id)
    }

    /// Whether the terminal should poll for log refreshes.
    pub fn should_poll(&self) -> bool {
        self.enabled || self.window.is_some()
    }

    /// Whether the terminal is docked into the main window right now.
    pub fn is_embedded_visible(&self) -> bool {
        self.embedded && self.enabled && self.window.is_none()
    }

    /// Handles the user toggling the terminal on/off from settings.
    pub fn toggle(&mut self, enabled: bool, mode: WindowingMode) -> Task<Message> {
        self.enabled = enabled;
        if enabled {
            self.refresh_logs();
            self.open(mode)
        } else if let Some(window) = self.window.take() {
            window::close(window)
        } else {
            self.embedded = false;
            Task::none()
        }
    }

    /// Notifies the terminal that its native window was closed.
    pub fn on_window_closed(&mut self, id: window::Id) -> bool {
        if self.window == Some(id) {
            self.window = None;
            self.enabled = false;
            true
        } else {
            false
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Opened(window) => {
                self.window = Some(window);
                self.refresh_logs();
                if self.auto_scroll {
                    return self.scroll_to_bottom();
                }
            }
            Message::Refresh => {
                if self.should_poll() {
                    self.refresh_logs();
                    if self.auto_scroll {
                        return self.scroll_to_bottom();
                    }
                }
            }
            Message::Clear => {
                logger::clear_debug_terminal();
                self.logs.clear();
            }
            Message::CopyAll => {
                return iced::clipboard::write(self.logs.join("\n")).discard();
            }
            Message::CopyVisible => {
                let visible: Vec<String> = self
                    .filtered_entries()
                    .into_iter()
                    .map(|entry| entry.raw)
                    .collect();
                return iced::clipboard::write(visible.join("\n")).discard();
            }
            Message::SearchChanged(value) => {
                self.search = value;
            }
            Message::ToggleLevel(level, enabled) => {
                self.level_filter.toggle(level, enabled);
            }
            Message::ToggleAutoScroll(enabled) => {
                self.auto_scroll = enabled;
                if enabled {
                    return self.scroll_to_bottom();
                }
            }
            Message::CategoryFilterChanged(category) => {
                self.category_filter = category;
            }
            Message::ToggleAppOnly(app_only) => {
                self.app_only = app_only;
            }
            Message::ToggleCompactMode(compact) => {
                self.compact_mode = compact;
            }
            // Handled by the application, never reaches `update`.
            Message::OpenDataFolder => {}
        }

        Task::none()
    }

    pub fn view(&self) -> Element<'_, Message> {
        let filtered = self.filtered_entries();
        let total = self.logs.len();
        let visible = filtered.len();
        let error_count = filtered
            .iter()
            .filter(|entry| entry.level == Some(LogLevel::Error))
            .count();
        let warn_count = filtered
            .iter()
            .filter(|entry| entry.level == Some(LogLevel::Warn))
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
                        color: Some(theme.palette().danger.base.color),
                    })
            } else {
                text("")
            },
            if warn_count > 0 {
                text(format!(" {warn_count} warnings"))
                    .size(crate::style::text_size::SMALL)
                    .style(|theme: &iced::Theme| iced::widget::text::Style {
                        color: Some(theme.palette().primary.strong.color),
                    })
            } else {
                text("")
            },
        ]
        .align_y(Alignment::Center)
        .spacing(12);

        // Toolbar row
        let toolbar = row![
            button(text("Clear")).on_press(Message::Clear),
            button(text("Refresh")).on_press(Message::Refresh),
            button(text("Copy all")).on_press(Message::CopyAll),
            button(text("Copy visible")).on_press(Message::CopyVisible),
            button(text("Open data folder")).on_press(Message::OpenDataFolder),
            checkbox(self.auto_scroll)
                .label("Auto-scroll")
                .on_toggle(Message::ToggleAutoScroll),
            checkbox(self.app_only)
                .label("App only")
                .on_toggle(Message::ToggleAppOnly),
            checkbox(self.compact_mode)
                .label("Compact")
                .on_toggle(Message::ToggleCompactMode),
        ]
        .align_y(Alignment::Center)
        .spacing(8);

        // Filter row
        let level_checkboxes = row![
            checkbox(self.level_filter.error)
                .label("Error")
                .on_toggle(|on| Message::ToggleLevel(LogLevel::Error, on)),
            checkbox(self.level_filter.warn)
                .label("Warn")
                .on_toggle(|on| Message::ToggleLevel(LogLevel::Warn, on)),
            checkbox(self.level_filter.info)
                .label("Info")
                .on_toggle(|on| Message::ToggleLevel(LogLevel::Info, on)),
            checkbox(self.level_filter.debug)
                .label("Debug")
                .on_toggle(|on| Message::ToggleLevel(LogLevel::Debug, on)),
            checkbox(self.level_filter.trace)
                .label("Trace")
                .on_toggle(|on| Message::ToggleLevel(LogLevel::Trace, on)),
        ]
        .align_y(Alignment::Center)
        .spacing(8);

        let filters = row![
            text_input("Search logs...", &self.search)
                .on_input(Message::SearchChanged)
                .width(Length::Fill),
            level_checkboxes,
            pick_list(
                LogCategory::ALL,
                Some(self.category_filter),
                Message::CategoryFilterChanged,
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
        } else if self.compact_mode {
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
                        .style(text_style(entry.level)),
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
        .id(HSCROLL_ID);

        // Vertical scrollable wraps the horizontal one
        let v_scroll = scrollable::Scrollable::with_direction(
            h_scroll,
            scrollable::Direction::Vertical(
                scrollable::Scrollbar::new().width(8).scroller_width(6),
            ),
        )
        .id(VSCROLL_ID);

        container(column![header, toolbar, filters, v_scroll].spacing(8))
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(16)
            .style(style::dashboard_modal)
            .into()
    }

    pub fn filtered_entries(&self) -> Vec<LogEntry> {
        let search = self.search.trim().to_lowercase();

        self.logs
            .iter()
            .filter(|line| self.level_filter.matches(line))
            .filter(|line| {
                if self.app_only {
                    let entry = parse_entry(line);
                    is_app_target(entry.target.as_deref())
                } else {
                    true
                }
            })
            .filter(|line| {
                if self.category_filter != LogCategory::All {
                    let entry = parse_entry(line);
                    entry.category == self.category_filter
                } else {
                    true
                }
            })
            .filter(|line| {
                if search.is_empty() {
                    true
                } else {
                    let entry = parse_entry(line);
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
            .map(|line| parse_entry(line))
            .collect()
    }

    pub fn open(&mut self, mode: WindowingMode) -> Task<Message> {
        if self.window.is_some() || self.embedded {
            return Task::none();
        }

        if mode.allows_native_popout() {
            let config = window::Settings {
                size: iced::Size::new(920.0, 520.0),
                position: window::Position::Centered,
                exit_on_close_request: false,
                min_size: Some(iced::Size::new(560.0, 320.0)),
                ..Default::default()
            };

            let (id, open) = window::open(config);
            open.map(move |_| Message::Opened(id))
        } else {
            log::info!(
                "WINDOW DebugTerminalEmbedded | reason={reason}",
                reason = mode.reason()
            );
            self.embedded = true;
            self.refresh_logs();
            if self.auto_scroll {
                return self.scroll_to_bottom();
            }
            Task::none()
        }
    }

    fn refresh_logs(&mut self) {
        self.logs = logger::debug_terminal_snapshot();
    }

    fn scroll_to_bottom(&self) -> Task<Message> {
        iced::widget::operation::snap_to(
            VSCROLL_ID,
            iced::widget::scrollable::RelativeOffset { x: 0.0, y: 1.0 },
        )
    }
}

fn compact_log_row(entry: LogEntry) -> Element<'static, Message> {
    let time_text = entry
        .timestamp
        .as_deref()
        .and_then(|ts| ts.split_whitespace().last())
        .unwrap_or("")
        .to_string();

    let level_str = match entry.level {
        Some(LogLevel::Error) => "ERR",
        Some(LogLevel::Warn) => "WRN",
        Some(LogLevel::Info) => "INF",
        Some(LogLevel::Debug) => "DBG",
        Some(LogLevel::Trace) => "TRC",
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
            .style(text_style(level)),
        text(cat_str)
            .size(crate::style::text_size::SMALL)
            .font(iced::Font::MONOSPACE)
            .width(Length::Fixed(72.0))
            .style(move |theme: &iced::Theme| {
                let palette = theme.palette();
                let color = match category {
                    LogCategory::Fetch => Some(palette.primary.strong.color),
                    LogCategory::Cache => Some(palette.secondary.strong.color),
                    LogCategory::Ws => Some(palette.warning.strong.color),
                    LogCategory::Chart => Some(palette.success.strong.color),
                    LogCategory::Data => Some(palette.primary.base.color),
                    LogCategory::ThirdParty => Some(palette.background.strongest.color),
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
mod tests {
    use super::*;

    fn state_with_logs(logs: Vec<&'static str>) -> State {
        State {
            enabled: true,
            window: None,
            embedded: false,
            logs: logs.into_iter().map(str::to_string).collect(),
            level_filter: LevelFilter::DEFAULT,
            category_filter: LogCategory::All,
            search: String::new(),
            auto_scroll: true,
            app_only: true,
            compact_mode: true,
        }
    }

    #[test]
    fn line_level_parses_known_and_unknown_levels() {
        assert_eq!(
            line_level("[2026-08-19 10:00:00.000] [ERROR] [flowsurface] boom"),
            Some(LogLevel::Error)
        );
        assert_eq!(
            line_level("[t] [FATAL] [flowsurface] boom"),
            Some(LogLevel::Error)
        );
        assert_eq!(line_level("[t] [WARN] [x] y"), Some(LogLevel::Warn));
        assert_eq!(line_level("[t] [INFO] [x] y"), Some(LogLevel::Info));
        assert_eq!(line_level("[t] [DEBUG] [x] y"), Some(LogLevel::Debug));
        assert_eq!(line_level("[t] [TRACE] [x] y"), Some(LogLevel::Trace));
        assert_eq!(line_level("[t] [OTHER] [x] y"), None);
        assert_eq!(line_level("no brackets at all"), None);
    }

    #[test]
    fn default_filter_matches_error_warn_info_only() {
        let filter = LevelFilter::DEFAULT;
        assert!(filter.matches("[t] [ERROR] [x] y"));
        assert!(filter.matches("[t] [WARN] [x] y"));
        assert!(filter.matches("[t] [INFO] [x] y"));
        assert!(!filter.matches("[t] [DEBUG] [x] y"));
        assert!(!filter.matches("[t] [TRACE] [x] y"));
        // Unknown level falls back to INFO.
        assert!(filter.matches("[t] [OTHER] [x] y"));
    }

    #[test]
    fn toggling_level_changes_matching() {
        let mut filter = LevelFilter::DEFAULT;
        filter.toggle(LogLevel::Debug, true);
        assert!(filter.matches("[t] [DEBUG] [x] y"));

        filter.toggle(LogLevel::Error, false);
        assert!(!filter.matches("[t] [ERROR] [x] y"));
    }

    #[test]
    fn parses_full_log_entry() {
        let entry = parse_entry("[2026-08-19 10:00:00.000] [INFO] [flowsurface] hello");
        assert_eq!(entry.timestamp.as_deref(), Some("2026-08-19 10:00:00.000"));
        assert_eq!(entry.level, Some(LogLevel::Info));
        assert_eq!(entry.target.as_deref(), Some("flowsurface"));
        assert_eq!(
            entry.raw,
            "[2026-08-19 10:00:00.000] [INFO] [flowsurface] hello"
        );
    }

    #[test]
    fn parses_entry_without_brackets() {
        let entry = parse_entry("bare message");
        assert_eq!(entry.timestamp, None);
        assert_eq!(entry.level, None);
        assert_eq!(entry.target, None);
        assert_eq!(entry.summary, "bare message");
    }

    #[test]
    fn classifies_structured_message() {
        let (category, event, summary) = classify_message(
            "FETCH Trade | symbol=BTCUSDT venue=binance",
            Some("flowsurface"),
        );
        assert_eq!(category, LogCategory::Fetch);
        assert_eq!(event, "Trade");
        assert!(summary.contains("symbol=BTCUSDT"));
        assert!(summary.contains("venue=binance"));
    }

    #[test]
    fn classifies_ws_backfill_specially() {
        let (category, _, _) =
            classify_message("WS Backfill | range=1h", Some("flowsurface_exchange"));
        assert_eq!(category, LogCategory::Backfill);
    }

    #[test]
    fn classifies_by_target_when_not_structured() {
        let (category, event, _) = classify_message("some log", Some("flowsurface"));
        assert_eq!(category, LogCategory::App);
        assert_eq!(event, "");

        let (third_party, _, _) = classify_message("gpu noise", Some("wgpu_core"));
        assert_eq!(third_party, LogCategory::ThirdParty);
    }

    #[test]
    fn extract_summary_keeps_known_keys_only() {
        let summary = extract_summary("symbol=BTC venue=binance noise=ignored", "TRADE");
        assert!(summary.contains("symbol=BTC"));
        assert!(summary.contains("venue=binance"));
        assert!(!summary.contains("noise"));
    }

    #[test]
    fn is_app_target_matches_app_and_panic() {
        assert!(is_app_target(Some("flowsurface")));
        assert!(is_app_target(Some("flowsurface_exchange")));
        assert!(is_app_target(Some("panic")));
        assert!(!is_app_target(Some("wgpu_core")));
        assert!(is_app_target(None));
    }

    #[test]
    fn filtered_entries_app_only_hides_third_party() {
        let state = state_with_logs(vec![
            "[t] [INFO] [flowsurface] app log",
            "[t] [INFO] [wgpu_core] gpu log",
        ]);
        let entries = state.filtered_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].target.as_deref(), Some("flowsurface"));
    }

    #[test]
    fn filtered_entries_category_filter() {
        let state = state_with_logs(vec![
            "[t] [INFO] [flowsurface] FETCH Trade | symbol=BTC",
            "[t] [INFO] [flowsurface] CACHE Store | key=1",
        ]);
        let mut filtered = state;
        filtered.category_filter = LogCategory::Cache;
        filtered.app_only = false;

        let entries = filtered.filtered_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].category, LogCategory::Cache);
    }

    #[test]
    fn filtered_entries_search_matches_raw_and_summary() {
        let state = state_with_logs(vec![
            "[t] [INFO] [flowsurface] FETCH Trade | symbol=BTCUSDT",
            "[t] [INFO] [flowsurface] CACHE Store | key=1",
        ]);
        let mut filtered = state;
        filtered.search = "BTC".to_string();

        let entries = filtered.filtered_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].category, LogCategory::Fetch);
    }
}
