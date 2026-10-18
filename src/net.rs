//! HTTP calls (run on worker threads), with the request's authentication (`auth.rs`).

use std::time::{Duration, Instant, SystemTime};

use parking_lot::Mutex;
use serde_json::Value;

use crate::model::{self, Prepared};

/// The OAuth tokens, shared by every worker.
pub type SharedAuth = crate::auth::SharedTokens;

/// The auth a request uses when its own (and its folders') is `Inherit`: the workspace's.
#[derive(Clone, Default)]
pub struct Credentials {
    pub auth: crate::auth::Auth,
}

pub struct ResponseData {
    pub status: u16,
    pub status_text: String,
    pub elapsed: Duration,
    pub size: usize,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub is_json: bool,
    /// What happened before the call (substitutions…), shown above the response.
    pub notes: Vec<String>,
    /// What the request's scripts did (tests, console).
    pub script: Option<crate::script::ScriptReport>,
}

impl ResponseData {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn header_u64(&self, name: &str) -> Option<u64> {
        self.header(name)?.trim().parse().ok()
    }
}

/// The HTTP agent: 4xx / 5xx are responses to show (not errors), and the token never follows a
/// redirect (ureq's default, `RedirectAuthHeaders::Never`).
pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(Some(Duration::from_secs(60)))
        .http_status_as_error(false)
        .user_agent(concat!("apifa/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// Biggest response body read (a big list with all its pages can be tens of MB).
const MAX_BODY: u64 = 64 * 1024 * 1024;

/// The agent to call with. Keep-alive connections don't survive the computer's sleep: the
/// server drops them meanwhile without the client knowing, so the next call fails (POST,
/// PATCH…) or hangs until the timeout. After a sleep (the wall clock moved further than
/// `Instant`, which stops during sleep) or a long idle time, calls start from a fresh pool.
fn live(agent: &ureq::Agent) -> ureq::Agent {
    struct Last {
        mono: Instant,
        wall: SystemTime,
        fresh: Option<ureq::Agent>,
    }
    static LAST: Mutex<Option<Last>> = Mutex::new(None);
    let (mono, wall) = (Instant::now(), SystemTime::now());
    let mut last = LAST.lock();
    let Some(l) = last.as_mut() else {
        *last = Some(Last { mono, wall, fresh: None });
        return agent.clone();
    };
    let idle = wall.duration_since(l.wall).unwrap_or_default();
    let slept = idle > mono.duration_since(l.mono) + Duration::from_secs(5);
    if slept || idle > Duration::from_secs(120) {
        l.fresh = Some(self::agent());
    }
    l.mono = mono;
    l.wall = wall;
    l.fresh.clone().unwrap_or_else(|| agent.clone())
}

fn mime_type(path: &std::path::Path) -> &'static str {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "txt" => "text/plain",
        _ => "application/octet-stream",
    }
}

