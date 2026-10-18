//! Optional MCP server (Settings › MCP server): lets AI agents on this computer (Claude Code, Cursor, Codex…)
//! use the running app with the assistant's tools — search the API docs, read the workspace, open requests /
//! flows / substitutions in tabs, and send a request after the user confirms it in the app.
//!
//! Transport: MCP "Streamable HTTP" on 127.0.0.1 only, JSON responses (no SSE stream), one POST per JSON-RPC
//! message. Every request needs `Authorization: Bearer <token>`; a browser `Origin` other than localhost is refused.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Value};

/// Protocol versions we speak; the client's is echoed back when known, else the newest.
const VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
pub const DEFAULT_PORT: u16 = 7642;
/// How long a tool call may wait for the app (a confirmation can take a while).
const CALL_TIMEOUT: Duration = Duration::from_secs(600);
const MAX_BODY: usize = 4 * 1024 * 1024;

/// A tool call handed to the app's UI thread: (tool name, arguments, where to send (text, is_error)).
pub struct Call {
    pub name: String,
    pub args: Value,
    pub reply: Sender<(String, bool)>,
    /// When the client stops waiting for the answer (it was told "No answer").
    pub deadline: Instant,
}

/// What the settings show.
#[derive(Clone, Default)]
pub struct Status {
    pub calls: u64,
    pub last: Option<(Instant, String)>,
    pub client: Option<String>,
}

pub struct Server {
    pub port: u16,
    stop: Arc<AtomicBool>,
    pub status: Arc<Mutex<Status>>,
    listener: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Server {
    /// Waits for the listener to let the port go (≤ 60 ms): a restart on the same port binds right after.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.listener.take() {
            let _ = h.join();
        }
    }
}

/// A random access key (no extra crate: the std hasher is seeded randomly per process).
pub fn new_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    for i in 0..2u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        h.write_u32(std::process::id());
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

/// How to plug the server into one MCP client: where it goes, and what to paste (key included).
pub struct ClientSetup {
    pub name: &'static str,
    /// Where to paste it.
    pub place: &'static str,
    pub snippet: String,
}

/// Every client we give a ready-made setup for. `token` is written in the snippets as is.
pub fn clients(port: u16, token: &str) -> Vec<ClientSetup> {
    let url = format!("http://127.0.0.1:{port}/mcp");
    let bearer = format!("Bearer {token}");
    let pretty = |v: Value| serde_json::to_string_pretty(&v).unwrap_or_default();
    vec![
        ClientSetup {
            name: "Claude Code",
            place: "Run it in a terminal (then /mcp in Claude Code shows it).",
            snippet: format!("claude mcp add --transport http apifa {url} --header \"Authorization: {bearer}\""),
        },
        ClientSetup {
            name: "Claude Desktop",
            place: "Settings › Developer › Edit Config (claude_desktop_config.json), inside \"mcpServers\". Needs Node.js (npx), then restart Claude Desktop.",
            snippet: pretty(json!({ "mcpServers": { "apifa": {
                "command": "npx",
                "args": ["-y", "mcp-remote", url, "--header", "Authorization:${APIFA_AUTH}"],
                "env": { "APIFA_AUTH": bearer }
            } } })),
        },
        ClientSetup {
            name: "OpenAI Codex",
            place: "Add it to ~/.codex/config.toml.",
            snippet: format!("[mcp_servers.apifa]\nurl = \"{url}\"\nhttp_headers = {{ \"Authorization\" = \"{bearer}\" }}\n"),
        },
        ClientSetup {
            name: "Cursor",
            place: "Settings › MCP › Add new MCP server, or ~/.cursor/mcp.json.",
            snippet: pretty(json!({ "mcpServers": { "apifa": { "url": url, "headers": { "Authorization": bearer } } } })),
        },
        ClientSetup {
            name: "VS Code (Copilot)",
            place: "In .vscode/mcp.json of a project, or the command \"MCP: Open User Configuration\".",
            snippet: pretty(json!({ "servers": { "apifa": { "type": "http", "url": url, "headers": { "Authorization": bearer } } } })),
        },
        ClientSetup {
            name: "Gemini CLI",
            place: "In ~/.gemini/settings.json, inside \"mcpServers\".",
            snippet: pretty(json!({ "mcpServers": { "apifa": { "httpUrl": url, "headers": { "Authorization": bearer } } } })),
        },
        ClientSetup {
            name: "Windsurf",
            place: "In ~/.codeium/windsurf/mcp_config.json.",
            snippet: pretty(json!({ "mcpServers": { "apifa": { "serverUrl": url, "headers": { "Authorization": bearer } } } })),
        },
    ]
}

