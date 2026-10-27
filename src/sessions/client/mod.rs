//! Sync client for the session protocol v1 WebSocket endpoint.
//!
//! The TUI is synchronous, so every connection gets a background thread with
//! its own single-threaded tokio runtime: [`HubClient`] carries control
//! traffic (`list`/`spawn`/`kill` plus hub events) and one [`SessionStream`]
//! per attached session carries output. Writes are fire-and-forget; reads
//! are non-blocking polls, except [`HubClient::request`], which waits for
//! the matching answer.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures::{SinkExt as _, StreamExt as _};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use super::protocol::{
    attach_frame, detach_frame, input_frame, kill_frame, parse_host_frame, prompt_frame,
    resize_frame, HostFrame, PROTO_VERSION,
};

#[cfg(test)]
mod tests;

/// How long [`HubClient::request`] and the stream handshake wait for the hub.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// [`HubClient::request`] poll slice: short enough to feel instant.
const REQUEST_POLL: Duration = Duration::from_millis(20);

/// The hub endpoint for a daemon on `port`.
pub fn ws_url(port: u16) -> String {
    format!("ws://127.0.0.1:{port}/sessions/ws")
}

/// The daemon bearer token, creating it when missing. Never logged.
pub fn read_token(data_dir: &Path) -> anyhow::Result<String> {
    super::auth::ensure_token(data_dir)
}

/// A fresh request id for a `list`/`spawn`/`prompt`/`kill` frame.
pub fn new_req() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// The notice shown when the hub speaks another protocol version.
pub fn proto_mismatch_message(proto: u32) -> String {
    format!(
        "canopy daemon speaks session protocol {proto}; restart the daemon (canopy daemon restart)"
    )
}

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Open an authenticated hub WebSocket.
async fn ws_connect(url: &str, token: &str) -> anyhow::Result<WsStream> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
    let mut request = url.into_client_request()?;
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {token}")
            .parse()
            .map_err(|e| anyhow::anyhow!("{e}"))?,
    );
    let (stream, _) = tokio_tungstenite::connect_async(request).await?;
    Ok(stream)
}

// ── HubClient ─────────────────────────────────────────────────────────

/// What [`HubClient::try_recv`] yields: a parsed host frame, or `Closed`
/// once the WebSocket drops. `Closed` repeats while disconnected.
#[derive(Debug)]
pub enum HubRecv {
    Frame(HostFrame),
    Closed,
}

struct Inbox {
    rx: mpsc::Receiver<HubRecv>,
    /// Frames `request` received while waiting for its own answer.
    pending: VecDeque<HostFrame>,
}

/// One control connection to the hub. [`Clone`] shares the connection:
/// clones read from the same inbox, so a control client held by both the
/// app poll and a request never loses a frame to the other, and closing any
/// clone closes the one WebSocket they all write to.
pub struct HubClient {
    tx: Arc<Mutex<Option<UnboundedSender<String>>>>,
    inbox: Arc<Mutex<Inbox>>,
    proto: u32,
}

impl Clone for HubClient {
    fn clone(&self) -> Self {
        Self {
            tx: Arc::clone(&self.tx),
            inbox: Arc::clone(&self.inbox),
            proto: self.proto,
        }
    }
}

impl HubClient {
    /// Connect and wait for `hello`; the hub's protocol version is
    /// available as [`Self::proto`] — the caller decides what a mismatch
    /// means.
    pub fn connect(url: &str, token: &str) -> anyhow::Result<Self> {
        let (out_tx, out_rx) = unbounded_channel();
        let (in_tx, in_rx) = mpsc::channel();
        let (url, token) = (url.to_string(), token.to_string());
        std::thread::spawn(move || run_control_connection(url, token, out_rx, in_tx));
        match in_rx.recv_timeout(REQUEST_TIMEOUT) {
            Ok(HubRecv::Frame(HostFrame::Hello { proto, host_label })) => {
                tracing::debug!("Connected to the session hub on {host_label}");
                Ok(Self {
                    tx: Arc::new(Mutex::new(Some(out_tx))),
                    inbox: Arc::new(Mutex::new(Inbox {
                        rx: in_rx,
                        pending: VecDeque::new(),
                    })),
                    proto,
                })
            }
            Ok(_) => Err(anyhow::anyhow!("hub did not greet with hello")),
            Err(_) => Err(anyhow::anyhow!("timed out waiting for the hub hello")),
        }
    }

