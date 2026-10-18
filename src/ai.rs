//! The AI assistant: chat with a model (your own API key) that can search the API docs, read the workspace,
//! open ready-to-send requests / flows / substitutions in the app, and send a request after you confirm.
//! Anthropic is called through its Messages API; the other providers through their OpenAI-compatible API.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Provider {
    Anthropic,
    OpenAi,
    Gemini,
    Mistral,
    OpenRouter,
    /// Any OpenAI-compatible server (Ollama, LM Studio, Groq…): base URL in the settings.
    Custom,
}

impl Provider {
    pub const ALL: [Provider; 6] = [Provider::Anthropic, Provider::OpenAi, Provider::Gemini, Provider::Mistral, Provider::OpenRouter, Provider::Custom];

    pub fn id(self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic",
            Provider::OpenAi => "openai",
            Provider::Gemini => "gemini",
            Provider::Mistral => "mistral",
            Provider::OpenRouter => "openrouter",
            Provider::Custom => "custom",
        }
    }

    pub fn from_id(id: &str) -> Self {
        Self::ALL.into_iter().find(|p| p.id() == id).unwrap_or(Provider::Anthropic)
    }

    pub fn label(self) -> &'static str {
        match self {
            Provider::Anthropic => "Anthropic (Claude)",
            Provider::OpenAi => "OpenAI",
            Provider::Gemini => "Google Gemini",
            Provider::Mistral => "Mistral",
            Provider::OpenRouter => "OpenRouter",
            Provider::Custom => "Other (OpenAI-compatible URL)",
        }
    }

    pub fn base_url(self) -> &'static str {
        match self {
            Provider::Anthropic => "https://api.anthropic.com/v1",
            Provider::OpenAi => "https://api.openai.com/v1",
            Provider::Gemini => "https://generativelanguage.googleapis.com/v1beta/openai",
            Provider::Mistral => "https://api.mistral.ai/v1",
            Provider::OpenRouter => "https://openrouter.ai/api/v1",
            Provider::Custom => "http://localhost:11434/v1",
        }
    }

    /// Where to get a key.
    pub fn key_help(self) -> &'static str {
        match self {
            Provider::Anthropic => "platform.claude.com › API keys",
            Provider::OpenAi => "platform.openai.com › API keys",
            Provider::Gemini => "aistudio.google.com › Get API key",
            Provider::Mistral => "console.mistral.ai › API keys",
            Provider::OpenRouter => "openrouter.ai › Keys",
            Provider::Custom => "your server's key (anything for a local Ollama)",
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Provider::Anthropic => "claude-opus-5-5",
            _ => "",
        }
    }
}

/// Saved in config.json; the keys go to the system keychain with the SECRET (`secrets.rs`).
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AiConfig {
    pub provider: String,
    /// Provider id → API key.
    pub keys: BTreeMap<String, String>,
    /// Provider id → chosen model.
    pub models: BTreeMap<String, String>,
    /// Base URL of the "Other" provider.
    pub custom_url: String,
}

impl AiConfig {
    pub fn provider(&self) -> Provider {
        Provider::from_id(&self.provider)
    }

    pub fn key(&self) -> &str {
        self.keys.get(self.provider().id()).map(|k| k.trim()).unwrap_or_default()
    }

    /// The assistant is offered only once a key is set.
    pub fn ready(&self) -> bool {
        !self.key().is_empty()
    }

    pub fn model(&self) -> String {
        let p = self.provider();
        self.models.get(p.id()).map(|m| m.trim().to_owned()).filter(|m| !m.is_empty()).unwrap_or_else(|| p.default_model().to_owned())
    }

    pub fn base_url(&self) -> String {
        let p = self.provider();
        if p == Provider::Custom && !self.custom_url.trim().is_empty() {
            self.custom_url.trim().trim_end_matches('/').to_owned()
        } else {
            p.base_url().to_owned()
        }
    }
}

// ---------------------------------------------------------------- conversation

#[derive(Clone, Debug)]
pub enum Part {
    Text(String),
    ToolCall { id: String, name: String, input: Value },
    ToolResult { id: String, content: String, is_error: bool },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Clone, Debug)]
pub struct Message {
    pub role: Role,
    pub parts: Vec<Part>,
    /// The answer exactly as the provider sent it, replayed unchanged to that provider: Anthropic's `content`
    /// (thinking blocks included), OpenAI's Responses `output` items (reasoning included). Another provider uses `parts`.
    pub raw: Option<(Provider, Value)>,
}

impl Message {
    pub fn user(parts: Vec<Part>) -> Self {
        Self { role: Role::User, parts, raw: None }
    }

