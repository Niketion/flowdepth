//! Windows title-bar drag adapter. All HWNDs and gesture state stay on the
//! iced event-loop thread. Only a confirmed drop crosses into app update.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ptr::{null, null_mut},
    sync::OnceLock,
};

use iced::{
    Color, Rectangle, Task, Theme,
    futures::channel::mpsc,
    widget::canvas,
    window::{self, raw_window_handle::RawWindowHandle},
};
use windows_sys::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{
            BeginPaint, ClientToScreen, CreateSolidBrush, DeleteObject, EndPaint, FillRect,
            FrameRect, InvalidateRect, PAINTSTRUCT, UpdateWindow,
        },
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
        UI::{
            HiDpi::GetDpiForWindow,
            Input::KeyboardAndMouse::{
                GetAsyncKeyState, IsWindowEnabled, VK_ESCAPE, VK_LBUTTON, VK_RBUTTON, VK_RETURN,
            },
            Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
            WindowsAndMessaging::*,
        },
    },
    w,
};

use crate::docking::{Drag, Geometry, Point, Preview, Rect, Zone};

#[derive(Debug, Clone, Copy)]
pub struct ConfirmedDrop {
    pub main: window::Id,
    pub popup: window::Id,
    pub zone: Zone,
}

struct Popup {
    id: window::Id,
    eligible: bool,
    events: mpsc::UnboundedSender<ConfirmedDrop>,
}

#[derive(Clone, Copy)]
struct Surface {
    id: window::Id,
    bounds: Rect,
    minimum: Point,
    zoom: f64,
    accent: Color,
}

thread_local! {
    static MAIN: Cell<Option<(window::Id, HWND)>> = const { Cell::new(None) };
    static SURFACE: Cell<Option<Surface>> = const { Cell::new(None) };
    static POPUPS: RefCell<HashMap<HWND, Popup>> = RefCell::new(HashMap::new());
    static ACTIVE: RefCell<Option<Gesture>> = const { RefCell::new(None) };
    static ACCENT: Cell<u32> = const { Cell::new(0) };
}

const SUBCLASS_ID: usize = 1;
const PREVIEW_CLASS: *const u16 = w!("FlowdepthDockPreview");

pub fn register_main<M: Send + 'static>(id: window::Id) -> Task<M> {
    window::run(id, move |handle| {
        MAIN.set(hwnd(handle).map(|hwnd| (id, hwnd)))
    })
    .discard()
}

pub fn forget_main() {
    MAIN.set(None);
    SURFACE.set(None);
}

pub fn watch<M: Send + 'static>(
    id: window::Id,
    eligible: bool,
    map: fn(ConfirmedDrop) -> M,
) -> Task<M> {
    window::run(id, move |handle| {
        let hwnd = hwnd(handle)?;
        let (events, receiver) = mpsc::unbounded();
        // iced runs this callback on the thread owning the HWND.
        if unsafe { SetWindowSubclass(hwnd, Some(popup_proc), SUBCLASS_ID, 0) } == 0 {
            log::warn!("Could not observe title-bar gestures for {id:?}");
            return None;
        }
        POPUPS.with_borrow_mut(|popups| {
            popups.insert(
                hwnd,
                Popup {
                    id,
                    eligible,
                    events,
                },
            )
        });
        log::debug!("WINDOW DockWatch | id={id:?} eligible={eligible}");
        Some(receiver)
    })
    .then(move |receiver| receiver.map_or_else(Task::none, |events| Task::run(events, map)))
}

pub fn enable_popup<M: Send + 'static>(id: window::Id) -> Task<M> {
    window::run(id, |handle| {
        if let Some(hwnd) = hwnd(handle) {
            POPUPS.with_borrow_mut(|popups| {
                if let Some(popup) = popups.get_mut(&hwnd) {
                    popup.eligible = true;
                }
            });
        }
    })
    .discard()
}

fn hwnd(handle: &dyn window::Window) -> Option<HWND> {
    match handle.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as HWND),
        _ => None,
    }
}

/// A transparent canvas records the actual pane-grid bounds in iced's client
/// coordinates. The Windows move loop can then use them without calling iced.
pub struct Measure {
    pub id: window::Id,
    pub minimum: Option<Point>,
    pub zoom: f64,
}

impl<M> canvas::Program<M> for Measure {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        _renderer: &iced::Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        SURFACE.set(self.minimum.map(|minimum| Surface {
            id: self.id,
            bounds: Rect {
                x: f64::from(bounds.x),
                y: f64::from(bounds.y),
                width: f64::from(bounds.width),
                height: f64::from(bounds.height),
            },
            minimum,
            zoom: self.zoom,
            accent: theme.palette().primary.base.color,
        }));
        Vec::new()
    }
}

