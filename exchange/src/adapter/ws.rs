use crate::adapter::{AdapterError, Event, StreamKind};
use crate::proxy::{Proxy, ProxyStream};
use crate::unit::qty::QtyNormalization;
use crate::{Ticker, TickerInfo, Trade, UnixMs};

use bytes::Bytes;
use fastwebsockets::{FragmentCollectorRead, Frame, OpCode, Payload, WebSocket, WebSocketError};
use http_body_util::Empty;
use hyper::{
    Request,
    header::{CONNECTION, UPGRADE},
    upgrade::Upgraded,
};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustc_hash::FxHashMap;
use tokio::time::Instant;
use tokio_rustls::{
    TlsConnector,
    rustls::{ClientConfig, OwnedTrustAnchor},
};
use url::Url;

use futures::StreamExt;
#[cfg(not(feature = "unbounded-channel"))]
use futures::channel::mpsc::Sender;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

const HEARTBEAT_SEND_FAILED_REASON: &str = "Failed to send heartbeat ping";
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const WS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);

pub(super) const TCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

const MAX_DRAIN_PER_TICK: usize = 256;

pub(super) static TLS_CONNECTOR: LazyLock<TlsConnector> = LazyLock::new(|| {
    let mut root_store = tokio_rustls::rustls::RootCertStore::empty();

    root_store.add_trust_anchors(webpki_roots::TLS_SERVER_ROOTS.0.iter().map(|ta| {
        OwnedTrustAnchor::from_subject_spki_name_constraints(
            ta.subject,
            ta.spki,
            ta.name_constraints,
        )
    }));

    let config = ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    TlsConnector::from(Arc::new(config))
});

enum AnySender<T> {
    #[cfg(not(feature = "unbounded-channel"))]
    Bounded(Sender<T>),
    Unbounded(UnboundedSender<T>),
}

impl<T> AnySender<T> {
    fn send(&mut self, item: T) -> Result<(), futures::channel::mpsc::TrySendError<T>> {
        match self {
            #[cfg(not(feature = "unbounded-channel"))]
            AnySender::Bounded(tx) => tx.try_send(item),
            AnySender::Unbounded(tx) => tx.unbounded_send(item),
        }
    }
}

enum AnyReceiver<T> {
    #[cfg(not(feature = "unbounded-channel"))]
    Bounded(futures::channel::mpsc::Receiver<T>),
    Unbounded(UnboundedReceiver<T>),
}

impl<T> futures::Stream for AnyReceiver<T> {
    type Item = T;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match self.get_mut() {
            #[cfg(not(feature = "unbounded-channel"))]
            AnyReceiver::Bounded(rx) => std::pin::Pin::new(rx).poll_next(cx),
            AnyReceiver::Unbounded(rx) => std::pin::Pin::new(rx).poll_next(cx),
        }
    }
}

impl<T> AnyReceiver<T> {
    fn try_recv(&mut self) -> Option<T> {
        match self {
            #[cfg(not(feature = "unbounded-channel"))]
            AnyReceiver::Bounded(rx) => rx.try_recv().ok(),
            AnyReceiver::Unbounded(rx) => rx.try_recv().ok(),
        }
    }
}

fn channel<T>(_capacity: usize) -> (AnySender<T>, AnyReceiver<T>) {
    #[cfg(not(feature = "unbounded-channel"))]
    {
        let (tx, rx) = futures::channel::mpsc::channel(_capacity);
        (AnySender::Bounded(tx), AnyReceiver::Bounded(rx))
    }
    #[cfg(feature = "unbounded-channel")]
    {
        let (tx, rx) = futures::channel::mpsc::unbounded();
        (AnySender::Unbounded(tx), AnyReceiver::Unbounded(rx))
    }
}

pub(super) struct ChannelStream<T> {
    receiver: AnyReceiver<T>,
    task: tokio::task::JoinHandle<()>,
}

impl<T> futures::Stream for ChannelStream<T> {
    type Item = T;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::pin::Pin::new(&mut self.get_mut().receiver).poll_next(cx)
    }
}