    pub fn tool_calls(&self) -> Vec<(String, String, Value)> {
        self.parts
            .iter()
            .filter_map(|p| match p {
                Part::ToolCall { id, name, input } => Some((id.clone(), name.clone(), input.clone())),
                _ => None,
            })
            .collect()
    }
}

/// What the model answered.
pub struct Reply {
    pub message: Message,
    /// `refusal`, `max_tokens`… (None = normal end or tool use).
    pub warning: Option<String>,
    pub usage: Option<Usage>,
}

/// Tokens of one answer, from the provider's `usage` (to see that the prompt cache works).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    /// The whole prompt, cached part included.
    pub input: u64,
    /// Read back from the provider's cache.
    pub cached: u64,
    pub output: u64,
}

impl Usage {
    /// Anthropic: `input_tokens` is only the part not cached; the prompt is it + the cache writes + the reads.
    fn anthropic(u: &Value) -> Option<Self> {
        let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
        u.is_object().then(|| Usage {
            input: n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens"),
            cached: n("cache_read_input_tokens"),
            output: n("output_tokens"),
        })
    }

    /// OpenAI-compatible: Chat Completions (`prompt_tokens`) or Responses (`input_tokens`).
    fn openai(u: &Value) -> Option<Self> {
        let n = |p: &str| u.pointer(p).and_then(Value::as_u64);
        u.is_object().then(|| Usage {
            input: n("/prompt_tokens").or(n("/input_tokens")).unwrap_or(0),
            cached: n("/prompt_tokens_details/cached_tokens").or(n("/input_tokens_details/cached_tokens")).unwrap_or(0),
            output: n("/completion_tokens").or(n("/output_tokens")).unwrap_or(0),
        })
    }
}

pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: Value,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_global(Some(Duration::from_secs(600)))
        // Errors are read here: the provider says what's wrong in the body.
        .http_status_as_error(false)
        .user_agent(concat!("apifa/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// The provider's JSON answer, or what went wrong (its own message for an HTTP error).
fn answer(result: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<Value, String> {
    let mut resp = result.map_err(|e| e.to_string())?;
    let code = resp.status().as_u16();
    let body = resp.body_mut().with_config().limit(32 * 1024 * 1024).read_to_string().map_err(|e| e.to_string())?;
    if code >= 400 {
        return Err(http_error(code, &body));
    }
    serde_json::from_str(&body).map_err(|e| format!("not JSON: {e}"))
}

/// An HTTP error of the provider, with its own message and what to check.
fn http_error(code: u16, body: &str) -> String {
    let msg = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.pointer("/error/message").or_else(|| v.get("message")).or_else(|| v.pointer("/0/error/message")).and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| body.chars().take(300).collect());
    let hint = match code {
        401 | 403 => " — check the API key in Settings",
        404 => " — check the model name in Settings",
        429 => " — rate limited or out of credits",
        _ => "",
    };
    format!("HTTP {code}: {msg}{hint}")
}

/// Models that take `fallbacks: "default"` (a declined request is retried on another model server-side).
const FALLBACK_MODELS: [&str; 4] = ["claude-fable-5-1", "claude-opus-5-5", "claude-opus-5", "claude-sonnet-5-5"];
/// Models that take `output_config.effort`.
const EFFORT_MODELS: [&str; 9] = [
    "claude-fable-5-1",
    "claude-fable-5",
    "claude-opus-5-5",
    "claude-opus-5",
    "claude-sonnet-5-5",
    "claude-sonnet-5",
    "claude-haiku-5-5",
    "claude-opus-4-8",
    "claude-opus-4-7",
];

/// One call to the model with the whole conversation.
pub fn complete(cfg: &AiConfig, system: &str, tools: &[Tool], history: &[Message]) -> Result<Reply, String> {
    let model = cfg.model();
    if model.is_empty() {
        return Err("Pick a model in Settings › AI assistant".into());
    }
    match cfg.provider() {
        Provider::Anthropic => anthropic(cfg, &model, system, tools, history),
        // OpenAI's own API: the Responses API (tools + reasoning models; Chat Completions refuses that pair on some models).
        Provider::OpenAi => openai_responses(cfg, &model, system, tools, history),
        _ => openai(cfg, &model, system, tools, history),
    }
}

/// The Messages API request body. Assistant turns are replayed exactly as received (thinking blocks included).
fn anthropic_body(model: &str, system: &str, tools: &[Tool], history: &[Message]) -> Value {
    let messages: Vec<Value> = history
        .iter()
        .map(|m| {
            let content = match (&m.raw, m.role) {
                (Some((Provider::Anthropic, raw)), Role::Assistant) => raw.clone(),
                _ => Value::Array(
                    m.parts
                        .iter()
                        .map(|p| match p {
                            Part::Text(t) => json!({ "type": "text", "text": t }),
                            Part::ToolCall { id, name, input } => json!({ "type": "tool_use", "id": id, "name": name, "input": input }),
                            Part::ToolResult { id, content, is_error } => {
                                json!({ "type": "tool_result", "tool_use_id": id, "content": content, "is_error": is_error })
                            }
                        })
                        .collect(),
                ),
            };
            json!({ "role": if m.role == Role::User { "user" } else { "assistant" }, "content": content })
        })
        .collect();
    let tools: Vec<Value> = tools.iter().map(|t| json!({ "name": t.name, "description": t.description, "input_schema": t.schema })).collect();
    // Cached: the tools and the system prompt (rendered first, the same every request) up to the
    // marker on the system block, then the conversation so far (the top-level marker goes on its last
    // block). A later request reads both back at a tenth of the price.
    let system = json!([{ "type": "text", "text": system, "cache_control": { "type": "ephemeral" } }]);
    let mut body = json!({
        "model": model, "max_tokens": 16000, "system": system, "tools": tools, "messages": messages,
        "cache_control": { "type": "ephemeral" },
    });
    if EFFORT_MODELS.contains(&model) {
        body["output_config"] = json!({ "effort": "medium" });
    }
    if FALLBACK_MODELS.contains(&model) {
        body["fallbacks"] = json!("default");
    }
    body
}

fn anthropic(cfg: &AiConfig, model: &str, system: &str, tools: &[Tool], history: &[Message]) -> Result<Reply, String> {
    let body = anthropic_body(model, system, tools, history);
    let mut req = agent()
        .post(&format!("{}/messages", cfg.base_url()))
        .header("x-api-key", cfg.key())
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json");
    if body.get("fallbacks").is_some() {
        req = req.header("anthropic-beta", "server-side-fallback-2026-07-01");
    }
    let v = answer(req.send_json(body))?;
    let stop = v.get("stop_reason").and_then(Value::as_str).unwrap_or_default().to_owned();
    if stop == "refusal" {
        let why = v.pointer("/stop_details/explanation").and_then(Value::as_str).unwrap_or("no reason given");
        return Err(format!("The model declined this request ({why}). Rephrase it or start a new chat."));
    }
    let raw = v.get("content").cloned().unwrap_or(Value::Array(Vec::new()));
    let mut parts = Vec::new();
    for block in raw.as_array().into_iter().flatten() {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => parts.push(Part::Text(block["text"].as_str().unwrap_or_default().to_owned())),
            Some("tool_use") => parts.push(Part::ToolCall {
                id: block["id"].as_str().unwrap_or_default().to_owned(),
                name: block["name"].as_str().unwrap_or_default().to_owned(),
                input: block.get("input").cloned().unwrap_or(json!({})),
            }),
            _ => {}
        }
    }
    let warning = (stop == "max_tokens").then(|| "The answer was cut (too long).".to_owned());
    Ok(Reply { message: Message { role: Role::Assistant, parts, raw: Some((Provider::Anthropic, raw)) }, warning, usage: v.get("usage").and_then(Usage::anthropic) })
}

fn openai(cfg: &AiConfig, model: &str, system: &str, tools: &[Tool], history: &[Message]) -> Result<Reply, String> {
    let mut messages = vec![json!({ "role": "system", "content": system })];
    for m in history {
        match m.role {
            Role::Assistant => {
                let text: String = m.parts.iter().filter_map(|p| if let Part::Text(t) = p { Some(t.as_str()) } else { None }).collect::<Vec<_>>().join("\n");
                let calls: Vec<Value> = m
                    .tool_calls()
                    .into_iter()
                    .map(|(id, name, input)| json!({ "id": id, "type": "function", "function": { "name": name, "arguments": input.to_string() } }))
                    .collect();
                let mut msg = json!({ "role": "assistant", "content": if text.is_empty() { Value::Null } else { Value::String(text) } });
                if !calls.is_empty() {
                    msg["tool_calls"] = Value::Array(calls);
                }
                messages.push(msg);
            }
            Role::User => {
                for p in &m.parts {
                    match p {
                        Part::Text(t) => messages.push(json!({ "role": "user", "content": t })),
                        Part::ToolResult { id, content, .. } => messages.push(json!({ "role": "tool", "tool_call_id": id, "content": content })),
                        Part::ToolCall { .. } => {}
                    }
                }
            }
        }
    }
    let tools: Vec<Value> =
        tools.iter().map(|t| json!({ "type": "function", "function": { "name": t.name, "description": t.description, "parameters": t.schema } })).collect();
    let mut body = json!({ "model": model, "messages": messages, "tools": tools });
    let post = |body: &Value| {
        let r = agent()
            .post(&format!("{}/chat/completions", cfg.base_url()))
            .header("authorization", &format!("Bearer {}", cfg.key()))
            .header("content-type", "application/json")
            .send_json(body.clone());
        answer(r)
    };
    let resp = match post(&body) {
        // Some reasoning models refuse tools with their default reasoning here: retry with it off.
        Err(e) if e.contains("reasoning_effort") => {
            body["reasoning_effort"] = json!("none");
            post(&body)?
        }
        other => other?,
    };
    let v: Value = resp;
    let choice = v.pointer("/choices/0").ok_or_else(|| format!("unexpected answer: {}", v.to_string().chars().take(300).collect::<String>()))?;
    let msg = &choice["message"];
    let mut parts = Vec::new();
    if let Some(t) = msg.get("content").and_then(Value::as_str).filter(|t| !t.trim().is_empty()) {
        parts.push(Part::Text(t.to_owned()));
    }
    for (i, call) in msg.get("tool_calls").and_then(Value::as_array).into_iter().flatten().enumerate() {
        let args = call.pointer("/function/arguments");
        let input = match args {
            Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(json!({})),
            Some(other) => other.clone(),
            None => json!({}),
        };
        parts.push(Part::ToolCall {
            id: call.get("id").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(|| format!("call_{i}")),
            name: call.pointer("/function/name").and_then(Value::as_str).unwrap_or_default().to_owned(),
            input,
        });
    }
    let warning = match choice.get("finish_reason").and_then(Value::as_str) {
        Some("length") => Some("The answer was cut (too long).".to_owned()),
        Some("content_filter") => Some("The provider filtered the answer.".to_owned()),
        _ => None,
    };
    Ok(Reply { message: Message { role: Role::Assistant, parts, raw: None }, warning, usage: v.get("usage").and_then(Usage::openai) })
}

/// The Responses API request body. OpenAI's own output items are replayed unchanged (reasoning items included).
fn responses_body(model: &str, system: &str, tools: &[Tool], history: &[Message]) -> Value {
    let mut input = Vec::new();
    for m in history {
        match (m.role, &m.raw) {
            (Role::Assistant, Some((Provider::OpenAi, Value::Array(items)))) => input.extend(items.iter().cloned()),
            (Role::Assistant, _) => {
                for p in &m.parts {
                    match p {
                        Part::Text(t) => input.push(json!({ "role": "assistant", "content": t })),
                        Part::ToolCall { id, name, input: args } => {
                            input.push(json!({ "type": "function_call", "call_id": id, "name": name, "arguments": args.to_string() }))
                        }
                        Part::ToolResult { .. } => {}
                    }
                }
            }
            (Role::User, _) => {
                for p in &m.parts {
                    match p {
                        Part::Text(t) => input.push(json!({ "role": "user", "content": t })),
                        Part::ToolResult { id, content, .. } => input.push(json!({ "type": "function_call_output", "call_id": id, "output": content })),
                        Part::ToolCall { .. } => {}
                    }
                }
            }
        }
    }
    let tools: Vec<Value> = tools.iter().map(|t| json!({ "type": "function", "name": t.name, "description": t.description, "parameters": t.schema })).collect();
    json!({ "model": model, "instructions": system, "input": input, "tools": tools })
}

fn openai_responses(cfg: &AiConfig, model: &str, system: &str, tools: &[Tool], history: &[Message]) -> Result<Reply, String> {
    let v = answer(
        agent()
            .post(&format!("{}/responses", cfg.base_url()))
            .header("authorization", &format!("Bearer {}", cfg.key()))
            .header("content-type", "application/json")
            .send_json(responses_body(model, system, tools, history)),
    )?;
    if let Some(msg) = v.pointer("/error/message").and_then(Value::as_str) {
        return Err(msg.to_owned());
    }
    let output = v.get("output").cloned().unwrap_or(Value::Array(Vec::new()));
    let mut parts = Vec::new();
    for item in output.as_array().into_iter().flatten() {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                for c in item.get("content").and_then(Value::as_array).into_iter().flatten() {
                    let text = c.get("text").or_else(|| c.get("refusal")).and_then(Value::as_str).unwrap_or_default();
                    if !text.trim().is_empty() {
                        parts.push(Part::Text(text.to_owned()));
                    }
                }
            }
            Some("function_call") => {
                let args = item.get("arguments").and_then(Value::as_str).unwrap_or("{}");
                parts.push(Part::ToolCall {
                    id: item.get("call_id").and_then(Value::as_str).unwrap_or_default().to_owned(),
                    name: item.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
                    input: serde_json::from_str(args).unwrap_or(json!({})),
                });
            }
            _ => {}
        }
    }
    let warning = (v.get("status").and_then(Value::as_str) == Some("incomplete")).then(|| {
        let why = v.pointer("/incomplete_details/reason").and_then(Value::as_str).unwrap_or("unknown reason");
        format!("The answer is incomplete ({why}).")
    });
    Ok(Reply { message: Message { role: Role::Assistant, parts, raw: Some((Provider::OpenAi, output)) }, warning, usage: v.get("usage").and_then(Usage::openai) })
}

