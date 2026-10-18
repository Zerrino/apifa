//! A request as edited in a tab, and how it turns into an HTTP call.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::docs::{Method, Param, ParamKind};

/// Where relative paths (`/users/:id`) go when the environment has no `baseUrl`: the workspace's
/// default (Settings), set by the app and the CLI.
static DEFAULT_BASE: parking_lot::RwLock<String> = parking_lot::RwLock::new(String::new());

pub fn set_default_base(url: &str) {
    *DEFAULT_BASE.write() = url.trim().trim_end_matches('/').to_owned();
}

/// The base URL now: the active environment's `baseUrl` (or `base_url`), else the workspace's default.
pub fn base_url() -> String {
    let vars = crate::env::current();
    vars.iter()
        .find(|(k, _)| k == "baseUrl" || k == "base_url" || k == "baseURL")
        .map(|(_, v)| v.trim().trim_end_matches('/').to_owned())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE.read().clone())
}

/// The base URL of `profile` (empty: the active one): its own, else [`base_url`].
pub fn base_url_for(profile: &str) -> String {
    crate::profiles::current(profile)
        .map(|p| crate::env::expand(p.base_url.trim()).trim_end_matches('/').to_owned())
        .filter(|b| !b.is_empty() && !b.contains("{{"))
        .unwrap_or_else(base_url)
}

/// `https://host:8080` and the rest of an absolute URL; `("", s)` for a path.
pub fn split_origin(s: &str) -> (&str, &str) {
    let t = s.trim();
    for scheme in ["https://", "http://", "wss://", "ws://"] {
        if let Some(rest) = t.strip_prefix(scheme) {
            let end = rest.find(['/', '?', '#']).map_or(t.len(), |i| scheme.len() + i);
            return (&t[..end], &t[end..]);
        }
    }
    ("", t)
}

/// A path made absolute with the base URL; an absolute URL stays as it is.
pub fn resolve_url(path_and_query: &str) -> String {
    resolve_url_for(path_and_query, "")
}

/// [`resolve_url`] with `profile`'s base URL.
pub fn resolve_url_for(path_and_query: &str, profile: &str) -> String {
    let (origin, _) = split_origin(path_and_query);
    if !origin.is_empty() {
        return path_and_query.trim().to_owned();
    }
    let p = path_and_query.trim();
    let p = if p.starts_with('/') || p.starts_with('?') || p.is_empty() { p.to_owned() } else { format!("/{p}") };
    format!("{}{p}", base_url_for(profile))
}

