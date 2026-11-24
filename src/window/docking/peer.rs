//! Discovery and transfer between Windows processes in the desktop session.

use std::{
    io,
    os::windows::io::AsRawHandle,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use iced::{Subscription, futures::SinkExt, stream};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions},
    sync::oneshot,
    time::timeout,
};
use windows_sys::{
    Win32::{
        Foundation::{HANDLE, HWND},
        System::Pipes::GetNamedPipeServerProcessId,
        UI::WindowsAndMessaging::{GetPropW, GetWindowThreadProcessId, RemovePropW, SetPropW},
    },
    w,
};

use crate::docking::{Point, Rect, Zone};

const MAX_OFFER: usize = 8 * 1024 * 1024;
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(4);
const COOKIE: *const u16 = w!("Flowdepth.Dock.v1.Cookie");
const PRECISE_ZOOM: *const u16 = w!("Flowdepth.Dock.v1.PreciseZoom");
const FIELDS: [*const u16; 9] = [
    w!("Flowdepth.Dock.v1.Layout"),
    w!("Flowdepth.Dock.v1.X"),
    w!("Flowdepth.Dock.v1.Y"),
    w!("Flowdepth.Dock.v1.Width"),
    w!("Flowdepth.Dock.v1.Height"),
    w!("Flowdepth.Dock.v1.MinimumWidth"),
    w!("Flowdepth.Dock.v1.MinimumHeight"),
    w!("Flowdepth.Dock.v1.Zoom"),
    w!("Flowdepth.Dock.v1.Accent"),
];

static READY: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Endpoint {
    pid: u32,
    cookie: u32,
}

impl Endpoint {
    fn local() -> Self {
        static ENDPOINT: OnceLock<Endpoint> = OnceLock::new();
        *ENDPOINT.get_or_init(|| Self {
            pid: std::process::id(),
            cookie: stamp(uuid::Uuid::new_v4()),
        })
    }

