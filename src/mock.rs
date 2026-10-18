//! Mock server: a local stand-in for your API (http://127.0.0.1:7643), to build and test
//! scripts, front-ends and flows without the network, the rate limit or real data.
//!
//! For `VERB /path?query` it answers, in this order:
//! 1. the mock of a saved request whose verb and path fit (`/v1/users/:id` fits `/v1/users/7`; a
//!    `:id` filled in the request must be the same; its query parameters, page ones aside, must be there),
//!    the most specific one winning — set it from a response (Save as mock) or in the request's Mock tab;
//! 2. the example response of the API documentation for that endpoint;
//! 3. the example recorded from a real call (`learned_examples.json`);
//! 4. else a 404 that says so. Every answer says where it comes from (`X-Mock-Source`).
//!
//! Lists are paginated like the API (`page[size]`, `page[number]`, `X-Total`, `X-Page`, `X-Per-Page`).
//! A mock body can use `{{path.store_id}}` (the `:store_id` of its path), `{{query.filter[username]}}`, `{{now}}`.
//! `POST /oauth/token` gives a fake token, so clients written for the real API work unchanged.
//! Only this computer can call it; browsers only from localhost pages.

use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::docs::ApiDoc;
use crate::mcp::{local_request, read_request, respond};
use crate::model::{Kv, RequestSpec};
use crate::workspace::Node;

pub const DEFAULT_PORT: u16 = 7643;

fn yes() -> bool {
    true
}

/// The answer a saved request gives on the mock server (its Mock tab).
#[derive(Clone, Serialize, Deserialize)]
pub struct Mock {
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "ok_status")]
    pub status: u16,
    /// JSON (or text); `{{path.x}}`, `{{query.x}}`, `{{now}}` are filled in.
    #[serde(default)]
    pub body: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<Kv>,
    /// Waits this long before answering (to try slow responses).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub delay_ms: u64,
}

fn ok_status() -> u16 {
    200
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl Mock {
    /// From a real response: its status and body (pretty JSON).
    pub fn from_response(status: u16, body: &str) -> Self {
        let body = serde_json::from_str::<Value>(body).ok().and_then(|v| serde_json::to_string_pretty(&v).ok()).unwrap_or_else(|| body.to_owned());
        Self { enabled: true, status, body, headers: Vec::new(), delay_ms: 0 }
    }
}

/// A saved request with a mock, as the server matches it.
#[derive(Clone)]
struct Rule {
    name: String,
    verb: String,
    /// Path segments, `None` = any value (`:id` not filled, `{{x}}`), with the variable name when there is one.
    segments: Vec<(Option<String>, Option<String>)>,
    query: Vec<(String, String)>,
    mock: Mock,
}

impl Rule {
    fn from_spec(name: &str, spec: &RequestSpec) -> Option<Vec<Rule>> {
        let mock = spec.mock.clone().filter(|m| m.enabled)?;
        let verb = spec.verb.trim().to_uppercase();
        let query = spec
            .query
            .iter()
            .filter(|q| q.enabled && !q.key.trim().is_empty() && !q.value.contains("{{"))
            .map(|q| (q.key.trim().to_owned(), q.value.trim().to_owned()))
            .collect::<Vec<_>>();
        // `/v1/users(/:id)`: with and without the optional part.
        let path = spec.path.trim().split('?').next().unwrap_or_default();
        let (mut with, mut without, mut depth) = (String::new(), String::new(), 0);
        for c in path.chars() {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                c => {
                    with.push(c);
                    if depth == 0 {
                        without.push(c);
                    }
                }
            }
        }
        let mut variants = vec![with];
        if without != variants[0] {
            variants.push(without);
        }
        Some(
            variants
                .into_iter()
                .map(|p| {
                    let segments = segments(&p)
                        .into_iter()
                        .map(|s| match s.strip_prefix(':') {
                            Some(var) => {
                                let filled = spec.path_values.get(var).map(|v| v.trim()).filter(|v| !v.is_empty() && !v.contains("{{"));
                                (filled.map(str::to_owned), Some(var.to_owned()))
                            }
                            None if s.contains("{{") => (None, None),
                            None => (Some(s), None),
                        })
                        .collect();
                    Rule { name: name.to_owned(), verb: verb.clone(), segments, query: query.clone(), mock: mock.clone() }
                })
                .collect(),
        )
    }

    /// How well it fits (higher = more specific), with the path variables it captured.
    fn score(&self, verb: &str, path: &[String], query: &[(String, String)]) -> Option<(usize, Map<String, Value>)> {
        if self.verb != verb || self.segments.len() != path.len() {
            return None;
        }
        let mut score = 0;
        let mut vars = Map::new();
        for ((fixed, var), seg) in self.segments.iter().zip(path) {
            match fixed {
                Some(f) if f.eq_ignore_ascii_case(seg) => score += 10,
                Some(_) => return None,
                None => {}
            }
            if let Some(v) = var {
                vars.insert(v.clone(), Value::String(seg.clone()));
            }
        }
        for (k, v) in &self.query {
            if k == "page[size]" || k == "page[number]" {
                continue;
            }
            // A mock made for `filter[login]=bob` answers only that.
            match query.iter().find(|(qk, _)| qk == k) {
                Some((_, qv)) if qv == v => score += 3,
                _ => return None,
            }
        }
        Some((score, vars))
    }
}