    /// The hub's `hello.proto`.
    pub fn proto(&self) -> u32 {
        self.proto
    }

    /// Fire-and-forget send; silently dropped once closed.
    pub fn send(&self, frame: String) {
        let Ok(tx) = self.tx.lock() else {
            return;
        };
        if let Some(tx) = tx.as_ref() {
            let _ = tx.send(frame);
        }
    }

    /// Close the WebSocket; the background thread exits on its own.
    pub fn close(&self) {
        if let Ok(mut tx) = self.tx.lock() {
            tx.take();
        }
    }

    /// The next inbound frame, if any is already waiting.
    pub fn try_recv(&self) -> Option<HubRecv> {
        let mut inbox = self.inbox.lock().ok()?;
        if let Some(frame) = inbox.pending.pop_front() {
            return Some(HubRecv::Frame(frame));
        }
        match inbox.rx.try_recv() {
            Ok(event) => Some(event),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(HubRecv::Closed),
        }
    }

    /// Send `frame` and wait up to 5 s for the answer carrying `req`.
    /// Frames for other requests are stashed for [`Self::try_recv`]; an
    /// `error` answer for `req` is returned as `Err`.
    pub fn request(&self, req: &str, frame: String) -> anyhow::Result<HostFrame> {
        self.send(frame);
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        loop {
            if let Some(frame) = self.take_matching(req) {
                return check_response(frame);
            }
            if Instant::now() >= deadline {
                anyhow::bail!("hub request '{req}' timed out");
            }
            let event = self
                .inbox
                .lock()
                .map_err(|_| anyhow::anyhow!("hub inbox lock poisoned"))?
                .rx
                .recv_timeout(REQUEST_POLL);
            match event {
                Ok(HubRecv::Frame(frame)) => {
                    if frame.req() == Some(req) {
                        return check_response(frame);
                    }
                    if let Ok(mut inbox) = self.inbox.lock() {
                        inbox.pending.push_back(frame);
                    }
                }
                Ok(HubRecv::Closed) => {
                    anyhow::bail!("hub connection closed during request '{req}'");
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    anyhow::bail!("hub connection closed during request '{req}'");
                }
            }
        }
    }

    /// Take a stashed answer carrying `req`, if one is waiting.
    fn take_matching(&self, req: &str) -> Option<HostFrame> {
        let mut inbox = self.inbox.lock().ok()?;
        let pos = inbox.pending.iter().position(|f| f.req() == Some(req))?;
        inbox.pending.remove(pos)
    }
}

/// An `error` answer becomes `Err`; every other answer is returned as-is.
fn check_response(frame: HostFrame) -> anyhow::Result<HostFrame> {
    if let HostFrame::Error { code, message, .. } = &frame {
        anyhow::bail!("hub error {code}: {message}");
    }
    Ok(frame)
}

/// Background thread: connect, then pump frames both ways until either side
/// drops. Always ends the inbox with `Closed`.
fn run_control_connection(
    url: String,
    token: String,
    outbound: UnboundedReceiver<String>,
    inbound: mpsc::Sender<HubRecv>,
) {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        let _ = inbound.send(HubRecv::Closed);
        return;
    };
    rt.block_on(async move {
        let Ok(stream) = ws_connect(&url, &token).await else {
            let _ = inbound.send(HubRecv::Closed);
            return;
        };
        pump_control(stream, outbound, &inbound).await;
        let _ = inbound.send(HubRecv::Closed);
    });
}