/// The path (and query) of a URL: what's after its origin, or after the base URL.
pub fn path_of(url: &str) -> &str {
    let base = base_url();
    if !base.is_empty() {
        if let Some(rest) = url.trim().strip_prefix(base.as_str()) {
            return rest;
        }
    }
    split_origin(url).1
}
pub const VERBS: [&str; 5] = ["GET", "POST", "PUT", "PATCH", "DELETE"];

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Kv {
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BodyMode {
    /// The documented fields, as JSON.
    #[default]
    Form,
    /// JSON written by hand (`raw_body`).
    Raw,
    /// Any text (XML, plain text, a form…) sent as `content_type`.
    Text,
    /// `raw_body` is a GraphQL operation, `graphql_variables` its variables (JSON).
    GraphQl,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocRef {
    pub resource: String,
    pub method: String,
    pub route: usize,
}

/// What a saved request speaks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    #[default]
    Http,
    /// `ws://` / `wss://`: messages both ways (`ws_messages` kept to send again).
    WebSocket,
    /// A gRPC method (`grpc_method`) of the server at `path` (`http(s)://host:port`).
    Grpc,
}

impl Protocol {
    pub fn is_http(&self) -> bool {
        *self == Protocol::Http
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RequestSpec {
    /// The API profile it belongs to (`profiles.rs`: base URL, auth); empty = its folder's, else the active one.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub profile: String,
    #[serde(skip_serializing_if = "Protocol::is_http")]
    pub protocol: Protocol,
    /// WebSocket: messages kept to send (the composer's history), and the subprotocols asked.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ws_messages: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ws_subprotocols: Vec<String>,
    /// gRPC: `.proto` files and include folders (empty: server reflection), the method
    /// (`/pkg.Service/Method`); the messages are `raw_body` (JSON; several for client streaming,
    /// as a JSON array), the metadata `headers`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub proto_files: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub proto_includes: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub grpc_method: String,
    pub doc: Option<DocRef>,
    pub verb: String,
    /// Path template, e.g. `/v1/users/:user_id/orders` (may contain `(optional)` groups).
    pub path: String,
    pub path_values: BTreeMap<String, String>,
    /// Documented params, keyed by their full name (`page[size]`, `user[login]`, …).
    pub values: BTreeMap<String, String>,
    pub query: Vec<Kv>,
    pub headers: Vec<Kv>,
    pub body_mode: BodyMode,
    pub raw_body: String,
    /// Raw mode: files sent as multipart/form-data (`key` = field name, `value` = local path).
    pub files: Vec<Kv>,
    /// HTML / JavaScript that shows the response in the browser (Visualize).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub visualizer: String,
    /// Checked on every response (`res.status eq 200`…): Tests tab, test runner.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assertions: Vec<crate::assert::Assertion>,
    /// Your notes about the request (Docs tab), shown in generated documentation.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// What the mock server answers for this request (Mock tab, response › Save as mock).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mock: Option<crate::mock::Mock>,
    /// How it authenticates (`Inherit`: its folder's, else the workspace's).
    #[serde(skip_serializing_if = "crate::auth::Auth::is_inherit")]
    pub auth: crate::auth::Auth,
    /// GraphQL mode: the operation's variables (JSON).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub graphql_variables: String,
    /// Text mode: the body's type (`application/xml`…; empty = text/plain).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub content_type: String,
    /// JavaScript run before sending (`pm.environment.set`, `pm.request.headers.add`…).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub pre_script: String,
    /// JavaScript run on the response (`pm.test`, `pm.expect`, `pm.response.json()`…).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub post_script: String,
}

impl Default for RequestSpec {
    fn default() -> Self {
        Self {
            profile: String::new(),
            protocol: Protocol::Http,
            ws_messages: Vec::new(),
            ws_subprotocols: Vec::new(),
            proto_files: Vec::new(),
            proto_includes: Vec::new(),
            grpc_method: String::new(),
            doc: None,
            verb: "GET".into(),
            path: "/".into(),
            path_values: BTreeMap::new(),
            values: BTreeMap::new(),
            query: Vec::new(),
            headers: Vec::new(),
            body_mode: BodyMode::Form,
            raw_body: String::new(),
            files: Vec::new(),
            visualizer: String::new(),
            assertions: Vec::new(),
            mock: None,
            auth: crate::auth::Auth::Inherit,
            pre_script: String::new(),
            post_script: String::new(),
            graphql_variables: String::new(),
            content_type: String::new(),
            description: String::new(),
        }
    }
}

pub fn has_body(verb: &str) -> bool {
    matches!(verb, "POST" | "PUT" | "PATCH")
}

pub struct PathVar {
    pub name: String,
    pub optional: bool,
}

/// `:name` variables in a path template, in order, with whether they sit in an optional `( )` group.
pub fn path_vars(template: &str) -> Vec<PathVar> {
    let mut out: Vec<PathVar> = Vec::new();
    let chars: Vec<char> = template.chars().collect();
    let mut depth = 0usize;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ':' => {
                let start = i + 1;
                let mut end = start;
                while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
                    end += 1;
                }
                if end > start {
                    let name: String = chars[start..end].iter().collect();
                    if !out.iter().any(|v| v.name == name) {
                        out.push(PathVar { name, optional: depth > 0 });
                    }
                }
                i = end;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    out
}

struct Rendered {
    text: String,
    complete: bool,
    had_var: bool,
}

fn render_group(chars: &[char], i: &mut usize, values: &BTreeMap<String, String>, encode: bool, missing: &mut Vec<String>) -> Rendered {
    let mut r = Rendered { text: String::new(), complete: true, had_var: false };
    while *i < chars.len() {
        match chars[*i] {
            '(' => {
                *i += 1;
                let mut ignored = Vec::new();
                let g = render_group(chars, i, values, encode, &mut ignored);
                if g.complete && g.had_var {
                    r.text.push_str(&g.text);
                }
            }
            ')' => {
                *i += 1;
                return r;
            }
            ':' => {
                *i += 1;
                let start = *i;
                while *i < chars.len() && (chars[*i].is_alphanumeric() || chars[*i] == '_') {
                    *i += 1;
                }
                let name: String = chars[start..*i].iter().collect();
                r.had_var = true;
                match values.get(&name).map(|v| v.trim()).filter(|v| !v.is_empty()) {
                    Some(v) => r.text.push_str(&if encode { encode_component(v) } else { v.to_owned() }),
                    None => {
                        r.complete = false;
                        missing.push(name.clone());
                        r.text.push(':');
                        r.text.push_str(&name);
                    }
                }
            }
            c => {
                r.text.push(c);
                *i += 1;
            }
        }
    }
    r
}

/// Substitutes path variables; returns the path and the names of missing mandatory variables.
pub fn render_path(template: &str, values: &BTreeMap<String, String>, encode: bool) -> (String, Vec<String>) {
    let chars: Vec<char> = template.trim().chars().collect();
    let mut missing = Vec::new();
    let mut i = 0;
    let mut out = String::new();
    // A stray ')' would end the group early; keep rendering the rest.
    while i < chars.len() {
        out.push_str(&render_group(&chars, &mut i, values, encode, &mut missing).text);
    }
    (out, missing)
}

pub fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `%41` → `A`, `+` → space (a query string or a form answer).
pub fn decode_component(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                Ok(v) => {
                    out.push(v);
                    i += 2;
                }
                Err(_) => out.push(b'%'),
            },
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Documented params that are not path variables.
pub fn field_params<'a>(method: &'a Method, path: &str) -> Vec<&'a Param> {
    let vars: HashSet<String> = path_vars(path).into_iter().map(|v| v.name).collect();
    method.params.iter().filter(|p| !vars.contains(&p.name)).collect()
}

fn filled<'v>(values: &'v BTreeMap<String, String>, key: &str) -> Option<&'v str> {
    values.get(key).map(|v| v.trim()).filter(|v| !v.is_empty())
}