/// Starts listening; `calls` go to the app, `wake` repaints it so it picks them up.
pub fn start(port: u16, token: String, calls: Sender<Call>, wake: impl Fn() + Send + Sync + 'static) -> Result<Server, String> {
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("can't listen on 127.0.0.1:{port}: {e}"))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let stop = Arc::new(AtomicBool::new(false));
    let status = Arc::new(Mutex::new(Status::default()));
    let (stop2, status2) = (stop.clone(), status.clone());
    let shared = Arc::new(Shared { token, calls: Mutex::new(calls), wake: Box::new(wake), status: status2 });
    let thread = std::thread::spawn(move || {
        while !stop2.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let shared = shared.clone();
                    std::thread::spawn(move || {
                        let _ = stream.set_nonblocking(false);
                        let _ = handle(stream, &shared);
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(60)),
                Err(_) => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    });
    Ok(Server { port, stop, status, listener: Some(thread) })
}

struct Shared {
    token: String,
    calls: Mutex<Sender<Call>>,
    wake: Box<dyn Fn() + Send + Sync>,
    status: Arc<Mutex<Status>>,
}

pub(crate) struct Request {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

pub(crate) fn read_request(stream: &mut TcpStream) -> Result<Request, String> {
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    // Request line + headers are capped (a huge line would be read whole before any check).
    let mut reader = BufReader::new(stream).take(64 * 1024);
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|e| e.to_string())?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        reader.read_line(&mut h).map_err(|e| e.to_string())?;
        if reader.limit() == 0 {
            return Err("headers too large".into());
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_owned(), v.trim().to_owned()));
        }
        if headers.len() > 100 {
            return Err("too many headers".into());
        }
    }
    let len: usize = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    if len > MAX_BODY {
        return Err("body too large".into());
    }
    let mut body = vec![0; len];
    reader.set_limit(len as u64);
    reader.read_exact(&mut body).map_err(|e| e.to_string())?;
    Ok(Request { method, path, headers, body })
}

pub(crate) fn respond(stream: &mut TcpStream, status: &str, extra: &[(&str, String)], body: &str) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    if !body.is_empty() {
        head.push_str("Content-Type: application/json\r\n");
    }
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()
}

/// Constant-time comparison of the access key.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A page in a browser may not call the server (DNS rebinding): only local origins, or none (CLI clients).
pub(crate) fn origin_ok(origin: Option<&str>) -> bool {
    match origin {
        None => true,
        Some(o) => local_host(o.split("://").nth(1).unwrap_or(o)),
    }
}

/// `host[:port][/…]` names this machine.
fn local_host(h: &str) -> bool {
    let h = h.split('/').next().unwrap_or_default();
    let host = if h.starts_with('[') { h.split_inclusive(']').next().unwrap_or_default() } else { h.split(':').next().unwrap_or_default() };
    matches!(host.to_ascii_lowercase().as_str(), "localhost" | "127.0.0.1" | "[::1]")
}

/// The request comes from this machine's pages or tools: a page from another site may be served
/// from 127.0.0.1 through DNS rebinding, but its `Host` header still names that site.
pub(crate) fn local_request(req: &Request) -> bool {
    origin_ok(req.header("origin")) && req.header("host").is_none_or(local_host)
}