struct Gesture {
    popup: HWND,
    hook: HHOOK,
    drag: Drag,
    start_origin: Point,
    button: u16,
    end: MoveEnd,
    preview: Option<(PreviewWindow, Preview)>,
    preview_failed: bool,
}

#[derive(Default)]
struct MoveEnd {
    cancelled: bool,
    released: Option<POINT>,
}

impl MoveEnd {
    fn observe(&mut self, message: &MSG) {
        match message.message {
            WM_KEYDOWN | WM_SYSKEYDOWN
                if [VK_ESCAPE, VK_RETURN].contains(&(message.wParam as u16)) =>
            {
                self.cancelled = true;
            }
            WM_LBUTTONUP | WM_NCLBUTTONUP => {
                self.released.get_or_insert(message.pt);
            }
            _ => {}
        }
    }

    fn confirm(self, fallback: impl FnOnce() -> Option<POINT>) -> Option<POINT> {
        if self.cancelled {
            return None;
        }
        self.released.or_else(fallback)
    }
}

impl Gesture {
    fn begin(popup: HWND) -> Option<Self> {
        let button = primary_button();
        let start = cursor()?;
        let start_origin = window_origin(popup)?;
        if !key_down(button) {
            return None;
        }

        // The OS can consume release and Escape inside its own move loop.
        // Observe queue input only on this GUI thread and only for the gesture.
        let hook = unsafe {
            SetWindowsHookExW(
                WH_GETMESSAGE,
                Some(move_messages),
                null_mut(),
                GetCurrentThreadId(),
            )
        };
        if hook.is_null() {
            log::warn!("Could not observe the native window move loop");
            return None;
        }
        log::debug!(
            "WINDOW DockDragStart | receiver={} geometry={}",
            MAIN.get().is_some(),
            SURFACE.get().is_some()
        );
        Some(Self {
            popup,
            hook,
            drag: Drag::new(point(start)),
            start_origin,
            button,
            end: MoveEnd::default(),
            preview: None,
            preview_failed: false,
        })
    }

    fn refresh(&mut self) {
        if self.end.cancelled || self.end.released.is_some() || self.preview_failed {
            return;
        }
        let sample = cursor().and_then(|position| {
            let (_, geometry, accent) = target(self.popup, position)?;
            Some((position, geometry, accent))
        });
        let candidate = match sample {
            Some((position, geometry, accent)) => self
                .drag
                .update(point(position), Some(geometry))
                .map(|preview| (preview, accent)),
            None => {
                self.drag.update(Point(0.0, 0.0), None);
                None
            }
        };

        let Some((preview, accent)) = candidate else {
            self.preview = None;
            return;
        };
        if self
            .preview
            .as_ref()
            .is_some_and(|(_, last)| *last == preview)
        {
            return;
        }
        let window = self
            .preview
            .take()
            .map(|(window, _)| window)
            .or_else(|| PreviewWindow::open(self.popup));
        self.preview = window
            .filter(|window| window.show(preview, accent))
            .map(|window| (window, preview));
        if self.preview.is_some() {
            log::debug!("WINDOW DockPreview | zone={:?}", preview.zone);
        } else {
            log::warn!("Could not show the native docking preview");
            self.preview_failed = true;
        }
    }

    fn cancel(&mut self) {
        self.end.cancelled = true;
        self.preview = None;
    }

    fn finish(self) {
        unsafe { UnhookWindowsHookEx(self.hook) };
        let displayed = self.preview.map(|(_, preview)| preview);
        let release = self.end.confirm(|| {
            // Some Windows move loops handle the mouse release without
            // delivering it to the thread hook. At WM_EXITSIZEMOVE, accept
            // the cursor only if a held-button move actually changed position.
            if unsafe { GetForegroundWindow() } != self.popup
                || key_down(self.button)
                || key_down(VK_ESCAPE)
                || key_down(VK_RETURN)
                || window_origin(self.popup)? == self.start_origin
            {
                return None;
            }
            cursor()
        });
        log::debug!(
            "WINDOW DockDragEnd | preview={} released={}",
            displayed.is_some(),
            release.is_some()
        );
        let (Some(release), Some(displayed)) = (release, displayed) else {
            return;
        };
        let Some((main, geometry, _)) = target(self.popup, release) else {
            return;
        };
        let Some(zone) = self.drag.release(point(release), Some(geometry)) else {
            return;
        };
        if zone == displayed.zone {
            POPUPS.with_borrow(|popups| {
                if let Some(popup) = popups.get(&self.popup) {
                    let drop = ConfirmedDrop {
                        main,
                        popup: popup.id,
                        zone,
                    };
                    if popup.events.unbounded_send(drop).is_ok() {
                        log::debug!("WINDOW DockDrop | popup={:?} zone={zone:?}", popup.id);
                    }
                }
            });
        }
    }
}

