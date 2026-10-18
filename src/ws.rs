//! WebSocket client (tungstenite, blocking, rustls for `wss://`). One background thread owns the
//! socket: it reads with a short timeout and, between reads, runs the commands of the `WsHandle`,
//! so a send never waits for incoming data. Everything that happens comes back as a `WsEvent`.

use std::io::ErrorKind;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use tungstenite::client::IntoClientRequest;
use tungstenite::handshake::HandshakeError;
use tungstenite::http::{HeaderName, HeaderValue};
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::protocol::CloseFrame;
use tungstenite::stream::{MaybeTlsStream, Mode};
use tungstenite::{Connector, Message, WebSocket};

/// How long a read waits before the thread looks at the commands again.
const POLL: Duration = Duration::from_millis(20);
/// How long a close we started waits for the server's reply.
const CLOSE_WAIT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug)]
pub struct WsConfig {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub subprotocols: Vec<String>,
    pub connect_timeout: Duration,
}

impl Default for WsConfig {
    fn default() -> Self {
        WsConfig { url: String::new(), headers: Vec::new(), subprotocols: Vec::new(), connect_timeout: Duration::from_secs(10) }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WsData {
    Text(String),
    Binary(Vec<u8>),
}

/// After `Connected`, the last event is always `Closed`. A connection that never opens ends with `Error`.
#[derive(Clone, Debug)]
pub enum WsEvent {
    Connected { status: u16, headers: Vec<(String, String)>, protocol: Option<String> },
    Received { data: WsData, at: SystemTime },
    Sent { data: WsData, at: SystemTime },
    Closed { code: Option<u16>, reason: String },
    Error(String),
}

enum Command {
    Send(WsData),
    Ping,
    Close(u16, String),
}

/// Talks to the connection's thread. Dropping it closes the connection (code 1000).
pub struct WsHandle {
    commands: Sender<Command>,
    open: Arc<AtomicBool>,
}

impl WsHandle {
    pub fn send_text(&self, s: String) {
        let _ = self.commands.send(Command::Send(WsData::Text(s)));
    }
    pub fn send_binary(&self, b: Vec<u8>) {
        let _ = self.commands.send(Command::Send(WsData::Binary(b)));
    }
    pub fn ping(&self) {
        let _ = self.commands.send(Command::Ping);
    }
    pub fn close(&self, code: u16, reason: String) {
        let _ = self.commands.send(Command::Close(code, reason));
    }
    /// Connected and not closing.
    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::Relaxed)
    }
}

/// Connects on a background thread; every event goes to `events`, then `wake()` is called (the UI repaints).
pub fn connect(cfg: WsConfig, events: Sender<WsEvent>, wake: Box<dyn Fn() + Send>) -> WsHandle {
    let (tx, rx) = mpsc::channel();
    let open = Arc::new(AtomicBool::new(false));
    let worker = Worker { events, wake, open: open.clone() };
    let spawned = std::thread::Builder::new().name("websocket".into()).spawn(move || worker.run(cfg, rx));
    if let Err(e) = spawned {
        // The worker moved into the closure is gone; nothing to report to but the caller's channel, which went with it.
        eprintln!("websocket: couldn't start the thread: {e}");
    }
    WsHandle { commands: tx, open }
}

struct Worker {
    events: Sender<WsEvent>,
    wake: Box<dyn Fn() + Send>,
    open: Arc<AtomicBool>,
}

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

impl Worker {
    fn emit(&self, ev: WsEvent) {
        if self.events.send(ev).is_ok() {
            (self.wake)();
        }
    }

    fn run(self, cfg: WsConfig, commands: Receiver<Command>) {
        let (mut ws, tcp) = match self.open_socket(&cfg) {
            Ok(v) => v,
            Err(e) => return self.emit(WsEvent::Error(e)),
        };
        if let Err(e) = tcp.set_read_timeout(Some(POLL)).and_then(|_| tcp.set_write_timeout(None)) {
            self.emit(WsEvent::Error(format!("Couldn't set up the socket: {e}")));
            return self.emit(WsEvent::Closed { code: None, reason: "socket setup failed".into() });
        }
        self.open.store(true, Ordering::Relaxed);
        let (code, reason) = self.pump(&mut ws, &commands);
        self.open.store(false, Ordering::Relaxed);
        let _ = tcp.shutdown(std::net::Shutdown::Both);
        self.emit(WsEvent::Closed { code, reason });
    }