fn segments(path: &str) -> Vec<String> {
    path.split('/').filter(|s| !s.is_empty()).map(str::to_owned).collect()
}

/// `%5B` → `[`, `+` → space.
fn decode(s: &str) -> String {
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push(h * 16 + l);
                    i += 3;
                    continue;
                }
                _ => out.push(b'%'),
            },
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&').filter(|p| !p.is_empty()).map(|p| p.split_once('=').map_or((decode(p), String::new()), |(k, v)| (decode(k), decode(v)))).collect()
}

/// The mocks of the workspace, read again when its files change (checked at most every second).
struct Rules {
    list: Vec<Rule>,
    signature: u64,
    checked: Option<Instant>,
}

impl Rules {
    fn refresh(&mut self) {
        if self.checked.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            return;
        }
        self.checked = Some(Instant::now());
        let sig = crate::workspace_fs::signature();
        if sig == self.signature && self.signature != 0 {
            return;
        }
        self.signature = sig;
        self.list = crate::workspace_fs::load().map(|l| rules_of(&Node::Folder(l.root))).unwrap_or_default();
    }
}

fn rules_of(node: &Node) -> Vec<Rule> {
    match node {
        Node::Folder(f) => f.items.iter().flat_map(rules_of).collect(),
        Node::Request(r) => Rule::from_spec(&r.name, &r.spec).unwrap_or_default(),
        Node::Flow(_) => Vec::new(),
    }
}

/// One answer of the mock server.
#[derive(Debug, Clone)]
pub struct Answer {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
    /// `saved request "Users"` (a header: ASCII quotes), `docs example`, `recorded example`, `none`.
    pub source: String,
    pub delay: Duration,
}

/// What the server knows: the workspace's mocks (`None`: read from the workspace folder), the docs.
pub struct Mocks {
    rules: Mutex<Rules>,
    fixed: Option<Vec<Rule>>,
    doc: Arc<ApiDoc>,
}

impl Mocks {
    pub fn new(doc: Arc<ApiDoc>) -> Self {
        Self { rules: Mutex::new(Rules { list: Vec::new(), signature: 0, checked: None }), fixed: None, doc }
    }

    #[cfg(test)]
    fn with(requests: &[(&str, RequestSpec)], doc: Arc<ApiDoc>) -> Self {
        let fixed = requests.iter().flat_map(|(n, s)| Rule::from_spec(n, s).unwrap_or_default()).collect();
        Self { fixed: Some(fixed), ..Self::new(doc) }
    }