fn handle(mut stream: TcpStream, shared: &Shared) -> std::io::Result<()> {
    let req = match read_request(&mut stream) {
        Ok(r) => r,
        Err(e) => return respond(&mut stream, "400 Bad Request", &[], &json!({ "error": e }).to_string()),
    };
    if !local_request(&req) {
        return respond(&mut stream, "403 Forbidden", &[], &json!({ "error": "origin not allowed" }).to_string());
    }
    let authorized = req.header("authorization").and_then(|h| h.strip_prefix("Bearer ")).is_some_and(|t| same(t.trim(), &shared.token));
    if !authorized {
        return respond(
            &mut stream,
            "401 Unauthorized",
            &[("WWW-Authenticate", "Bearer".into())],
            &json!({ "error": "missing or wrong access key (Settings › MCP server)" }).to_string(),
        );
    }
    if req.path.split('?').next() != Some("/mcp") {
        return respond(&mut stream, "404 Not Found", &[], "");
    }
    match req.method.as_str() {
        "POST" => {}
        // No server-initiated stream; ending a session needs nothing.
        "GET" => return respond(&mut stream, "405 Method Not Allowed", &[("Allow", "POST, DELETE".into())], ""),
        "DELETE" => return respond(&mut stream, "200 OK", &[], ""),
        _ => return respond(&mut stream, "405 Method Not Allowed", &[("Allow", "POST, DELETE".into())], ""),
    }
    let msg: Value = match serde_json::from_slice(&req.body) {
        Ok(v) => v,
        Err(e) => return respond(&mut stream, "400 Bad Request", &[], &rpc_error(Value::Null, -32700, &format!("parse error: {e}")).to_string()),
    };
    // A notification or a response from the client: nothing to answer.
    let Some(id) = msg.get("id").cloned().filter(|_| msg.get("method").is_some()) else {
        return respond(&mut stream, "202 Accepted", &[], "");
    };
    let method = msg["method"].as_str().unwrap_or_default();
    let params = msg.get("params").cloned().unwrap_or(json!({}));
    let mut extra = Vec::new();
    let answer = match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or_default();
            let version = VERSIONS.iter().find(|v| **v == asked).copied().unwrap_or(VERSIONS[0]);
            let client = params.pointer("/clientInfo/name").and_then(Value::as_str).unwrap_or("an MCP client").to_owned();
            shared.status.lock().client = Some(client);
            extra.push(("Mcp-Session-Id", new_token()));
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "apifa", "title": "apifa", "version": env!("CARGO_PKG_VERSION") },
                "instructions": instructions(),
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(
            json!({ "tools": crate::ai::tools().iter().map(|t| json!({ "name": t.name, "description": t.description, "inputSchema": t.schema })).collect::<Vec<_>>() }),
        ),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            if !crate::ai::tools().iter().any(|t| t.name == name) {
                Err((-32602, format!("unknown tool {name}")))
            } else {
                let (text, is_error) = call_app(shared, name, args);
                Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }))
            }
        }
        other => Err((-32601, format!("method not found: {other}"))),
    };
    let body = match answer {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, msg)) => rpc_error(id, code, &msg),
    };
    respond(&mut stream, "200 OK", &extra, &body.to_string())
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Runs a tool in the app (UI thread) and waits for its result.
fn call_app(shared: &Shared, name: String, args: Value) -> (String, bool) {
    {
        let mut st = shared.status.lock();
        st.calls += 1;
        st.last = Some((Instant::now(), name.clone()));
    }
    let (tx, rx) = mpsc::channel();
    if shared.calls.lock().send(Call { name, args, reply: tx, deadline: Instant::now() + CALL_TIMEOUT }).is_err() {
        return ("The app is closing.".into(), true);
    }
    (shared.wake)();
    rx.recv_timeout(CALL_TIMEOUT).unwrap_or_else(|_| ("No answer from the app (closed, or the confirmation was left waiting).".into(), true))
}

