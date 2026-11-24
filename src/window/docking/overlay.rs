//! Click-through native overlays drawn while Windows owns the move loop.

use std::{
    cell::Cell,
    ptr::{null, null_mut},
    sync::OnceLock,
};

use windows_sys::{
    Win32::{
        Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::*,
    },
    w,
};

use super::Destination;
use crate::docking::{Guide, Rect, Zone};

thread_local! {
    static PAINT: Cell<Option<(Guide, Destination, COLORREF)>> = const { Cell::new(None) };
}

pub(super) struct Overlay {
    guide: OverlayWindow,
    placement: OverlayWindow,
}

impl Overlay {
    pub(super) fn open(owner: HWND) -> Option<Self> {
        static GUIDE: OnceLock<bool> = OnceLock::new();
        static PLACEMENT: OnceLock<bool> = OnceLock::new();
        Some(Self {
            guide: OverlayWindow::open(owner, &GUIDE, w!("FlowdepthDockGuide"), guide_proc)?,
            placement: OverlayWindow::open(
                owner,
                &PLACEMENT,
                w!("FlowdepthDockPlacement"),
                placement_proc,
            )?,
        })
    }

    pub(super) fn show(&self, guide: Guide, destination: Destination, accent: COLORREF) -> bool {
        PAINT.set(Some((guide, destination, accent)));
        if let Some(preview) = guide.preview {
            if !self.placement.show(preview.placement, 82) {
                return false;
            }
        } else {
            self.placement.hide();
        }
        // Put the guide above the placement preview, even where they overlap.
        self.guide.show(guide.bounds, 255)
    }

    pub(super) fn hide(&self) {
        self.guide.hide();
        self.placement.hide();
    }
}

struct OverlayWindow(HWND);

impl OverlayWindow {
    fn open(
        owner: HWND,
        registered: &OnceLock<bool>,
        name: *const u16,
        procedure: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
    ) -> Option<Self> {
        let instance = unsafe { GetModuleHandleW(null()) };
        if !registered.get_or_init(|| {
            let class = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                lpszClassName: name,
                ..Default::default()
            };
            unsafe { RegisterClassW(&class) != 0 }
        }) {
            return None;
        }
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT,
                name,
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

    fn show(&self, bounds: Rect, opacity: u8) -> bool {
        let rect = native_rect(bounds);
        unsafe {
            let visible = SetLayeredWindowAttributes(self.0, 0, opacity, LWA_ALPHA) != 0
                && SetWindowPos(
                    self.0,
                    HWND_TOP,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_SHOWWINDOW,
                ) != 0;
            visible && InvalidateRect(self.0, null(), 0) != 0 && UpdateWindow(self.0) != 0
        }
    }

    fn hide(&self) {
        unsafe { ShowWindow(self.0, SW_HIDE) };
    }
}

impl Drop for OverlayWindow {
    fn drop(&mut self) {
        unsafe { DestroyWindow(self.0) };
    }
}

fn native_rect(rect: Rect) -> RECT {
    RECT {
        left: rect.x.round() as i32,
        top: rect.y.round() as i32,
        right: (rect.x + rect.width).round() as i32,
        bottom: (rect.y + rect.height).round() as i32,
    }
}

unsafe extern "system" fn guide_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    overlay_proc(hwnd, message, wparam, lparam, draw_guide)
}

unsafe extern "system" fn placement_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    overlay_proc(hwnd, message, wparam, lparam, |dc, rect| {
        if let Some((_, _, accent)) = PAINT.get() {
            fill(dc, rect, accent);
            frame(dc, rect, 0xffffff);
        }
    })
}

