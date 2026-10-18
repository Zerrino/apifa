//! How a request authenticates: nothing, HTTP Basic, a Bearer token, an API key (header or query),
//! or OAuth 2.0 (client credentials, password, authorization code with PKCE; tokens cached, refreshed).
//!
//! A request's auth is `Inherit` by default: it takes its folder's, else the workspace's (Settings).
//! Every text field is a template (`{{password}}` from the environment), filled just before sending.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::Kv;

fn bearer() -> String {
    "Bearer".into()
}

fn yes() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyLocation {
    #[default]
    Header,
    Query,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grant {
    #[default]
    ClientCredentials,
    Password,
    AuthorizationCode,
}

impl Grant {
    pub const ALL: [Grant; 3] = [Grant::ClientCredentials, Grant::Password, Grant::AuthorizationCode];

    pub fn label(self) -> &'static str {
        match self {
            Grant::ClientCredentials => "Client credentials",
            Grant::Password => "Password",
            Grant::AuthorizationCode => "Authorization code (browser)",
        }
    }
}

/// How the client proves who it is to the token endpoint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientAuth {
    /// `client_id` / `client_secret` in the form.
    #[default]
    Body,
    /// HTTP Basic with them.
    Header,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OAuth2 {
    pub grant: Grant,
    pub token_url: String,
    /// Authorization code: the page the browser opens.
    pub auth_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub scope: String,
    pub audience: String,
    /// Password grant.
    pub username: String,
    pub password: String,
    pub client_auth: ClientAuth,
    /// Authorization code: PKCE (S256).
    pub pkce: bool,
    /// Authorization code: the local port the browser comes back to (`0` = any free one;
    /// the redirect URI is `http://127.0.0.1:<port>/callback`).
    pub redirect_port: u16,
    /// `Authorization: <prefix> <token>`.
    pub header_prefix: String,
    /// More fields of the token request (`resource`…).
    pub extra: Vec<Kv>,
}

