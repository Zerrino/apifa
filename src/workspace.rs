//! The workspace: folders of saved requests and flows. Every node can be exported to / imported
//! from a self-contained JSON document, so it can be shared between users.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::flow::{Block, BlockKind, Flow, FlowInput, InputType, RequestBlock};
use crate::model::{BodyMode, Kv, RequestSpec};

pub const FORMAT: &str = "apifa";
pub const FORMAT_VERSION: u32 = 1;

static NEXT_UID: AtomicU64 = AtomicU64::new(0);

/// Unique id, also stable across restarts (it is saved with the node).
pub fn new_uid() -> u64 {
    if NEXT_UID.load(Ordering::Relaxed) == 0 {
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) | 1;
        let _ = NEXT_UID.compare_exchange(0, seed, Ordering::Relaxed, Ordering::Relaxed);
    }
    NEXT_UID.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SavedRequest {
    #[serde(default = "new_uid")]
    pub uid: u64,
    #[serde(default)]
    pub name: String,
    pub spec: RequestSpec,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Folder {
    #[serde(default = "new_uid")]
    pub uid: u64,
    pub name: String,
    #[serde(default)]
    pub items: Vec<Node>,
    /// The auth of what's inside whose own is `Inherit` (like a Postman / Bruno collection's).
    #[serde(default, skip_serializing_if = "crate::auth::Auth::is_inherit")]
    pub auth: crate::auth::Auth,
    /// The API profile of what's inside (empty: its parent's).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub profile: String,
}

// A few hundred nodes at most: the size difference doesn't matter.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Node {
    Folder(Folder),
    Request(SavedRequest),
    Flow(Flow),
}

impl Node {
    pub fn uid(&self) -> u64 {
        match self {
            Node::Folder(f) => f.uid,
            Node::Request(r) => r.uid,
            Node::Flow(f) => f.uid,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Node::Folder(f) => &f.name,
            Node::Request(r) => &r.name,
            Node::Flow(f) => &f.name,
        }
    }

    pub fn set_name(&mut self, name: String) {
        match self {
            Node::Folder(f) => f.name = name,
            Node::Request(r) => r.name = name,
            Node::Flow(f) => f.name = name,
        }
    }

    pub fn kind_label(&self) -> &'static str {
        match self {
            Node::Folder(_) => "folder",
            Node::Request(_) => "request",
            Node::Flow(_) => "flow",
        }
    }

    /// Gives fresh ids to the node and everything inside (for imports and duplicates).
    pub fn refresh_uids(&mut self) {
        match self {
            Node::Folder(f) => {
                f.uid = new_uid();
                f.items.iter_mut().for_each(Node::refresh_uids);
            }
            Node::Request(r) => r.uid = new_uid(),
            Node::Flow(f) => f.refresh_uids(),
        }
    }
}

impl Folder {
    pub fn new(name: impl Into<String>) -> Self {
        Self { uid: new_uid(), name: name.into(), items: Vec::new(), auth: crate::auth::Auth::Inherit, profile: String::new() }
    }

    /// The profile that applies to `uid`: the nearest folder's that names one (empty: none).
    pub fn profile_for(&self, uid: u64) -> String {
        fn walk(f: &Folder, uid: u64, chain: &mut Vec<String>) -> bool {
            chain.push(f.profile.clone());
            for n in &f.items {
                match n {
                    Node::Folder(sub) => {
                        if walk(sub, uid, chain) {
                            return true;
                        }
                    }
                    other if other.uid() == uid => return true,
                    _ => {}
                }
            }
            chain.pop();
            false
        }
        let mut chain = Vec::new();
        if !walk(self, uid, &mut chain) {
            return String::new();
        }
        chain.into_iter().rev().find(|p| !p.is_empty()).unwrap_or_default()
    }