    /// The answer to `verb target` (`target` = path and query, as in the request line).
    pub fn answer(&self, verb: &str, target: &str) -> Answer {
        let verb = verb.to_uppercase();
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let path = decode(path);
        let query = parse_query(query);
        if path == "/oauth/token" && verb == "POST" {
            let body = json!({ "access_token": "mock-token", "token_type": "bearer", "expires_in": 7200, "scope": "public", "created_at": crate::storage::now(), "secret_valid_until": crate::storage::now() + 7200 * 24 });
            return Answer { status: 200, headers: Vec::new(), body: body.to_string(), source: "fake token".into(), delay: Duration::ZERO };
        }
        if path == "/oauth/token/info" {
            let body = json!({ "resource_owner_id": null, "scopes": ["public"], "expires_in_seconds": 7200, "application": { "uid": "mock" } });
            return Answer { status: 200, headers: Vec::new(), body: body.to_string(), source: "fake token".into(), delay: Duration::ZERO };
        }
        let segs = segments(&path);
        let best = {
            let mut guard = self.rules.lock();
            let list = match &self.fixed {
                Some(f) => f,
                None => {
                    guard.refresh();
                    &guard.list
                }
            };
            list.iter().filter_map(|r| r.score(&verb, &segs, &query).map(|(s, vars)| (s, vars, r.clone()))).max_by_key(|(s, _, _)| *s)
        };
        let mut query_vars = Map::new();
        for (k, v) in &query {
            query_vars.insert(k.clone(), Value::String(v.clone()));
        }
        if let Some((_, path_vars, rule)) = best {
            let mut vars = Map::new();
            vars.insert("path".into(), Value::Object(path_vars));
            vars.insert("query".into(), Value::Object(query_vars));
            let body = crate::flow::render_lenient(&rule.mock.body, &vars);
            let headers = rule.mock.headers.iter().filter(|h| h.enabled && !h.key.trim().is_empty()).map(|h| (h.key.trim().to_owned(), h.value.clone())).collect();
            let a =
                Answer { status: rule.mock.status, headers, body, source: format!("saved request \"{}\"", rule.name), delay: Duration::from_millis(rule.mock.delay_ms) };
            return paginate(a, &query);
        }
        if let Some(d) = self.doc.match_route(&verb, &path) {
            if let Some((_, m)) = self.doc.lookup(&d) {
                let documented = m
                    .examples
                    .iter()
                    .filter(|e| e.verb.eq_ignore_ascii_case(&verb))
                    .find_map(|e| Some((e.code.parse().unwrap_or(200), e.response.clone().filter(|r| !r.trim().is_empty())?)));
                if let Some((status, body)) = documented {
                    return paginate(Answer { status, headers: Vec::new(), body, source: "docs example".into(), delay: Duration::ZERO }, &query);
                }
                let learned = crate::learned::Examples::load();
                if let Some(l) = learned.get(&d) {
                    return paginate(
                        Answer { status: l.status, headers: Vec::new(), body: l.response.clone(), source: "recorded example".into(), delay: Duration::ZERO },
                        &query,
                    );
                }
            }
        }
        let body = json!({
            "error": "Not Found",
            "message": format!("No mock for {verb} {path}: save a response as mock (response › Save as mock) or fill a saved request's Mock tab"),
        });
        Answer { status: 404, headers: Vec::new(), body: body.to_string(), source: "none".into(), delay: Duration::ZERO }
    }
}