/// Told to the agent once, at `initialize`.
fn instructions() -> String {
    format!(
        "This server is the \"apifa\" desktop app (API For All: an API client like Postman / Bruno — HTTP, GraphQL, WebSocket, gRPC), open on the user's computer. \
Its tools search the docs of the imported APIs, read the user's saved requests / flows / substitutions and the open tab, open filled-in request / flow / substitution tabs in the app \
(the user reviews them, then presses Send / Run / Save), and send a request (send_request: the user confirms each one in the app).\n\
Start with data_map (no arguments): the map of the user's data built from real GET responses — every entity, the endpoints returning it, \
how entities link (embedded, lists, *_id references, confirmed by ids) and the dependency layers; then data_map with `entity` or `field` for details. \
Check endpoints and fields with search_endpoints / endpoint_details before writing anything; a field you can sort or filter on isn't always returned. \
Everything you get is anonymized (each value is its type and a fingerprint, `string#5d1e0a9b3c`): to use a value, write its fingerprint as is in a tool input, the app puts the real value back. \
App tokens only have the public scope.\n\n\
The app's documentation (file formats of requests, flows, substitutions; the command line) is read with read_docs: \
read the section you need before writing a file. {}",
        crate::ai::read_docs("")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn post(port: u16, token: &str, origin: Option<&str>, body: &Value) -> (String, String) {
        post_as(port, &format!("127.0.0.1:{port}"), token, origin, body)
    }

    fn post_as(port: u16, host: &str, token: &str, origin: Option<&str>, body: &Value) -> (String, String) {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let body = body.to_string();
        let origin = origin.map(|o| format!("Origin: {o}\r\n")).unwrap_or_default();
        write!(
            s,
            "POST /mcp HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {token}\r\n{origin}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        let (head, body) = out.split_once("\r\n\r\n").unwrap();
        (head.lines().next().unwrap().to_owned(), body.to_owned())
    }

    #[test]
    fn speaks_mcp_over_http() {
        let (tx, rx) = mpsc::channel::<Call>();
        // A fake app: answers every call with its name.
        std::thread::spawn(move || {
            for c in rx {
                let _ = c.reply.send((format!("ran {} with {}", c.name, c.args), false));
            }
        });
        let port = 47642;
        let _server = start(port, "secret".into(), tx, || {}).unwrap();

        let (status, body) = post(
            port,
            "secret",
            None,
            &json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18", "clientInfo": { "name": "test" } } }),
        );
        assert_eq!(status, "HTTP/1.1 200 OK");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["result"]["protocolVersion"], "2025-06-18");
        assert!(v["result"]["capabilities"]["tools"].is_object());

        let (status, _) = post(port, "secret", None, &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        assert_eq!(status, "HTTP/1.1 202 Accepted");

        let (_, body) = post(port, "secret", None, &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));
        let v: Value = serde_json::from_str(&body).unwrap();
        assert!(v["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == "send_request" && t["inputSchema"]["type"] == "object"));

        let (_, body) = post(
            port,
            "secret",
            None,
            &json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "search_endpoints", "arguments": { "query": "users" } } }),
        );
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["result"]["content"][0]["text"], r#"ran search_endpoints with {"query":"users"}"#);
        assert_eq!(v["result"]["isError"], false);

        let (_, body) = post(port, "secret", None, &json!({ "jsonrpc": "2.0", "id": 4, "method": "nope" }));
        assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["error"]["code"], -32601);

        // Security: the key, and no web page.
        assert_eq!(post(port, "wrong", None, &json!({ "jsonrpc": "2.0", "id": 5, "method": "ping" })).0, "HTTP/1.1 401 Unauthorized");
        assert_eq!(post(port, "secret", Some("https://evil.example"), &json!({ "jsonrpc": "2.0", "id": 6, "method": "ping" })).0, "HTTP/1.1 403 Forbidden");
        assert_eq!(post(port, "secret", Some("http://localhost:3000"), &json!({ "jsonrpc": "2.0", "id": 7, "method": "ping" })).0, "HTTP/1.1 200 OK");
        // DNS rebinding: a page of evil.example served from 127.0.0.1 still names its own host.
        assert_eq!(post_as(port, "evil.example:47642", "secret", None, &json!({ "jsonrpc": "2.0", "id": 8, "method": "ping" })).0, "HTTP/1.1 403 Forbidden");
        assert_eq!(post_as(port, "localhost", "secret", None, &json!({ "jsonrpc": "2.0", "id": 9, "method": "ping" })).0, "HTTP/1.1 200 OK");
        assert_eq!(post_as(port, "[::1]:47642", "secret", None, &json!({ "jsonrpc": "2.0", "id": 10, "method": "ping" })).0, "HTTP/1.1 200 OK");
    }

    #[test]
    fn client_setups() {
        let all = clients(7650, "KEY123");
        assert!(all.len() >= 6);
        for c in &all {
            assert!(c.snippet.contains("http://127.0.0.1:7650/mcp"), "{}", c.name);
            assert!(c.snippet.contains("Bearer KEY123"), "{}: key written in", c.name);
            if c.snippet.trim_start().starts_with('{') {
                serde_json::from_str::<Value>(&c.snippet).unwrap_or_else(|e| panic!("{}: {e}", c.name));
            }
        }
        assert!(all[0].snippet.starts_with("claude mcp add --transport http"));
    }

    #[test]
    fn tokens_differ() {
        let (a, b) = (new_token(), new_token());
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }
}