    /// The auth that applies to `uid` (a request or flow inside): the nearest folder's that isn't
    /// `Inherit`, from it up to this one; `Inherit` when none says anything.
    pub fn auth_for(&self, uid: u64) -> crate::auth::Auth {
        fn walk(f: &Folder, uid: u64, chain: &mut Vec<crate::auth::Auth>) -> bool {
            chain.push(f.auth.clone());
            for n in &f.items {
                match n {
                    Node::Folder(sub) => {
                        if walk(sub, uid, chain) {
                            return true;
                        }
                    }
                    other if other.uid() == uid => return true,
                    _ => {}
                }
            }
            chain.pop();
            false
        }
        let mut chain = Vec::new();
        if !walk(self, uid, &mut chain) {
            return crate::auth::Auth::Inherit;
        }
        chain.into_iter().rev().find(|a| !a.is_inherit()).unwrap_or_default()
    }

    /// Every flow, in folders too.
    pub fn flows(&self) -> Vec<&Flow> {
        let mut out = Vec::new();
        for n in &self.items {
            match n {
                Node::Flow(f) => out.push(f),
                Node::Folder(sub) => out.extend(sub.flows()),
                Node::Request(_) => {}
            }
        }
        out
    }

    pub fn find(&self, uid: u64) -> Option<&Node> {
        for n in &self.items {
            if n.uid() == uid {
                return Some(n);
            }
            if let Node::Folder(f) = n {
                if let Some(found) = f.find(uid) {
                    return Some(found);
                }
            }
        }
        None
    }

    pub fn find_mut(&mut self, uid: u64) -> Option<&mut Node> {
        for n in &mut self.items {
            if n.uid() == uid {
                return Some(n);
            }
            if let Node::Folder(f) = n {
                if let Some(found) = f.find_mut(uid) {
                    return Some(found);
                }
            }
        }
        None
    }

    /// The folder with this uid (the root itself included).
    pub fn folder_mut(&mut self, uid: u64) -> Option<&mut Folder> {
        if self.uid == uid {
            return Some(self);
        }
        match self.find_mut(uid) {
            Some(Node::Folder(f)) => Some(f),
            _ => None,
        }
    }

    pub fn remove(&mut self, uid: u64) -> Option<Node> {
        if let Some(i) = self.items.iter().position(|n| n.uid() == uid) {
            return Some(self.items.remove(i));
        }
        self.items.iter_mut().find_map(|n| match n {
            Node::Folder(f) => f.remove(uid),
            _ => None,
        })
    }

    /// Folder containing `uid`.
    pub fn parent_of(&self, uid: u64) -> Option<u64> {
        if self.items.iter().any(|n| n.uid() == uid) {
            return Some(self.uid);
        }
        self.items.iter().find_map(|n| match n {
            Node::Folder(f) => f.parent_of(uid),
            _ => None,
        })
    }

    /// Every folder as (uid, "A / B / C"), depth first, the root first.
    pub fn folder_paths(&self) -> Vec<(u64, String)> {
        fn walk(f: &Folder, prefix: &str, out: &mut Vec<(u64, String)>) {
            for n in &f.items {
                if let Node::Folder(sub) = n {
                    let path = if prefix.is_empty() { sub.name.clone() } else { format!("{prefix} / {}", sub.name) };
                    out.push((sub.uid, path.clone()));
                    walk(sub, &path, out);
                }
            }
        }
        let mut out = vec![(self.uid, "(workspace root)".to_owned())];
        walk(self, "", &mut out);
        out
    }

    pub fn count(&self) -> (usize, usize) {
        self.items.iter().fold((0, 0), |(r, f), n| match n {
            Node::Folder(sub) => {
                let (a, b) = sub.count();
                (r + a, f + b)
            }
            Node::Request(_) => (r + 1, f),
            Node::Flow(_) => (r, f + 1),
        })
    }
}