impl Default for OAuth2 {
    fn default() -> Self {
        Self {
            grant: Grant::default(),
            token_url: String::new(),
            auth_url: String::new(),
            client_id: String::new(),
            client_secret: String::new(),
            scope: String::new(),
            audience: String::new(),
            username: String::new(),
            password: String::new(),
            client_auth: ClientAuth::default(),
            pkce: yes(),
            redirect_port: 0,
            header_prefix: bearer(),
            extra: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Auth {
    /// The folder's, else the workspace's.
    #[default]
    Inherit,
    None,
    Basic {
        #[serde(default)]
        username: String,
        #[serde(default)]
        password: String,
    },
    Bearer {
        #[serde(default)]
        token: String,
        #[serde(default = "bearer")]
        prefix: String,
    },
    ApiKey {
        #[serde(default)]
        key: String,
        #[serde(default)]
        value: String,
        #[serde(default)]
        location: KeyLocation,
    },
    #[serde(rename = "oauth2")]
    OAuth2(OAuth2),
}

impl Auth {
    /// The kinds offered in a picker, with their labels.
    pub fn kinds(with_inherit: bool) -> Vec<(&'static str, Auth)> {
        let mut v = Vec::new();
        if with_inherit {
            v.push(("Inherit", Auth::Inherit));
        }
        v.extend([
            ("No auth", Auth::None),
            ("Basic", Auth::Basic { username: String::new(), password: String::new() }),
            ("Bearer token", Auth::Bearer { token: String::new(), prefix: bearer() }),
            ("API key", Auth::ApiKey { key: "X-API-Key".into(), value: String::new(), location: KeyLocation::Header }),
            ("OAuth 2.0", Auth::OAuth2(OAuth2::default())),
        ]);
        v
    }

    pub fn label(&self) -> &'static str {
        match self {
            Auth::Inherit => "Inherit",
            Auth::None => "No auth",
            Auth::Basic { .. } => "Basic",
            Auth::Bearer { .. } => "Bearer token",
            Auth::ApiKey { .. } => "API key",
            Auth::OAuth2(_) => "OAuth 2.0",
        }
    }

    pub fn same_kind(&self, other: &Auth) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }

    pub fn is_inherit(&self) -> bool {
        matches!(self, Auth::Inherit)
    }

    /// This one, or `parent` when it inherits.
    pub fn or<'a>(&'a self, parent: &'a Auth) -> &'a Auth {
        if self.is_inherit() {
            parent
        } else {
            self
        }
    }

    /// Every text field through `f` (templates: environment values, dates).
    pub fn expand(&self, f: &dyn Fn(&str) -> String) -> Auth {
        match self {
            Auth::Inherit | Auth::None => self.clone(),
            Auth::Basic { username, password } => Auth::Basic { username: f(username), password: f(password) },
            Auth::Bearer { token, prefix } => Auth::Bearer { token: f(token), prefix: prefix.clone() },
            Auth::ApiKey { key, value, location } => Auth::ApiKey { key: f(key), value: f(value), location: *location },
            Auth::OAuth2(o) => Auth::OAuth2(OAuth2 {
                token_url: f(&o.token_url),
                auth_url: f(&o.auth_url),
                client_id: f(&o.client_id),
                client_secret: f(&o.client_secret),
                scope: f(&o.scope),
                audience: f(&o.audience),
                username: f(&o.username),
                password: f(&o.password),
                extra: o.extra.iter().map(|kv| Kv { enabled: kv.enabled, key: kv.key.clone(), value: f(&kv.value) }).collect(),
                ..o.clone()
            }),
        }
    }

    /// The same with its secrets emptied (what goes to a file when the keychain keeps them).
    pub fn without_secrets(&self) -> Auth {
        let mut a = self.clone();
        match &mut a {
            Auth::Basic { password, .. } => password.clear(),
            Auth::Bearer { token, .. } => token.clear(),
            Auth::ApiKey { value, .. } => value.clear(),
            Auth::OAuth2(o) => {
                o.client_secret.clear();
                o.password.clear();
            }
            Auth::Inherit | Auth::None => {}
        }
        a
    }

    /// Secrets left empty here are taken from `from` (the keychain's copy), when it's the same kind.
    pub fn fill_secrets(&mut self, from: &Auth) {
        let take = |mine: &mut String, theirs: &String| {
            if mine.is_empty() {
                *mine = theirs.clone();
            }
        };
        match (self, from) {
            (Auth::Basic { password, .. }, Auth::Basic { password: p, .. }) => take(password, p),
            (Auth::Bearer { token, .. }, Auth::Bearer { token: t, .. }) => take(token, t),
            (Auth::ApiKey { value, .. }, Auth::ApiKey { value: v, .. }) => take(value, v),
            (Auth::OAuth2(o), Auth::OAuth2(p)) => {
                take(&mut o.client_secret, &p.client_secret);
                take(&mut o.password, &p.password);
            }
            _ => {}
        }
    }

    pub fn has_secrets(&self) -> bool {
        self.without_secrets() != *self
    }
}

// ---------------------------------------------------------------- tokens

pub struct Token {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Wall-clock time (`Instant` stops while the computer sleeps); None = the server said nothing.
    pub expires_at: Option<SystemTime>,
    fetched_at: SystemTime,
}

impl Token {
    pub fn remaining(&self) -> Option<Duration> {
        self.expires_at.map(|e| e.duration_since(SystemTime::now()).unwrap_or_default())
    }

    /// Good for a request now: not in its last 30 s, unless it was just fetched.
    fn usable(&self) -> bool {
        let Some(left) = self.remaining() else { return true };
        let fresh = SystemTime::now().duration_since(self.fetched_at).is_ok_and(|age| age < Duration::from_secs(30));
        left > Duration::from_secs(30) || (fresh && left > Duration::from_secs(2))
    }
}

#[derive(Default, Clone)]
pub enum Status {
    #[default]
    Idle,
    Fetching,
    Ready,
    Failed(String),
}

/// The OAuth tokens got so far, one per configuration (token URL, client, grant, scope…), and how
/// the last fetch went.
#[derive(Default)]
pub struct Tokens {
    map: HashMap<String, Token>,
    pub status: Status,
    /// The last token handed out, for the title bar (masked there).
    pub last: Option<(String, Option<SystemTime>)>,
}

impl Tokens {
    pub fn forget(&mut self) {
        self.map.clear();
        self.last = None;
        self.status = Status::Idle;
    }
}

pub type SharedTokens = Arc<Mutex<Tokens>>;

fn fingerprint(o: &OAuth2) -> String {
    let extra: Vec<String> = o.extra.iter().filter(|kv| kv.enabled).map(|kv| format!("{}={}", kv.key, kv.value)).collect();
    format!(
        "{:?}{:?}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
        o.grant,
        o.client_auth,
        o.token_url.trim(),
        o.client_id.trim(),
        o.client_secret,
        o.scope,
        o.audience,
        o.username,
        extra.join("&")
    )
}

/// Base64 (standard, padded), for HTTP Basic.
pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn base64url(bytes: &[u8]) -> String {
    base64(bytes).trim_end_matches('=').replace('+', "-").replace('/', "_")
}

fn random_text(len: usize) -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    while out.len() < len {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out.truncate(len);
    out
}

fn error_summary(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        for key in ["error_description", "message", "error"] {
            if let Some(s) = v.get(key).and_then(Value::as_str) {
                return s.to_owned();
            }
        }
    }
    let t: String = body.chars().take(200).collect();
    if t.is_empty() {
        "empty response".into()
    } else {
        t
    }
}