    fn pipe(self) -> String {
        format!(
            r"\\.\pipe\Flowdepth.Dock.v1.{}.{:08x}",
            self.pid, self.cookie
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Remote {
    endpoint: Endpoint,
    pub layout: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Advert {
    pub layout: u32,
    pub bounds: Rect,
    pub minimum: Point,
    pub zoom: f64,
    pub accent: u32,
}

pub fn stamp(layout: uuid::Uuid) -> u32 {
    let (first, second, third, last) = layout.as_fields();
    (first
        ^ ((u32::from(second) << 16) | u32::from(third))
        ^ u32::from_le_bytes(last[..4].try_into().unwrap()))
    .max(1)
}

pub(super) fn available() -> bool {
    READY.load(Ordering::Acquire)
}

fn fixed_encode(number: f64) -> u32 {
    ((number * 64.0).round() as u32).saturating_add(1)
}

fn fixed_decode(number: u32) -> f64 {
    f64::from(number - 1) / 64.0
}

fn zoom_encode(zoom: f64) -> u32 {
    // The UI scale originates as f32. Preserve its bits in the window property.
    (zoom as f32).to_bits()
}

fn zoom_decode(precise: u32, legacy: u32) -> f64 {
    if precise == 0 {
        fixed_decode(legacy)
    } else {
        f64::from(f32::from_bits(precise))
    }
}

pub(super) fn publish(hwnd: HWND, advert: Advert) -> bool {
    let values = [
        advert.layout,
        fixed_encode(advert.bounds.x),
        fixed_encode(advert.bounds.y),
        fixed_encode(advert.bounds.width),
        fixed_encode(advert.bounds.height),
        fixed_encode(advert.minimum.0),
        fixed_encode(advert.minimum.1),
        fixed_encode(advert.zoom),
        advert.accent + 1,
    ];
    unsafe {
        // Publish the pipe last so readers can ignore an incomplete surface.
        RemovePropW(hwnd, COOKIE);
        FIELDS
            .iter()
            .zip(values)
            .all(|(&key, value)| SetPropW(hwnd, key, value as usize as HANDLE) != 0)
            && SetPropW(
                hwnd,
                PRECISE_ZOOM,
                zoom_encode(advert.zoom) as usize as HANDLE,
            ) != 0
            && SetPropW(hwnd, COOKIE, Endpoint::local().cookie as usize as HANDLE) != 0
    }
}

pub(super) fn unpublish(hwnd: HWND) {
    unsafe {
        RemovePropW(hwnd, COOKIE);
        RemovePropW(hwnd, PRECISE_ZOOM);
        for field in FIELDS {
            RemovePropW(hwnd, field);
        }
    }
}

pub(super) fn read(hwnd: HWND) -> Option<(Remote, Advert)> {
    let cookie = unsafe { GetPropW(hwnd, COOKIE) } as usize as u32;
    if cookie == 0 {
        return None;
    }
    let fields = FIELDS.map(|key| unsafe { GetPropW(hwnd, key) } as usize as u32);
    let precise_zoom = unsafe { GetPropW(hwnd, PRECISE_ZOOM) } as usize as u32;
    let [
        layout,
        x,
        y,
        width,
        height,
        min_width,
        min_height,
        zoom,
        accent,
    ] = fields;
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if fields.contains(&0)
        || pid == 0
        || pid == std::process::id()
        || unsafe { GetPropW(hwnd, COOKIE) } as usize as u32 != cookie
    {
        return None;
    }
    Some((
        Remote {
            endpoint: Endpoint { pid, cookie },
            layout,
        },
        Advert {
            layout,
            bounds: Rect {
                x: fixed_decode(x),
                y: fixed_decode(y),
                width: fixed_decode(width),
                height: fixed_decode(height),
            },
            minimum: Point(fixed_decode(min_width), fixed_decode(min_height)),
            zoom: zoom_decode(precise_zoom, zoom),
            accent: accent - 1,
        },
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Offer {
    pub layout: u32,
    pub zone: Zone,
    pub pane: data::Pane,
}

#[derive(Debug, Clone)]
pub struct Incoming {
    pub offer: Arc<Offer>,
    reply: Arc<Mutex<Option<oneshot::Sender<bool>>>>,
}

impl Incoming {
    pub fn take_reply(&self) -> Option<oneshot::Sender<bool>> {
        self.reply
            .lock()
            .ok()?
            .take()
            .filter(|reply| !reply.is_closed())
    }
}

pub fn listen() -> Subscription<Incoming> {
    Subscription::run(|| {
        stream::channel(1, async |mut output| {
            let mut options = ServerOptions::new();
            options
                .first_pipe_instance(true)
                .reject_remote_clients(true);
            let mut server = match options.create(Endpoint::local().pipe()) {
                Ok(server) => server,
                Err(error) => {
                    log::warn!("WINDOW DockPeerUnavailable | error={error}");
                    return;
                }
            };
            struct Readiness;
            impl Drop for Readiness {
                fn drop(&mut self) {
                    READY.store(false, Ordering::Release);
                }
            }
            let _ready = Readiness;
            READY.store(true, Ordering::Release);
            loop {
                if let Err(error) = server.connect().await {
                    log::warn!("WINDOW DockPeerUnavailable | error={error}");
                    break;
                }
                let _ = timeout(TRANSFER_TIMEOUT, async {
                    let offer = read_offer(&mut server).await?;
                    let (reply, accepted) = oneshot::channel();
                    output
                        .send(Incoming {
                            offer: Arc::new(offer),
                            reply: Arc::new(Mutex::new(Some(reply))),
                        })
                        .await
                        .map_err(io::Error::other)?;
                    acknowledge(&mut server, accepted.await.unwrap_or(false)).await
                })
                .await;
                if let Err(error) = server.disconnect() {
                    log::warn!("WINDOW DockPeerUnavailable | error={error}");
                    break;
                }
            }
        })
    })
}

async fn acknowledge(server: &mut NamedPipeServer, accepted: bool) -> io::Result<()> {
    server.write_u8(u8::from(accepted)).await?;
    // DisconnectNamedPipe discards unread bytes. The receipt confirms that
    // the client read the response before the pipe instance is reused.
    server.read_u8().await?;
    Ok(())
}

async fn read_offer(server: &mut NamedPipeServer) -> io::Result<Offer> {
    let length = server.read_u32_le().await? as usize;
    if !(1..=MAX_OFFER).contains(&length) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid pane length",
        ));
    }
    let mut bytes = vec![0; length];
    server.read_exact(&mut bytes).await?;
    decode(&bytes)
}

fn decode(bytes: &[u8]) -> io::Result<Offer> {
    let offer: Offer = serde_json::from_slice(bytes)?;
    if matches!(offer.pane, data::Pane::Split { .. }) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected one pane",
        ));
    }
    Ok(offer)
}

pub async fn send(remote: Remote, zone: Zone, pane: data::Pane) -> io::Result<bool> {
    timeout(TRANSFER_TIMEOUT, async {
        let bytes = serde_json::to_vec(&Offer {
            layout: remote.layout,
            zone,
            pane,
        })?;
        if bytes.len() > MAX_OFFER {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "pane is too large to transfer",
            ));
        }
        let mut client = ClientOptions::new().open(remote.endpoint.pipe())?;
        let mut pid = 0;
        if unsafe { GetNamedPipeServerProcessId(client.as_raw_handle(), &mut pid) } == 0
            || pid != remote.endpoint.pid
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Merge destination has changed",
            ));
        }
        client.write_u32_le(bytes.len() as u32).await?;
        client.write_all(&bytes).await?;
        let accepted = client.read_u8().await? == 1;
        let _ = client.write_u8(0).await;
        Ok(accepted)
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHILD_COOKIE: &str = "FLOWSURFACE_TEST_DOCK_PEER_COOKIE";

    #[test]
    fn docking_advert_preserves_fractional_zoom_and_accepts_legacy_properties() {
        for zoom in [0.9_f32, 1.0, 1.1, 1.2] {
            let zoom = f64::from(zoom);
            let legacy = fixed_encode(zoom);
            assert_eq!(zoom_decode(zoom_encode(zoom), legacy), zoom);
            assert_eq!(zoom_decode(0, legacy), fixed_decode(legacy));
        }
        let zoom = f64::from(0.9_f32);
        let legacy = fixed_encode(zoom);
        let dashboard_right = fixed_decode(fixed_encode(1920.0 / zoom - 8.0));
        assert!(dashboard_right > 1920.0 / zoom_decode(0, legacy) + 1.0);
        assert!(dashboard_right <= 1920.0 / zoom_decode(zoom_encode(zoom), legacy) + 1.0);
    }

    #[test]
    fn docking_pipe_child() {
        let Ok(cookie) = std::env::var(CHILD_COOKIE) else {
            return;
        };
        let endpoint = Endpoint {
            pid: std::process::id(),
            cookie: cookie.parse().unwrap(),
        };
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let mut server = ServerOptions::new()
                    .first_pipe_instance(true)
                    .reject_remote_clients(true)
                    .create(endpoint.pipe())
                    .unwrap();
                println!("dock-peer-ready");
                timeout(TRANSFER_TIMEOUT, async {
                    server.connect().await.unwrap();
                    let offer = read_offer(&mut server).await.unwrap();
                    assert_eq!((offer.layout, offer.zone), (42, Zone::Right));
                    assert!(matches!(offer.pane, data::Pane::Starter { .. }));
                    acknowledge(&mut server, true).await.unwrap();
                })
                .await
                .unwrap();
            });
    }

    #[test]
    fn docking_pipe_transfers_between_processes_and_returns_acknowledgement() {
        use std::{
            io::{BufRead, BufReader},
            process::{Command, Stdio},
        };

        let cookie = stamp(uuid::Uuid::new_v4());
        let test_name = concat!(module_path!(), "::docking_pipe_child");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                test_name.split_once("::").unwrap().1,
                "--exact",
                "--nocapture",
            ])
            .env(CHILD_COOKIE, cookie.to_string())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let ready = (&mut stdout)
            .lines()
            .any(|line| line.unwrap().ends_with("dock-peer-ready"));
        assert!(ready, "the receiving test process did not start listening");

        let remote = Remote {
            endpoint: Endpoint {
                pid: child.id(),
                cookie,
            },
            layout: 42,
        };
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(send(remote, Zone::Right, data::Pane::default()));
        assert!(child.wait().unwrap().success());
        assert!(result.unwrap());
    }

    #[test]
    fn docking_peer_roundtrips_only_a_single_pane() {
        let pane = data::Pane::default();
        let offer = Offer {
            layout: 42,
            zone: Zone::Left,
            pane: pane.clone(),
        };
        let received = decode(&serde_json::to_vec(&offer).unwrap()).unwrap();
        assert_eq!(received.layout, 42);
        assert_eq!(received.zone, Zone::Left);
        let split = Offer {
            pane: data::Pane::Split {
                axis: data::layout::pane::Axis::Vertical,
                ratio: 0.5,
                a: Box::new(pane.clone()),
                b: Box::new(pane),
            },
            ..offer
        };
        assert!(decode(&serde_json::to_vec(&split).unwrap()).is_err());
    }
}