/// The workspace created on first launch, with examples to learn from — on JSONPlaceholder, a free
/// public test API (full URLs: they work before any base URL is set).
pub fn default_workspace() -> Folder {
    const API: &str = "https://jsonplaceholder.typicode.com";
    let mut root = Folder::new("Workspace");
    let mut examples = Folder::new("Examples");
    let kv = |k: &str, v: &str| Kv { enabled: true, key: k.into(), value: v.into() };
    examples.items.push(Node::Request(SavedRequest {
        uid: new_uid(),
        name: "List users".into(),
        spec: RequestSpec {
            path: format!("{API}/users"),
            post_script: "pm.test(\"Status code is 200\", () => pm.response.to.have.status(200));\npm.test(\"Ten users\", () => pm.expect(pm.response.json()).to.have.lengthOf(10));\n".into(),
            ..Default::default()
        },
    }));
    examples.items.push(Node::Request(SavedRequest {
        uid: new_uid(),
        name: "Create a post".into(),
        spec: RequestSpec {
            verb: "POST".into(),
            path: format!("{API}/posts"),
            body_mode: BodyMode::Raw,
            raw_body: "{\n  \"title\": \"Hello\",\n  \"body\": \"Sent with apifa\",\n  \"userId\": 1\n}".into(),
            headers: vec![kv("X-Example", "{{now}}")],
            ..Default::default()
        },
    }));
    examples.items.push(Node::Request(SavedRequest {
        uid: new_uid(),
        name: "Echo over WebSocket".into(),
        spec: RequestSpec {
            protocol: crate::model::Protocol::WebSocket,
            path: "wss://echo.websocket.org".into(),
            ws_messages: vec!["{\"hello\": \"apifa\"}".into()],
            ..Default::default()
        },
    }));
    examples.items.push(Node::Flow(example_flow(API)));
    root.items.push(Node::Folder(examples));
    root.items.push(Node::Folder(Folder::new("My requests")));
    root
}

fn example_flow(api: &str) -> Flow {
    let request = |save_as: &str, path: String, query: &[(&str, &str)]| {
        Block::new_kind(BlockKind::Request(RequestBlock {
            save_as: save_as.into(),
            path,
            query: query.iter().map(|(k, v)| Kv { enabled: true, key: (*k).into(), value: (*v).into() }).collect(),
            ..RequestBlock::default()
        }))
    };
    let mut flow = Flow::new("Posts of a user");
    flow.description = "Finds a user by username, then lists the titles of their posts.".into();
    flow.inputs.push(FlowInput {
        kind: InputType::String,
        name: "username".into(),
        value: "Bret".into(),
        description: "A JSONPlaceholder username".into(),
        ..Default::default()
    });
    flow.blocks = vec![
        request("users", format!("{api}/users"), &[("username", "{{username}}")]),
        Block::new_kind(BlockKind::If {
            left: "{{users.body}}".into(),
            op: crate::flow::CompareOp::Empty,
            right: String::new(),
            then_blocks: vec![Block::new_kind(BlockKind::Fail { message: "No user named {{username}}".into() })],
            else_blocks: Vec::new(),
        }),
        Block::new_kind(BlockKind::Extract { from: "users.body[0].id".into(), into: "user_id".into() }),
        request("posts", format!("{api}/posts"), &[("userId", "{{user_id}}")]),
        Block::new_kind(BlockKind::Log { message: "{{users.body[0].name}} wrote {{posts.body.length}} posts".into() }),
        Block::new_kind(BlockKind::ForEach {
            list: "posts.body".into(),
            item: "p".into(),
            blocks: vec![Block::new_kind(BlockKind::Log { message: "• {{p.title}}".into() })],
            parallel: false,
        }),
    ];
    flow
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    format: String,
    version: u32,
    item: Node,
}

/// Shareable JSON document for a node.
pub fn export(node: &Node) -> String {
    let env = Envelope { format: FORMAT.into(), version: FORMAT_VERSION, item: node.clone() };
    serde_json::to_string_pretty(&env).unwrap_or_default()
}