fn key_down(key: u16) -> bool {
    (unsafe { GetAsyncKeyState(i32::from(key)) }) < 0
}

fn primary_button() -> u16 {
    if unsafe { GetSystemMetrics(SM_SWAPBUTTON) } == 0 {
        VK_LBUTTON
    } else {
        VK_RBUTTON
    }
}

fn window_origin(hwnd: HWND) -> Option<Point> {
    let mut rect = RECT::default();
    (unsafe { GetWindowRect(hwnd, &mut rect) } != 0)
        .then_some(Point(f64::from(rect.left), f64::from(rect.top)))
}

unsafe extern "system" fn move_messages(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && wparam == PM_REMOVE as usize {
        // WH_GETMESSAGE provides a live MSG for the duration of this callback.
        let message = unsafe { &*(lparam as *const MSG) };
        ACTIVE.with_borrow_mut(|active| {
            if let Some(gesture) = active.as_mut() {
                gesture.end.observe(message);
            }
        });
    }
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

unsafe extern "system" fn popup_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if message == WM_ENTERSIZEMOVE
        && ACTIVE.with_borrow(Option::is_none)
        && POPUPS.with_borrow(|popups| popups.get(&hwnd).is_some_and(|popup| popup.eligible))
    {
        ACTIVE.replace(Gesture::begin(hwnd));
    }

    // The native loop can reenter the window procedure while a preview is painted.
    let move_message = matches!(
        message,
        WM_MOVING
            | WM_SIZING
            | WM_EXITSIZEMOVE
            | WM_CANCELMODE
            | WM_CAPTURECHANGED
            | WM_ACTIVATE
            | WM_CLOSE
            | WM_NCDESTROY
    );
    if move_message
        && let Some(mut gesture) =
            ACTIVE.with_borrow_mut(|active| active.take_if(|gesture| gesture.popup == hwnd))
    {
        if message == WM_EXITSIZEMOVE {
            gesture.finish();
            return unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
        }
        match message {
            WM_MOVING => gesture.refresh(),
            // The move loop normally releases capture to NULL before reporting exit.
            WM_CAPTURECHANGED if lparam == 0 || gesture.end.released.is_some() => {}
            WM_ACTIVATE if wparam & 0xffff != WA_INACTIVE as usize => {}
            _ => gesture.cancel(),
        }
        if message == WM_NCDESTROY {
            unsafe { UnhookWindowsHookEx(gesture.hook) };
        } else {
            ACTIVE.replace(Some(gesture));
        }
    }
    if message == WM_NCDESTROY {
        POPUPS.with_borrow_mut(|popups| popups.remove(&hwnd));
        unsafe { RemoveWindowSubclass(hwnd, Some(popup_proc), SUBCLASS_ID) };
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}
fn cursor() -> Option<POINT> {
    let mut position = POINT::default();
    (unsafe { GetCursorPos(&mut position) } != 0).then_some(position)
}

fn point(position: POINT) -> Point {
    Point(f64::from(position.x), f64::from(position.y))
}

fn target(popup: HWND, pointer: POINT) -> Option<(window::Id, Geometry, Color)> {
    let (id, main) = MAIN.get()?;
    let surface = SURFACE.get().filter(|surface| surface.id == id)?;
    // winit marks its Windows event-loop thread per-monitor DPI aware.
    // ClientToScreen and the native message point therefore share screen pixels.
    unsafe {
        if IsWindowVisible(popup) == 0
            || IsZoomed(popup) != 0
            || IsIconic(popup) != 0
            || IsWindowVisible(main) == 0
            || IsIconic(main) != 0
            || IsWindowEnabled(main) == 0
        {
            return None;
        }
        let mut origin = POINT::default();
        let mut client = RECT::default();
        let scale = f64::from(GetDpiForWindow(main)) / 96.0 * surface.zoom;
        if ClientToScreen(main, &mut origin) == 0 || GetClientRect(main, &mut client) == 0 {
            return None;
        }
        let under_pointer = GetAncestor(WindowFromPoint(pointer), GA_ROOT);
        if under_pointer != main
            && under_pointer != popup
            && GetWindow(under_pointer, GW_OWNER) != popup
        {
            return None;
        }

        let bounds = surface.bounds;
        if bounds.x + bounds.width > f64::from(client.right) / scale + 1.0
            || bounds.y + bounds.height > f64::from(client.bottom) / scale + 1.0
        {
            return None;
        }
        Some((
            id,
            Geometry::from_client(point(origin), scale, bounds, surface.minimum),
            surface.accent,
        ))
    }
}

struct PreviewWindow(HWND);

impl PreviewWindow {
    fn open(owner: HWND) -> Option<Self> {
        static REGISTERED: OnceLock<bool> = OnceLock::new();
        let instance = unsafe { GetModuleHandleW(null()) };
        if !REGISTERED.get_or_init(|| {
            let class = WNDCLASSW {
                lpfnWndProc: Some(preview_proc),
                hInstance: instance,
                lpszClassName: PREVIEW_CLASS,
                ..Default::default()
            };
            unsafe { RegisterClassW(&class) != 0 }
        }) {
            return None;
        }
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT,
                PREVIEW_CLASS,
                null(),
                WS_POPUP,
                0,
                0,
                0,
                0,
                owner,
                null_mut(),
                instance,
                null(),
            )
        };
        (!hwnd.is_null()).then_some(Self(hwnd))
    }

    fn show(&self, preview: Preview, accent: Color) -> bool {
        let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u32;
        ACCENT.set(channel(accent.r) | channel(accent.g) << 8 | channel(accent.b) << 16);
        let rect = preview.placement;
        let x = rect.x.round() as i32;
        let y = rect.y.round() as i32;
        let width = (rect.x + rect.width).round() as i32 - x;
        let height = (rect.y + rect.height).round() as i32 - y;
        unsafe {
            let visible = SetLayeredWindowAttributes(self.0, 0, 90, LWA_ALPHA) != 0
                && SetWindowPos(
                    self.0,
                    HWND_TOP,
                    x,
                    y,
                    width,
                    height,
                    SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_SHOWWINDOW,
                ) != 0;
            if visible {
                InvalidateRect(self.0, null(), 0);
                UpdateWindow(self.0);
            }
            visible
        }
    }
}

