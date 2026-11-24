use std::{collections::HashMap, sync::OnceLock};

use data::layout::WindowSpec;
use iced::{Point, Size, Subscription, Task, window};

pub use iced::window::{Id, Position, Settings, close, open};

#[cfg(target_os = "windows")]
pub mod docking;

#[derive(Debug, Clone, Copy)]
pub struct Window {
    pub id: Id,
    pub position: Option<Point>,
}

impl Window {
    pub fn new(id: Id) -> Self {
        Self { id, position: None }
    }
}

pub fn default_size() -> Size {
    WindowSpec::default().size()
}

pub fn startup_popup_settings(size: Size) -> Settings {
    let mut popup = settings();
    popup.size = size;
    popup.position = Position::Centered;
    popup.min_size = Some(size);
    popup.max_size = Some(size);
    popup.resizable = false;
    popup.decorations = false;
    popup.transparent = true;
    popup.exit_on_close_request = false;

    #[cfg(target_os = "windows")]
    {
        popup.platform_specific.skip_taskbar = true;
        popup.platform_specific.corner_preference =
            iced::window::settings::platform::CornerPreference::Round;
    }

    popup
}

const APP_ICON_ICO: &[u8] = include_bytes!("../assets/icon.ico");
const APP_ICON_SIZE: u32 = 64;

fn app_icon() -> window::Icon {
    static ICON: OnceLock<window::Icon> = OnceLock::new();

    ICON.get_or_init(decode_app_icon).clone()
}

fn decode_app_icon() -> window::Icon {
    let source = image::load_from_memory_with_format(APP_ICON_ICO, image::ImageFormat::Ico)
        .expect("embedded application icon must be a valid ICO");
    let rgba = source
        .resize_exact(
            APP_ICON_SIZE,
            APP_ICON_SIZE,
            image::imageops::FilterType::Lanczos3,
        )
        .into_rgba8();

    window::icon::from_rgba(rgba.into_raw(), APP_ICON_SIZE, APP_ICON_SIZE)
        .expect("decoded application icon must contain valid RGBA data")
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct WorkArea {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

impl WorkArea {
    #[cfg(any(target_os = "windows", test))]
    fn from_physical(left: i32, top: i32, right: i32, bottom: i32, scale: f32) -> Self {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };

        Self {
            x: left as f32 / scale,
            y: top as f32 / scale,
            width: (right - left).max(0) as f32 / scale,
            height: (bottom - top).max(0) as f32 / scale,
        }
    }
}

const MIN_VISIBLE_TITLE_WIDTH: f32 = 128.0;
const TITLE_BAR_HEIGHT: f32 = 32.0;
const MIN_VISIBLE_TITLE_HEIGHT: f32 = 16.0;

/// Drops a saved position when its title bar cannot be reached on any current display.
///
/// Returning `true` means the caller should open the window centered. If display
/// enumeration is unavailable, the saved position is preserved instead of producing
/// a false recovery.
pub fn recover_offscreen_position(window: &mut Option<WindowSpec>) -> bool {
    let Some(spec) = *window else {
        return false;
    };
    let Some(work_areas) = platform_work_areas() else {
        return false;
    };

    if title_bar_is_reachable(spec, &work_areas) {
        false
    } else {
        *window = None;
        true
    }
}

fn title_bar_is_reachable(window: WindowSpec, work_areas: &[WorkArea]) -> bool {
    if work_areas.is_empty() {
        return true;
    }

    let window_right = window.pos_x + window.width;
    let title_bottom = window.pos_y + window.height.min(TITLE_BAR_HEIGHT);
    let required_width = window.width.min(MIN_VISIBLE_TITLE_WIDTH);
    let required_height = window.height.min(MIN_VISIBLE_TITLE_HEIGHT);

    work_areas.iter().any(|area| {
        let area_right = area.x + area.width;
        let area_bottom = area.y + area.height;
        let visible_width = window_right.min(area_right) - window.pos_x.max(area.x);
        let visible_title_height = title_bottom.min(area_bottom) - window.pos_y.max(area.y);

        visible_width >= required_width && visible_title_height >= required_height
    })
}

#[cfg(target_os = "windows")]
fn platform_work_areas() -> Option<Vec<WorkArea>> {
    use std::{mem, ptr};
    use windows_sys::Win32::{
        Foundation::{LPARAM, RECT, S_OK},
        Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO},
        UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
    };
    use windows_sys::core::BOOL;

    unsafe extern "system" fn collect_monitor(
        monitor: HMONITOR,
        _dc: HDC,
        _rect: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        let work_areas = unsafe { &mut *(data as *mut Vec<WorkArea>) };
        let mut info: MONITORINFO = unsafe { mem::zeroed() };
        info.cbSize = mem::size_of::<MONITORINFO>() as u32;

        if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
            return 1;
        }

        let mut dpi_x = 96;
        let mut dpi_y = 96;
        let scale =
            if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }
                == S_OK
            {
                dpi_x as f32 / 96.0
            } else {
                1.0
            };
        let work = info.rcWork;
        work_areas.push(WorkArea::from_physical(
            work.left,
            work.top,
            work.right,
            work.bottom,
            scale,
        ));

        1
    }

    let mut work_areas = Vec::new();
    let result = unsafe {
        EnumDisplayMonitors(
            ptr::null_mut(),
            ptr::null(),
            Some(collect_monitor),
            &mut work_areas as *mut Vec<WorkArea> as LPARAM,
        )
    };

    (result != 0 && !work_areas.is_empty()).then_some(work_areas)
}

