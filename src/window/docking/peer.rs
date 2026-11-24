//! Discovery and transfer between Windows processes in the desktop session.

use std::{
    collections::HashSet,
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
const COOKIE: *const u16 = w!("Flowdepth.Dock.v2.Cookie");
const FIELDS: [*const u16; 9] = [
    w!("Flowdepth.Dock.v2.Layout"),
    w!("Flowdepth.Dock.v2.X"),
    w!("Flowdepth.Dock.v2.Y"),
    w!("Flowdepth.Dock.v2.Width"),
    w!("Flowdepth.Dock.v2.Height"),
    w!("Flowdepth.Dock.v2.MinimumWidth"),
    w!("Flowdepth.Dock.v2.MinimumHeight"),
    w!("Flowdepth.Dock.v2.Zoom"),
    w!("Flowdepth.Dock.v2.Accent"),
];

static READY: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
            r"\\.\pipe\Flowdepth.Dock.v2.{}.{:08x}",
            self.pid, self.cookie
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

fn zoom_decode(bits: u32) -> f64 {
    f64::from(f32::from_bits(bits))
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
        zoom_encode(advert.zoom),
        advert.accent + 1,
    ];
    unsafe {
        // Publish the pipe last so readers can ignore an incomplete surface.
        RemovePropW(hwnd, COOKIE);
        FIELDS
            .iter()
            .zip(values)
            .all(|(&key, value)| SetPropW(hwnd, key, value as usize as HANDLE) != 0)
            && SetPropW(hwnd, COOKIE, Endpoint::local().cookie as usize as HANDLE) != 0
    }
}

pub(super) fn unpublish(hwnd: HWND) {
    unsafe {
        RemovePropW(hwnd, COOKIE);
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
            zoom: zoom_decode(zoom),
            accent: accent - 1,
        },
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Offer {
    pub id: [u8; 16],
    pub layout: u32,
    pub zone: Zone,
    pub pane: data::Pane,
}

impl Offer {
    pub fn new(remote: Remote, zone: Zone, pane: data::Pane) -> Self {
        Self {
            id: *uuid::Uuid::new_v4().as_bytes(),
            layout: remote.layout,
            zone,
            pane,
        }
    }
}

#[derive(Default)]
pub struct Receipts(HashSet<[u8; 16]>);

pub enum Receipt<T> {
    AlreadyAccepted,
    Imported(T),
    Rejected,
}

impl Receipts {
    pub fn import<T>(&mut self, id: [u8; 16], import: impl FnOnce() -> Option<T>) -> Receipt<T> {
        if self.0.contains(&id) {
            return Receipt::AlreadyAccepted;
        }
        let Some(value) = import() else {
            return Receipt::Rejected;
        };
        self.0.insert(id);
        Receipt::Imported(value)
    }
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
            let pipe = Endpoint::local().pipe();
            let mut server = match options.create(&pipe) {
                Ok(server) => server,
                Err(error) => {
                    log::warn!("WINDOW DockPeerUnavailable | error={error}");
                    return;
                }
            };
            options.first_pipe_instance(false);
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
                // Keep the endpoint reserved and give each connection a fresh
                // Tokio pipe handle, including after a broken acknowledgement.
                let next = match options.create(&pipe) {
                    Ok(next) => next,
                    Err(error) => {
                        log::warn!("WINDOW DockPeerUnavailable | error={error}");
                        break;
                    }
                };
                let mut connected = std::mem::replace(&mut server, next);
                let _ = timeout(TRANSFER_TIMEOUT, async {
                    let offer = read_offer(&mut connected).await?;
                    let (reply, accepted) = oneshot::channel();
                    output
                        .send(Incoming {
                            offer: Arc::new(offer),
                            reply: Arc::new(Mutex::new(Some(reply))),
                        })
                        .await
                        .map_err(io::Error::other)?;
                    acknowledge(&mut connected, accepted.await.unwrap_or(false)).await
                })
                .await;
            }
        })
    })
}

async fn acknowledge(server: &mut NamedPipeServer, accepted: bool) -> io::Result<()> {
    server.write_u8(u8::from(accepted)).await?;
    // The receipt confirms that the client read the response before the
    // server closes this connection.
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

pub async fn send(remote: Remote, offer: Arc<Offer>) -> io::Result<bool> {
    timeout(TRANSFER_TIMEOUT, async {
        let bytes = serde_json::to_vec(offer.as_ref())?;
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
    fn docking_advert_preserves_fractional_zoom() {
        for zoom in [0.9_f32, 1.0, 1.1, 1.2] {
            let zoom = f64::from(zoom);
            assert_eq!(zoom_decode(zoom_encode(zoom)), zoom);
        }
        let zoom = f64::from(0.9_f32);
        let legacy = fixed_decode(fixed_encode(zoom));
        let dashboard_right = fixed_decode(fixed_encode(1920.0 / zoom - 8.0));
        assert!(dashboard_right > 1920.0 / legacy + 1.0);
        assert!(dashboard_right <= 1920.0 / zoom_decode(zoom_encode(zoom)) + 1.0);
    }

    #[test]
    fn docking_retry_returns_committed_acceptance_without_importing_again() {
        let mut receipts = Receipts::default();
        let id = *uuid::Uuid::new_v4().as_bytes();

        assert!(matches!(
            receipts.import::<()>(id, || None),
            Receipt::Rejected
        ));
        assert!(matches!(
            receipts.import(id, || Some(())),
            Receipt::Imported(())
        ));
        assert!(matches!(
            receipts.import::<()>(id, || panic!("an accepted offer must not import again")),
            Receipt::AlreadyAccepted
        ));
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
                    let mut receipts = Receipts::default();
                    assert!(matches!(
                        receipts.import(offer.id, || Some(())),
                        Receipt::Imported(())
                    ));
                    // The receiver committed the import, but the acknowledgement was lost.
                    let mut next = ServerOptions::new()
                        .reject_remote_clients(true)
                        .create(endpoint.pipe())
                        .unwrap();
                    drop(server);
                    next.connect().await.unwrap();
                    let retry = read_offer(&mut next).await.unwrap();
                    assert_eq!(retry.id, offer.id);
                    assert!(matches!(
                        receipts
                            .import::<()>(retry.id, || panic!("retry attempted a second import")),
                        Receipt::AlreadyAccepted
                    ));
                    acknowledge(&mut next, true).await.unwrap();
                })
                .await
                .unwrap();
            });
    }

    #[test]
    fn docking_pipe_retries_across_processes_after_acknowledgement_loss() {
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
            .block_on(async {
                let offer = Arc::new(Offer::new(remote, Zone::Right, data::Pane::default()));
                assert!(send(remote, Arc::clone(&offer)).await.is_err());
                send(remote, offer).await
            });
        assert!(child.wait().unwrap().success());
        assert!(result.unwrap());
    }

    #[test]
    fn docking_peer_roundtrips_only_a_single_pane() {
        let pane = data::Pane::default();
        let offer = Offer {
            id: *uuid::Uuid::new_v4().as_bytes(),
            layout: 42,
            zone: Zone::Left,
            pane: pane.clone(),
        };
        let received = decode(&serde_json::to_vec(&offer).unwrap()).unwrap();
        assert_eq!(received.id, offer.id);
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