impl Drop for PreviewWindow {
    fn drop(&mut self) {
        unsafe { DestroyWindow(self.0) };
    }
}

unsafe extern "system" fn preview_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_NCHITTEST => HTTRANSPARENT as LRESULT,
        WM_PAINT => unsafe {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut paint);
            let mut rect = RECT::default();
            if !dc.is_null() && GetClientRect(hwnd, &mut rect) != 0 {
                let color = ACCENT.get();
                let fill = CreateSolidBrush(color);
                let border = CreateSolidBrush((color & 0xfefefe) >> 1 | 0x808080);
                FillRect(dc, &rect, fill);
                FrameRect(dc, &rect, border);
                DeleteObject(fill);
                DeleteObject(border);
            }
            EndPaint(hwnd, &paint);
            0
        },
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(x: i32) -> MSG {
        MSG {
            hwnd: std::ptr::dangling_mut(),
            message: WM_LBUTTONUP,
            pt: POINT { x, y: -100 },
            ..Default::default()
        }
    }

    #[test]
    fn docking_uses_exit_position_if_the_system_consumed_the_release() {
        assert!(MoveEnd::default().confirm(|| None).is_none());
        let point = MoveEnd::default()
            .confirm(|| Some(release(300).pt))
            .unwrap();
        assert_eq!((point.x, point.y), (300, -100));
    }

    #[test]
    fn docking_uses_the_first_release_point() {
        let mut end = MoveEnd::default();
        let event = release(300);
        end.observe(&event);
        end.observe(&release(500));
        let point = end.confirm(|| Some(release(900).pt)).unwrap();
        assert_eq!((point.x, point.y), (300, -100));
    }

    #[test]
    fn docking_rejects_escape_and_keyboard_completion() {
        for key in [VK_ESCAPE, VK_RETURN] {
            let mut end = MoveEnd::default();
            let event = release(300);
            end.observe(&MSG {
                message: WM_KEYDOWN,
                wParam: key.into(),
                ..Default::default()
            });
            end.observe(&event);
            assert!(end.confirm(|| Some(release(900).pt)).is_none());
        }
    }
}
