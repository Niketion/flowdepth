//! Windows title-bar drag adapter. All HWNDs and gesture state stay on the
//! iced event-loop thread. Only a confirmed drop crosses into app update.

mod overlay;
pub mod peer;

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ptr::null_mut,
    time::Instant,
};

use iced::{
    Color, Rectangle, Task, Theme,
    futures::channel::mpsc,
    widget::canvas,
    window::{self, raw_window_handle::RawWindowHandle},
};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
    Graphics::Gdi::{ClientToScreen, PtInRect},
    System::Threading::GetCurrentThreadId,
    UI::{
        HiDpi::GetDpiForWindow,
        Input::KeyboardAndMouse::{
            GetAsyncKeyState, IsWindowEnabled, VK_ESCAPE, VK_LBUTTON, VK_RBUTTON, VK_RETURN,
        },
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::*,
    },
};

use crate::docking::{Drag, Geometry, Guide, Point, Rect, Zone};

#[derive(Debug, Clone, Copy)]
pub struct ConfirmedDrop {
    pub destination: Destination,
    pub popup: window::Id,
    pub zone: Zone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    Local(window::Id),
    Peer(peer::Remote),
}

struct Popup {
    id: window::Id,
    eligible: bool,
    events: mpsc::UnboundedSender<ConfirmedDrop>,
}

#[derive(Clone, Copy)]
struct Surface {
    id: window::Id,
    layout: u32,
    bounds: Rect,
    minimum: Point,
    zoom: f64,
    accent: Color,
}

thread_local! {
    static MAIN: Cell<Option<(window::Id, HWND)>> = const { Cell::new(None) };
    static SURFACE: Cell<Option<Surface>> = const { Cell::new(None) };
    static PUBLISHED: Cell<Option<peer::Advert>> = const { Cell::new(None) };
    static POPUPS: RefCell<HashMap<HWND, Popup>> = RefCell::new(HashMap::new());
    static ACTIVE: RefCell<Option<Gesture>> = const { RefCell::new(None) };
}

const SUBCLASS_ID: usize = 1;

pub fn register_main<M: Send + 'static>(id: window::Id) -> Task<M> {
    window::run(id, move |handle| {
        let Some(hwnd) = hwnd(handle) else { return };
        MAIN.set(Some((id, hwnd)));
        if unsafe { SetWindowSubclass(hwnd, Some(main_proc), SUBCLASS_ID, 0) } == 0 {
            log::warn!("WINDOW DockPeerUnavailable | reason=window_registration_failed");
        }
    })
    .discard()
}

pub fn forget_main() {
    if let Some((_, hwnd)) = MAIN.get()
        && PUBLISHED.take().is_some()
    {
        peer::unpublish(hwnd);
    }
    MAIN.set(None);
    SURFACE.set(None);
}

unsafe extern "system" fn main_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if message == WM_NCDESTROY {
        if MAIN.get().is_some_and(|(_, main)| main == hwnd) {
            forget_main();
        }
        unsafe { RemoveWindowSubclass(hwnd, Some(main_proc), SUBCLASS_ID) };
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

pub fn accepts_peer(layout: u32) -> bool {
    SURFACE
        .get()
        .is_some_and(|surface| surface.layout == layout)
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
    pub layout: Option<uuid::Uuid>,
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
        let surface = self
            .minimum
            .zip(self.layout)
            .map(|(minimum, layout)| Surface {
                id: self.id,
                layout: peer::stamp(layout),
                bounds: Rect {
                    x: f64::from(bounds.x),
                    y: f64::from(bounds.y),
                    width: f64::from(bounds.width),
                    height: f64::from(bounds.height),
                },
                minimum,
                zoom: self.zoom,
                accent: theme.palette().primary.base.color,
            });
        SURFACE.set(surface);
        if let Some((_, hwnd)) = MAIN.get().filter(|(id, _)| *id == self.id) {
            let advert = surface
                .filter(|_| peer::available())
                .map(|surface| peer::Advert {
                    layout: surface.layout,
                    bounds: surface.bounds,
                    minimum: surface.minimum,
                    zoom: surface.zoom,
                    accent: rgb(surface.accent),
                });
            publish(hwnd, advert);
        }
        Vec::new()
    }
}