impl<T> Drop for ChannelStream<T> {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Clone, Debug)]
pub(super) struct WsSession {
    ping_payload: PingPayload,
    streams: Arc<[StreamKind]>,
}

/// How often [`on_tick`](WsAdapter::on_tick) fires. Also serves as the time-bucket
/// granularity for trade aggregation — trades within one interval are collapsed
/// into a single [`Event::TradesReceived`].
const ADAPTER_TICK_INTERVAL: Duration = Duration::from_micros(33_333);

pub(super) trait WsAdapter {
    /// Connects to the WebSocket and returns a transport for it.
    /// This will be retried indefinitely until it succeeds, with an exponential backoff
    /// between attempts (base ~500ms, doubling, capped at 30s, with jitter).
    fn connect(&mut self) -> impl std::future::Future<Output = Result<WsTransport, String>> + Send;

    /// Tick interval controlling how often [`on_tick`](WsAdapter::on_tick) is called
    /// and the time-bucket granularity for trade aggregation.
    fn tick_interval(&self) -> Duration {
        ADAPTER_TICK_INTERVAL
    }

    /// Called when a connection is established.
    /// This is called on every successful connection, including after reconnects.
    fn on_connected(&mut self) -> impl std::future::Future<Output = Vec<Event>> + Send;

    /// Called periodically while connected, at the cadence returned by
    /// [`tick_interval`].
    ///
    /// This is where **trade adapters** flush their [`TradeBuffer`] — trades are
    /// batched across one tick interval and emerge as a single
    /// [`Event::TradesReceived`]. Non-trade adapters leave this as the default
    /// no-op because they push events directly in [`on_text`](Self::on_text).
    fn on_tick(&mut self) -> impl std::future::Future<Output = Vec<Event>> + Send {
        async { Vec::new() }
    }

    /// Called when a text message is received.
    ///
    /// Adapters parse incoming data and return resulting `Event`s.
    /// The session loop sends them to the output channel.
    /// If the output channel is full, events are silently dropped
    ///
    /// **Flush model**: non-trade adapters return events here directly.
    /// Trade adapters only buffer into [`TradeBuffer`] here and return
    /// events later in [`on_tick`](Self::on_tick).
    fn on_text(
        &mut self,
        payload: &[u8],
    ) -> impl std::future::Future<Output = Result<Vec<Event>, String>> + Send;

    /// Called when the connection is closed or a fatal error occurs.
    fn on_disconnected(
        &mut self,
        reason: &str,
    ) -> impl std::future::Future<Output = Vec<Event>> + Send;
}

impl WsSession {
    pub(super) fn with_text_ping(ping_payload: &'static [u8], streams: Arc<[StreamKind]>) -> Self {
        Self {
            ping_payload: PingPayload::Text(ping_payload),
            streams,
        }
    }

    pub(super) fn with_opcode_ping(
        ping_payload: &'static [u8],
        streams: Arc<[StreamKind]>,
    ) -> Self {
        Self {
            ping_payload: PingPayload::OpCode(ping_payload),
            streams,
        }
    }

    pub(super) fn run<A: WsAdapter + Send + 'static>(self, mut adapter: A) -> ChannelStream<Event> {
        let (mut event_tx, event_rx) = channel(512);

        let ping_payload = self.ping_payload;
        let streams = Arc::clone(&self.streams);