async fn pump_control(
    stream: WsStream,
    mut outbound: UnboundedReceiver<String>,
    inbound: &mpsc::Sender<HubRecv>,
) {
    use tokio_tungstenite::tungstenite::Message;
    let (mut sink, mut ws) = stream.split();
    loop {
        tokio::select! {
            outgoing = outbound.recv() => {
                let Some(text) = outgoing else {
                    let _ = sink.send(Message::Close(None)).await;
                    break;
                };
                if sink.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            incoming = ws.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(frame) = parse_host_frame(&text) {
                            if inbound.send(HubRecv::Frame(frame)).is_err() {
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
        }
    }
}

// ── SessionStream ─────────────────────────────────────────────────────

/// Output events for one attached session.
#[derive(Debug)]
pub enum SessionEvent {
    /// A full screen capture; also re-sent when a lagged subscriber resyncs.
    Snapshot {
        cols: u16,
        rows: u16,
        seq: u64,
        data: Vec<u8>,
    },
    /// One output chunk after the snapshot's `seq`.
    Out { seq: u64, data: Vec<u8> },
    /// The child exited.
    Exited { code: i32 },
    /// The WebSocket dropped; the session keeps running in the hub.
    Closed,
}

/// Why [`SessionStream::open`] failed before the first snapshot.
#[derive(Debug)]
pub enum SessionOpenError {
    /// The hub speaks another protocol version.
    Proto(u32),
    /// The connection failed or went quiet.
    Transport(String),
}

impl std::fmt::Display for SessionOpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionOpenError::Proto(proto) => write!(f, "{}", proto_mismatch_message(*proto)),
            SessionOpenError::Transport(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for SessionOpenError {}

/// The write half of one attached session. [`Clone`] shares the stream;
/// dropping every handle detaches and closes the WebSocket.
#[derive(Debug, Clone)]
pub struct SessionSend {
    tx: UnboundedSender<String>,
    id: String,
    req_seq: Arc<AtomicU64>,
}

impl SessionSend {
    fn new(tx: UnboundedSender<String>, id: String) -> Self {
        Self {
            tx,
            id,
            req_seq: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Raw bytes for the session's PTY stdin.
    pub fn input(&self, data: &[u8]) {
        let _ = self.tx.send(input_frame(&self.id, data));
    }

    /// Resize the session's PTY and parser.
    pub fn resize(&self, cols: u16, rows: u16) {
        let _ = self.tx.send(resize_frame(&self.id, cols, rows));
    }

    /// One submitted prompt through the hub's paste path.
    pub fn prompt(&self, text: &str) {
        let req = self.next_req();
        let _ = self.tx.send(prompt_frame(&req, &self.id, text));
    }

    /// End the session.
    pub fn kill(&self) {
        let req = self.next_req();
        let _ = self.tx.send(kill_frame(&req, &self.id));
    }

    fn next_req(&self) -> String {
        format!("tui-{}", self.req_seq.fetch_add(1, Ordering::Relaxed))
    }
}

impl Drop for SessionSend {
    fn drop(&mut self) {
        let _ = self.tx.send(detach_frame(&self.id));
    }
}

/// One attached session: its own WebSocket plus `attach`, so tests need no
/// app loop.
pub struct SessionStream;

impl SessionStream {
    /// Attach to `id`: connect, check the hub's protocol, send `attach`,
    /// and return the write half plus the output events. The first event
    /// is the `Snapshot` unless the session is gone (`Closed`).
    pub fn open(
        url: &str,
        token: &str,
        id: &str,
    ) -> Result<(SessionSend, mpsc::Receiver<SessionEvent>), SessionOpenError> {
        let (out_tx, out_rx) = unbounded_channel();
        let (in_tx, in_rx) = mpsc::channel();
        let (hello_tx, hello_rx) = mpsc::channel();
        let (url, token, id) = (url.to_string(), token.to_string(), id.to_string());
        let send_id = id.clone();
        std::thread::spawn(move || run_session_connection(url, token, id, out_rx, in_tx, hello_tx));
        match hello_rx.recv_timeout(REQUEST_TIMEOUT) {
            Ok(Ok(proto)) if proto == PROTO_VERSION => {}
            Ok(Ok(proto)) => return Err(SessionOpenError::Proto(proto)),
            Ok(Err(message)) => return Err(SessionOpenError::Transport(message)),
            Err(_) => {
                return Err(SessionOpenError::Transport(
                    "timed out waiting for the hub hello".to_string(),
                ));
            }
        }
        Ok((SessionSend::new(out_tx, send_id), in_rx))
    }
}

/// Background thread: connect, report the `hello.proto` handshake, send
/// `attach`, then pump until either side drops.
fn run_session_connection(
    url: String,
    token: String,
    id: String,
    outbound: UnboundedReceiver<String>,
    inbound: mpsc::Sender<SessionEvent>,
    hello: mpsc::Sender<Result<u32, String>>,
) {
    use tokio_tungstenite::tungstenite::Message;
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let _ = hello.send(Err(e.to_string()));
            let _ = inbound.send(SessionEvent::Closed);
            return;
        }
    };
    rt.block_on(async move {
        let stream = match ws_connect(&url, &token).await {
            Ok(stream) => stream,
            Err(e) => {
                let _ = hello.send(Err(e.to_string()));
                let _ = inbound.send(SessionEvent::Closed);
                return;
            }
        };
        let (mut sink, mut ws) = stream.split();
        let proto = wait_for_hello(&mut ws, &hello, &inbound).await;
        let Some(proto) = proto else {
            return;
        };
        if hello.send(Ok(proto)).is_err() {
            return;
        }
        if proto != PROTO_VERSION {
            let _ = inbound.send(SessionEvent::Closed);
            return;
        }
        if sink
            .send(Message::Text(attach_frame(&id).into()))
            .await
            .is_err()
        {
            let _ = inbound.send(SessionEvent::Closed);
            return;
        }
        pump_session(&id, &mut sink, &mut ws, outbound, &inbound).await;
        let _ = inbound.send(SessionEvent::Closed);
    });
}

/// Read frames until `hello`; report transport failures and return `None`.
async fn wait_for_hello(
    ws: &mut futures::stream::SplitStream<WsStream>,
    hello: &mpsc::Sender<Result<u32, String>>,
    inbound: &mpsc::Sender<SessionEvent>,
) -> Option<u32> {
    use tokio_tungstenite::tungstenite::Message;
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(text))) => match parse_host_frame(&text) {
                Ok(HostFrame::Hello { proto, .. }) => return Some(proto),
                Ok(_) => {
                    let _ = hello.send(Err("hub did not greet with hello".to_string()));
                    let _ = inbound.send(SessionEvent::Closed);
                    return None;
                }
                Err(_) => {}
            },
            _ => {
                let _ = hello.send(Err("hub connection closed before hello".to_string()));
                let _ = inbound.send(SessionEvent::Closed);
                return None;
            }
        }
    }
}