    /// TCP connect (with the timeout), TLS for `wss://`, WebSocket handshake. Emits `Connected`.
    fn open_socket(&self, cfg: &WsConfig) -> Result<(Socket, TcpStream), String> {
        let mut request = cfg.url.trim().into_client_request().map_err(|e| format!("Invalid URL: {}", describe(&e)))?;
        for (k, v) in &cfg.headers {
            if k.trim().is_empty() {
                continue;
            }
            let name = HeaderName::from_bytes(k.trim().as_bytes()).map_err(|_| format!("Invalid header name: {k}"))?;
            let value = HeaderValue::from_str(v).map_err(|_| format!("Invalid value for header {k}"))?;
            request.headers_mut().append(name, value);
        }
        let protocols: Vec<&str> = cfg.subprotocols.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        if !protocols.is_empty() {
            let value = HeaderValue::from_str(&protocols.join(", ")).map_err(|_| "Invalid subprotocol".to_string())?;
            request.headers_mut().insert("sec-websocket-protocol", value);
        }
        let uri = request.uri().clone();
        let mode = tungstenite::client::uri_mode(&uri).map_err(|_| "The URL must start with ws:// or wss://".to_string())?;
        let host = uri.host().ok_or("The URL has no host")?;
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let port = uri.port_u16().unwrap_or(if matches!(mode, Mode::Tls) { 443 } else { 80 });
        let timeout = if cfg.connect_timeout.is_zero() { Duration::from_secs(10) } else { cfg.connect_timeout };

        let addrs: Vec<_> = (host, port).to_socket_addrs().map_err(|e| format!("Couldn't resolve {host}: {e}"))?.collect();
        let mut last = format!("{host} has no address");
        let mut tcp = None;
        for addr in addrs {
            match TcpStream::connect_timeout(&addr, timeout) {
                Ok(s) => {
                    tcp = Some(s);
                    break;
                }
                Err(e) => last = e.to_string(),
            }
        }
        let tcp = tcp.ok_or_else(|| format!("Couldn't connect to {host}:{port}: {last}"))?;
        let _ = tcp.set_nodelay(true);
        // The handshake (TLS included) is bounded by the connect timeout too.
        tcp.set_read_timeout(Some(timeout)).and_then(|_| tcp.set_write_timeout(Some(timeout))).map_err(|e| e.to_string())?;
        let control = tcp.try_clone().map_err(|e| e.to_string())?;
        let connector = match mode {
            Mode::Plain => Connector::Plain,
            Mode::Tls => Connector::Rustls(tls_config()?),
        };
        let (ws, response) = tungstenite::client_tls_with_config(request, tcp, None, Some(connector)).map_err(|e| match e {
            HandshakeError::Failure(e) => describe(&e),
            HandshakeError::Interrupted(_) => format!("No answer from {host}:{port} within {} s", timeout.as_secs_f32()),
        })?;
        let headers = response.headers().iter().map(|(k, v)| (k.to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned())).collect();
        let protocol = response.headers().get("sec-websocket-protocol").and_then(|v| v.to_str().ok()).map(str::to_string);
        self.emit(WsEvent::Connected { status: response.status().as_u16(), headers, protocol });
        Ok((ws, control))
    }