/// multipart/form-data body: the JSON body's fields as text parts, then the files. Returns (Content-Type, bytes).
pub fn multipart(req: &Prepared) -> Result<(String, Vec<u8>), String> {
    let boundary = format!("----apifa{:016x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos() as u64);
    let quote = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"").replace(['\r', '\n'], " ");
    let mut out = Vec::new();
    if let Some(b) = &req.body {
        let v: Value = serde_json::from_str(b).map_err(|e| format!("body is not valid JSON: {e}"))?;
        for (k, v) in model::multipart_fields(&v) {
            out.extend(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n{v}\r\n", quote(&k)).as_bytes());
        }
    }
    for (k, path) in &req.files {
        let full = model::expand_home(path);
        let data = std::fs::read(&full).map_err(|e| format!("can't read the file for {k} ({}): {e}", full.display()))?;
        let name = full.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
        out.extend(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\nContent-Type: {}\r\n\r\n", quote(k), quote(&name), mime_type(&full))
                .as_bytes(),
        );
        out.extend(data);
        out.extend(b"\r\n");
    }
    out.extend(format!("--{boundary}--\r\n").as_bytes());
    Ok((format!("multipart/form-data; boundary={boundary}"), out))
}

static MOCK_TARGET: parking_lot::RwLock<Option<String>> = parking_lot::RwLock::new(None);

/// Sends every request to the mock server at `url` (`http://127.0.0.1:7643`) instead of the API; `None` = the API.
pub fn set_mock_target(url: Option<String>) {
    *MOCK_TARGET.write() = url;
}

pub fn mock_target() -> Option<String> {
    MOCK_TARGET.read().clone()
}

pub fn execute(agent: &ureq::Agent, tokens: &SharedAuth, creds: &Credentials, req: &Prepared) -> Result<ResponseData, String> {
    let fill = |v: &str| crate::dates::expand(&crate::env::expand(v));
    // Its own auth, else its profile's, else the workspace's.
    let profile_auth = crate::profiles::current(&req.profile).map(|p| p.auth).unwrap_or_default();
    let auth = req.auth.or(profile_auth.or(&creds.auth)).expand(&fill);
    if let Some(base) = mock_target() {
        // Nothing of yours goes there: no token, no credentials.
        let mut req = req.clone();
        req.url = format!("{base}{}", model::split_origin(&req.url).1);
        return send(agent, &req, tokens, &crate::auth::Auth::None);
    }
    if model::split_origin(&req.url).0.is_empty() {
        return Err(format!("No base URL for {}: set baseUrl in the environment, or Settings › Base URL (or write a full URL)", req.url));
    }
    send(agent, req, tokens, &auth)
}

/// The call itself, with `auth`'s headers / query; an OAuth 2.0 token answered by a 401 is
/// replaced once and the call replayed.
fn send(agent: &ureq::Agent, req: &Prepared, tokens: &SharedAuth, auth: &crate::auth::Auth) -> Result<ResponseData, String> {
    use crate::auth::Refresh;
    let upload = if req.files.is_empty() { None } else { Some(multipart(req)?) };
    let mut refresh = Refresh::IfExpired;
    loop {
        let (auth_headers, auth_query) = crate::auth::apply(agent, tokens, auth, &req.headers, &refresh)?;
        let url = if auth_query.is_empty() {
            req.url.clone()
        } else {
            let q = auth_query.iter().map(|(k, v)| format!("{}={}", model::encode_component(k), model::encode_component(v))).collect::<Vec<_>>().join("&");
            format!("{}{}{q}", req.url, if req.url.contains('?') { "&" } else { "?" })
        };
        let start = Instant::now();
        let mut r = ureq::http::Request::builder().method(req.verb.as_str()).uri(&url);
        let has = |name: &str| req.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name));
        if !has("Accept") {
            r = r.header("Accept", "application/json, */*;q=0.8");
        }
        if let Some((content_type, _)) = &upload {
            r = r.header("Content-Type", content_type);
        } else if req.body.is_some() && !has("Content-Type") {
            r = r.header("Content-Type", req.content_type.as_deref().unwrap_or("application/json"));
        }
        for (k, v) in auth_headers.iter().chain(&req.headers) {
            r = r.header(k, v);
        }
        let body: Vec<u8> = match (&upload, &req.body) {
            (Some((_, bytes)), _) => bytes.clone(),
            (None, Some(b)) => b.clone().into_bytes(),
            (None, None) => Vec::new(),
        };
        let http_req = r.body(body).map_err(|e| format!("invalid request: {e}"))?;
        let mut resp = live(agent).run(http_req).map_err(|e| e.to_string())?;
        // An OAuth token revoked or expired early: a fresh one once, and the call again.
        if resp.status().as_u16() == 401 && matches!(auth, crate::auth::Auth::OAuth2(_)) && matches!(refresh, Refresh::IfExpired) {
            if let Some((_, v)) = auth_headers.iter().find(|(k, _)| k == "Authorization") {
                refresh = Refresh::Replacing(v.rsplit(' ').next().unwrap_or_default().to_owned());
                continue;
            }
        }
        let status = resp.status().as_u16();
        let status_text = resp.status().canonical_reason().unwrap_or_default().to_owned();
        // One entry per header name, repeated values joined (as the API sends them).
        let mut headers: Vec<(String, String)> = Vec::new();
        for (name, value) in resp.headers() {
            let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
            match headers.iter_mut().find(|(n, _)| n == name.as_str()) {
                Some((_, v)) => {
                    v.push_str(", ");
                    v.push_str(&value);
                }
                None => headers.push((name.as_str().to_owned(), value)),
            }
        }
        let bytes = resp.body_mut().with_config().limit(MAX_BODY).read_to_vec().map_err(|e| format!("reading the response failed: {e}"))?;
        let elapsed = start.elapsed();
        let body = String::from_utf8_lossy(&bytes).into_owned();
        // Only checked here (no tree built): the request tab pretty-prints the body when it shows it,
        // a flow's thousands of calls don't need it.
        let is_json = serde_json::from_str::<serde::de::IgnoredAny>(&body).is_ok();
        return Ok(ResponseData { status, status_text, elapsed, size: bytes.len(), headers, body, is_json, notes: Vec::new(), script: None });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_body() {
        let dir = std::env::temp_dir().join(format!("apifa_mp_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("me.png");
        std::fs::write(&file, b"PNGDATA").unwrap();
        let req = Prepared {
            verb: "PATCH".into(),
            url: String::new(),
            display_url: String::new(),
            path: String::new(),
            query: Vec::new(),
            headers: Vec::new(),
            body: Some(r#"{"user":{"login":"jdoe"}}"#.into()),
            files: vec![("user[image]".into(), file.display().to_string())],
            missing_path: Vec::new(),
            missing_required: Vec::new(),
            body_error: None,
            ..Default::default()
        };
        let (ct, bytes) = multipart(&req).unwrap();
        let boundary = ct.strip_prefix("multipart/form-data; boundary=").unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("name=\"user[login]\"\r\n\r\njdoe\r\n"), "{text}");
        assert!(text.contains("name=\"user[image]\"; filename=\"me.png\"\r\nContent-Type: image/png\r\n\r\nPNGDATA\r\n"), "{text}");
        assert!(text.ends_with(&format!("--{boundary}--\r\n")));
        let missing = Prepared { files: vec![("f".into(), dir.join("nope.jpg").display().to_string())], ..req };
        assert!(multipart(&missing).unwrap_err().contains("can't read the file for f"));
        std::fs::remove_dir_all(dir).ok();
    }

    /// A request as the server gets it (verb, headers, JSON body), and a 4xx with repeated headers
    /// coming back as a response, not an error.
    #[test]
    fn sends_and_reads_back() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 4096];
            // Headers, then the body (Content-Length).
            loop {
                let n = s.read(&mut buf).unwrap();
                got.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&got).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let len: usize = text.lines().find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap())).unwrap_or(0);
                    if got.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            s.write_all(b"HTTP/1.1 422 Unprocessable Entity\r\nContent-Type: application/json\r\nX-Tag: a\r\nX-Tag: b\r\nContent-Length: 13\r\nConnection: close\r\n\r\n{\"error\":\"x\"}").unwrap();
            String::from_utf8(got).unwrap()
        });
        let req = Prepared {
            verb: "PATCH".into(),
            url: format!("{url}/v1/users/1"),
            display_url: String::new(),
            path: String::new(),
            query: Vec::new(),
            headers: vec![("X-Mine".into(), "1".into())],
            body: Some(r#"{"user":{"login":"jdoe"}}"#.into()),
            files: Vec::new(),
            missing_path: Vec::new(),
            missing_required: Vec::new(),
            body_error: None,
            ..Default::default()
        };
        let r = send(&agent(), &req, &SharedAuth::default(), &crate::auth::Auth::Bearer { token: "tok".into(), prefix: "Bearer".into() }).unwrap();
        let seen = server.join().unwrap();
        assert!(seen.starts_with("PATCH /v1/users/1 HTTP/1.1"), "{seen}");
        let lower = seen.to_lowercase();
        assert!(lower.contains("authorization: bearer tok") && lower.contains("content-type: application/json") && lower.contains("x-mine: 1"), "{seen}");
        assert!(seen.ends_with(r#"{"user":{"login":"jdoe"}}"#), "{seen}");
        assert_eq!((r.status, r.status_text.as_str()), (422, "Unprocessable Entity"));
        assert_eq!(r.header("x-tag"), Some("a, b"), "repeated headers joined");
        assert_eq!(r.body, r#"{"error":"x"}"#);
        assert!(r.is_json);
    }
}