/// Pump one attached session: only frames carrying our `id` become events.
/// An `error` before the first snapshot means the attach was refused, so
/// the stream closes; later errors (e.g. typing into an exited session)
/// are ignored.
async fn pump_session(
    id: &str,
    sink: &mut futures::stream::SplitSink<WsStream, tokio_tungstenite::tungstenite::Message>,
    ws: &mut futures::stream::SplitStream<WsStream>,
    mut outbound: UnboundedReceiver<String>,
    inbound: &mpsc::Sender<SessionEvent>,
) {
    use tokio_tungstenite::tungstenite::Message;
    let mut snapshot_seen = false;
    loop {
        tokio::select! {
            outgoing = outbound.recv() => {
                let Some(text) = outgoing else {
                    let _ = sink.send(Message::Close(None)).await;
                    break;
                };
                if sink.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            incoming = ws.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if pump_session_text(id, &text, &mut snapshot_seen, inbound) {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
        }
    }
}

/// Forward one text frame; returns true when the pump must stop.
fn pump_session_text(
    id: &str,
    text: &str,
    snapshot_seen: &mut bool,
    inbound: &mpsc::Sender<SessionEvent>,
) -> bool {
    let Ok(frame) = parse_host_frame(text) else {
        return false;
    };
    match frame {
        HostFrame::Snapshot {
            id: sid,
            cols,
            rows,
            seq,
            data,
        } if sid == id => {
            *snapshot_seen = true;
            inbound
                .send(SessionEvent::Snapshot {
                    cols,
                    rows,
                    seq,
                    data,
                })
                .is_err()
        }
        HostFrame::Out { id: sid, seq, data } if sid == id => {
            inbound.send(SessionEvent::Out { seq, data }).is_err()
        }
        HostFrame::Exited { id: sid, code } if sid == id => {
            inbound.send(SessionEvent::Exited { code }).is_err()
        }
        HostFrame::Error { .. } if !*snapshot_seen => {
            let _ = inbound.send(SessionEvent::Closed);
            true
        }
        _ => false,
    }
}

// ── Global client ─────────────────────────────────────────────────────

/// The TUI's shared control connection plus the endpoint that session
/// streams dial. Spawns go through this so no call site changes.
pub struct ActiveHub {
    pub url: String,
    pub token: String,
    pub client: HubClient,
}

fn active_cell() -> &'static Mutex<Option<ActiveHub>> {
    static ACTIVE: OnceLock<Mutex<Option<ActiveHub>>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(None))
}