fn collect_leaves<'a>(params: &[&'a Param], values: &BTreeMap<String, String>, out: &mut Vec<(&'a Param, String)>) {
    for p in params {
        if let Some(v) = filled(values, &p.full_name) {
            out.push((p, v.to_owned()));
        }
        if p.is_group() {
            let children: Vec<&Param> = p.children.iter().collect();
            collect_leaves(&children, values, out);
        }
    }
}

fn split_list(v: &str) -> impl Iterator<Item = &str> {
    v.split(',').map(str::trim).filter(|s| !s.is_empty())
}

fn scalar(s: &str) -> Value {
    if let Ok(i) = s.parse::<i64>() {
        Value::from(i)
    } else if let Ok(f) = s.parse::<f64>() {
        Value::from(f)
    } else {
        Value::String(s.to_owned())
    }
}

fn typed_value(p: &Param, v: &str) -> Value {
    match p.kind {
        ParamKind::Integer | ParamKind::Float => scalar(v),
        ParamKind::Bool => match v {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            other => Value::String(other.to_owned()),
        },
        ParamKind::IntArray | ParamKind::Array => {
            if v.starts_with('[') {
                if let Ok(json) = serde_json::from_str(v) {
                    return json;
                }
            }
            Value::Array(split_list(v).map(scalar).collect())
        }
        ParamKind::Hash | ParamKind::NestedArray => serde_json::from_str(v).unwrap_or_else(|_| Value::String(v.to_owned())),
        _ => Value::String(v.to_owned()),
    }
}