/// POSTs the token request (form), with the client's credentials as configured.
fn token_request(agent: &ureq::Agent, o: &OAuth2, mut form: Vec<(String, String)>) -> Result<Token, String> {
    let url = o.token_url.trim();
    if url.is_empty() {
        return Err("OAuth 2.0: the token URL is empty".into());
    }
    let mut req = agent.post(url).header("Accept", "application/json");
    match o.client_auth {
        ClientAuth::Header => {
            req = req.header("Authorization", format!("Basic {}", base64(format!("{}:{}", o.client_id.trim(), o.client_secret).as_bytes())));
        }
        ClientAuth::Body => {
            form.push(("client_id".into(), o.client_id.trim().to_owned()));
            if !o.client_secret.is_empty() {
                form.push(("client_secret".into(), o.client_secret.clone()));
            }
        }
    }
    if !o.scope.trim().is_empty() {
        form.push(("scope".into(), o.scope.trim().to_owned()));
    }
    if !o.audience.trim().is_empty() {
        form.push(("audience".into(), o.audience.trim().to_owned()));
    }
    form.extend(o.extra.iter().filter(|kv| kv.enabled && !kv.key.trim().is_empty()).map(|kv| (kv.key.trim().to_owned(), kv.value.clone())));
    let mut resp = req.send_form(form.iter().map(|(k, v)| (k.as_str(), v.as_str()))).map_err(|e| e.to_string())?;
    let code = resp.status().as_u16();
    let body = resp.body_mut().read_to_string().unwrap_or_default();
    if code >= 400 {
        return Err(format!("token endpoint answered HTTP {code}: {}", error_summary(&body)));
    }
    // Some servers answer a form (GitHub without Accept: JSON).
    let v: Value = serde_json::from_str(&body).unwrap_or_else(|_| {
        Value::Object(body.split('&').filter_map(|p| p.split_once('=')).map(|(k, v)| (k.to_owned(), Value::from(crate::model::decode_component(v)))).collect())
    });
    let access_token = v.get("access_token").and_then(Value::as_str).ok_or_else(|| format!("no access_token in the answer: {}", error_summary(&body)))?.to_owned();
    let expires_in = v.get("expires_in").and_then(|e| e.as_u64().or_else(|| e.as_str()?.parse().ok()));
    Ok(Token {
        access_token,
        refresh_token: v.get("refresh_token").and_then(Value::as_str).map(str::to_owned),
        expires_at: expires_in.map(|s| SystemTime::now() + Duration::from_secs(s)),
        fetched_at: SystemTime::now(),
    })
}