/// Install the shared control connection (replacing any previous one).
pub fn set_active(url: String, token: String, client: HubClient) {
    if let Ok(mut active) = active_cell().lock() {
        *active = Some(ActiveHub { url, token, client });
    }
}

/// Run `f` on the shared control connection. In tests the shared
/// connection self-installs against the in-process hub, so existing
/// spawn-based tests need no setup edits.
pub fn with_active<R>(f: impl FnOnce(&ActiveHub) -> R) -> Option<R> {
    #[cfg(test)]
    ensure_test_active();
    let active = active_cell().lock().ok()?;
    let active = active.as_ref()?;
    Some(f(active))
}

// ── In-process test hub ───────────────────────────────────────────────

/// The in-process hub that tests spawn against: its endpoint plus the hub
/// itself for assertions. One per test process.
#[cfg(test)]
pub struct TestHub {
    pub url: String,
    pub token: String,
    pub hub: super::SessionHub,
}

/// Serve the hub on 127.0.0.1 with an ephemeral port. The server thread
/// runs until the test process exits.
#[cfg(test)]
pub fn test_client() -> &'static TestHub {
    static TEST_HUB: OnceLock<TestHub> = OnceLock::new();
    TEST_HUB.get_or_init(|| {
        let dir = Box::leak(Box::new(tempfile::tempdir().expect("test hub dir")));
        let db = Arc::new(
            crate::db::Database::new(&dir.path().join("test.db")).expect("test hub database"),
        );
        let hub = super::SessionHub::with_broadcast_capacity(db, "test-host".to_string(), 256);
        let token = "test-token".to_string();
        let state = super::ws::SessionsState::for_test(hub.clone(), token.clone());
        let (addr_tx, addr_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("test hub runtime");
            rt.block_on(async move {
                let app = axum::Router::new()
                    .route(
                        "/sessions/ws",
                        axum::routing::get(super::ws::sessions_ws_handler),
                    )
                    .with_state(state);
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("bind test hub");
                let addr = listener.local_addr().expect("test hub addr");
                addr_tx.send(addr).expect("test hub addr channel");
                axum::serve(listener, app).await.expect("serve test hub");
            });
        });
        let addr: std::net::SocketAddr = addr_rx.recv().expect("test hub address");
        TestHub {
            url: format!("ws://{addr}/sessions/ws"),
            token,
            hub,
        }
    })
}

/// Install the in-process hub as the shared connection, once.
#[cfg(test)]
fn ensure_test_active() {
    let needs = active_cell()
        .lock()
        .map(|active| active.is_none())
        .unwrap_or(false);
    if !needs {
        return;
    }
    let test = test_client();
    if let Ok(client) = HubClient::connect(&test.url, &test.token) {
        set_active(test.url.clone(), test.token.clone(), client);
    }
}
