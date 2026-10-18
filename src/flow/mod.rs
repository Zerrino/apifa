//! Flows: small programs made of building blocks that chain API calls
//! (call → extract a field → loop → condition → another call …).
//!
//! Values flow through variables. Any text field accepts `{{expr}}` where `expr` is a path
//! like `user.body.id`, `users.body[0].login`, `projects.body.length` or `users.body.id`
//! (on a list, a field name collects it from every element).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use std::collections::BTreeMap;

use crate::model::{DocRef, Kv};
use crate::workspace::new_uid;

mod runner;
mod template;
#[cfg(test)]
mod tests;

pub use runner::*;
pub use template::*;

fn yes() -> bool {
    true
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Flow {
    #[serde(default = "new_uid")]
    pub uid: u64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub inputs: Vec<FlowInput>,
    #[serde(default)]
    pub blocks: Vec<Block>,
    /// What the flow gives back when another flow runs it (and shown after a run).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<FlowOutput>,
    /// HTML / JavaScript showing a run's results in the browser (Visualize): gets `flow`, `outputs`, `vars`, `body`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub visualizer: String,
    /// Each run's results are written to a file and come back when the flow is opened (no need to run it again).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keep_results: bool,
}

/// A value exposed at the end of a flow: `value` is an expression like `{{user.body.id}}`.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct FlowOutput {
    pub name: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub description: String,
}

impl Flow {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            uid: new_uid(),
            name: name.into(),
            description: String::new(),
            inputs: Vec::new(),
            blocks: Vec::new(),
            outputs: Vec::new(),
            visualizer: String::new(),
            keep_results: false,
        }
    }

    /// The outputs evaluated against the variables at the end of a run (`null` when missing).
    pub fn outputs_of(&self, vars: &Map<String, Value>) -> Map<String, Value> {
        self.outputs.iter().filter(|o| !o.name.trim().is_empty()).map(|o| (o.name.trim().to_owned(), eval_output(&o.value, vars))).collect()
    }

    pub fn refresh_uids(&mut self) {
        self.uid = new_uid();
        fn walk(blocks: &mut [Block]) {
            for b in blocks {
                b.refresh_uids();
            }
        }
        walk(&mut self.blocks);
    }
}

/// What an input holds. `auto` (files written before types existed) guesses from the text.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputType {
    #[default]
    Auto,
    String,
    Integer,
    Number,
    Boolean,
    Date,
    File,
    Enum,
    Json,
}

impl InputType {
    pub const ALL: [InputType; 9] = [
        InputType::String,
        InputType::Integer,
        InputType::Number,
        InputType::Boolean,
        InputType::Date,
        InputType::File,
        InputType::Enum,
        InputType::Json,
        InputType::Auto,
    ];