#[cfg(not(target_os = "windows"))]
fn platform_work_areas() -> Option<Vec<WorkArea>> {
    None
}

#[derive(Debug, Clone, Copy)]
pub enum Event {
    CloseRequested(window::Id),
    #[cfg(target_os = "windows")]
    Opened(window::Id),
    Focused(window::Id),
    Unfocused(window::Id),
}

pub fn events() -> Subscription<Event> {
    iced::event::listen_with(filtered_events)
}

fn filtered_events(
    event: iced::Event,
    _status: iced::event::Status,
    window: window::Id,
) -> Option<Event> {
    match &event {
        #[cfg(target_os = "windows")]
        iced::Event::Window(iced::window::Event::Opened { .. }) => Some(Event::Opened(window)),
        iced::Event::Window(iced::window::Event::CloseRequested) => {
            Some(Event::CloseRequested(window))
        }
        iced::Event::Window(iced::window::Event::Focused) => Some(Event::Focused(window)),
        iced::Event::Window(iced::window::Event::Unfocused) => Some(Event::Unfocused(window)),
        _ => None,
    }
}

pub fn collect_window_specs<M, F>(window_ids: Vec<window::Id>, message: F) -> Task<M>
where
    F: Fn(HashMap<window::Id, WindowSpec>) -> M + Send + 'static,
    M: Send + 'static,
{
    // Create a task that collects specs for each window
    let window_spec_tasks = window_ids
        .into_iter()
        .map(|window_id| {
            // Map both tasks to produce an enum or tuple to distinguish them
            let pos_task: Task<(Option<Point>, Option<Size>)> =
                iced::window::position(window_id).map(|pos| (pos, None));

            let size_task: Task<(Option<Point>, Option<Size>)> =
                iced::window::size(window_id).map(|size| (None, Some(size)));

            Task::batch(vec![pos_task, size_task])
                .collect()
                .map(move |results| {
                    let position = results.iter().find_map(|(pos, _)| *pos);
                    let size = results
                        .iter()
                        .find_map(|(_, size)| *size)
                        .unwrap_or_else(|| Size::new(1024.0, 768.0));

                    (window_id, (position, size))
                })
        })
        .collect::<Vec<_>>();

    // Batch all window tasks together and collect results
    Task::batch(window_spec_tasks)
        .collect()
        .map(move |results| {
            let specs: HashMap<window::Id, WindowSpec> = results
                .into_iter()
                .filter_map(|(id, (pos, size))| {
                    pos.map(|position| (id, WindowSpec::from((&position, &size))))
                })
                .collect();

            message(specs)
        })
}