pub(crate) fn segments(full_name: &str) -> Vec<String> {
    full_name.split(['[', ']']).filter(|s| !s.is_empty()).map(str::to_owned).collect()
}

pub(crate) fn insert_path(obj: &mut Map<String, Value>, segs: &[String], val: Value) {
    match segs {
        [] => {}
        [last] => {
            // Don't let a parent's raw JSON wipe children already merged in, or vice versa.
            match (obj.get_mut(last), val) {
                (Some(Value::Object(existing)), Value::Object(new)) => existing.extend(new),
                (_, val) => {
                    obj.insert(last.clone(), val);
                }
            }
        }
        [first, rest @ ..] => {
            let entry = obj.entry(first.clone()).or_insert_with(|| Value::Object(Map::new()));
            if !entry.is_object() {
                *entry = Value::Object(Map::new());
            }
            if let Value::Object(child) = entry {
                insert_path(child, rest, val);
            }
        }
    }
}

/// JSON body built from the documented fields that have a value.
pub fn form_body(method: &Method, spec: &RequestSpec) -> Value {
    let params = field_params(method, &spec.path);
    let mut leaves = Vec::new();
    collect_leaves(&params, &spec.values, &mut leaves);
    let mut root = Map::new();
    for (p, v) in leaves.into_iter().filter(|(p, _)| p.kind != ParamKind::File) {
        insert_path(&mut root, &segments(&p.full_name), typed_value(p, &v));
    }
    Value::Object(root)
}

/// Documented fields that have a value, as query pairs (GET / DELETE).
pub fn form_query(method: &Method, spec: &RequestSpec) -> Vec<(String, String)> {
    let params = field_params(method, &spec.path);
    let mut leaves = Vec::new();
    collect_leaves(&params, &spec.values, &mut leaves);
    leaves.into_iter().map(|(p, v)| (p.full_name.clone(), v)).collect()
}

/// Documented file fields that have a value, as (field name, local path).
pub fn form_files(method: &Method, spec: &RequestSpec) -> Vec<(String, String)> {
    let params = field_params(method, &spec.path);
    let mut leaves = Vec::new();
    collect_leaves(&params, &spec.values, &mut leaves);
    leaves.into_iter().filter(|(p, _)| p.kind == ParamKind::File).map(|(p, v)| (p.full_name.clone(), v)).collect()
}

/// Enabled rows of a files list, as (field name, path).
pub fn file_rows(rows: &[Kv]) -> impl Iterator<Item = (&str, &str)> {
    rows.iter().filter(|kv| kv.enabled && !kv.key.trim().is_empty() && !kv.value.trim().is_empty()).map(|kv| (kv.key.trim(), kv.value.trim()))
}

/// A JSON body as multipart text fields, Rails style: `{"user":{"tags":["a"]}}` → `user[tags][]=a`.
pub fn multipart_fields(body: &Value) -> Vec<(String, String)> {
    fn walk(prefix: &str, v: &Value, out: &mut Vec<(String, String)>) {
        match v {
            Value::Object(o) => {
                for (k, child) in o {
                    let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}[{k}]") };
                    walk(&key, child, out);
                }
            }
            Value::Array(a) => a.iter().for_each(|child| walk(&format!("{prefix}[]"), child, out)),
            Value::String(s) => out.push((prefix.to_owned(), s.clone())),
            Value::Null => out.push((prefix.to_owned(), String::new())),
            other => out.push((prefix.to_owned(), other.to_string())),
        }
    }
    let mut out = Vec::new();
    walk("", body, &mut out);
    out
}

/// `~/x` → `$HOME/x`.
pub fn expand_home(path: &str) -> std::path::PathBuf {
    let path = path.trim();
    match (path.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ if path == "~" => dirs::home_dir().unwrap_or_default(),
        _ => path.into(),
    }
}