/// Authorization code: the browser opens the authorization page, the answer comes back to a local
/// listener, the code is exchanged (with the PKCE verifier).
fn authorization_code(agent: &ureq::Agent, o: &OAuth2) -> Result<Token, String> {
    if o.auth_url.trim().is_empty() {
        return Err("OAuth 2.0: the authorization URL is empty".into());
    }
    let listener = std::net::TcpListener::bind(("127.0.0.1", o.redirect_port)).map_err(|e| format!("can't listen on port {}: {e}", o.redirect_port))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect = format!("http://127.0.0.1:{port}/callback");
    let state = random_text(24);
    let verifier = random_text(64);
    let enc = crate::model::encode_component;
    let mut url = format!(
        "{}{}response_type=code&client_id={}&redirect_uri={}&state={state}",
        o.auth_url.trim(),
        if o.auth_url.contains('?') { "&" } else { "?" },
        enc(o.client_id.trim()),
        enc(&redirect)
    );
    if !o.scope.trim().is_empty() {
        url.push_str(&format!("&scope={}", enc(o.scope.trim())));
    }
    if !o.audience.trim().is_empty() {
        url.push_str(&format!("&audience={}", enc(o.audience.trim())));
    }
    if o.pkce {
        use sha2::{Digest, Sha256};
        url.push_str(&format!("&code_challenge={}&code_challenge_method=S256", base64url(&Sha256::digest(verifier.as_bytes()))));
    }
    open_url(&url)?;
    // Wait for the browser (3 minutes at most).
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let deadline = std::time::Instant::now() + Duration::from_secs(180);
    let (mut stream, _) = loop {
        match listener.accept() {
            Ok(s) => break s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() > deadline {
                    return Err("OAuth 2.0: no answer from the browser within 3 minutes".into());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    stream.set_nonblocking(false).ok();
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).map_err(|e| e.to_string())?;
    let target = line.split_whitespace().nth(1).unwrap_or_default();
    let query = target.split_once('?').map(|(_, q)| q).unwrap_or_default();
    let param = |name: &str| query.split('&').filter_map(|p| p.split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| crate::model::decode_component(v));
    let page = |ok: bool, text: &str| {
        let html = format!(
            "<!doctype html><meta charset=utf-8><title>apifa</title><body style=\"font-family:sans-serif;padding:40px\"><h2>{}</h2><p>{text}</p>",
            if ok { "Signed in" } else { "Sign-in failed" }
        );
        format!("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}", html.len())
    };
    let result = (|| {
        if let Some(e) = param("error") {
            return Err(format!("the authorization server said: {e} {}", param("error_description").unwrap_or_default()));
        }
        if param("state").as_deref() != Some(state.as_str()) {
            return Err("the answer's state doesn't match (not the request we sent)".to_owned());
        }
        let code = param("code").ok_or("no code in the answer")?;
        let mut form = vec![("grant_type".to_owned(), "authorization_code".to_owned()), ("code".to_owned(), code), ("redirect_uri".to_owned(), redirect.clone())];
        if o.pkce {
            form.push(("code_verifier".into(), verifier.clone()));
        }
        token_request(agent, o, form)
    })();
    let _ =
        stream.write_all(page(result.is_ok(), if result.is_ok() { "You can close this tab and go back to apifa." } else { "Go back to apifa to see why." }).as_bytes());
    result
}

/// Opens `url` in the default browser.
pub fn open_url(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(url).spawn().map(|_| ()).map_err(|e| format!("can't open the browser: {e}"))
}

fn fetch(agent: &ureq::Agent, o: &OAuth2, refresh: Option<&str>) -> Result<Token, String> {
    if let Some(rt) = refresh {
        let t = token_request(agent, o, vec![("grant_type".into(), "refresh_token".into()), ("refresh_token".into(), rt.to_owned())]);
        if let Ok(mut t) = t {
            if t.refresh_token.is_none() {
                t.refresh_token = Some(rt.to_owned());
            }
            return Ok(t);
        }
    }
    match o.grant {
        Grant::ClientCredentials => token_request(agent, o, vec![("grant_type".into(), "client_credentials".into())]),
        Grant::Password => token_request(
            agent,
            o,
            vec![("grant_type".into(), "password".into()), ("username".into(), o.username.trim().to_owned()), ("password".into(), o.password.clone())],
        ),
        Grant::AuthorizationCode => authorization_code(agent, o),
    }
}

/// What a fetch should do with the cached token.
#[derive(Clone)]
pub enum Refresh {
    IfExpired,
    Always,
    /// This token got a 401: replace it, unless another thread already did.
    Replacing(String),
}

/// A valid access token for `o`, fetched (or refreshed) when needed.
pub fn token(agent: &ureq::Agent, tokens: &SharedTokens, o: &OAuth2, refresh: &Refresh) -> Result<String, String> {
    // One fetch at a time: the workers of a parallel loop whose token expired wait for the first one's.
    static FETCH: Mutex<()> = Mutex::new(());
    let fp = fingerprint(o);
    let usable = |t: &Tokens| {
        let cached = t.map.get(&fp).filter(|t| t.usable()).map(|t| t.access_token.clone());
        match refresh {
            Refresh::IfExpired => cached,
            Refresh::Always => None,
            Refresh::Replacing(old) => cached.filter(|t| t != old),
        }
    };
    if let Some(t) = usable(&tokens.lock()) {
        return Ok(t);
    }
    let _guard = FETCH.lock();
    let refresh_token = {
        let mut t = tokens.lock();
        if !matches!(refresh, Refresh::Always) {
            if let Some(tok) = usable(&t) {
                return Ok(tok);
            }
        }
        t.status = Status::Fetching;
        t.map.get(&fp).and_then(|t| t.refresh_token.clone())
    };
    let result = fetch(agent, o, refresh_token.as_deref());
    let mut t = tokens.lock();
    match result {
        Ok(tok) => {
            let at = tok.access_token.clone();
            t.last = Some((at.clone(), tok.expires_at));
            t.map.insert(fp, tok);
            t.status = Status::Ready;
            Ok(at)
        }
        Err(e) => {
            t.map.remove(&fp);
            t.status = Status::Failed(e.clone());
            Err(format!("OAuth 2.0: {e}"))
        }
    }
}

/// What `auth` adds to a request: headers, and query pairs (an API key in the query). A header
/// the request already sets (`Authorization`…) is left as it is. OAuth 2.0 fetches its token here.
pub fn apply(agent: &ureq::Agent, tokens: &SharedTokens, auth: &Auth, headers: &[(String, String)], refresh: &Refresh) -> Result<(Pairs, Pairs), String> {
    let has = |name: &str| headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name));
    let mut h = Vec::new();
    let mut q = Vec::new();
    let mut header = |k: &str, v: String| {
        if !has(k) {
            h.push((k.to_owned(), v));
        }
    };
    match auth {
        Auth::Inherit | Auth::None => {}
        Auth::Basic { username, password } => header("Authorization", format!("Basic {}", base64(format!("{username}:{password}").as_bytes()))),
        Auth::Bearer { token, prefix } => {
            if !token.trim().is_empty() {
                header("Authorization", if prefix.trim().is_empty() { token.trim().to_owned() } else { format!("{} {}", prefix.trim(), token.trim()) });
            }
        }
        Auth::ApiKey { key, value, location } => {
            if !key.trim().is_empty() {
                match location {
                    KeyLocation::Header => header(key.trim(), value.clone()),
                    KeyLocation::Query => q.push((key.trim().to_owned(), value.clone())),
                }
            }
        }
        Auth::OAuth2(o) => {
            if !has("Authorization") {
                let t = token(agent, tokens, o, refresh)?;
                let prefix = o.header_prefix.trim();
                h.push(("Authorization".into(), if prefix.is_empty() { t } else { format!("{prefix} {t}") }));
            }
        }
    }
    Ok((h, q))
}