#[cfg(target_os = "linux")]
pub fn settings() -> Settings {
    Settings {
        icon: Some(app_icon()),
        min_size: Some(Size::new(800.0, 600.0)),
        ..Default::default()
    }
}

#[cfg(target_os = "macos")]
pub fn settings() -> Settings {
    use iced::window;

    Settings {
        icon: Some(app_icon()),
        platform_specific: window::settings::PlatformSpecific {
            title_hidden: true,
            titlebar_transparent: true,
            fullsize_content_view: true,
        },
        min_size: Some(Size::new(800.0, 600.0)),
        ..Default::default()
    }
}

#[cfg(target_os = "windows")]
pub fn settings() -> Settings {
    Settings {
        icon: Some(app_icon()),
        min_size: Some(Size::new(800.0, 600.0)),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(x: f32, y: f32) -> WindowSpec {
        WindowSpec {
            width: 800.0,
            height: 600.0,
            pos_x: x,
            pos_y: y,
        }
    }

    #[test]
    fn embedded_application_icon_decodes_to_expected_rgba_size() {
        let (pixels, size) = decode_app_icon().into_raw();

        assert_eq!(size, Size::new(APP_ICON_SIZE, APP_ICON_SIZE));
        assert_eq!(pixels.len(), (APP_ICON_SIZE * APP_ICON_SIZE * 4) as usize);
        assert!(pixels.chunks_exact(4).any(|pixel| pixel[3] != 0));
    }

    #[test]
    fn rejects_position_from_removed_right_hand_monitor() {
        let displays = [
            WorkArea {
                x: 0.0,
                y: 0.0,
                width: 1536.0,
                height: 816.0,
            },
            WorkArea {
                x: 0.0,
                y: -1080.0,
                width: 1920.0,
                height: 1032.0,
            },
        ];

        assert!(!title_bar_is_reachable(spec(1912.0, -8.0), &displays));
    }

    #[test]
    fn accepts_position_on_right_hand_monitor_when_it_exists() {
        let displays = [
            WorkArea {
                x: 0.0,
                y: 0.0,
                width: 1920.0,
                height: 1040.0,
            },
            WorkArea {
                x: 1920.0,
                y: 0.0,
                width: 1920.0,
                height: 1040.0,
            },
        ];

        assert!(title_bar_is_reachable(spec(1912.0, -8.0), &displays));
    }

    #[test]
    fn accepts_position_on_monitor_above_primary() {
        let displays = [
            WorkArea {
                x: 0.0,
                y: 0.0,
                width: 1536.0,
                height: 816.0,
            },
            WorkArea {
                x: 0.0,
                y: -1080.0,
                width: 1920.0,
                height: 1032.0,
            },
        ];

        assert!(title_bar_is_reachable(spec(400.0, -900.0), &displays));
    }

    #[test]
    fn keeps_slightly_offscreen_window_with_reachable_title_bar() {
        let displays = [WorkArea {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1040.0,
        }];

        assert!(title_bar_is_reachable(spec(-8.0, -8.0), &displays));
    }

    #[test]
    fn rejects_window_when_only_a_narrow_sliver_is_visible() {
        let displays = [WorkArea {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1040.0,
        }];

        assert!(!title_bar_is_reachable(spec(1912.0, 100.0), &displays));
    }

    #[test]
    fn converts_physical_work_area_using_monitor_dpi_scale() {
        assert_eq!(
            WorkArea::from_physical(0, 0, 1920, 1020, 1.25),
            WorkArea {
                x: 0.0,
                y: 0.0,
                width: 1536.0,
                height: 816.0
            }
        );
        assert_eq!(
            WorkArea::from_physical(0, -1080, 1920, -48, 1.0),
            WorkArea {
                x: 0.0,
                y: -1080.0,
                width: 1920.0,
                height: 1032.0
            }
        );
    }
}