fn overlay_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    draw: impl FnOnce(HDC, &RECT),
) -> LRESULT {
    match message {
        WM_NCHITTEST => HTTRANSPARENT as LRESULT,
        WM_PAINT => unsafe {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut paint);
            let mut rect = RECT::default();
            if !dc.is_null() && GetClientRect(hwnd, &mut rect) != 0 {
                draw(dc, &rect);
            }
            EndPaint(hwnd, &paint);
            0
        },
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn draw_guide(dc: HDC, client: &RECT) {
    let Some((guide, destination, accent)) = PAINT.get() else {
        return;
    };
    let active = guide.preview.map(|preview| preview.zone);
    let header = match (destination, active) {
        (Destination::Local(_), None) => w!("Dock pane"),
        (Destination::Local(_), Some(Zone::Left)) => w!("Release to dock on the left"),
        (Destination::Local(_), Some(Zone::Right)) => w!("Release to dock on the right"),
        (Destination::Local(_), Some(Zone::Top)) => w!("Release to dock at the top"),
        (Destination::Local(_), Some(Zone::Bottom)) => w!("Release to dock at the bottom"),
        (Destination::Peer(_), None) => w!("Merge into this window"),
        (Destination::Peer(_), Some(Zone::Left)) => w!("Release to merge on the left"),
        (Destination::Peer(_), Some(Zone::Right)) => w!("Release to merge on the right"),
        (Destination::Peer(_), Some(Zone::Top)) => w!("Release to merge at the top"),
        (Destination::Peer(_), Some(Zone::Bottom)) => w!("Release to merge at the bottom"),
    };
    let pixels = |logical: f64| (logical * guide.scale).round() as i32;
    let mut font = LOGFONTW {
        lfHeight: -pixels(14.0).max(1),
        ..Default::default()
    };
    for (slot, value) in font.lfFaceName.iter_mut().zip("Segoe UI".encode_utf16()) {
        *slot = value;
    }
    let font = unsafe { CreateFontIndirectW(&font) };
    let previous_font = unsafe {
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, 0xffffff);
        SelectObject(dc, font)
    };
    fill(dc, client, 0x24201e);
    frame(dc, client, 0x69635e);

    let origin = native_rect(guide.bounds);
    let mut targets = guide.targets().peekable();
    if let Some((_, first)) = targets.peek() {
        let mut title = *client;
        title.top += pixels(3.0);
        title.bottom = native_rect(*first).top - origin.top;
        label(dc, &mut title, header);
    }
    for (zone, rect) in targets {
        let mut tile = native_rect(rect);
        unsafe { OffsetRect(&mut tile, -origin.left, -origin.top) };
        let hovered = active == Some(zone);
        fill(dc, &tile, if hovered { 0x544237 } else { 0x34302c });
        frame(dc, &tile, if hovered { accent } else { 0x69635e });

        let glyph_top = (tile.top + tile.bottom - pixels(18.0)) / 2;
        let glyph = RECT {
            left: tile.left + pixels(12.0),
            top: glyph_top,
            right: tile.left + pixels(36.0),
            bottom: glyph_top + pixels(18.0),
        };
        let mut section = glyph;
        let x = (glyph.left + glyph.right) / 2;
        let y = (glyph.top + glyph.bottom) / 2;
        let direction = match zone {
            Zone::Left => {
                section.right = x;
                w!("Left")
            }
            Zone::Right => {
                section.left = x;
                w!("Right")
            }
            Zone::Top => {
                section.bottom = y;
                w!("Top")
            }
            Zone::Bottom => {
                section.top = y;
                w!("Bottom")
            }
        };
        fill(dc, &section, if hovered { accent } else { 0xbcb5ac });
        frame(dc, &glyph, 0xe0dbd6);
        tile.left = glyph.right;
        tile.right -= pixels(3.0);
        label(dc, &mut tile, direction);
    }
    unsafe {
        if !previous_font.is_null() {
            SelectObject(dc, previous_font);
        }
        DeleteObject(font);
    }
}

fn fill(dc: HDC, rect: &RECT, color: COLORREF) {
    unsafe {
        let brush = CreateSolidBrush(color);
        FillRect(dc, rect, brush);
        DeleteObject(brush);
    }
}

fn frame(dc: HDC, rect: &RECT, color: COLORREF) {
    unsafe {
        let brush = CreateSolidBrush(color);
        FrameRect(dc, rect, brush);
        DeleteObject(brush);
    }
}

fn label(dc: HDC, rect: &mut RECT, text: *const u16) {
    unsafe { DrawTextW(dc, text, -1, rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE) };
}