    /// Reads and runs commands until the connection ends; returns the close code and reason.
    fn pump(&self, ws: &mut Socket, commands: &Receiver<Command>) -> (Option<u16>, String) {
        let mut close: Option<(u16, String)> = None;
        let mut closing_since: Option<Instant> = None;
        let mut handle_alive = true;
        loop {
            while handle_alive {
                let cmd = match commands.try_recv() {
                    Ok(c) => c,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        handle_alive = false;
                        Command::Close(1000, String::new())
                    }
                };
                match cmd {
                    Command::Close(code, reason) => {
                        if closing_since.is_none() {
                            closing_since = Some(Instant::now());
                            self.open.store(false, Ordering::Relaxed);
                            let frame = CloseFrame { code: CloseCode::from(close_code(code)), reason: truncate(&reason, 123).into() };
                            if let Err(e) = ws.close(Some(frame)) {
                                if !closed(&e) {
                                    self.emit(WsEvent::Error(describe(&e)));
                                }
                                return lost(close, &e);
                            }
                        }
                    }
                    _ if closing_since.is_some() => self.emit(WsEvent::Error("The connection is closing".into())),
                    Command::Ping => {
                        if let Err(e) = ws.send(Message::Ping(Default::default())) {
                            self.emit(WsEvent::Error(describe(&e)));
                            return lost(close, &e);
                        }
                    }
                    Command::Send(data) => {
                        let msg = match &data {
                            WsData::Text(s) => Message::text(s.clone()),
                            WsData::Binary(b) => Message::binary(b.clone()),
                        };
                        match ws.send(msg) {
                            Ok(()) => self.emit(WsEvent::Sent { data, at: SystemTime::now() }),
                            Err(e) => {
                                self.emit(WsEvent::Error(describe(&e)));
                                return lost(close, &e);
                            }
                        }
                    }
                }
            }
            if closing_since.is_some_and(|t| t.elapsed() > CLOSE_WAIT) {
                return close.map_or((None, "No close reply from the server".into()), |(c, r)| (Some(c), r));
            }
            match ws.read() {
                Ok(Message::Text(t)) => self.emit(WsEvent::Received { data: WsData::Text(t.as_str().to_string()), at: SystemTime::now() }),
                Ok(Message::Binary(b)) => self.emit(WsEvent::Received { data: WsData::Binary(b.to_vec()), at: SystemTime::now() }),
                Ok(Message::Close(frame)) => {
                    // tungstenite answers the close itself; the next reads finish the handshake.
                    self.open.store(false, Ordering::Relaxed);
                    close = Some(frame.map_or((1005, String::new()), |f| (u16::from(f.code), f.reason.as_str().to_string())));
                }
                Ok(_) => {} // ping (answered automatically), pong, raw frame
                Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(e) if closed(&e) => return close.map_or((None, String::new()), |(c, r)| (Some(c), r)),
                Err(e) => {
                    if close.is_none() && closing_since.is_none() {
                        self.emit(WsEvent::Error(format!("Connection lost: {}", describe(&e))));
                    }
                    return lost(close, &e);
                }
            }
        }
    }
}

/// The close code / reason to report when the connection ends on `e`.
fn lost(close: Option<(u16, String)>, e: &tungstenite::Error) -> (Option<u16>, String) {
    match close {
        Some((c, r)) => (Some(c), r),
        None if closed(e) => (None, String::new()),
        None => (None, describe(e)),
    }
}

fn closed(e: &tungstenite::Error) -> bool {
    matches!(e, tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed)
}

/// A code that may be sent in a close frame (others become 1000).
fn close_code(code: u16) -> u16 {
    match code {
        1000..=1003 | 1007..=1014 | 3000..=4999 => code,
        _ => 1000,
    }
}