fn publish(hwnd: HWND, advert: Option<peer::Advert>) {
    let previous = PUBLISHED.get();
    if previous == advert {
        return;
    }
    if let Some(advert) = advert
        && peer::publish(hwnd, advert)
    {
        PUBLISHED.set(Some(advert));
        if previous.is_none() {
            log::debug!("WINDOW DockPeerPublished | pid={}", std::process::id());
        }
        return;
    }
    peer::unpublish(hwnd);
    PUBLISHED.set(None);
}

struct Gesture {
    popup: HWND,
    hook: HHOOK,
    drag: Drag,
    start_origin: Point,
    button: u16,
    end: MoveEnd,
    targets: Vec<HWND>,
    receiver: Option<Destination>,
    edge_timer: usize,
    overlay: Option<overlay::Overlay>,
    shown: Option<(Destination, Guide, u32)>,
    overlay_failed: bool,
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
            targets: receiving_windows(),
            receiver: None,
            edge_timer: 0,
            overlay: None,
            shown: None,
            overlay_failed: false,
        })
    }

    fn refresh(&mut self) {
        if self.end.cancelled || self.end.released.is_some() || self.overlay_failed {
            return;
        }
        let sample = cursor().and_then(|position| {
            let (destination, geometry, accent) = target(&self.targets, self.popup, position)?;
            Some((position, destination, geometry, accent))
        });
        let receiver = sample.map(|(_, destination, _, _)| destination);
        if self.receiver != receiver {
            self.drag.reset_target();
            self.receiver = receiver;
        }
        let now = Instant::now();
        let candidate = match sample {
            Some((position, destination, geometry, accent)) => self
                .drag
                .update(point(position), Some(geometry), now)
                .map(|guide| (destination, guide, rgb(accent))),
            None => {
                self.drag.reset_target();
                None
            }
        };
        if self.drag.waiting_for_edge() {
            if self.edge_timer == 0 {
                self.edge_timer = unsafe { SetTimer(null_mut(), 0, 30, None) };
            }
        } else {
            self.stop_edge_timer();
        }

        if self.shown == candidate {
            return;
        }
        let Some((destination, guide, accent)) = candidate else {
            if let Some(overlay) = &self.overlay {
                overlay.hide();
            }
            self.shown = None;
            return;
        };
        if self.overlay.is_none() {
            self.overlay = overlay::Overlay::open(self.popup);
        }
        if self
            .overlay
            .as_ref()
            .is_some_and(|overlay| overlay.show(guide, destination, accent))
        {
            self.shown = candidate;
            if let Some(preview) = guide.preview {
                log::debug!("WINDOW DockPreview | zone={:?}", preview.zone);
            }
        } else {
            log::warn!("Could not show the native docking guide");
            self.overlay = None;
            self.shown = None;
            self.overlay_failed = true;
            self.stop_edge_timer();
        }
    }

    fn stop_edge_timer(&mut self) {
        if self.edge_timer != 0 {
            unsafe { KillTimer(null_mut(), self.edge_timer) };
            self.edge_timer = 0;
        }
    }

    fn cancel(&mut self) {
        self.end.cancelled = true;
        self.stop_edge_timer();
        self.overlay = None;
        self.shown = None;
    }

    fn finish(mut self) {
        self.stop_edge_timer();
        unsafe { UnhookWindowsHookEx(self.hook) };
        drop(self.overlay);
        let displayed = self
            .shown
            .and_then(|(receiver, guide, _)| Some((receiver, guide.preview?)));
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
        let (Some(release), Some((shown_receiver, displayed))) = (release, displayed) else {
            return;
        };
        let Some((destination, geometry, _)) = target(&self.targets, self.popup, release) else {
            return;
        };
        let Some(zone) = self.drag.release(point(release), Some(geometry)) else {
            return;
        };
        if destination == shown_receiver && zone == displayed.zone {
            POPUPS.with_borrow(|popups| {
                if let Some(popup) = popups.get(&self.popup) {
                    let drop = ConfirmedDrop {
                        destination,
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
        if message.message == WM_TIMER && message.hwnd.is_null() {
            // A thread timer runs through the move-loop hook as well, so a
            // stationary pointer can complete the dwell without a WM_MOVING.
            let pending = ACTIVE.with_borrow_mut(|active| {
                active.take_if(|gesture| gesture.edge_timer == message.wParam)
            });
            if let Some(mut gesture) = pending {
                if key_down(gesture.button) {
                    gesture.refresh();
                } else {
                    gesture.stop_edge_timer();
                }
                ACTIVE.replace(Some(gesture));
            }
        }
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

fn receiving_windows() -> Vec<HWND> {
    unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> i32 {
        // EnumWindows calls this synchronously with the live Vec below.
        if unsafe { IsWindowVisible(hwnd) } != 0 {
            unsafe { &mut *(data as *mut Vec<HWND>) }.push(hwnd);
        }
        1
    }

    let mut windows = Vec::new();
    unsafe { EnumWindows(Some(collect), &mut windows as *mut Vec<HWND> as LPARAM) };
    windows
}

fn target(windows: &[HWND], popup: HWND, pointer: POINT) -> Option<(Destination, Geometry, Color)> {
    let under_pointer = unsafe {
        if IsWindowVisible(popup) == 0 || IsZoomed(popup) != 0 || IsIconic(popup) != 0 {
            return None;
        }
        GetAncestor(WindowFromPoint(pointer), GA_ROOT)
    };
    // The moving title bar is often the top hit. Find the window underneath
    // it before deciding whether that window can receive the pane.
    let covered_by_popup =
        under_pointer == popup || unsafe { GetWindow(under_pointer, GW_OWNER) } == popup;
    let hwnd = if covered_by_popup {
        windows.iter().copied().find(|&hwnd| {
            if hwnd == popup {
                return false;
            }
            let mut rect = RECT::default();
            unsafe {
                IsWindowVisible(hwnd) != 0
                    && IsIconic(hwnd) == 0
                    && (GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TRANSPARENT == 0)
                    && GetWindowRect(hwnd, &mut rect) != 0
                    && PtInRect(&rect, pointer) != 0
            }
        })?
    } else {
        under_pointer
    };
    let (destination, geometry, accent) = destination_geometry(hwnd)?;
    geometry
        .screen
        .contains(point(pointer))
        .then_some((destination, geometry, accent))
}

fn destination_geometry(hwnd: HWND) -> Option<(Destination, Geometry, Color)> {
    let (destination, bounds, minimum, zoom, accent) =
        if let Some((id, _)) = MAIN.get().filter(|&(_, main)| main == hwnd) {
            let surface = SURFACE.get().filter(|surface| surface.id == id)?;
            (
                Destination::Local(id),
                surface.bounds,
                surface.minimum,
                surface.zoom,
                surface.accent,
            )
        } else {
            let (remote, advert) = peer::read(hwnd)?;
            let color = advert.accent;
            let accent = Color::from_rgb8(color as u8, (color >> 8) as u8, (color >> 16) as u8);
            (
                Destination::Peer(remote),
                advert.bounds,
                advert.minimum,
                advert.zoom,
                accent,
            )
        };

    // winit marks its Windows event-loop thread per-monitor DPI aware.
    // ClientToScreen and the native message point therefore share screen pixels.
    unsafe {
        if IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 || IsWindowEnabled(hwnd) == 0 {
            return None;
        }
        let mut origin = POINT::default();
        let mut client = RECT::default();
        let scale = f64::from(GetDpiForWindow(hwnd)) / 96.0 * zoom;
        if !scale.is_finite()
            || scale <= 0.0
            || ClientToScreen(hwnd, &mut origin) == 0
            || GetClientRect(hwnd, &mut client) == 0
            || bounds.x + bounds.width > f64::from(client.right) / scale + 1.0
            || bounds.y + bounds.height > f64::from(client.bottom) / scale + 1.0
        {
            return None;
        }
        Some((
            destination,
            Geometry::from_client(point(origin), scale, bounds, minimum),
            accent,
        ))
    }
}

fn rgb(accent: Color) -> u32 {
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u32;
    channel(accent.r) | channel(accent.g) << 8 | channel(accent.b) << 16
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