/// Required fields left empty (groups count through their own required fields).
pub fn missing_required(method: &Method, spec: &RequestSpec) -> Vec<String> {
    fn walk(params: &[&Param], values: &BTreeMap<String, String>, out: &mut Vec<String>) {
        for p in params {
            if p.is_group() {
                let children: Vec<&Param> = p.children.iter().collect();
                walk(&children, values, out);
            } else if p.required && p.count_set(values) == 0 {
                out.push(p.full_name.clone());
            }
        }
    }
    let mut out = Vec::new();
    walk(&field_params(method, &spec.path), &spec.values, &mut out);
    out
}

#[derive(Clone, Default)]
pub struct Prepared {
    pub verb: String,
    pub url: String,
    pub display_url: String,
    /// Path with variables substituted (unencoded); missing ones stay as `:name`.
    pub path: String,
    /// Query pairs, unencoded.
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    /// Files to upload as (field name, local path): the request is then sent as multipart/form-data,
    /// with the JSON body's fields as text parts.
    pub files: Vec<(String, String)>,
    pub missing_path: Vec<String>,
    pub missing_required: Vec<String>,
    pub body_error: Option<String>,
    /// The request's own auth (`Inherit`: its folder's / the workspace's, decided when sending).
    pub auth: crate::auth::Auth,
    /// The body's type when it isn't JSON (GraphQL, text, XML, a form).
    pub content_type: Option<String>,
    /// Its API profile (empty: the active one): the auth when its own is `Inherit`.
    pub profile: String,
}

/// The spec with `{{now}}`-style dynamic dates replaced by their current value.
/// The spec with `{{…}}` filled: the active environment's values first (`{{store_id}}`), then the dates.
fn with_dates(spec: &RequestSpec) -> RequestSpec {
    let mut s = spec.clone();
    let expand = |v: &str| crate::dates::expand(&crate::env::expand(v));
    s.path = crate::env::expand(&s.path);
    s.path_values.values_mut().for_each(|v| *v = expand(v));
    s.values.values_mut().for_each(|v| *v = expand(v));
    s.query.iter_mut().chain(s.headers.iter_mut()).for_each(|kv| kv.value = expand(&kv.value));
    s.raw_body = expand(&s.raw_body);
    s.graphql_variables = expand(&s.graphql_variables);
    s.files.iter_mut().for_each(|kv| kv.value = expand(&kv.value));
    s
}