/// The provider's model list (ids, sorted).
pub fn list_models(cfg: &AiConfig) -> Result<Vec<String>, String> {
    let url = format!("{}/models", cfg.base_url());
    let req = match cfg.provider() {
        Provider::Anthropic => agent().get(&format!("{url}?limit=100")).header("x-api-key", cfg.key()).header("anthropic-version", "2023-06-01"),
        _ => agent().get(&url).header("authorization", &format!("Bearer {}", cfg.key())),
    };
    let v = answer(req.call())?;
    let mut ids: Vec<String> = v
        .get("data")
        .or_else(|| v.get("models"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| m.get("id").or_else(|| m.get("name")).and_then(Value::as_str))
        .map(|id| id.trim_start_matches("models/").to_owned())
        .collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

// ---------------------------------------------------------------- tools

fn obj(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required })
}

fn kv_list(desc: &str) -> Value {
    json!({ "type": "array", "description": desc, "items": { "type": "object", "properties": { "key": { "type": "string" }, "value": { "type": "string" } }, "required": ["key", "value"] } })
}

/// Tools sending something to the API: they wait for the user's OK.
pub const NEEDS_CONFIRMATION: [&str; 1] = ["send_request"];

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "read_docs",
            description: "A section of the app's documentation (file formats, command line). Without `section`: the list of sections.",
            schema: obj(json!({ "section": { "type": "string", "description": "A section title, or a word of it, e.g. \"Command line\", \"Flow\", \"Substitutions\"" } }), &[]),
        },
        Tool {
            name: "data_map",
            description: "The map of the user's API data, built from real GET responses by Understand: entities, their fields (types, formats, how often present), \
                          and how they link (embedded objects, lists, *_id references, confirmed when the ids match). No value: structure only. \
                          Use it to know where a piece of information lives and which endpoint returns it. \
                          Without arguments: the overview (each entity, its endpoints, what it uses / what uses it, the dependency layers).",
            schema: obj(
                json!({
                    "entity": { "type": "string", "description": "One entity (e.g. users, orders): its fields as a tree, two levels deep, and its links. \"all\": the whole map (long, may be cut: prefer the overview, then one entity)" },
                    "field": { "type": "string", "description": "A field name or part of it (e.g. email, user_id): every entity and path where it is" }
                }),
                &[],
            ),
        },
        Tool {
            name: "search_endpoints",
            description: "Search the imported API documentation. Returns matching endpoints (verb, path, resource › method).",
            schema: obj(json!({ "query": { "type": "string", "description": "Words, e.g. \"users orders\" or \"pets\"" } }), &["query"]),
        },
        Tool {
            name: "endpoint_details",
            description: "The documented parameters of one endpoint (required / optional, types, descriptions).",
            schema: obj(json!({ "verb": { "type": "string" }, "path": { "type": "string", "description": "e.g. /v1/users/:user_id/orders" } }), &["verb", "path"]),
        },
        Tool {
            name: "list_workspace",
            description: "The user's saved requests and flows (with folders, flow inputs and outputs).",
            schema: obj(json!({}), &[]),
        },
        Tool {
            name: "read_saved",
            description: "The JSON of a saved request or flow, by name.",
            schema: obj(json!({ "name": { "type": "string" } }), &["name"]),
        },
        Tool {
            name: "current_tab",
            description: "What the user has open: the request (and its last response, shortened) or the flow (and its console).",
            schema: obj(json!({}), &[]),
        },
        Tool {
            name: "open_request",
            description: "Open a new request tab, filled in and ready: the user reviews it and presses Send. Use this to create requests. Nothing is sent.",
            schema: obj(
                json!({
                    "title": { "type": "string", "description": "Tab name" },
                    "verb": { "type": "string", "enum": ["GET", "POST", "PUT", "PATCH", "DELETE"] },
                    "protocol": { "type": "string", "enum": ["http", "web_socket", "grpc"], "description": "Default http" },
                    "path": { "type": "string", "description": "A path relative to the base URL (/users/:id), or a full URL (https://…, wss://…); :name segments are filled from path_values" },
                    "path_values": { "type": "object", "description": "Values of the :name segments, e.g. {\"user_id\": \"7\"}" },
                    "query": kv_list("Query parameters"),
                    "headers": kv_list("Headers (gRPC: metadata)"),
                    "body": { "type": "object", "description": "JSON body for POST / PUT / PATCH (gRPC: the message)" },
                    "graphql": { "type": "object", "description": "GraphQL instead of a JSON body: {\"query\": \"…\", \"variables\": {…}}" },
                    "auth": { "type": "object", "description": "Optional auth (default: inherit the folder's / workspace's): {\"type\": \"bearer\", \"token\": \"{{token}}\"}, {\"type\": \"basic\", \"username\", \"password\"}, {\"type\": \"api_key\", \"key\", \"value\", \"location\": \"header|query\"}, {\"type\": \"oauth2\", \"grant\": \"client_credentials|password|authorization_code\", \"token_url\", \"client_id\", \"client_secret\", \"scope\"}, {\"type\": \"none\"}. Put secrets in {{variables}}." },
                    "pre_script": { "type": "string", "description": "Optional JavaScript run before sending (Postman's pm API: pm.environment.set, pm.request.headers.upsert…)" },
                    "post_script": { "type": "string", "description": "Optional JavaScript run on the response: pm.test(name, () => pm.expect(…)), pm.response.json(), pm.environment.set(…)" },
                    "grpc_method": { "type": "string", "description": "gRPC: /package.Service/Method" },
                    "files": kv_list("Files to upload: key = field (user[image]), value = local path"),
                    "visualizer": { "type": "string", "description": "Optional HTML + JS to show the response in the browser (variables: response, body)" }
                }),
                &["verb", "path"],
            ),
        },
        Tool {
            name: "open_flow",
            description: "Open a new flow tab (edit mode) from a flow JSON in the documented format (name, description, inputs, blocks, outputs). The user reviews, saves and runs it. Returns validation problems if any.",
            schema: obj(json!({ "flow": { "type": "object", "description": "The flow, same format as a .flow.json file" } }), &["flow"]),
        },
        Tool {
            name: "list_substitutions",
            description: "The user's substitutions (a login typed where an id is expected is looked up before sending).",
            schema: obj(json!({}), &[]),
        },
        Tool {
            name: "open_substitution",
            description: "Open the substitution editor filled with a new rule; the user tests it and clicks Save.",
            schema: obj(
                json!({
                    "name": { "type": "string" },
                    "description": { "type": "string" },
                    "fields": { "type": "array", "items": { "type": "string" }, "description": "Field names it watches, e.g. [\"user_id\"]" },
                    "when": { "type": "string", "enum": ["not_number", "always"] },
                    "path": { "type": "string", "description": "GET path of the lookup, e.g. /v1/users" },
                    "query": kv_list("Lookup query; {{value}} is what the user typed, e.g. filter[login]={{value}}"),
                    "extract": { "type": "string", "description": "Where the value is: body[0].id" }
                }),
                &["name", "fields", "path", "extract"],
            ),
        },
        Tool {
            name: "send_request",
            description: "Send a request to the API and get the response. The user must confirm first (they may refuse). Prefer open_request when the user wants to send it themselves.",
            schema: obj(
                json!({
                    "verb": { "type": "string", "enum": ["GET", "POST", "PUT", "PATCH", "DELETE"] },
                    "path": { "type": "string" },
                    "query": kv_list("Query parameters"),
                    "body": { "type": "object", "description": "JSON body" },
                    "why": { "type": "string", "description": "One sentence shown to the user: why you want to send it" }
                }),
                &["verb", "path"],
            ),
        },
    ]
}