/// Name / value pairs (headers, query).
pub type Pairs = Vec<(String, String)>;

/// The default auth (Settings › Authentication), for requests and flows whose auth is `Inherit`
/// and whose folders say nothing; set by the app and the CLI.
static DEFAULT: parking_lot::RwLock<Auth> = parking_lot::RwLock::new(Auth::None);

pub fn set_default(a: Auth) {
    *DEFAULT.write() = if a.is_inherit() { Auth::None } else { a };
}

pub fn default_auth() -> Auth {
    DEFAULT.read().clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_like_the_rfc() {
        for (i, o) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")] {
            assert_eq!(base64(i.as_bytes()), o);
        }
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }

    #[test]
    fn simple_kinds_add_headers_or_query() {
        let (agent, tokens) = (crate::net::agent(), SharedTokens::default());
        let apply = |a: &Auth, existing: &[(String, String)]| apply(&agent, &tokens, a, existing, &Refresh::IfExpired).unwrap();
        let (h, _) = apply(&Auth::Basic { username: "Aladdin".into(), password: "open sesame".into() }, &[]);
        assert_eq!(h, [("Authorization".to_owned(), "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==".to_owned())]);
        let (h, _) = apply(&Auth::Bearer { token: "abc".into(), prefix: "Bearer".into() }, &[]);
        assert_eq!(h[0].1, "Bearer abc");
        let (h, q) = apply(&Auth::ApiKey { key: "api_key".into(), value: "k".into(), location: KeyLocation::Query }, &[]);
        assert!(h.is_empty() && q == [("api_key".to_owned(), "k".to_owned())]);
        // A header the request sets wins.
        let (h, _) = apply(&Auth::Bearer { token: "abc".into(), prefix: "Bearer".into() }, &[("authorization".into(), "Token x".into())]);
        assert!(h.is_empty());
        assert!(apply(&Auth::None, &[]).0.is_empty());
    }

    #[test]
    fn templates_and_secrets() {
        let a = Auth::Basic { username: "{{user}}".into(), password: "{{pass}}".into() };
        let e = a.expand(&|s: &str| s.replace("{{user}}", "u").replace("{{pass}}", "p"));
        assert_eq!(e, Auth::Basic { username: "u".into(), password: "p".into() });
        let mut file = e.without_secrets();
        assert_eq!(file, Auth::Basic { username: "u".into(), password: String::new() });
        file.fill_secrets(&e);
        assert_eq!(file, e);
        let json = serde_json::to_string(&Auth::OAuth2(OAuth2::default())).unwrap();
        assert!(json.contains("\"type\":\"oauth2\""), "{json}");
        let back: Auth = serde_json::from_str(r#"{"type":"api_key","key":"X","value":"v"}"#).unwrap();
        assert_eq!(back, Auth::ApiKey { key: "X".into(), value: "v".into(), location: KeyLocation::Header });
        assert_eq!(Auth::Inherit.or(&back), &back);
    }

    /// A token endpoint on 127.0.0.1: client credentials (form, then Basic), the token cached, a
    /// refresh after a 401.
    #[test]
    fn oauth_client_credentials_and_refresh() {
        use std::io::Read;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/token", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen2 = seen.clone();
        std::thread::spawn(move || {
            for (n, s) in listener.incoming().enumerate() {
                let mut s = s.unwrap();
                let mut buf = vec![0u8; 8192];
                let mut got = String::new();
                loop {
                    let k = s.read(&mut buf).unwrap();
                    got.push_str(&String::from_utf8_lossy(&buf[..k]));
                    if let Some(end) = got.find("\r\n\r\n") {
                        let len: usize = got.lines().find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap())).unwrap_or(0);
                        if got.len() >= end + 4 + len || k == 0 {
                            break;
                        }
                    }
                }
                seen2.lock().push(got);
                let body = format!(r#"{{"access_token":"tok{n}","expires_in":3600,"refresh_token":"r{n}"}}"#);
                let _ = s.write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes(),
                );
            }
        });
        let (agent, tokens) = (crate::net::agent(), SharedTokens::default());
        let o = OAuth2 { token_url: url, client_id: "id".into(), client_secret: "sec".into(), scope: "read".into(), ..OAuth2::default() };
        assert_eq!(token(&agent, &tokens, &o, &Refresh::IfExpired).unwrap(), "tok0");
        assert_eq!(token(&agent, &tokens, &o, &Refresh::IfExpired).unwrap(), "tok0", "cached");
        let first = seen.lock()[0].clone();
        assert!(first.contains("grant_type=client_credentials") && first.contains("client_secret=sec") && first.contains("scope=read"), "{first}");
        // A 401 with tok0: the refresh token is used.
        assert_eq!(token(&agent, &tokens, &o, &Refresh::Replacing("tok0".into())).unwrap(), "tok1");
        assert!(seen.lock()[1].contains("grant_type=refresh_token") && seen.lock()[1].contains("refresh_token=r0"));
        // Basic client auth: the secret goes in the header.
        let b = OAuth2 { client_auth: ClientAuth::Header, ..o.clone() };
        token(&agent, &tokens, &b, &Refresh::IfExpired).unwrap();
        let third = seen.lock()[2].clone();
        assert!(
            third.contains(&format!("authorization: Basic {}", base64(b"id:sec"))) || third.contains(&format!("Authorization: Basic {}", base64(b"id:sec"))),
            "{third}"
        );
        assert!(!third.contains("client_secret="), "{third}");
    }
}