pub fn prepare(spec: &RequestSpec, method: Option<&Method>) -> Prepared {
    let spec = &with_dates(spec);
    let verb = spec.verb.trim().to_uppercase();
    // An absolute URL keeps its origin aside: `:8080` isn't a path variable.
    let (origin, path_template) = split_origin(spec.path.trim());
    let (path, missing_path) = render_path(path_template, &spec.path_values, true);
    let (display_path, _) = render_path(path_template, &spec.path_values, false);
    let path = normalize_path(&path);
    let display_path = normalize_path(&display_path);
    let origin = if origin.is_empty() { base_url_for(&spec.profile) } else { origin.to_owned() };

    let mut query: Vec<(String, String)> = Vec::new();
    let mut body = None;
    let mut body_error = None;
    let mut files = Vec::new();
    let mut content_type = None;

    if let Some(m) = method {
        if !has_body(&verb) {
            query.extend(form_query(m, spec));
        }
    }
    query.extend(spec.query.iter().filter(|kv| kv.enabled && !kv.key.trim().is_empty()).map(|kv| (kv.key.trim().to_owned(), kv.value.clone())));

    if has_body(&verb) {
        match spec.body_mode {
            BodyMode::Form => {
                if let Some(m) = method {
                    let v = form_body(m, spec);
                    if v.as_object().is_some_and(|o| !o.is_empty()) {
                        body = Some(v.to_string());
                    }
                    files = form_files(m, spec);
                }
            }
            BodyMode::Raw => {
                let raw = spec.raw_body.trim();
                if !raw.is_empty() {
                    if let Err(e) = serde_json::from_str::<Value>(raw) {
                        body_error = Some(format!("Raw body is not valid JSON: {e}"));
                    }
                    body = Some(raw.to_owned());
                }
                files = file_rows(&spec.files).map(|(k, v)| (k.to_owned(), v.to_owned())).collect();
            }
            BodyMode::Text => {
                if !spec.raw_body.is_empty() {
                    body = Some(spec.raw_body.clone());
                }
                content_type = Some(if spec.content_type.trim().is_empty() { "text/plain".to_owned() } else { spec.content_type.trim().to_owned() });
            }
            BodyMode::GraphQl => match crate::graphql::request_body(&spec.raw_body, &spec.graphql_variables, "") {
                Ok(b) => body = Some(b),
                Err(e) => body_error = Some(format!("GraphQL: {e}")),
            },
        }
    }

    let encoded_query = query.iter().map(|(k, v)| format!("{}={}", encode_component(k), encode_component(v))).collect::<Vec<_>>().join("&");
    let display_query = query.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");
    let join = |p: &str, q: &str| if q.is_empty() { format!("{origin}{p}") } else { format!("{origin}{p}?{q}") };

    let headers = spec.headers.iter().filter(|kv| kv.enabled && !kv.key.trim().is_empty()).map(|kv| (kv.key.trim().to_owned(), kv.value.clone())).collect();

    Prepared {
        url: join(&path, &encoded_query),
        display_url: join(&display_path, &display_query),
        path: display_path,
        query,
        verb,
        headers,
        body,
        files,
        missing_path,
        missing_required: method.map(|m| missing_required(m, spec)).unwrap_or_default(),
        body_error,
        auth: spec.auth.clone(),
        content_type,
        profile: spec.profile.clone(),
    }
}

fn normalize_path(p: &str) -> String {
    let p = p.trim();
    if p.starts_with('/') {
        p.to_owned()
    } else {
        format!("/{p}")
    }
}