/// Reads a shared document: this app's format, a Postman v2 collection, or a bare request/flow.
pub fn import(text: &str) -> Result<Node, String> {
    let v: Value = serde_json::from_str(text.trim()).map_err(|e| format!("Not valid JSON: {e}"))?;
    let mut node = if matches!(v.get("format").and_then(Value::as_str), Some(FORMAT)) {
        let env: Envelope = serde_json::from_value(v).map_err(|e| format!("Invalid {FORMAT} document: {e}"))?;
        if env.version > FORMAT_VERSION {
            return Err(format!("This file was made by a newer version (format v{})", env.version));
        }
        env.item
    } else if v.get("info").is_some() && v.get("item").is_some() {
        from_postman(&v)?
    } else if let Ok(n) = serde_json::from_value::<Node>(v.clone()) {
        n
    } else if let Ok(flow) = serde_json::from_value::<Flow>(v.clone()) {
        Node::Flow(flow)
    } else if let Ok(spec) = serde_json::from_value::<RequestSpec>(v) {
        Node::Request(SavedRequest { uid: 0, name: "Imported request".into(), spec })
    } else {
        return Err("Unrecognized document: expected an apifa export or a Postman collection".into());
    };
    node.refresh_uids();
    Ok(node)
}

// ---------------------------------------------------------------- Postman v2.x collections

fn from_postman(v: &Value) -> Result<Node, String> {
    let name = v["info"]["name"].as_str().unwrap_or("Postman collection").to_owned();
    let items = v["item"].as_array().ok_or("Postman collection without items")?;
    // The collection's auth applies to its requests (theirs is Inherit unless they say otherwise).
    let auth = postman_auth(&v["auth"]).unwrap_or_default();
    Ok(Node::Folder(Folder { uid: 0, name, items: postman_items(items), auth, profile: String::new() }))
}

fn postman_items(items: &[Value]) -> Vec<Node> {
    items
        .iter()
        .filter_map(|it| {
            let name = it["name"].as_str().unwrap_or("Untitled").to_owned();
            if let Some(children) = it.get("item").and_then(Value::as_array) {
                return Some(Node::Folder(Folder {
                    uid: 0,
                    name,
                    items: postman_items(children),
                    auth: postman_auth(&it["auth"]).unwrap_or_default(),
                    profile: String::new(),
                }));
            }
            let req = it.get("request")?;
            Some(Node::Request(SavedRequest { uid: 0, name, spec: postman_request(req) }))
        })
        .collect()
}