/// A JSON list is cut like the API does: `page[size]` (30 by default, at most 100), `page[number]`.
fn paginate(mut a: Answer, query: &[(String, String)]) -> Answer {
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&a.body) else { return a };
    let get = |k: &str| query.iter().find(|(qk, _)| qk == k).and_then(|(_, v)| v.parse::<usize>().ok());
    let size = get("page[size]").unwrap_or(30).clamp(1, 100);
    let page = get("page[number]").unwrap_or(1).max(1);
    let total = items.len();
    let slice: Vec<Value> = items.into_iter().skip((page - 1).saturating_mul(size)).take(size).collect();
    a.body = Value::Array(slice).to_string();
    for (k, v) in [("X-Total", total), ("X-Page", page), ("X-Per-Page", size)] {
        if !a.headers.iter().any(|(h, _)| h.eq_ignore_ascii_case(k)) {
            a.headers.push((k.into(), v.to_string()));
        }
    }
    a
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "",
    }
}

/// What the settings show, and what the CLI prints.
#[derive(Clone, Default)]
pub struct Status {
    pub served: u64,
    /// When, `GET /v1/users → 200 (saved request "Users")`.
    pub last: Option<(Instant, String)>,
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

impl Server {
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

/// Listens on 127.0.0.1:`port` (`0` = any free port, see `Server::port`). `log` gets one line per request.
pub fn start(port: u16, mocks: Arc<Mocks>, log: impl Fn(&str) + Send + Sync + 'static) -> Result<Server, String> {
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("can't listen on 127.0.0.1:{port}: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let stop = Arc::new(AtomicBool::new(false));
    let status = Arc::new(Mutex::new(Status::default()));
    let (stop2, status2) = (stop.clone(), status.clone());
    let log: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(log);
    let thread = std::thread::spawn(move || {
        while !stop2.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let (mocks, status, log) = (mocks.clone(), status2.clone(), log.clone());
                    std::thread::spawn(move || {
                        let _ = stream.set_nonblocking(false);
                        let _ = handle(stream, &mocks, &status, &*log);
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    });
    Ok(Server { port, stop, status, listener: Some(thread) })
}

fn handle(mut stream: TcpStream, mocks: &Mocks, status: &Mutex<Status>, log: &(dyn Fn(&str) + Send + Sync)) -> std::io::Result<()> {
    let req = match read_request(&mut stream) {
        Ok(r) => r,
        Err(e) => return respond(&mut stream, "400 Bad Request", &[], &json!({ "error": e }).to_string()),
    };
    // Pages on other sites may not read it (it can hold real responses saved as mocks).
    let origin = req.header("origin").map(str::to_owned);
    if !local_request(&req) {
        return respond(&mut stream, "403 Forbidden", &[], &json!({ "error": "origin not allowed (only pages served from localhost)" }).to_string());
    }
    let mut cors = Vec::new();
    if let Some(o) = &origin {
        cors.push(("Access-Control-Allow-Origin", o.clone()));
        cors.push(("Access-Control-Expose-Headers", "X-Total, X-Page, X-Per-Page, X-Mock-Source".into()));
    }
    if req.method == "OPTIONS" {
        cors.push(("Access-Control-Allow-Methods", "GET, POST, PUT, PATCH, DELETE".into()));
        cors.push(("Access-Control-Allow-Headers", "Authorization, Content-Type".into()));
        return respond(&mut stream, "204 No Content", &cors, "");
    }
    let a = mocks.answer(&req.method, &req.path);
    if !a.delay.is_zero() {
        std::thread::sleep(a.delay.min(Duration::from_secs(60)));
    }
    let line = format!("{} {} → {} ({})", req.method, req.path, a.status, a.source);
    {
        let mut s = status.lock();
        s.served += 1;
        s.last = Some((Instant::now(), line.clone()));
    }
    log(&line);
    let mut headers: Vec<(&str, String)> = a.headers.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    headers.push(("X-Mock-Source", a.source.clone()));
    headers.extend(cors);
    respond(&mut stream, &format!("{} {}", a.status, reason(a.status)), &headers, &a.body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn spec(verb: &str, path: &str, values: &[(&str, &str)], query: &[(&str, &str)], body: &str) -> RequestSpec {
        RequestSpec {
            verb: verb.into(),
            path: path.into(),
            path_values: values.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<BTreeMap<_, _>>(),
            query: query.iter().map(|(k, v)| Kv { enabled: true, key: k.to_string(), value: v.to_string() }).collect(),
            mock: Some(Mock::from_response(200, body)),
            ..Default::default()
        }
    }

    #[test]
    fn most_specific_mock_wins() {
        let mocks = Mocks::with(
            &[
                ("Any user", spec("GET", "/v1/users/:id", &[], &[], r#"{"id": "{{path.id}}", "login": "anyone"}"#)),
                ("jdoe", spec("GET", "/v1/users/:id", &[("id", "jdoe")], &[], r#"{"login": "jdoe"}"#)),
                (
                    "Store users",
                    spec("GET", "/v1/stores/:store_id/users", &[], &[], &serde_json::to_string(&(1..=45).map(|i| json!({ "id": i })).collect::<Vec<_>>()).unwrap()),
                ),
                ("Filtered", spec("GET", "/v1/stores/:store_id/users", &[], &[("filter[login]", "bob")], r#"[{"login":"bob"}]"#)),
            ],
            Arc::new(ApiDoc::empty()),
        );
        let a = mocks.answer("GET", "/v1/users/jdoe");
        assert_eq!(a.source, "saved request \"jdoe\"");
        let a = mocks.answer("get", "/v1/users/7");
        assert_eq!(a.source, "saved request \"Any user\"");
        assert_eq!(serde_json::from_str::<Value>(&a.body).unwrap()["id"], "7", "{{path.id}} is filled");

        // Lists are paginated like the API.
        let a = mocks.answer("GET", "/v1/stores/12/users?page%5Bsize%5D=20&page[number]=3");
        let page: Vec<Value> = serde_json::from_str(&a.body).unwrap();
        assert_eq!(page.len(), 5);
        assert_eq!(page[0]["id"], 41);
        assert!(a.headers.contains(&("X-Total".into(), "45".into())));
        // A matching query parameter makes a mock more specific; a different one rules it out.
        assert_eq!(mocks.answer("GET", "/v1/stores/12/users?filter[login]=bob").source, "saved request \"Filtered\"");
        assert_eq!(mocks.answer("GET", "/v1/stores/12/users?filter[login]=ann").source, "saved request \"Store users\"");

        // Nothing: a 404 that says why; the fake token.
        let a = mocks.answer("DELETE", "/v1/users/1");
        assert_eq!((a.status, a.source.as_str()), (404, "none"));
        assert_eq!(mocks.answer("POST", "/oauth/token").status, 200);
    }

    #[test]
    fn docs_examples_answer_too() {
        let doc = Arc::new(ApiDoc::load().unwrap());
        let mocks = Mocks::with(&[], doc);
        let a = mocks.answer("GET", "/v1/stores");
        assert_eq!(a.source, "docs example", "{a:?}");
        assert!(serde_json::from_str::<Value>(&a.body).is_ok());
    }

    #[test]
    fn over_http() {
        let mocks = Arc::new(Mocks::with(&[("Me", spec("GET", "/v1/users/:id", &[], &[], r#"{"login":"jdoe"}"#))], Arc::new(ApiDoc::empty())));
        let server = start(0, mocks, |_| {}).unwrap();
        let mut r = ureq::get(format!("{}/v1/users/7", server.url())).call().unwrap();
        assert_eq!(r.headers().get("x-mock-source").and_then(|v| v.to_str().ok()), Some("saved request \"Me\""));
        assert_eq!(serde_json::from_str::<Value>(&r.body_mut().read_to_string().unwrap()).unwrap(), json!({ "login": "jdoe" }));
        let err = ureq::get(format!("{}/v1/users/7", server.url())).header("Origin", "https://evil.example").call().unwrap_err();
        assert!(matches!(err, ureq::Error::StatusCode(403)), "{err:?}");
        assert_eq!(server.status.lock().served, 1);
    }
}