    pub fn label(self) -> &'static str {
        match self {
            InputType::Auto => "auto",
            InputType::String => "string",
            InputType::Integer => "integer",
            InputType::Number => "number",
            InputType::Boolean => "boolean",
            InputType::Date => "date",
            InputType::File => "file",
            InputType::Enum => "enum",
            InputType::Json => "json",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            InputType::Auto => "Guessed from the text: 17 is a number, [1,2] a list, anything else text",
            InputType::String => "Text",
            InputType::Integer => "A whole number, e.g. 17",
            InputType::Number => "A number, e.g. 3.5",
            InputType::Boolean => "true or false",
            InputType::Date => "A date like 2026-10-15T09:00:00Z, or {{now-7d}}",
            InputType::File => "Path of a local file (for a request's files)",
            InputType::Enum => "One of the choices you define",
            InputType::Json => "Any JSON value: a list, an object…",
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct FlowInput {
    #[serde(default, rename = "type")]
    pub kind: InputType,
    pub name: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub description: String,
    /// `enum` only: the values to pick from.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
}

impl FlowInput {
    /// The variable's value, checked against the type. An empty value is `null` (text for text-like types).
    pub fn to_value(&self) -> Result<Value, String> {
        let raw = self.value.trim();
        let name = self.name.trim();
        if raw.is_empty() {
            return Ok(match self.kind {
                InputType::Auto | InputType::String | InputType::Date | InputType::File => Value::String(String::new()),
                _ => Value::Null,
            });
        }
        let bad = |what: &str| format!("input “{name}” must be {what} (got “{raw}”)");
        Ok(match self.kind {
            InputType::Auto => loose_value(&self.value),
            InputType::String => Value::String(self.value.clone()),
            InputType::Integer => Value::from(raw.parse::<i64>().map_err(|_| bad("an integer"))?),
            InputType::Number => {
                let n: f64 = raw.parse().map_err(|_| bad("a number"))?;
                serde_json::Number::from_f64(n).map(Value::Number).ok_or_else(|| bad("a number"))?
            }
            InputType::Boolean => match raw {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                _ => return Err(bad("true or false")),
            },
            InputType::Date => {
                let v = crate::dates::expand(raw);
                if crate::dates::parse(&v).is_none() {
                    return Err(bad("a date"));
                }
                Value::String(v)
            }
            InputType::File => Value::String(crate::model::expand_home(raw).display().to_string()),
            InputType::Enum => {
                if !self.options.is_empty() && !self.options.iter().any(|o| o == raw) {
                    return Err(bad(&format!("one of {}", self.options.join(", "))));
                }
                loose_value(raw)
            }
            InputType::Json => serde_json::from_str(raw).map_err(|e| bad(&format!("valid JSON ({e})")))?,
        })
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Block {
    #[serde(default = "new_uid", skip_serializing)]
    pub uid: u64,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// Show this block in the console; `None` = the block type's default (see [`Block::logs`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<bool>,
    #[serde(flatten)]
    pub kind: BlockKind,
}

impl Block {
    pub fn new_kind(kind: BlockKind) -> Self {
        Self { uid: new_uid(), enabled: true, note: String::new(), log: None, kind }
    }

    /// Conditions are quiet by default (they repeat in every loop turn); everything else logs.
    pub fn logs(&self) -> bool {
        self.log.unwrap_or(!matches!(self.kind, BlockKind::If { .. }))
    }

    pub fn refresh_uids(&mut self) {
        self.uid = new_uid();
        for child in self.kind.children_mut() {
            child.iter_mut().for_each(Block::refresh_uids);
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RequestBlock {
    /// Variable receiving `{status, ok, headers, body}`.
    pub save_as: String,
    pub verb: String,
    pub path: String,
    #[serde(default)]
    pub query: Vec<Kv>,
    #[serde(default)]
    pub headers: Vec<Kv>,
    /// JSON body template (POST / PUT / PATCH).
    #[serde(default)]
    pub body: String,
    /// Documented fields (needs `doc`), templates allowed: query parameters for GET / DELETE, body fields
    /// (or files) otherwise. The `body` text wins over them for the same key.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<String, String>,
    /// Files to upload (POST / PUT / PATCH): `key` = field name (`user[image]`), `value` = local path,
    /// templates allowed (`~/photos/{{login}}.jpg`). The request then goes out as multipart/form-data.
    #[serde(default)]
    pub files: Vec<Kv>,
    /// GET only: follow every page and concatenate the results.
    #[serde(default)]
    pub all_pages: bool,
    /// Page limit of this block for `all_pages` (`0` = no limit); unset = the one in Settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_pages: Option<u32>,
    #[serde(default = "yes")]
    pub stop_on_error: bool,
    #[serde(default)]
    pub doc: Option<DocRef>,
    /// Checked on the response (`res.status eq 200`…). With assertions, they decide whether the block failed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assertions: Vec<crate::assert::Assertion>,
    /// How it authenticates (`Inherit`: its profile's, else the workspace's); fields can use `{{variables}}`.
    #[serde(default, skip_serializing_if = "crate::auth::Auth::is_inherit")]
    pub auth: crate::auth::Auth,
    /// GraphQL instead of a JSON body: the operation, and its variables (JSON); templates allowed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub graphql: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub graphql_variables: String,
    /// The body sent as text of this type (`application/xml`…) instead of JSON.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content_type: String,
    /// JavaScript before sending and on the response (Postman's `pm`): `pm.variables` are the flow's
    /// variables, `pm.test` results count like assertions.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pre_script: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub post_script: String,
    /// The API profile it uses (empty: the flow's).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub profile: String,
}

/// A WebSocket conversation: connect, send `messages`, collect what comes back, close.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WsBlock {
    /// Variable receiving `{connected, status, protocol, sent, received: [...], closed}`.
    pub save_as: String,
    /// `wss://…`, or a path (the base URL as ws:// / wss://); templates allowed.
    pub url: String,
    pub headers: Vec<Kv>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subprotocols: Vec<String>,
    /// Sent in order once connected (text; JSON is fine); templates allowed.
    pub messages: Vec<String>,
    /// How long to wait for answers after the last message or answer (ms).
    pub wait_ms: u64,
    /// Stop collecting after this many messages (0: no limit).
    pub max_messages: usize,
    /// Stop collecting once a message contains this text (empty: no such stop).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub until: String,
    #[serde(skip_serializing_if = "crate::auth::Auth::is_inherit")]
    pub auth: crate::auth::Auth,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub profile: String,
    pub stop_on_error: bool,
}

impl Default for WsBlock {
    fn default() -> Self {
        Self {
            save_as: "ws".into(),
            url: "wss://echo.websocket.org".into(),
            headers: Vec::new(),
            subprotocols: Vec::new(),
            messages: vec!["{\"hello\": \"{{now}}\"}".into()],
            wait_ms: 2000,
            max_messages: 0,
            until: String::new(),
            auth: crate::auth::Auth::Inherit,
            profile: String::new(),
            stop_on_error: true,
        }
    }
}

/// A gRPC call: one method, its message(s), its metadata.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GrpcBlock {
    /// Variable receiving `{status, status_name, ok, message, messages: [...], headers, trailers}`.
    pub save_as: String,
    /// `https://host:port` (`http://` for plaintext), or empty: the profile's base URL.
    pub url: String,
    /// `/package.Service/Method`.
    pub method: String,
    /// `.proto` files and include folders; empty: server reflection.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub proto_files: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub proto_includes: Vec<String>,
    /// The message as JSON (client / bidi streaming: a JSON array, one element per message); templates allowed.
    pub message: String,
    pub metadata: Vec<Kv>,
    pub timeout_ms: u64,
    #[serde(skip_serializing_if = "crate::auth::Auth::is_inherit")]
    pub auth: crate::auth::Auth,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub profile: String,
    pub stop_on_error: bool,
}

impl Default for GrpcBlock {
    fn default() -> Self {
        Self {
            save_as: "rpc".into(),
            url: String::new(),
            method: String::new(),
            proto_files: Vec::new(),
            proto_includes: Vec::new(),
            message: "{}".into(),
            metadata: Vec::new(),
            timeout_ms: 30_000,
            auth: crate::auth::Auth::Inherit,
            profile: String::new(),
            stop_on_error: true,
        }
    }
}

impl Default for RequestBlock {
    fn default() -> Self {
        Self {
            save_as: "res".into(),
            verb: "GET".into(),
            path: "/".into(),
            query: Vec::new(),
            headers: Vec::new(),
            body: String::new(),
            values: BTreeMap::new(),
            files: Vec::new(),
            all_pages: false,
            max_pages: None,
            stop_on_error: true,
            doc: None,
            assertions: Vec::new(),
            auth: crate::auth::Auth::Inherit,
            graphql: String::new(),
            graphql_variables: String::new(),
            content_type: String::new(),
            pre_script: String::new(),
            post_script: String::new(),
            profile: String::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompareOp {
    Eq,
    Ne,
    Contains,
    Gt,
    Lt,
    NotEmpty,
    Empty,
}

impl CompareOp {
    pub const ALL: [CompareOp; 7] = [CompareOp::Eq, CompareOp::Ne, CompareOp::Contains, CompareOp::Gt, CompareOp::Lt, CompareOp::NotEmpty, CompareOp::Empty];

    pub fn label(self) -> &'static str {
        match self {
            CompareOp::Eq => "equals",
            CompareOp::Ne => "is not",
            CompareOp::Contains => "contains",
            CompareOp::Gt => ">",
            CompareOp::Lt => "<",
            CompareOp::NotEmpty => "is not empty",
            CompareOp::Empty => "is empty",
        }
    }

    pub fn unary(self) -> bool {
        matches!(self, CompareOp::NotEmpty | CompareOp::Empty)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
// A request block is much bigger than the others; blocks are few, boxing it would only add noise.
#[allow(clippy::large_enum_variant)]
pub enum BlockKind {
    Request(RequestBlock),
    Extract {
        from: String,
        into: String,
    },
    Set {
        name: String,
        value: String,
    },
    ForEach {
        list: String,
        item: String,
        #[serde(default)]
        blocks: Vec<Block>,
        /// Runs several items at once, as fast as the API's per-second limit allows. Each item starts from the
        /// variables before the loop; the last item's variables are kept after it.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        parallel: bool,
    },
    If {
        left: String,
        op: CompareOp,
        right: String,
        #[serde(default)]
        then_blocks: Vec<Block>,
        #[serde(default)]
        else_blocks: Vec<Block>,
    },
    Wait {
        ms: u64,
    },
    Log {
        message: String,
    },
    Fail {
        message: String,
    },
    Flow(FlowCall),
    /// Talks to a WebSocket server.
    WebSocket(WsBlock),
    /// Calls a gRPC method.
    Grpc(GrpcBlock),
}

/// Runs another flow of the workspace: its inputs are filled from `inputs`, its outputs land in `save_as`.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FlowCall {
    /// Name of the flow to run.
    pub flow: String,
    /// Its uid when picked in the app, so a rename doesn't break the link.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flow_uid: Option<u64>,
    /// Input name → value (templates allowed). A missing one keeps the flow's own value.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, String>,
    /// Variable receiving the outputs, as `{name: value}`.
    pub save_as: String,
    /// The API profile the sub-flow runs with (empty: this flow's).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub profile: String,
}

/// The flow a `flow` block points at: by uid first, then by name.
pub fn find_flow<'a>(flows: impl IntoIterator<Item = &'a Flow> + Clone, uid: Option<u64>, name: &str) -> Option<&'a Flow> {
    let name = name.trim();
    uid.and_then(|u| flows.clone().into_iter().find(|f| f.uid == u)).or_else(|| flows.into_iter().find(|f| !name.is_empty() && f.name.trim().eq_ignore_ascii_case(name)))
}

/// Palette of blocks: (id, title, help).
pub const BLOCK_TYPES: [(&str, &str, &str); 11] = [
    ("request", "API request", "Call an endpoint and store the response in a variable"),
    ("extract", "Extract field", "Copy a field of a previous response into a variable"),
    ("set", "Set variable", "Create or change a variable"),
    ("for_each", "For each", "Repeat blocks for every element of a list"),
    ("if", "If / else", "Run blocks only when a condition holds"),
    ("wait", "Wait", "Pause (rate limits…)"),
    ("log", "Log", "Write a message in the console"),
    ("fail", "Stop with error", "Stop the flow with a message"),
    ("flow", "Run a flow", "Reuse another flow: fill its inputs, get its outputs"),
    ("web_socket", "WebSocket", "Connect, send messages, collect the answers"),
    ("grpc", "gRPC call", "Call a gRPC method and store its answer"),
];

impl BlockKind {
    pub fn from_id(id: &str) -> Self {
        match id {
            "request" => BlockKind::Request(RequestBlock::default()),
            "extract" => BlockKind::Extract { from: "res.body.id".into(), into: "id".into() },
            "set" => BlockKind::Set { name: "x".into(), value: String::new() },
            "for_each" => BlockKind::ForEach { list: "res.body".into(), item: "item".into(), blocks: Vec::new(), parallel: false },
            "if" => BlockKind::If { left: "{{res.status}}".into(), op: CompareOp::Eq, right: "200".into(), then_blocks: Vec::new(), else_blocks: Vec::new() },
            "wait" => BlockKind::Wait { ms: 1000 },
            "fail" => BlockKind::Fail { message: "Something went wrong".into() },
            "flow" => BlockKind::Flow(FlowCall { save_as: "sub".into(), ..Default::default() }),
            "web_socket" => BlockKind::WebSocket(WsBlock::default()),
            "grpc" => BlockKind::Grpc(GrpcBlock::default()),
            _ => BlockKind::Log { message: String::new() },
        }
    }

    pub fn title(&self) -> &'static str {
        match self {
            BlockKind::Request(_) => "API REQUEST",
            BlockKind::Extract { .. } => "EXTRACT FIELD",
            BlockKind::Set { .. } => "SET VARIABLE",
            BlockKind::ForEach { .. } => "FOR EACH",
            BlockKind::If { .. } => "IF",
            BlockKind::Wait { .. } => "WAIT",
            BlockKind::Log { .. } => "LOG",
            BlockKind::Fail { .. } => "STOP WITH ERROR",
            BlockKind::Flow(_) => "RUN FLOW",
            BlockKind::WebSocket(_) => "WEBSOCKET",
            BlockKind::Grpc(_) => "GRPC CALL",
        }
    }

    /// One line: `GET /v1/users  → res`, `for each p in projects.body`… (outline, generated docs).
    pub fn summary(&self) -> String {
        match self {
            BlockKind::Request(r) => format!("{} {}  → {}", r.verb, r.path, r.save_as.trim()),
            BlockKind::Extract { from, into } => format!("{into} = {from}"),
            BlockKind::Set { name, value } => format!("{name} = {value}"),
            BlockKind::ForEach { list, item, parallel, .. } => format!("for each {item} in {list}{}", if *parallel { " ⇉ parallel" } else { "" }),
            BlockKind::If { left, op, right, .. } => {
                if op.unary() {
                    format!("if {left} {}", op.label())
                } else {
                    format!("if {left} {} {right}", op.label())
                }
            }
            BlockKind::Wait { ms } => format!("{ms} ms"),
            BlockKind::Log { message } | BlockKind::Fail { message } => message.clone(),
            BlockKind::Flow(c) => format!("“{}”  → {}", c.flow.trim(), c.save_as.trim()),
            BlockKind::WebSocket(w) => format!("{} · {} message(s)  → {}", w.url.trim(), w.messages.len(), w.save_as.trim()),
            BlockKind::Grpc(g) => format!("{} {}  → {}", g.url.trim(), g.method.trim(), g.save_as.trim()),
        }
    }

    pub fn children(&self) -> Vec<(&'static str, &Vec<Block>)> {
        match self {
            BlockKind::ForEach { blocks, .. } => vec![("", blocks)],
            BlockKind::If { then_blocks, else_blocks, .. } => vec![("then", then_blocks), ("else", else_blocks)],
            _ => Vec::new(),
        }
    }

    pub fn children_mut(&mut self) -> Vec<&mut Vec<Block>> {
        match self {
            BlockKind::ForEach { blocks, .. } => vec![blocks],
            BlockKind::If { then_blocks, else_blocks, .. } => vec![then_blocks, else_blocks],
            _ => Vec::new(),
        }
    }
}