fn postman_request(req: &Value) -> RequestSpec {
    let raw = match &req["url"] {
        Value::String(s) => s.clone(),
        u => u["raw"].as_str().unwrap_or_default().to_owned(),
    };
    // A leading {{baseUrl}}-like variable becomes the base URL (a relative path); a full URL stays.
    let mut rest = raw.as_str();
    if rest.starts_with("{{") {
        if let Some(end) = rest.find("}}") {
            rest = &rest[end + 2..];
        }
    }
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let path = if path.starts_with('/') || !crate::model::split_origin(path).0.is_empty() { path.to_owned() } else { format!("/{path}") };
    let query = query
        .split('&')
        .filter(|s| !s.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            Kv { enabled: true, key: k.to_owned(), value: v.to_owned() }
        })
        .collect();
    // The auth: Postman's own (`auth`), else an Authorization header (Bearer / Basic) becomes it.
    let header_auth = req["header"].as_array().and_then(|hs| {
        let v = hs.iter().find(|h| h["key"].as_str().unwrap_or_default().eq_ignore_ascii_case("authorization"))?["value"].as_str()?.trim().to_owned();
        match v.split_once(' ') {
            Some((p, t)) if p.eq_ignore_ascii_case("bearer") => Some(crate::auth::Auth::Bearer { token: t.trim().to_owned(), prefix: "Bearer".into() }),
            _ => None,
        }
    });
    let auth = postman_auth(&req["auth"]).or(header_auth).unwrap_or_default();
    let headers = req["header"]
        .as_array()
        .map(|hs| {
            hs.iter()
                .filter(|h| auth.is_inherit() || !h["key"].as_str().unwrap_or_default().eq_ignore_ascii_case("authorization"))
                .map(|h| Kv {
                    enabled: !h["disabled"].as_bool().unwrap_or(false),
                    key: h["key"].as_str().unwrap_or_default().to_owned(),
                    value: h["value"].as_str().unwrap_or_default().to_owned(),
                })
                .collect()
        })
        .unwrap_or_default();
    let (raw_body, files, mut notes) = postman_body(&req["body"]);
    // Only the verbs the app sends (a shared collection's text ends up in generated HTML and cURL lines).
    let verb = req["method"].as_str().unwrap_or("GET").trim().to_uppercase();
    let verb = if crate::model::VERBS.contains(&verb.as_str()) { verb } else { "GET".into() };
    // Path variables: Postman's `:id` with `url.variable` [{key, value}].
    let path_values = req["url"]["variable"]
        .as_array()
        .map(|vs| vs.iter().filter_map(|v| Some((v["key"].as_str()?.to_owned(), v["value"].as_str().unwrap_or_default().to_owned()))).collect())
        .unwrap_or_default();
    // The request's own notes ({content} in newer collections), then what couldn't be imported.
    let description = match &req["description"] {
        Value::String(s) => s.clone(),
        d => d["content"].as_str().unwrap_or_default().to_owned(),
    };
    if !description.trim().is_empty() {
        notes.insert(0, description.trim().to_owned());
    }
    RequestSpec {
        verb,
        path,
        path_values,
        query,
        headers,
        // Files go out with the Raw mode (its JSON becomes the multipart's text fields).
        body_mode: if raw_body.is_empty() && files.is_empty() { BodyMode::Form } else { BodyMode::Raw },
        raw_body,
        files,
        description: notes.join("\n\n"),
        auth,
        ..Default::default()
    }
}

/// Postman's `auth` object (`{type: "bearer", bearer: [{key: "token", value}]}`…) as the app's.
pub fn postman_auth(a: &Value) -> Option<crate::auth::Auth> {
    use crate::auth::{Auth, Grant, KeyLocation, OAuth2};
    let kind = a["type"].as_str()?;
    let get = |k: &str| {
        a[kind]
            .as_array()
            .and_then(|items| items.iter().find(|i| i["key"] == k))
            .and_then(|i| i["value"].as_str().map(str::to_owned).or_else(|| Some(i["value"].to_string())))
            .unwrap_or_default()
    };
    Some(match kind {
        "noauth" => Auth::None,
        "basic" => Auth::Basic { username: get("username"), password: get("password") },
        "bearer" => Auth::Bearer { token: get("token"), prefix: "Bearer".into() },
        "apikey" => Auth::ApiKey { key: get("key"), value: get("value"), location: if get("in") == "query" { KeyLocation::Query } else { KeyLocation::Header } },
        "oauth2" => Auth::OAuth2(OAuth2 {
            grant: match get("grant_type").as_str() {
                "password_credentials" => Grant::Password,
                "authorization_code" | "authorization_code_with_pkce" => Grant::AuthorizationCode,
                _ => Grant::ClientCredentials,
            },
            token_url: get("accessTokenUrl"),
            auth_url: get("authUrl"),
            client_id: get("clientId"),
            client_secret: get("clientSecret"),
            scope: get("scope"),
            username: get("username"),
            password: get("password"),
            ..OAuth2::default()
        }),
        _ => return None,
    })
}