        let task = tokio::spawn(async move {
            if streams.is_empty() {
                let _ = event_tx.send(Event::Disconnected(
                    streams,
                    "Empty stream payload".to_string(),
                ));
                return;
            }

            let mut backoff = ReconnectBackoff::new();
            let mut disconnect_count: u32 = 0;
            let mut last_disconnect_instant: Option<std::time::Instant> = None;
            let mut attempt: u32 = 0;

            loop {
                attempt += 1;
                log::info!(
                    "WS ReconnectAttempt | attempt={attempt} backoff_next={:?}",
                    backoff.peek_delay()
                );

                let transport = match adapter.connect().await {
                    Ok(t) => t,
                    Err(reason) => {
                        disconnect_count += 1;
                        log::warn!(
                            "WS ConnectFailed | reason={reason} count={disconnect_count} backoff_next={:?}",
                            backoff.peek_delay()
                        );
                        let _ = event_tx.send(Event::Disconnected(Arc::clone(&streams), reason));
                        let delay = backoff.delay();
                        backoff.record_failure();
                        log::info!("WS ReconnectScheduled | attempt={attempt} delay={delay:?}");
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                };

                let (frame_tx, mut frame_rx) = {
                    let (tx, rx) = futures::channel::mpsc::unbounded();
                    (AnySender::Unbounded(tx), AnyReceiver::Unbounded(rx))
                };
                let io_handle = tokio::spawn(transport.read_frame(ping_payload, frame_tx));

                for event in adapter.on_connected().await {
                    let _ = event_tx.send(event);
                }
                let _ = event_tx.send(Event::Connected(Arc::clone(&streams)));

                let tick_interval = adapter.tick_interval();
                let tick_sleep = tokio::time::sleep(tick_interval);
                tokio::pin!(tick_sleep);

                // Track connection stability for backoff reset.
                let connected_at = std::time::Instant::now();
                let stability_timer = tokio::time::sleep(ReconnectBackoff::STABLE_THRESHOLD);
                tokio::pin!(stability_timer);
                let mut is_stable = false;
                let mut data_received = false;

                let disconnect_reason = loop {
                    tokio::select! {
                        biased;
                        // Raw trade streams can remain continuously ready on active symbols.
                        // Service the batching timer first so detector traffic cannot starve
                        // legacy TradesReceived events used by bubbles, footprint and CVD.
                        _ = &mut tick_sleep => {
                            for event in adapter.on_tick().await {
                                let _ = event_tx.send(event);
                            }
                            tick_sleep
                                .as_mut()
                                .reset(tokio::time::Instant::now() + tick_interval);
                        }
                        frame = frame_rx.next() => {
                            let mut disconnect_reason: Option<String> = None;

                            match frame {
                                Some(Ok(payload)) => {
                                    if !payload.is_empty() {
                                        match adapter.on_text(&payload).await {
                                            Ok(events) => {
                                                let had_events = !events.is_empty();
                                                for event in events {
                                                    let _ = event_tx.send(event);
                                                }
                                                if had_events {
                                                    data_received = true;
                                                    backoff.record_success();
                                                }
                                            }
                                            Err(reason) => {
                                                disconnect_reason = Some(reason);
                                            }
                                        }
                                    }

                                    if disconnect_reason.is_none() {
                                        let mut drained = 0;
                                        while drained < MAX_DRAIN_PER_TICK {
                                            let Some(drain) = frame_rx.try_recv() else { break };
                                            drained += 1;
                                            match drain {
                                                Ok(payload) => {
                                                    match adapter.on_text(&payload).await {
                                                        Ok(events) => {
                                                            let had_events = !events.is_empty();
                                                            for event in events {
                                                                let _ = event_tx.send(event);
                                                            }
                                                            if had_events {
                                                                data_received = true;
                                                                backoff.record_success();
                                                            }
                                                        }
                                                        Err(reason) => {
                                                            disconnect_reason = Some(reason);
                                                            break;
                                                        }
                                                    }
                                                }
                                                Err(reason) => {
                                                    disconnect_reason = Some(reason);
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                }
                                Some(Err(reason)) => {
                                    disconnect_reason = Some(reason);
                                }
                                None => {
                                    disconnect_reason = Some(
                                        "I/O task exited".to_string(),
                                    );
                                }
                            }

                            if let Some(reason) = disconnect_reason {
                                break Some(reason);
                            }
                        }
                        _ = &mut stability_timer, if !is_stable => {
                            is_stable = true;
                            let connected_for = connected_at.elapsed();
                            log::info!(
                                "WS Stable | connected_for={connected_for:?}"
                            );
                            if backoff.is_inflated() {
                                log::info!(
                                    "WS BackoffReset | reason=connected_stable connected_for={connected_for:?}"
                                );
                                backoff.record_success();
                            }
                        }
                    }
                };

                if io_handle.is_finished() {
                    if let Err(e) = io_handle.await {
                        log::error!("WebSocket I/O task panicked (reconnecting): {e}");
                    }
                } else {
                    io_handle.abort();
                }

                if let Some(reason) = disconnect_reason {
                    disconnect_count += 1;

                    // Only inflate backoff for genuine failures (no data received).
                    // Stable connections that received data before dropping should
                    // not increase the reconnection delay.
                    if !data_received {
                        backoff.record_failure();
                    }

                    let since_last_str = match last_disconnect_instant {
                        Some(t) => format!("{:?}", t.elapsed()),
                        None => "none".to_string(),
                    };
                    last_disconnect_instant = Some(std::time::Instant::now());

                    log::warn!(
                        "WS ReconnectDiag | reason={reason} count={disconnect_count} since_last={since_last_str} backoff_next={:?}",
                        backoff.peek_delay()
                    );

                    for event in adapter.on_disconnected(&reason).await {
                        let _ = event_tx.send(event);
                    }
                    let _ = event_tx.send(Event::Disconnected(Arc::clone(&streams), reason));
                }

                let delay = backoff.delay();
                log::info!("WS ReconnectScheduled | attempt={attempt} delay={delay:?}");
                tokio::time::sleep(delay).await;
            }
        });

        ChannelStream {
            receiver: event_rx,
            task,
        }
    }
}

pub(super) struct WsTransport(WebSocket<TokioIo<Upgraded>>);

enum ReaderEvent {
    Frame { opcode: OpCode, payload: Vec<u8> },
    Write(Frame<'static>),
    Error(String),
}

impl WsTransport {
    pub(super) async fn write_frame(&mut self, frame: Frame<'_>) -> Result<(), WebSocketError> {
        self.0.write_frame(frame).await
    }

    /// Reads frames, handles heartbeat and Ping/Pong at transport level,
    /// forwards text frames to the processor task.
    async fn read_frame(
        self,
        ping_payload: PingPayload,
        mut frame_tx: AnySender<Result<Vec<u8>, String>>,
    ) {
        // `fastwebsockets::read_frame` is not cancellation-safe: it may have
        // consumed part of a frame when its future is dropped. Wrapping it in
        // `timeout` used to cancel an in-progress read every heartbeat tick,
        // desynchronizing the parser under busy depth/trade streams. The next
        // payload byte was then interpreted as a frame header and commonly
        // failed with "Reserved bits are not zero".
        //
        // Keep the reader future in its own task for the whole connection and
        // use the split write half for heartbeat and protocol replies.
        let (read_half, mut write_half) = self.0.split(tokio::io::split);
        let mut reader = FragmentCollectorRead::new(read_half);
        let (reader_tx, mut reader_rx) = tokio::sync::mpsc::unbounded_channel();
        let reader_task = tokio::spawn(async move {
            loop {
                let obligated_tx = reader_tx.clone();
                let result = reader
                    .read_frame(&mut move |frame| {
                        let tx = obligated_tx.clone();
                        let owned = Frame::new(
                            frame.fin,
                            frame.opcode,
                            None,
                            Payload::Owned(frame.payload.to_vec()),
                        );
                        async move {
                            tx.send(ReaderEvent::Write(owned)).map_err(|_| {
                                std::io::Error::new(
                                    std::io::ErrorKind::BrokenPipe,
                                    "websocket writer task exited",
                                )
                            })
                        }
                    })
                    .await;

                match result {
                    Ok(frame) => {
                        if reader_tx
                            .send(ReaderEvent::Frame {
                                opcode: frame.opcode,
                                payload: frame.payload.to_vec(),
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = reader_tx
                            .send(ReaderEvent::Error(format!("Error reading frame: {error}")));
                        break;
                    }
                }
            }
        });

        let heartbeat = WsHeartbeat::default();
        let mut heartbeat_interval =
            tokio::time::interval_at(Instant::now() + heartbeat.interval, heartbeat.interval);
        heartbeat_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let heartbeat_deadline = tokio::time::sleep(heartbeat.timeout);
        tokio::pin!(heartbeat_deadline);

        loop {
            tokio::select! {
                biased;
                event = reader_rx.recv() => {
                    let Some(event) = event else {
                        let _ = frame_tx.send(Err("I/O task exited".into()));
                        break;
                    };

                    match event {
                        ReaderEvent::Frame { opcode, payload } => {
                            heartbeat_deadline
                                .as_mut()
                                .reset(Instant::now() + heartbeat.timeout);

                            match opcode {
                                OpCode::Text => {
                                    if frame_tx.send(Ok(payload)).is_err() {
                                        break;
                                    }
                                }
                                OpCode::Ping => {
                                    // `FragmentCollectorRead` has already queued the
                                    // protocol-mandated pong through `ReaderEvent::Write`.
                                    let _ = frame_tx.send(Ok(Vec::new()));
                                }
                                OpCode::Close => {
                                    let _ = frame_tx.send(Err("Connection closed".into()));
                                    break;
                                }
                                _ => {}
                            }
                        }
                        ReaderEvent::Write(frame) => {
                            if let Err(error) = write_half.write_frame(frame).await {
                                let _ = frame_tx.send(Err(format!(
                                    "Failed to write websocket control frame: {error}"
                                )));
                                break;
                            }
                        }
                        ReaderEvent::Error(error) => {
                            let _ = frame_tx.send(Err(error));
                            break;
                        }
                    }
                }
                _ = heartbeat_interval.tick() => {
                    let frame = match ping_payload {
                        PingPayload::Text(payload) => Frame::text(Payload::Borrowed(payload)),
                        PingPayload::OpCode(payload) => {
                            Frame::new(true, OpCode::Ping, None, Payload::Borrowed(payload))
                        }
                    };
                    if write_half.write_frame(frame).await.is_err() {
                        let _ = frame_tx.send(Err(HEARTBEAT_SEND_FAILED_REASON.into()));
                        break;
                    }
                }
                _ = &mut heartbeat_deadline => {
                    let _ = frame_tx.send(Err("Heartbeat timeout (no websocket activity)".into()));
                    break;
                }
            }
        }

        reader_task.abort();
    }

    pub(super) async fn establish(
        domain: &str,
        url: &str,
        proxy_cfg: Option<&Proxy>,
    ) -> Result<Self, AdapterError> {
        let parsed = Url::parse(url).map_err(|e| AdapterError::InvalidRequest(e.to_string()))?;

        let url_host = parsed.host_str().ok_or_else(|| {
            AdapterError::InvalidRequest("Missing host in websocket URL".to_string())
        })?;

        if !url_host.eq_ignore_ascii_case(domain) {
            return Err(AdapterError::InvalidRequest(format!(
                "WebSocket URL host mismatch: url_host={url_host}, domain_arg={domain}"
            )));
        }

        let target_port = parsed.port_or_known_default().ok_or_else(|| {
            AdapterError::InvalidRequest("Missing port for websocket URL".to_string())
        })?;

        let tcp_stream = ProxyStream::connect_tcp(domain, target_port, proxy_cfg).await?;

        match parsed.scheme() {
            "wss" => {
                let tls_stream = tokio::time::timeout(
                    TLS_HANDSHAKE_TIMEOUT,
                    Self::upgrade_to_tls(tcp_stream, domain),
                )
                .await
                .map_err(|_| {
                    AdapterError::WebsocketError("TLS handshake to target timed out".to_string())
                })??;

                tokio::time::timeout(
                    WS_HANDSHAKE_TIMEOUT,
                    Self::handshake_tls(tls_stream, domain, &parsed),
                )
                .await
                .map_err(|_| {
                    AdapterError::WebsocketError("WebSocket handshake timed out".to_string())
                })?
            }
            "ws" => tokio::time::timeout(
                WS_HANDSHAKE_TIMEOUT,
                Self::handshake_tcp(tcp_stream, domain, &parsed),
            )
            .await
            .map_err(|_| {
                AdapterError::WebsocketError("WebSocket handshake timed out".to_string())
            })?,
            _ => Err(AdapterError::InvalidRequest(
                "Invalid scheme for websocket URL".to_string(),
            )),
        }
    }

    async fn upgrade_to_tls(
        stream: ProxyStream,
        domain: &str,
    ) -> Result<Box<tokio_rustls::client::TlsStream<ProxyStream>>, AdapterError> {
        let server_name = tokio_rustls::rustls::ServerName::try_from(domain)
            .map_err(|_| AdapterError::ParseError("invalid dnsname".to_string()))?;

        let tls_stream = TLS_CONNECTOR
            .connect(server_name, stream)
            .await
            .map_err(|e| AdapterError::WebsocketError(e.to_string()))?;

        Ok(Box::new(tls_stream))
    }

    async fn handshake_tcp(
        stream: ProxyStream,
        domain: &str,
        parsed: &Url,
    ) -> Result<Self, AdapterError> {
        let req = Self::build_ws_request(domain, parsed)?;
        let exec = TokioExecutor::new();
        let (ws, _http_resp) = fastwebsockets::handshake::client(&exec, req, stream)
            .await
            .map_err(|e| AdapterError::WebsocketError(e.to_string()))?;
        Ok(Self(ws))
    }

    async fn handshake_tls(
        tls: Box<tokio_rustls::client::TlsStream<ProxyStream>>,
        domain: &str,
        parsed: &Url,
    ) -> Result<Self, AdapterError> {
        let req = Self::build_ws_request(domain, parsed)?;
        let exec = TokioExecutor::new();
        let (ws, _http_resp) = fastwebsockets::handshake::client(&exec, req, tls)
            .await
            .map_err(|e| AdapterError::WebsocketError(e.to_string()))?;
        Ok(Self(ws))
    }

    fn build_ws_request(domain: &str, parsed: &Url) -> Result<Request<Empty<Bytes>>, AdapterError> {
        let mut path_and_query = parsed.path().to_string();
        if let Some(q) = parsed.query() {
            path_and_query.push('?');
            path_and_query.push_str(q);
        }
        if path_and_query.is_empty() {
            path_and_query.push('/');
        }

        let host_header = match parsed.port() {
            Some(explicit_port) => {
                let default_port = parsed.port_or_known_default().unwrap_or(explicit_port);
                if explicit_port != default_port {
                    format!("{domain}:{explicit_port}")
                } else {
                    domain.to_string()
                }
            }
            None => domain.to_string(),
        };

        Request::builder()
            .method("GET")
            .uri(path_and_query)
            .header("Host", host_header)
            .header(UPGRADE, "websocket")
            .header(CONNECTION, "upgrade")
            .header(
                "Sec-WebSocket-Key",
                fastwebsockets::handshake::generate_key(),
            )
            .header("Sec-WebSocket-Version", "13")
            .body(Empty::<Bytes>::new())
            .map_err(|e| AdapterError::WebsocketError(e.to_string()))
    }
}

#[derive(Clone, Copy, Debug)]
enum PingPayload {
    Text(&'static [u8]),
    OpCode(&'static [u8]),
}

struct WsHeartbeat {
    interval: Duration,
    timeout: Duration,
}

impl WsHeartbeat {
    const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
    const DEFAULT_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(45);

    fn new(interval: Duration, timeout: Duration) -> Self {
        Self { interval, timeout }
    }
}

impl Default for WsHeartbeat {
    fn default() -> Self {
        Self::new(
            Self::DEFAULT_HEARTBEAT_INTERVAL,
            Self::DEFAULT_HEARTBEAT_TIMEOUT,
        )
    }
}

/// Exponential backoff for WebSocket reconnection attempts.
///
/// Delay doubles on each failure, resets to the initial 500ms on success.
/// Capped at 30s with ±25% multiplicative jitter to spread reconnections
/// across streams when multiple disconnect at once.
struct ReconnectBackoff {
    current: Duration,
}

impl ReconnectBackoff {
    const INITIAL: Duration = Duration::from_millis(500);
    const MAX: Duration = Duration::from_secs(30);
    const JITTER: f32 = 0.25;
    /// How long a connection must remain alive before backoff is reset.
    const STABLE_THRESHOLD: Duration = Duration::from_secs(30);

    fn new() -> Self {
        Self {
            current: Self::INITIAL,
        }
    }

    /// Returns the delay before the next reconnect attempt, with ±jitter applied.
    fn delay(&self) -> Duration {
        let factor = 1.0 + (rand::random::<f32>() * 2.0 - 1.0) * Self::JITTER;
        let secs = self.current.as_secs_f32() * factor;
        Duration::from_secs_f32(secs.max(0.0)).min(Self::MAX)
    }

    /// Doubles the delay (capped) after a failed attempt.
    fn record_failure(&mut self) {
        self.current = (self.current.mul_f32(2.0)).min(Self::MAX);
    }

    /// Resets the delay to the initial value after genuine success
    /// (real market-data events were produced by the connection).
    fn record_success(&mut self) {
        self.current = Self::INITIAL;
    }

    /// Returns the current delay without jitter, for diagnostic logging.
    fn peek_delay(&self) -> Duration {
        self.current.min(Self::MAX)
    }

    /// Returns true if the backoff has been inflated above the initial value.
    fn is_inflated(&self) -> bool {
        self.current > Self::INITIAL
    }
}

pub(super) struct TradeBuffer {
    buffer_map: FxHashMap<Ticker, Vec<Trade>>,
    ticker_info_map: FxHashMap<Ticker, (TickerInfo, QtyNormalization)>,
}

impl TradeBuffer {
    pub(super) fn new(ticker_info_map: FxHashMap<Ticker, (TickerInfo, QtyNormalization)>) -> Self {
        Self {
            buffer_map: FxHashMap::default(),
            ticker_info_map,
        }
    }

    pub(super) fn ticker_info(&self, ticker: &Ticker) -> Option<&(TickerInfo, QtyNormalization)> {
        self.ticker_info_map.get(ticker)
    }

    pub(super) fn push(&mut self, ticker: Ticker, trade: Trade) {
        self.buffer_map.entry(ticker).or_default().push(trade);
    }

    pub(super) fn ticker_infos(&self) -> impl Iterator<Item = TickerInfo> + '_ {
        self.ticker_info_map.values().map(|(info, _)| *info)
    }

    /// Drain all buffered trades, clearing internal buffers.
    ///
    /// Each ticker's trades are collapsed into a single [`Event::TradesReceived`]
    /// keyed by the most recent trade's time rounded down to the nearest
    /// [`ADAPTER_TICK_INTERVAL`] bucket.
    pub(super) fn flush(&mut self) -> Vec<Event> {
        let interval_ms = ADAPTER_TICK_INTERVAL.as_millis() as u64;
        let mut events = Vec::new();

        for (ticker, trades_buffer) in self.buffer_map.iter_mut() {
            if trades_buffer.is_empty() {
                continue;
            }

            let bucket_update_t = trades_buffer
                .iter()
                .map(|t| t.time.as_u64())
                .max()
                .map(|t| UnixMs::new((t / interval_ms) * interval_ms));

            if let Some((ticker_info, _)) = self.ticker_info_map.get(ticker)
                && let Some(update_t) = bucket_update_t
            {
                events.push(Event::TradesReceived(
                    StreamKind::Trades {
                        ticker_info: *ticker_info,
                    },
                    update_t,
                    std::mem::take(trades_buffer).into_boxed_slice(),
                ));
            }
        }

        events
    }
}
