//! A tiny HTTP stub standing in for the catalogue in tests: scripted
//! routes on a loopback `TcpListener`, recording the request target and
//! User-Agent of every hit.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One scripted route: status, extra headers, and body.
pub(crate) struct StubRoute {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl StubRoute {
    pub(crate) fn ok(body: &str) -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body: body.as_bytes().to_vec(),
        }
    }

    pub(crate) fn redirect_to(location: &str) -> Self {
        Self {
            status: 302,
            headers: vec![("Location".to_string(), location.to_string())],
            body: Vec::new(),
        }
    }
}

/// What the stub recorded about one request: the full request target
/// (path plus query) and the User-Agent header, if any.
#[derive(Debug, Clone)]
pub(crate) struct StubHit {
    pub target: String,
    pub user_agent: Option<String>,
}

/// A catalogue stub: `routes` maps a request path (without the query) to
/// its response; anything unmapped is a 404. Serves until dropped.
pub(crate) struct StubCatalogue {
    addr: SocketAddr,
    hits: Arc<Mutex<Vec<StubHit>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl StubCatalogue {
    pub(crate) fn start(routes: HashMap<String, StubRoute>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("stub binds loopback");
        listener
            .set_nonblocking(true)
            .expect("stub sets nonblocking");
        let addr = listener.local_addr().expect("stub has an address");
        let hits: Arc<Mutex<Vec<StubHit>>> = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let hits = Arc::clone(&hits);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || serve(&listener, &routes, &hits, &stop))
        };
        Self {
            addr,
            hits,
            stop,
            thread: Some(thread),
        }
    }

    /// The base URL to fetch from, e.g. `http://127.0.0.1:41235`.
    pub(crate) fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub(crate) fn hits(&self) -> Vec<StubHit> {
        self.hits.lock().expect("hits lock").clone()
    }
}

impl Drop for StubCatalogue {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(
    listener: &TcpListener,
    routes: &HashMap<String, StubRoute>,
    hits: &Arc<Mutex<Vec<StubHit>>>,
    stop: &Arc<AtomicBool>,
) {
    while !stop.load(Ordering::SeqCst) {
        let (stream, _) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            Err(_) => break,
        };
        respond(stream, routes, hits);
    }
}

fn respond(
    mut stream: std::net::TcpStream,
    routes: &HashMap<String, StubRoute>,
    hits: &Arc<Mutex<Vec<StubHit>>>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while head.len() < 8192 {
        match stream.read(&mut byte) {
            Ok(1) => {
                head.push(byte[0]);
                if head.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            _ => break,
        }
    }
    let head = String::from_utf8_lossy(&head);
    let mut lines = head.lines();
    let target = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_string();
    let user_agent = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("user-agent"))
        .map(|(_, value)| value.trim().to_string());
    hits.lock().expect("hits lock").push(StubHit {
        target: target.clone(),
        user_agent,
    });

    let path = target.split('?').next().unwrap_or(&target).to_string();
    let route = routes.get(&path);
    let (status, headers, body) = match route {
        Some(route) => (route.status, &route.headers, route.body.as_slice()),
        None => (404, &Vec::new(), b"no such graph".as_slice()),
    };
    let mut response = format!(
        "HTTP/1.1 {status} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        reason(status),
        body.len()
    );
    for (name, value) in headers {
        response.push_str(&format!("{name}: {value}\r\n"));
    }
    response.push_str("\r\n");
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(body);
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        302 => "Found",
        404 => "Not Found",
        _ => "OK",
    }
}