/// A Postman body as (raw JSON body, files, notes on what was left out). `urlencoded` and `formdata`
/// fields become the JSON body (`user[login]=x` → `{"user":{"login":"x"}}`, `ids[]` → a list), which
/// the app sends as JSON, or as multipart text fields when there are files.
fn postman_body(body: &Value) -> (String, Vec<Kv>, Vec<String>) {
    let mode = body["mode"].as_str().unwrap_or("raw");
    match mode {
        "raw" => (body["raw"].as_str().unwrap_or_default().to_owned(), Vec::new(), Vec::new()),
        "urlencoded" | "formdata" => {
            let mut files = Vec::new();
            // Field name → its values, in the collection's order (`ids[]` can repeat).
            let mut fields: Vec<(String, Vec<String>)> = Vec::new();
            for f in body[mode].as_array().map(Vec::as_slice).unwrap_or_default() {
                let key = f["key"].as_str().unwrap_or_default().trim();
                if key.is_empty() {
                    continue;
                }
                let enabled = !f["disabled"].as_bool().unwrap_or(false);
                if f["type"].as_str() == Some("file") {
                    let srcs: Vec<&str> = match &f["src"] {
                        Value::String(s) => vec![s.as_str()],
                        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
                        _ => Vec::new(),
                    };
                    for src in srcs.into_iter().filter(|s| !s.trim().is_empty()) {
                        files.push(Kv { enabled, key: key.to_owned(), value: src.to_owned() });
                    }
                    continue;
                }
                if !enabled {
                    continue;
                }
                let value = f["value"].as_str().unwrap_or_default().to_owned();
                match fields.iter_mut().find(|(k, _)| k == key) {
                    Some((_, values)) => values.push(value),
                    None => fields.push((key.to_owned(), vec![value])),
                }
            }
            let mut root = serde_json::Map::new();
            for (key, values) in fields {
                let value = match key.strip_suffix("[]") {
                    Some(_) => Value::Array(values.into_iter().map(Value::String).collect()),
                    None => Value::String(values.into_iter().last().unwrap_or_default()),
                };
                crate::model::insert_path(&mut root, &crate::model::segments(&key), value);
            }
            let raw = if root.is_empty() { String::new() } else { serde_json::to_string_pretty(&Value::Object(root)).unwrap_or_default() };
            (raw, files, Vec::new())
        }
        other => (String::new(), Vec::new(), vec![format!("Imported from Postman: its “{other}” body was left out (the app sends JSON, form fields or files).")]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_import_roundtrip() {
        let root = default_workspace();
        let node = Node::Folder(root.clone());
        let text = export(&node);
        let back = import(&text).unwrap();
        let Node::Folder(f) = back else { panic!("not a folder") };
        assert_eq!(f.name, "Workspace");
        assert_eq!(f.count(), root.count());
        assert_ne!(f.uid, root.uid, "imports get fresh ids");
    }

    #[test]
    fn postman_collection() {
        let text = r#"{"info":{"name":"Shop"},"item":[{"name":"Users","item":[{"name":"Get user","request":{"method":"GET",
            "header":[{"key":"Authorization","value":"Bearer x"},{"key":"X-Test","value":"1"}],
            "url":{"raw":"https://shop.example.com/v1/users/:id?page[size]=5"}}}]}]}"#;
        let Node::Folder(f) = import(text).unwrap() else { panic!() };
        let Node::Folder(users) = &f.items[0] else { panic!() };
        let Node::Request(r) = &users.items[0] else { panic!() };
        assert_eq!(r.spec.path, "https://shop.example.com/v1/users/:id", "a full URL stays one");
        assert_eq!(r.spec.query[0].key, "page[size]");
        assert_eq!(r.spec.headers.len(), 1, "the Authorization header became the auth");
        assert_eq!(r.spec.auth, crate::auth::Auth::Bearer { token: "x".into(), prefix: "Bearer".into() });
    }

    #[test]
    fn postman_bodies() {
        let text = r#"{"info":{"name":"Shop"},"item":[
            {"name":"Form","request":{"method":"PATCH","url":{"raw":"https://shop.example.com/v1/users/:id","variable":[{"key":"id","value":"jdoe"}]},
              "description":"Changes a user",
              "body":{"mode":"urlencoded","urlencoded":[
                {"key":"user[login]","value":"jdoe"},{"key":"user[kind]","value":"admin","disabled":true},
                {"key":"ids[]","value":"1"},{"key":"ids[]","value":"2"}]}}},
            {"name":"Upload","request":{"method":"POST","url":"https://shop.example.com/v1/users","body":{"mode":"formdata","formdata":[
                {"key":"user[login]","value":"x","type":"text"},{"key":"user[image]","type":"file","src":"/tmp/me.jpg"}]}}},
            {"name":"Gql","request":{"method":"POST","url":"https://shop.example.com/v1/x","body":{"mode":"graphql","graphql":{}}}},
            {"name":"Bad verb","request":{"method":"<b>","url":"https://shop.example.com/v1/x"}}
        ]}"#;
        let Node::Folder(f) = import(text).unwrap() else { panic!() };
        let spec = |i: usize| match &f.items[i] {
            Node::Request(r) => r.spec.clone(),
            _ => panic!(),
        };
        let form = spec(0);
        assert_eq!(form.path_values.get("id").map(String::as_str), Some("jdoe"));
        assert_eq!(serde_json::from_str::<Value>(&form.raw_body).unwrap(), serde_json::json!({ "user": { "login": "jdoe" }, "ids": ["1", "2"] }));
        assert!(matches!(form.body_mode, BodyMode::Raw));
        assert_eq!(form.description, "Changes a user");
        let upload = spec(1);
        assert_eq!(upload.files.len(), 1);
        assert_eq!((upload.files[0].key.as_str(), upload.files[0].value.as_str()), ("user[image]", "/tmp/me.jpg"));
        assert_eq!(serde_json::from_str::<Value>(&upload.raw_body).unwrap(), serde_json::json!({ "user": { "login": "x" } }));
        assert!(spec(2).description.contains("graphql"), "what is left out is said");
        assert_eq!(spec(3).verb, "GET");
    }

    #[test]
    fn folder_ops() {
        let mut root = default_workspace();
        let examples = root.items[0].uid();
        let flow_uid = match &root.items[0] {
            Node::Folder(f) => f.items[1].uid(),
            _ => unreachable!(),
        };
        assert_eq!(root.parent_of(flow_uid), Some(examples));
        let node = root.remove(flow_uid).unwrap();
        let mine = root.items[1].uid();
        root.folder_mut(mine).unwrap().items.push(node);
        assert_eq!(root.parent_of(flow_uid), Some(mine));
        assert_eq!(root.folder_paths().len(), 3);
    }

    #[test]
    fn folders_give_their_auth() {
        use crate::auth::Auth;
        let req = |name: &str, auth: Auth| Node::Request(SavedRequest { uid: new_uid(), name: name.into(), spec: RequestSpec { auth, ..Default::default() } });
        let (a, b, c) = (req("a", Auth::Inherit), req("b", Auth::None), req("c", Auth::Inherit));
        let (ua, ub, uc) = (a.uid(), b.uid(), c.uid());
        let mut inner = Folder::new("inner");
        inner.items.push(c);
        let mut api = Folder::new("api");
        api.auth = Auth::Bearer { token: "t".into(), prefix: "Bearer".into() };
        api.items.extend([a, b, Node::Folder(inner)]);
        let mut root = Folder::new("root");
        root.items.push(Node::Folder(api));
        assert!(matches!(root.auth_for(ua), Auth::Bearer { .. }), "from its folder");
        assert!(matches!(root.auth_for(uc), Auth::Bearer { .. }), "from a folder above");
        assert!(matches!(root.auth_for(ub), Auth::Bearer { .. }), "auth_for is the folders' (the request's own None wins when sending)");
        assert!(root.auth_for(12345).is_inherit());
        let items = crate::testrun::collect(&Node::Folder(root), "");
        let get = |n: &str| match &items.iter().find(|i| i.path.ends_with(n)).unwrap().node {
            Node::Request(r) => r.spec.auth.clone(),
            _ => unreachable!(),
        };
        assert!(matches!(get("a"), Auth::Bearer { .. }) && get("b") == Auth::None && matches!(get("c"), Auth::Bearer { .. }));
    }
}