fn truncate(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn describe(e: &tungstenite::Error) -> String {
    use tungstenite::Error as E;
    match e {
        E::Http(res) => {
            let status = res.status();
            let mut s = format!("The server answered {} {} instead of switching to WebSocket", status.as_u16(), status.canonical_reason().unwrap_or(""));
            if let Some(body) = res.body().as_deref().map(String::from_utf8_lossy).filter(|b| !b.trim().is_empty()) {
                s.push_str(": ");
                s.push_str(truncate(body.trim(), 300));
            }
            s
        }
        E::Io(e) => e.to_string(),
        E::Tls(e) => format!("TLS: {e}"),
        E::Url(e) => e.to_string(),
        E::Protocol(tungstenite::error::ProtocolError::ResetWithoutClosingHandshake) => "the server closed the connection without a close frame".into(),
        E::ConnectionClosed | E::AlreadyClosed => "the connection is closed".into(),
        e => e.to_string(),
    }
}

/// rustls with the ring provider and the webpki roots, built once.
fn tls_config() -> Result<Arc<rustls::ClientConfig>, String> {
    static CONFIG: OnceLock<Result<Arc<rustls::ClientConfig>, String>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
            rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .map(|b| Arc::new(b.with_root_certificates(roots).with_no_client_auth()))
                .map_err(|e| format!("TLS setup: {e}"))
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::Mutex;
    use tungstenite::handshake::server::{Request, Response};

    const WAIT: Duration = Duration::from_secs(5);

    struct Server {
        url: String,
        /// The handshake request's headers (lowercase names).
        headers: Arc<Mutex<Vec<(String, String)>>>,
        /// What the server saw: "text:…", "binary:…", "close:<code>:<reason>".
        seen: Receiver<String>,
    }

    /// One connection on 127.0.0.1; answers the `chat` subprotocol when asked, then runs `f`.
    #[allow(clippy::result_large_err)] // tungstenite's handshake callback signature
    fn server(f: impl FnOnce(WebSocket<TcpStream>, &Sender<String>) + Send + 'static) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}/socket", listener.local_addr().unwrap());
        let headers = Arc::new(Mutex::new(Vec::new()));
        let (tx, seen) = mpsc::channel();
        let h = headers.clone();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let callback = |req: &Request, mut res: Response| {
                let list: Vec<_> = req.headers().iter().map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string())).collect();
                if list.iter().any(|(k, v)| k == "sec-websocket-protocol" && v.split(',').any(|p| p.trim() == "chat")) {
                    res.headers_mut().insert("sec-websocket-protocol", HeaderValue::from_static("chat"));
                }
                *h.lock().unwrap() = list;
                Ok(res)
            };
            if let Ok(ws) = tungstenite::accept_hdr(stream, callback) {
                f(ws, &tx);
            }
        });
        Server { url, headers, seen }
    }

    /// Echoes text and binary messages, reports what it saw, until the connection ends.
    fn echo(mut ws: WebSocket<TcpStream>, seen: &Sender<String>) {
        loop {
            match ws.read() {
                Ok(Message::Text(t)) => {
                    let _ = seen.send(format!("text:{t}"));
                    let _ = ws.send(Message::Text(t));
                }
                Ok(Message::Binary(b)) => {
                    let _ = seen.send(format!("binary:{:?}", b.to_vec()));
                    let _ = ws.send(Message::Binary(b));
                }
                Ok(Message::Close(f)) => {
                    let _ = seen.send(f.map_or("close:none".into(), |f| format!("close:{}:{}", u16::from(f.code), f.reason)));
                }
                Ok(_) => {}
                Err(_) => return,
            }
        }
    }

    fn client(url: &str, cfg: impl FnOnce(&mut WsConfig)) -> (WsHandle, Receiver<WsEvent>) {
        let mut c = WsConfig { url: url.into(), connect_timeout: Duration::from_secs(3), ..Default::default() };
        cfg(&mut c);
        let (tx, rx) = mpsc::channel();
        (connect(c, tx, Box::new(|| {})), rx)
    }

    fn next(rx: &Receiver<WsEvent>) -> WsEvent {
        rx.recv_timeout(WAIT).expect("no event")
    }

    fn connected(rx: &Receiver<WsEvent>) -> Option<String> {
        match next(rx) {
            WsEvent::Connected { status, protocol, .. } => {
                assert_eq!(status, 101);
                protocol
            }
            e => panic!("expected Connected, got {e:?}"),
        }
    }

    fn received(rx: &Receiver<WsEvent>) -> WsData {
        loop {
            match next(rx) {
                WsEvent::Received { data, .. } => return data,
                WsEvent::Sent { .. } => {}
                e => panic!("expected Received, got {e:?}"),
            }
        }
    }

    /// Skips to the `Closed` event (`Error`s before it are returned too).
    fn closed_event(rx: &Receiver<WsEvent>) -> (Vec<String>, Option<u16>, String) {
        let mut errors = Vec::new();
        loop {
            match next(rx) {
                WsEvent::Closed { code, reason } => return (errors, code, reason),
                WsEvent::Error(e) => errors.push(e),
                _ => {}
            }
        }
    }

    #[test]
    fn handshake_headers_subprotocol_and_echo() {
        let s = server(echo);
        let (h, rx) = client(&s.url, |c| {
            c.headers = vec![("X-Test".into(), "hello".into()), ("Authorization".into(), "Bearer abc".into())];
            c.subprotocols = vec!["v2".into(), "chat".into()];
        });
        assert_eq!(connected(&rx).as_deref(), Some("chat"));
        let headers = s.headers.lock().unwrap().clone();
        assert!(headers.contains(&("x-test".into(), "hello".into())), "{headers:?}");
        assert!(headers.contains(&("authorization".into(), "Bearer abc".into())));
        assert!(headers.contains(&("sec-websocket-protocol".into(), "v2, chat".into())));
        assert!(h.is_open());

        h.send_text("héllo".into());
        match next(&rx) {
            WsEvent::Sent { data, .. } => assert_eq!(data, WsData::Text("héllo".into())),
            e => panic!("{e:?}"),
        }
        assert_eq!(received(&rx), WsData::Text("héllo".into()));
        h.send_binary(vec![0, 1, 255]);
        assert_eq!(received(&rx), WsData::Binary(vec![0, 1, 255]));
        h.ping();
        h.send_text("after ping".into());
        assert_eq!(received(&rx), WsData::Text("after ping".into()));
        assert_eq!(s.seen.recv_timeout(WAIT).unwrap(), "text:héllo");
        assert_eq!(s.seen.recv_timeout(WAIT).unwrap(), "binary:[0, 1, 255]");
    }

    #[test]
    fn no_subprotocol_when_none_asked() {
        let s = server(echo);
        let (_h, rx) = client(&s.url, |_| {});
        assert_eq!(connected(&rx), None);
        assert!(!s.headers.lock().unwrap().iter().any(|(k, _)| k == "sec-websocket-protocol"));
    }

    #[test]
    fn server_sends_first_and_closes_with_a_code() {
        let s = server(|mut ws, _| {
            ws.send(Message::text("welcome")).unwrap();
            ws.send(Message::binary(vec![7u8; 3])).unwrap();
            ws.close(Some(CloseFrame { code: CloseCode::from(4001), reason: "done".into() })).unwrap();
            while ws.read().is_ok() {}
        });
        let (h, rx) = client(&s.url, |_| {});
        connected(&rx);
        assert_eq!(received(&rx), WsData::Text("welcome".into()));
        assert_eq!(received(&rx), WsData::Binary(vec![7, 7, 7]));
        let (errors, code, reason) = closed_event(&rx);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!((code, reason.as_str()), (Some(4001), "done"));
        assert!(!h.is_open());
    }

    #[test]
    fn client_close_code_reaches_the_server() {
        let s = server(echo);
        let (h, rx) = client(&s.url, |_| {});
        connected(&rx);
        h.close(4000, "bye".into());
        let (errors, code, reason) = closed_event(&rx);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!((code, reason.as_str()), (Some(4000), "bye"));
        assert_eq!(s.seen.recv_timeout(WAIT).unwrap(), "close:4000:bye");
        assert!(!h.is_open());
    }

    #[test]
    fn dropping_the_handle_closes_normally() {
        let s = server(echo);
        let (h, rx) = client(&s.url, |_| {});
        connected(&rx);
        drop(h);
        assert_eq!(s.seen.recv_timeout(WAIT).unwrap(), "close:1000:");
        let (_, code, _) = closed_event(&rx);
        assert_eq!(code, Some(1000));
    }

    #[test]
    fn connection_refused_is_an_error() {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let (_h, rx) = client(&format!("ws://127.0.0.1:{port}/"), |_| {});
        match next(&rx) {
            WsEvent::Error(e) => assert!(e.contains(&format!("127.0.0.1:{port}")), "{e}"),
            e => panic!("expected Error, got {e:?}"),
        }
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err(), "nothing after the error");
    }

    #[test]
    fn bad_urls_and_http_answers_are_errors() {
        let (_h, rx) = client("http://example.com/", |_| {});
        assert!(matches!(next(&rx), WsEvent::Error(e) if e.contains("ws://")));
        let (_h, rx) = client("not a url", |_| {});
        assert!(matches!(next(&rx), WsEvent::Error(_)));

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 6\r\n\r\nno way");
        });
        let (_h, rx) = client(&format!("ws://{addr}/"), |_| {});
        match next(&rx) {
            WsEvent::Error(e) => assert!(e.contains("403"), "{e}"),
            e => panic!("{e:?}"),
        }
    }

    #[test]
    fn abrupt_server_close_is_reported() {
        let s = server(|ws, _| {
            let tcp = ws.get_ref();
            let _ = tcp.shutdown(std::net::Shutdown::Both);
        });
        let (h, rx) = client(&s.url, |_| {});
        connected(&rx);
        let (errors, code, reason) = closed_event(&rx);
        assert_eq!(code, None);
        assert!(!errors.is_empty() && !reason.is_empty(), "{errors:?} {reason}");
        assert!(!h.is_open());
    }

    #[test]
    fn handshake_timeout() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let _s = listener.accept();
            std::thread::sleep(Duration::from_secs(3));
        });
        let (_h, rx) = client(&format!("ws://{addr}/"), |c| c.connect_timeout = Duration::from_millis(300));
        match next(&rx) {
            WsEvent::Error(e) => assert!(e.contains("No answer") || e.contains("timed out") || e.contains("Resource"), "{e}"),
            e => panic!("{e:?}"),
        }
    }

    /// `wss://` against a public echo server (network).
    #[test]
    #[ignore]
    fn wss_public_echo() {
        let (h, rx) = client("wss://echo.websocket.org/", |c| c.connect_timeout = Duration::from_secs(10));
        connected(&rx);
        h.send_text("over tls".into());
        while received(&rx) != WsData::Text("over tls".into()) {}
    }

    #[test]
    fn helpers() {
        assert_eq!(close_code(1005), 1000);
        assert_eq!(close_code(4321), 4321);
        assert_eq!(truncate("ééé", 3), "é");
        assert!(tls_config().is_ok());
    }
}