/// The app's documentation (`AGENTS.md`) cut at its `## ` headings: (title, text). The first one is the introduction.
pub fn doc_sections() -> Vec<(&'static str, &'static str)> {
    const DOC: &str = include_str!("../AGENTS.md");
    let mut out = Vec::new();
    let mut starts: Vec<usize> = DOC.match_indices("\n## ").map(|(i, _)| i + 1).collect();
    starts.insert(0, 0);
    starts.push(DOC.len());
    for w in starts.windows(2) {
        let text = &DOC[w[0]..w[1]];
        let title = text.lines().next().unwrap_or_default().trim_start_matches('#').trim();
        out.push((title, text.trim_end()));
    }
    out
}

/// `read_docs`: one section (by title or a word of it), or the list of them.
pub fn read_docs(section: &str) -> String {
    let all = doc_sections();
    let want = section.trim().to_lowercase();
    if !want.is_empty() {
        if let Some((_, text)) = all.iter().find(|(t, _)| t.to_lowercase().contains(&want)) {
            return (*text).to_owned();
        }
    }
    let list: Vec<&str> = all.iter().skip(1).map(|(t, _)| *t).collect();
    format!("Sections: {}", list.join(" · "))
}

/// Sections the assistant doesn't need for its own tools: on demand (`read_docs`) instead of in every request.
const ON_DEMAND: [&str; 1] = ["Command line"];