/// cURL equivalent of the request, with its authentication (secrets as shell variables).
pub fn curl_snippet(p: &Prepared) -> String {
    let q = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    let mut s = String::new();
    let auth = p.auth.or(&crate::auth::default_auth()).clone();
    let mut url = p.url.clone();
    let mut auth_header = None;
    match &auth {
        crate::auth::Auth::Basic { username, .. } => {
            s.push_str("export PASSWORD='<the password>'\n");
            auth_header = Some(format!(" \\\n  -u {}", q(&format!("{username}:$PASSWORD")).replace("'$PASSWORD'", "$PASSWORD")));
        }
        crate::auth::Auth::Bearer { prefix, .. } => {
            s.push_str("export TOKEN='<the token>'\n");
            auth_header = Some(format!(" \\\n  -H \"Authorization: {} $TOKEN\"", prefix.trim()));
        }
        crate::auth::Auth::ApiKey { key, location, .. } => {
            s.push_str("export API_KEY='<the key>'\n");
            match location {
                crate::auth::KeyLocation::Header => auth_header = Some(format!(" \\\n  -H \"{}: $API_KEY\"", key.trim())),
                crate::auth::KeyLocation::Query => url = format!("{url}{}{}=$API_KEY", if url.contains('?') { "&" } else { "?" }, encode_component(key.trim())),
            }
        }
        crate::auth::Auth::OAuth2(o) => {
            s.push_str("# An OAuth 2.0 access token (client credentials shown; see your provider for the other grants)\n");
            s.push_str("export CLIENT_ID='<client id>' CLIENT_SECRET='<client secret>'\n");
            s.push_str(&format!(
                "TOKEN=$(curl -s -X POST {} \\\n  -d grant_type=client_credentials -d \"client_id=$CLIENT_ID\" -d \"client_secret=$CLIENT_SECRET\"{} \\\n  | sed -E 's/.*\"access_token\":\"([^\"]+)\".*/\\1/')\n\n",
                q(o.token_url.trim()),
                if o.scope.trim().is_empty() { String::new() } else { format!(" -d {}", q(&format!("scope={}", o.scope.trim()))) }
            ));
            auth_header = Some(format!(" \\\n  -H \"Authorization: {} $TOKEN\"", o.header_prefix.trim()));
        }
        crate::auth::Auth::Inherit | crate::auth::Auth::None => {}
    }
    s.push_str(&format!("curl -s -X {} {}", q(&p.verb), if url.contains("$API_KEY") { format!("\"{url}\"") } else { q(&url) }));
    if let Some(h) = auth_header {
        s.push_str(&h);
    }
    for (k, v) in &p.headers {
        s.push_str(&format!(" \\\n  -H {}", q(&format!("{k}: {v}"))));
    }
    if !p.files.is_empty() {
        // curl builds the multipart body (and its Content-Type) itself.
        let fields = p.body.as_deref().and_then(|b| serde_json::from_str(b).ok()).map(|v| multipart_fields(&v)).unwrap_or_default();
        for (k, v) in fields {
            s.push_str(&format!(" \\\n  --form-string {}", q(&format!("{k}={v}"))));
        }
        for (k, path) in &p.files {
            s.push_str(&format!(" \\\n  -F {}", q(&format!("{k}=@{}", expand_home(path).display()))));
        }
    } else if let Some(b) = &p.body {
        s.push_str(&format!(" \\\n  -H {}", q(&format!("Content-Type: {}", p.content_type.as_deref().unwrap_or("application/json")))));
        s.push_str(&format!(" \\\n  --data-raw {}", q(b)));
    }
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vals(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn optional_groups() {
        let t = "/v1/users/graph(/on/:field(/by/:interval))";
        assert_eq!(render_path(t, &vals(&[]), true).0, "/v1/users/graph");
        assert_eq!(render_path(t, &vals(&[("field", "created_at")]), true).0, "/v1/users/graph/on/created_at");
        assert_eq!(render_path(t, &vals(&[("field", "created_at"), ("interval", "day")]), true).0, "/v1/users/graph/on/created_at/by/day");
        assert!(render_path(t, &vals(&[]), true).1.is_empty());
        let vars = path_vars(t);
        assert!(vars.iter().all(|v| v.optional));
    }

    #[test]
    fn mandatory_vars() {
        let (p, missing) = render_path("/v1/users/:user_id/orders", &vals(&[]), true);
        assert_eq!(p, "/v1/users/:user_id/orders");
        assert_eq!(missing, vec!["user_id"]);
        let (p, _) = render_path("/v1/users/:user_id", &vals(&[("user_id", "jo hn")]), true);
        assert_eq!(p, "/v1/users/jo%20hn");
    }

    #[test]
    fn multipart_from_json() {
        let body = serde_json::json!({ "user": { "login": "x", "kind": null, "ids": [1, 2], "staff?": false } });
        let f = multipart_fields(&body);
        let f: Vec<(&str, &str)> = f.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        assert_eq!(f, vec![("user[login]", "x"), ("user[kind]", ""), ("user[ids][]", "1"), ("user[ids][]", "2"), ("user[staff?]", "false")]);
    }

    #[test]
    fn nested_insert() {
        let mut m = Map::new();
        insert_path(&mut m, &segments("user[login]"), Value::from("x"));
        insert_path(&mut m, &segments("user[store][id]"), Value::from(1));
        assert_eq!(Value::Object(m).to_string(), r#"{"user":{"login":"x","store":{"id":1}}}"#);
    }

    #[test]
    fn environment_values_in_a_request() {
        // A name no other test uses: the current environment is shared by the whole process.
        crate::env::set_current(vec![("t_env_store".into(), "12".into())]);
        let spec = RequestSpec { verb: "GET".into(), path: "/v1/stores/{{t_env_store}}/users".into(), ..Default::default() };
        let p = prepare(&spec, None);
        crate::env::set_current(Vec::new());
        assert!(p.url.ends_with("/v1/stores/12/users"), "{}", p.url);
    }
}