/// System prompt: the app, the file formats (AGENTS.md, but for the on-demand sections) and the rules.
/// Today's date goes last: everything before it is the same from one request to the next (cached).
pub fn system_prompt(today: &str) -> String {
    let docs: Vec<&str> = doc_sections().into_iter().filter(|(t, _)| !ON_DEMAND.iter().any(|o| t.starts_with(o))).map(|(_, text)| text).collect();
    format!(
        "You are the assistant inside \"apifa\" (API For All), a desktop API client like Postman or Bruno: HTTP, GraphQL, WebSocket and gRPC requests, flows, tests. \
Answer in the user's language, briefly.\n\n\
What you can do with your tools: search the docs of the APIs the user imported, read the user's saved requests / flows / substitutions and the open tab, \
open new request tabs and flow tabs already filled in (the user reviews them and presses Send / Run / Save), open a new substitution in its editor, \
and send a request yourself with send_request (the user confirms each one).\n\n\
Rules:\n\
- Before writing a request or a flow, check the endpoint and its parameters with search_endpoints / endpoint_details: never invent paths or fields.\n\
- data_map tells where each piece of information lives (entities, fields, links), built from the user's real GET responses: check it before writing a flow.\n\
- What tools give you is anonymized: no value of the user's data ever reaches you, only its structure (types, and fingerprints: equal values = equal fingerprints). Don't ask the user to paste data; work from the structure. \
To use a value you were given (a login, an id), write its fingerprint as is (`string#5d1e0a9b3c`, `integer#…`) in the tool input (path, query, body, flow input): \
the app puts the real value back on the user's computer before using it, and shows your answers to the user with the real values. \
Logins and names in the user's messages may come to you as fingerprints too: use them the same way.\n\
- A field listed for sort / filter isn't always returned. Before a flow tests a field of the response, look at one real element (send_request with page[size]=1, or a docs example).\n\
- To create something, open it (open_request / open_flow / open_substitution) and tell the user what to check and which button to press. You can't save files yourself.\n\
- Only use send_request when you need data to answer, or when the user asks you to send it. It waits for the user's OK.\n\
- The app authenticates with an app token (client_credentials, `public` scope): /v1/me and staff-only endpoints answer 401/403.\n\
- Flows: follow the format below exactly. In a flow, a request block with documented fields can use \"values\" instead of a JSON body.\n\n\
The app's file formats and conventions (read_docs gives the command line and any section again):\n\n{}\n\nToday is {today}.",
        docs.join("\n\n")
    )
}

/// Tool input helper: `[{key, value}]` → pairs.
pub fn pairs(v: Option<&Value>) -> Vec<(String, String)> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|kv| {
            let k = kv.get("key")?.as_str()?.to_owned();
            let v = match kv.get("value")? {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            Some((k, v))
        })
        .collect()
}

pub fn string_map(v: Option<&Value>) -> BTreeMap<String, String> {
    v.and_then(Value::as_object)
        .map(|o: &Map<String, Value>| {
            o.iter()
                .map(|(k, v)| {
                    let v = match v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    (k.clone(), v)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A body given as an object, or as JSON text.
pub fn body_text(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

/// Long tool results are cut (the model doesn't need 2 MB of JSON).
pub fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… ({} more bytes cut)", &s[..end], s.len() - end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_schemas_are_objects() {
        for t in tools() {
            assert_eq!(t.schema["type"], "object", "{}", t.name);
            assert!(t.schema["properties"].is_object(), "{}", t.name);
        }
        let sys = system_prompt("2026-10-16");
        assert!(sys.contains("Saved request"), "AGENTS.md is embedded");
        assert!(!sys.contains("apifa mock [--port 7643]"), "the command line is read on demand");
        assert!(sys.trim_end().ends_with("Today is 2026-10-16."), "the date last: the rest stays cached");
        assert!(read_docs("command").contains("apifa mock [--port 7643]"));
        assert!(read_docs("").starts_with("Sections: ") && read_docs("").contains("Flow"));
    }

    #[test]
    fn config_defaults() {
        let mut c = AiConfig::default();
        assert!(!c.ready());
        assert_eq!(c.model(), "claude-opus-5-5");
        c.keys.insert("anthropic".into(), " sk-x ".into());
        assert!(c.ready());
        c.provider = "openai".into();
        assert!(!c.ready(), "keys are per provider");
        assert_eq!(c.model(), "", "no default model outside Anthropic");
        c.provider = "custom".into();
        c.custom_url = "http://localhost:1234/v1/".into();
        assert_eq!(c.base_url(), "http://localhost:1234/v1");
    }

    #[test]
    fn anthropic_wire_format() {
        let thinking = json!([{ "type": "thinking", "thinking": "", "signature": "sig" }, { "type": "tool_use", "id": "t1", "name": "list_workspace", "input": {} }]);
        let history = vec![
            Message::user(vec![Part::Text("hi".into())]),
            Message {
                role: Role::Assistant,
                parts: vec![Part::ToolCall { id: "t1".into(), name: "list_workspace".into(), input: json!({}) }],
                raw: Some((Provider::Anthropic, thinking.clone())),
            },
            Message::user(vec![Part::ToolResult { id: "t1".into(), content: "empty".into(), is_error: false }]),
        ];
        let body = anthropic_body("claude-opus-5-5", "sys", &tools(), &history);
        assert_eq!(body["messages"][1]["content"], thinking, "assistant turns are replayed unchanged");
        assert_eq!(body["messages"][2]["content"][0], json!({ "type": "tool_result", "tool_use_id": "t1", "content": "empty", "is_error": false }));
        assert_eq!(body["fallbacks"], "default");
        assert_eq!(body["output_config"]["effort"], "medium");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral", "tools + system prompt cached");
        assert_eq!(body["cache_control"]["type"], "ephemeral", "the conversation cached");
        let u = Usage::anthropic(&json!({ "input_tokens": 50, "cache_creation_input_tokens": 200, "cache_read_input_tokens": 9000, "output_tokens": 30 })).unwrap();
        assert_eq!(u, Usage { input: 9250, cached: 9000, output: 30 }, "input_tokens alone is only the uncached rest");
        assert!(body.get("thinking").is_none(), "Opus 5.5 thinks adaptively by default; disabling it is a 400");
        let haiku = anthropic_body("claude-haiku-5-5", "sys", &tools(), &history);
        assert!(haiku.get("fallbacks").is_none(), "no server-side fallback for Haiku");
    }

    #[test]
    fn responses_wire_format() {
        let output = json!([
            { "type": "reasoning", "id": "rs_1", "summary": [] },
            { "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "list_workspace", "arguments": "{}" }
        ]);
        let history = vec![
            Message::user(vec![Part::Text("hi".into())]),
            Message {
                role: Role::Assistant,
                parts: vec![Part::ToolCall { id: "call_1".into(), name: "list_workspace".into(), input: json!({}) }],
                raw: Some((Provider::OpenAi, output.clone())),
            },
            Message::user(vec![Part::ToolResult { id: "call_1".into(), content: "empty".into(), is_error: false }]),
        ];
        let body = responses_body("gpt-x", "sys", &tools(), &history);
        let input = body["input"].as_array().unwrap();
        assert_eq!(input[0], json!({ "role": "user", "content": "hi" }));
        assert_eq!(input[1], output[0], "reasoning items are replayed");
        assert_eq!(input[2], output[1]);
        assert_eq!(input[3], json!({ "type": "function_call_output", "call_id": "call_1", "output": "empty" }));
        assert_eq!(body["instructions"], "sys");
        assert_eq!(body["tools"][0]["type"], "function");
        assert!(body["tools"][0].get("function").is_none(), "Responses tools are flat");
        // A turn from another provider is rebuilt from its parts.
        let mut other = history.clone();
        other[1].raw = Some((Provider::Anthropic, json!([])));
        let body = responses_body("gpt-x", "sys", &tools(), &other);
        assert_eq!(body["input"][1], json!({ "type": "function_call", "call_id": "call_1", "name": "list_workspace", "arguments": "{}" }));
    }

    #[test]
    fn helpers() {
        assert_eq!(pairs(Some(&json!([{ "key": "page[size]", "value": 5 }]))), vec![("page[size]".to_owned(), "5".to_owned())]);
        assert_eq!(body_text(Some(&json!({ "a": 1 }))), "{\n  \"a\": 1\n}");
        assert!(clip(&"é".repeat(100), 51).ends_with("bytes cut)"));
    }
}
