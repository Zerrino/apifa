//! An API definition read from OpenAPI 3.0/3.1 or Swagger 2.0 (JSON or YAML) → the app's docs model
//! (`docs::ApiDoc`), so the endpoint list, forms, examples and generated docs work on any API.
//!
//! Mapping: a resource per tag (first tag of an operation; untagged → first literal path segment), a
//! method + route per operation (`{id}` → `:id`). Path and query parameters and the request body's
//! fields become params (`user[login]` groups for nested objects, like apipie). Header and cookie
//! parameters aren't params (the app would send them as query / body fields): they are listed in
//! the method's description. HEAD / OPTIONS / TRACE operations are left out (the app sends the five
//! usual verbs). Only local `$ref`s (`#/…`) are followed; recursive schemas stop at the first repeat.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

use crate::docs::{ApiDoc, Example, Method, Param, ParamKind, Resource, Route};

/// Nesting of body fields turned into groups; deeper objects are one JSON field.
const MAX_DEPTH: usize = 6;
/// Nesting of the sample responses made from a schema.
const SAMPLE_DEPTH: usize = 5;
const VERBS: [&str; 5] = ["get", "post", "put", "patch", "delete"];

pub struct Imported {
    pub doc: ApiDoc,
    pub title: String,
    pub version: String,
    /// OpenAPI 3 `servers` (variables at their default), Swagger 2 `schemes` + `host` + `basePath`.
    pub base_urls: Vec<String>,
    pub security: Vec<SecurityScheme>,
}

#[derive(Clone, Debug)]
pub enum SecurityScheme {
    Basic { name: String },
    Bearer { name: String, format: String },
    ApiKey { name: String, key: String, location: String },
    OAuth2 { name: String, flows: Vec<OAuthFlow> },
}

#[derive(Clone, Debug)]
pub struct OAuthFlow {
    /// `clientCredentials`, `authorizationCode`, `password` or `implicit` (Swagger 2 names are converted).
    pub kind: String,
    pub auth_url: String,
    pub token_url: String,
    pub refresh_url: String,
    pub scopes: Vec<(String, String)>,
}

/// Reads an OpenAPI 3.x / Swagger 2.0 document (JSON or YAML text).
pub fn import(text: &str) -> Result<Imported, String> {
    let root = parse(text)?;
    let swagger2 = match (root.get("swagger"), root.get("openapi")) {
        (Some(v), _) if text_of(v).starts_with('2') => true,
        (_, Some(v)) if text_of(v).starts_with('3') => false,
        _ => return Err("not an OpenAPI 3 or Swagger 2.0 document (no `openapi: 3.x` or `swagger: \"2.0\"`)".into()),
    };
    let cx = Cx { root: &root, swagger2 };
    let info = root.get("info");
    let title = info.and_then(|i| i.get("title")).map(text_of).unwrap_or_default();
    let version = info.and_then(|i| i.get("version")).map(text_of).unwrap_or_default();
    Ok(Imported { doc: cx.doc(), title, version, base_urls: cx.base_urls(), security: cx.security() })
}

fn parse(text: &str) -> Result<Value, String> {
    let t = text.trim_start_matches('\u{feff}').trim_start();
    if t.starts_with('{') {
        return serde_json::from_str(t).map_err(|e| format!("invalid JSON: {e}"));
    }
    let y: serde_yaml_ng::Value = serde_yaml_ng::from_str(t).map_err(|e| format!("invalid YAML: {e}"))?;
    Ok(yaml_to_json(y))
}

fn yaml_to_json(y: serde_yaml_ng::Value) -> Value {
    use serde_yaml_ng::Value as Y;
    match y {
        Y::Null => Value::Null,
        Y::Bool(b) => Value::Bool(b),
        Y::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.into()
            } else if let Some(u) = n.as_u64() {
                u.into()
            } else {
                n.as_f64().and_then(serde_json::Number::from_f64).map(Value::Number).unwrap_or(Value::Null)
            }
        }
        Y::String(s) => Value::String(s),
        Y::Sequence(seq) => Value::Array(seq.into_iter().map(yaml_to_json).collect()),
        // `200:` keys are numbers in YAML.
        Y::Mapping(m) => Value::Object(m.into_iter().map(|(k, v)| (text_of(&yaml_to_json(k)), yaml_to_json(v))).collect()),
        Y::Tagged(t) => yaml_to_json(t.value),
    }
}

/// A scalar as text (`2.0` → `2.0`, `"x"` → `x`).
fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn str_field<'a>(m: &'a Map<String, Value>, key: &str) -> &'a str {
    m.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn is_true(m: &Map<String, Value>, key: &str) -> bool {
    m.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// `Pet Store` → `pet_store`.
fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    out
}

/// A path variable name the app's `:name` syntax can hold.
fn var_name(s: &str) -> String {
    s.chars().map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' }).collect()
}

/// `/pets/{pet-id}` → `/pets/:pet_id`.
fn convert_path(p: &str) -> String {
    let mut out = String::with_capacity(p.len());
    let mut rest = p;
    while let Some(start) = rest.find('{') {
        let Some(len) = rest[start..].find('}') else { break };
        out.push_str(&rest[..start]);
        out.push(':');
        out.push_str(&var_name(&rest[start + 1..start + len]));
        rest = &rest[start + len + 1..];
    }
    out.push_str(rest);
    out
}

/// The resource of an untagged operation: its first literal segment, versions (`v1`, `api`) aside.
fn untagged_resource(path: &str) -> String {
    path.split('/')
        .filter(|s| !s.is_empty() && !s.contains('{'))
        .find(|s| {
            let l = s.to_lowercase();
            l != "api" && !(l.len() > 1 && l.starts_with('v') && l[1..].chars().all(|c| c.is_ascii_digit() || c == '.'))
        })
        .unwrap_or("default")
        .to_owned()
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = (b[i] == b'%').then(|| b.get(i + 1..i + 3)).flatten().and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok());
        match hex {
            Some(n) => {
                out.push(n);
                i += 3;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn pretty(v: &Value) -> String {
    match v {
        Value::String(s) => match serde_json::from_str::<Value>(s) {
            Ok(inner) if inner.is_object() || inner.is_array() => serde_json::to_string_pretty(&inner).unwrap_or_else(|_| s.clone()),
            _ => s.clone(),
        },
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

/// The JSON media type of a `content` map (else multipart, form, then the first).
fn pick_media(content: &Map<String, Value>) -> Option<(&str, &Value)> {
    let json = |k: &str| {
        let k = k.split(';').next().unwrap_or(k).trim();
        k == "application/json" || k.ends_with("+json") || k == "*/*"
    };
    content
        .iter()
        .find(|(k, _)| json(k))
        .or_else(|| content.iter().find(|(k, _)| k.starts_with("multipart/form-data")))
        .or_else(|| content.iter().find(|(k, _)| k.starts_with("application/x-www-form-urlencoded")))
        .or_else(|| content.iter().next())
        .map(|(k, v)| (k.as_str(), v))
}

/// The type of a schema (3.1 `["string", "null"]` included) and whether it can be null.
fn type_of(s: &Map<String, Value>) -> (String, bool) {
    let mut nullable = is_true(s, "nullable") || is_true(s, "x-nullable");
    let ty = match s.get("type") {
        Some(Value::String(t)) => t.clone(),
        Some(Value::Array(ts)) => {
            nullable |= ts.iter().any(|t| t == "null");
            ts.iter().filter_map(Value::as_str).find(|t| *t != "null").unwrap_or("null").to_owned()
        }
        _ => {
            let first = s.get("enum").and_then(|e| e.as_array()).and_then(|e| e.iter().find(|v| !v.is_null())).or_else(|| s.get("const"));
            if s.contains_key("properties") || s.contains_key("additionalProperties") {
                "object".into()
            } else if s.contains_key("items") {
                "array".into()
            } else {
                match first {
                    Some(Value::String(_)) => "string".into(),
                    Some(Value::Bool(_)) => "boolean".into(),
                    Some(Value::Number(n)) if n.is_f64() => "number".into(),
                    Some(Value::Number(_)) => "integer".into(),
                    _ => String::new(),
                }
            }
        }
    };
    (ty, nullable)
}

fn enum_options(s: &Map<String, Value>) -> Vec<String> {
    s.get("enum").and_then(Value::as_array).map(|e| e.iter().filter(|v| !v.is_null()).map(text_of).collect()).unwrap_or_default()
}

fn is_binary(s: &Map<String, Value>) -> bool {
    matches!(str_field(s, "format"), "binary") || str_field(s, "type") == "file" || s.contains_key("contentMediaType") && !s.contains_key("contentEncoding")
}

/// What a field must be, apipie style (shown next to it).
fn validator(s: &Map<String, Value>, kind: ParamKind, options: &[String]) -> String {
    let mut parts = vec![match kind {
        ParamKind::Enum => format!("Must be one of: {}", options.join(", ")),
        ParamKind::String => "Must be a String".into(),
        ParamKind::Integer => "Must be an Integer".into(),
        ParamKind::Float => "Must be a Float".into(),
        ParamKind::Bool => "Must be one of: true, false".into(),
        ParamKind::DateTime => "Must be a DateTime".into(),
        ParamKind::Hash => "Must be a Hash".into(),
        ParamKind::Array if !options.is_empty() => format!("Must be an Array of: {}", options.join(", ")),
        ParamKind::Array => "Must be an Array".into(),
        ParamKind::IntArray => "Must be an Array of Integer".into(),
        ParamKind::NestedArray => "Must be an Array of nested elements".into(),
        ParamKind::File => "Must be a File".into(),
    }];
    let format = str_field(s, "format");
    if !format.is_empty() && !matches!(format, "binary" | "date-time" | "int32" | "int64") {
        parts.push(format!("format {format}"));
    }
    for (key, label) in [
        ("minimum", "≥"),
        ("exclusiveMinimum", ">"),
        ("maximum", "≤"),
        ("exclusiveMaximum", "<"),
        ("minLength", "min length"),
        ("maxLength", "max length"),
        ("minItems", "min items"),
        ("maxItems", "max items"),
        ("pattern", "pattern"),
        ("default", "default"),
    ] {
        // 3.0 `exclusiveMinimum: true` qualifies `minimum`.
        if let Some(v) = s.get(key).filter(|v| !v.is_boolean()) {
            parts.push(format!("{label} {}", text_of(v)));
        }
    }
    parts.join("; ")
}

fn sample_string(format: &str) -> &'static str {
    match format {
        "date-time" => "2026-01-01T00:00:00Z",
        "date" => "2026-01-01",
        "time" => "12:00:00",
        "email" => "user@example.com",
        "uuid" => "3fa85f64-5717-4562-b3fc-2c963f66afa6",
        "uri" | "url" => "https://example.com",
        "hostname" => "example.com",
        "ipv4" => "192.0.2.1",
        "ipv6" => "2001:db8::1",
        "byte" => "U3dhZ2dlciByb2Nrcw==",
        "binary" => "<binary>",
        "password" => "********",
        _ => "string",
    }
}

struct Cx<'a> {
    root: &'a Value,
    swagger2: bool,
}

impl<'a> Cx<'a> {
    fn pointer(&self, r: &str) -> Option<&'a Value> {
        let p = r.strip_prefix('#')?;
        self.root.pointer(&percent_decode(p))
    }

    /// `v` with its `$ref` followed (parameters, responses, request bodies, examples).
    fn deref<'v>(&self, v: &'v Value) -> &'v Value
    where
        'a: 'v,
    {
        let mut v = v;
        for _ in 0..16 {
            match v.get("$ref").and_then(Value::as_str).and_then(|r| self.pointer(r)) {
                Some(target) => v = target,
                None => break,
            }
        }
        v
    }

    /// A schema with its `$ref`s followed, `allOf` merged and the first `oneOf` / `anyOf` variant taken
    /// (properties stay as written), and the refs gone through. None on a cycle (a ref of `stack`).
    fn norm(&self, v: &Value, stack: &[String]) -> Option<(Map<String, Value>, Vec<String>)> {
        let mut refs = Vec::new();
        let mut out = Map::new();
        self.norm_into(v, stack, &mut refs, &mut out, 0)?;
        Some((out, refs))
    }

    fn norm_into(&self, v: &Value, stack: &[String], refs: &mut Vec<String>, out: &mut Map<String, Value>, depth: usize) -> Option<()> {
        let obj = match v {
            Value::Object(o) if depth < 32 => o,
            _ => return Some(()),
        };
        if let Some(r) = obj.get("$ref").and_then(Value::as_str) {
            if stack.iter().chain(refs.iter()).any(|s| s == r) {
                return None;
            }
            refs.push(r.to_owned());
            if let Some(target) = self.pointer(r) {
                self.norm_into(target, stack, refs, out, depth + 1)?;
            }
        }
        for part in obj.get("allOf").and_then(Value::as_array).into_iter().flatten() {
            self.norm_into(part, stack, refs, out, depth + 1)?;
        }
        let variants = obj.get("oneOf").or_else(|| obj.get("anyOf")).and_then(Value::as_array);
        if let Some(variants) = variants.filter(|_| !obj.contains_key("type") && !obj.contains_key("properties")) {
            let is_null = |v: &&Value| v.get("type").is_some_and(|t| t == "null");
            if variants.iter().any(|v| is_null(&v)) {
                out.insert("nullable".into(), Value::Bool(true));
            }
            if let Some(first) = variants.iter().find(|v| !is_null(v)) {
                let (before, mut variant) = (refs.len(), Map::new());
                if self.norm_into(first, stack, refs, &mut variant, depth + 1).is_some() {
                    merge(out, variant);
                } else {
                    refs.truncate(before);
                }
            }
        }
        let own = obj.iter().filter(|(k, _)| !matches!(k.as_str(), "$ref" | "allOf" | "oneOf" | "anyOf")).map(|(k, v)| (k.clone(), v.clone())).collect();
        merge(out, own);
        Some(())
    }

    fn doc(&self) -> ApiDoc {
        let tags: Vec<&Value> = self.root.get("tags").and_then(Value::as_array).into_iter().flatten().collect();
        let tag_desc: HashMap<String, String> =
            tags.iter().filter_map(|t| Some((t.get("name").map(text_of)?, t.get("description").map(text_of).unwrap_or_default()))).collect();
        // Tags in the order of the `tags` list, then as operations use them.
        let mut groups: Vec<(String, Vec<Method>)> = tags.iter().filter_map(|t| t.get("name").map(text_of)).map(|n| (n, Vec::new())).collect();
        let mut used_names = HashSet::new();
        for (path, item) in self.root.get("paths").and_then(Value::as_object).into_iter().flatten() {
            let Some(item) = self.deref(item).as_object() else { continue };
            for verb in VERBS {
                let Some(op) = item.get(verb).and_then(Value::as_object) else { continue };
                let tag = op.get("tags").and_then(Value::as_array).and_then(|t| t.first()).map(text_of).filter(|t| !t.trim().is_empty());
                let tag = tag.unwrap_or_else(|| untagged_resource(path));
                let method = self.method(verb, path, item, op, &mut used_names);
                match groups.iter_mut().find(|(t, _)| *t == tag) {
                    Some((_, methods)) => methods.push(method),
                    None => groups.push((tag, vec![method])),
                }
            }
        }
        let mut keys = HashSet::new();
        let resources = groups
            .into_iter()
            .filter(|(_, methods)| !methods.is_empty())
            .map(|(tag, methods)| {
                let key = unique(&Some(slug(&tag)).filter(|s| !s.is_empty()).unwrap_or_else(|| "default".into()), &mut keys);
                Resource { key, description: tag_desc.get(&tag).map(|d| d.trim().to_owned()).unwrap_or_default(), name: tag, methods, api: String::new() }
            })
            .collect();
        ApiDoc::from_resources(resources)
    }

    fn method(&self, verb: &str, path: &str, item: &Map<String, Value>, op: &Map<String, Value>, used: &mut HashSet<String>) -> Method {
        let route_path = convert_path(path);
        let summary = str_field(op, "summary").trim().to_owned();
        let long = str_field(op, "description").trim().to_owned();
        let mut description = [summary.as_str(), long.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n\n");

        // Path-level parameters, replaced by operation-level ones with the same name and place.
        let mut raw: Vec<&Map<String, Value>> = Vec::new();
        for p in item.get("parameters").into_iter().chain(op.get("parameters")).filter_map(Value::as_array).flatten() {
            let Some(p) = self.deref(p).as_object() else { continue };
            let id = |m: &Map<String, Value>| (str_field(m, "name").to_owned(), str_field(m, "in").to_owned());
            raw.retain(|q| id(q) != id(p));
            raw.push(p);
        }
        let mut params = Vec::new();
        let mut headers = Vec::new();
        let mut request_example = None;
        let mut stack = Vec::new();
        for p in raw {
            let name = str_field(p, "name");
            let desc = p.get("description").map(text_of).filter(|d| !d.trim().is_empty());
            let required = is_true(p, "required");
            let schema = self.param_schema(p);
            match str_field(p, "in") {
                "path" => params.extend(self.field(&var_name(name), var_name(name), &schema, true, desc, MAX_DEPTH, &mut stack)),
                "query" => {
                    let s = self.norm(&schema, &stack).map(|(s, _)| s).unwrap_or_default();
                    match s.get("properties").and_then(Value::as_object).filter(|_| type_of(&s).0 == "object") {
                        // `style: form` (the default) spreads an object's fields as their own parameters.
                        Some(props) if str_field(p, "style") != "deepObject" => {
                            let req = required_list(&s);
                            for (k, pv) in props {
                                params.extend(self.field(k, k.clone(), pv, req.contains(k), None, 0, &mut stack));
                            }
                        }
                        _ => params.extend(self.field(name, name.to_owned(), &schema, required, desc, 0, &mut stack)),
                    }
                }
                "formData" => params.extend(self.field(name, name.to_owned(), &schema, required, desc, 0, &mut stack)),
                "body" => {
                    params.extend(self.body_fields(&schema));
                    request_example = self.norm(&schema, &[]).and_then(|(s, _)| s.get("example").cloned());
                }
                place @ ("header" | "cookie") => {
                    let req = if required { ", required" } else { "" };
                    let d = desc.map(|d| format!(": {}", d.trim())).unwrap_or_default();
                    headers.push(format!("• {name} ({place}{req}){d}"));
                }
                _ => {}
            }
        }
        if let Some(body) = op.get("requestBody").map(|b| self.deref(b)) {
            if let Some((_, media)) = body.get("content").and_then(Value::as_object).and_then(pick_media) {
                let schema = media.get("schema").cloned().unwrap_or_default();
                params.extend(self.body_fields(&schema));
                request_example = self.media_example(media).or_else(|| self.norm(&schema, &[]).and_then(|(s, _)| s.get("example").cloned()));
            }
        }
        if !headers.is_empty() {
            if !description.is_empty() {
                description.push_str("\n\n");
            }
            description.push_str("Header / cookie parameters (add them in Headers):\n");
            description.push_str(&headers.join("\n"));
        }

        let mut examples = self.examples(verb, &route_path, op);
        if let Some(req) = request_example {
            match examples.first_mut() {
                Some(e) => e.request = Some(pretty(&req)),
                None => examples.push(Example {
                    verb: verb.to_uppercase(),
                    path: route_path.clone(),
                    query: String::new(),
                    code: String::new(),
                    request: Some(pretty(&req)),
                    response: None,
                }),
            }
        }

        let base = op.get("operationId").map(text_of).filter(|s| !s.trim().is_empty()).unwrap_or_else(|| slug(&format!("{verb} {path}")));
        let name = unique(&base, used);
        let route_summary = if summary.is_empty() { long.lines().next().unwrap_or_default().trim().to_owned() } else { summary };
        let route = Route { verb: verb.to_uppercase(), path: route_path, summary: route_summary, deprecated: is_true(op, "deprecated"), hay: String::new() };
        Method { name, description, routes: vec![route], params, examples }
    }

    /// The schema of a parameter: OpenAPI 3 has it in `schema` / `content`, Swagger 2 on the
    /// parameter itself (`type`, `format`, `items`, `enum`…) except for a body.
    fn param_schema(&self, p: &Map<String, Value>) -> Value {
        if let Some(s) = p.get("schema") {
            return s.clone();
        }
        if let Some((_, media)) = p.get("content").and_then(Value::as_object).and_then(pick_media) {
            return media.get("schema").cloned().unwrap_or_default();
        }
        if self.swagger2 {
            let mut s = p.clone();
            s.remove("description");
            return Value::Object(s);
        }
        Value::Null
    }

    fn media_example(&self, media: &Value) -> Option<Value> {
        let named = || media.get("examples").and_then(Value::as_object).and_then(|m| m.values().next()).and_then(|e| self.deref(e).get("value")).cloned();
        media.get("example").cloned().or_else(named)
    }

    /// The fields of an object body (`{"user": {"login"}}` → a `user` group with `user[login]`); none
    /// for another body (a list, a scalar), which is typed as raw JSON.
    fn body_fields(&self, schema: &Value) -> Vec<Param> {
        let Some((s, mut stack)) = self.norm(schema, &[]) else { return Vec::new() };
        let req = required_list(&s);
        let props = s.get("properties").and_then(Value::as_object).into_iter().flatten();
        props.filter_map(|(k, pv)| self.field(k, k.clone(), pv, req.contains(k), None, 1, &mut stack)).collect()
    }

    /// A field from its schema: an object becomes a group of its fields (`user[login]`) until
    /// MAX_DEPTH. Read-only fields are left out (it's for requests).
    #[allow(clippy::too_many_arguments)]
    fn field(&self, name: &str, full_name: String, schema: &Value, required: bool, description: Option<String>, depth: usize, stack: &mut Vec<String>) -> Option<Param> {
        let mut param = Param {
            name: name.to_owned(),
            full_name,
            description: description.unwrap_or_default().trim().to_owned(),
            required,
            allow_nil: false,
            kind: ParamKind::Hash,
            validator: "Must be a Hash (recursive: JSON)".into(),
            options: Vec::new(),
            children: Vec::new(),
        };
        let Some((s, refs)) = self.norm(schema, stack) else { return Some(param) };
        if is_true(&s, "readOnly") {
            return None;
        }
        if param.description.is_empty() {
            param.description = s.get("description").map(text_of).unwrap_or_default().trim().to_owned();
        }
        let (ty, nullable) = type_of(&s);
        stack.extend(refs.iter().cloned());
        let (kind, options) = self.kind_of(&s, &ty, stack);
        if kind == ParamKind::Hash && depth < MAX_DEPTH {
            let req = required_list(&s);
            for (k, pv) in s.get("properties").and_then(Value::as_object).into_iter().flatten() {
                param.children.extend(self.field(k, format!("{}[{k}]", param.full_name), pv, req.contains(k), None, depth + 1, stack));
            }
        }
        stack.truncate(stack.len() - refs.len());
        param.validator = validator(&s, kind, &options);
        Some(Param { allow_nil: nullable, kind, options, ..param })
    }

    fn kind_of(&self, s: &Map<String, Value>, ty: &str, stack: &[String]) -> (ParamKind, Vec<String>) {
        let options = enum_options(s);
        if !options.is_empty() && ty != "array" {
            if options.iter().all(|o| o == "true" || o == "false") {
                return (ParamKind::Bool, Vec::new());
            }
            return (ParamKind::Enum, options);
        }
        let kind = match ty {
            "integer" => ParamKind::Integer,
            "number" => ParamKind::Float,
            "boolean" => ParamKind::Bool,
            "file" => ParamKind::File,
            "string" if is_binary(s) => ParamKind::File,
            "string" if matches!(str_field(s, "format"), "date-time" | "date") => ParamKind::DateTime,
            "object" => ParamKind::Hash,
            "array" => {
                let Some(items) = s.get("items") else { return (ParamKind::Array, Vec::new()) };
                let Some((items, _)) = self.norm(items, stack) else { return (ParamKind::NestedArray, Vec::new()) };
                return match type_of(&items).0.as_str() {
                    "integer" => (ParamKind::IntArray, Vec::new()),
                    "object" | "array" => (ParamKind::NestedArray, Vec::new()),
                    "file" => (ParamKind::File, Vec::new()),
                    "string" if is_binary(&items) => (ParamKind::File, Vec::new()),
                    _ => (ParamKind::Array, enum_options(&items)),
                };
            }
            _ => ParamKind::String,
        };
        (kind, Vec::new())
    }

    /// Examples of the 2xx responses: the given ones, else a sample made from the schema.
    fn examples(&self, verb: &str, path: &str, op: &Map<String, Value>) -> Vec<Example> {
        let mut out = Vec::new();
        let ex = |code: &str, response: Value| Example {
            verb: verb.to_uppercase(),
            path: path.to_owned(),
            query: String::new(),
            code: code.to_owned(),
            request: None,
            response: Some(pretty(&response)),
        };
        for (code, resp) in op.get("responses").and_then(Value::as_object).into_iter().flatten().filter(|(code, _)| code.starts_with('2')) {
            let resp = self.deref(resp);
            if self.swagger2 {
                let given = resp
                    .get("examples")
                    .and_then(Value::as_object)
                    .and_then(|m| m.iter().find(|(k, _)| k.contains("json")).or_else(|| m.iter().next()))
                    .map(|(_, v)| v.clone());
                out.extend(given.or_else(|| self.sample(resp.get("schema")?, 0, &mut Vec::new())).map(|v| ex(code, v)));
                continue;
            }
            // A 204 has nothing to show.
            let Some((_, media)) = resp.get("content").and_then(Value::as_object).and_then(pick_media) else { continue };
            let named: Vec<&Value> = media.get("examples").and_then(Value::as_object).into_iter().flatten().filter_map(|(_, e)| self.deref(e).get("value")).collect();
            if !named.is_empty() {
                out.extend(named.into_iter().map(|v| ex(code, v.clone())));
                continue;
            }
            out.extend(media.get("example").cloned().or_else(|| self.sample(media.get("schema")?, 0, &mut Vec::new())).map(|v| ex(code, v)));
        }
        out
    }

    /// A plausible value for a schema: its own example / default, else made from the types (limited
    /// depth). None on a cycle.
    fn sample(&self, v: &Value, depth: usize, stack: &mut Vec<String>) -> Option<Value> {
        let (s, refs) = self.norm(v, stack)?;
        let given =
            s.get("example").or_else(|| s.get("examples").and_then(Value::as_array).and_then(|a| a.first())).or_else(|| s.get("default")).or_else(|| s.get("const"));
        if let Some(e) = given.or_else(|| s.get("enum").and_then(Value::as_array).and_then(|e| e.first())) {
            return Some(e.clone());
        }
        stack.extend(refs.iter().cloned());
        let value = match type_of(&s).0.as_str() {
            "object" => {
                let mut obj = Map::new();
                if depth < SAMPLE_DEPTH {
                    for (k, pv) in s.get("properties").and_then(Value::as_object).into_iter().flatten() {
                        let write_only = self.norm(pv, stack).is_some_and(|(p, _)| is_true(&p, "writeOnly"));
                        if let Some(x) = self.sample(pv, depth + 1, stack).filter(|_| !write_only) {
                            obj.insert(k.clone(), x);
                        }
                    }
                    if let Some(extra) = s.get("additionalProperties").filter(|a| a.is_object() && obj.is_empty()) {
                        obj.extend(self.sample(extra, depth + 1, stack).map(|x| ("key".to_owned(), x)));
                    }
                }
                Value::Object(obj)
            }
            "array" => Value::Array(s.get("items").filter(|_| depth < SAMPLE_DEPTH).and_then(|i| self.sample(i, depth + 1, stack)).into_iter().collect()),
            "string" => Value::String(sample_string(str_field(&s, "format")).to_owned()),
            "integer" => s.get("minimum").filter(|m| m.is_number()).cloned().unwrap_or(Value::from(0)),
            "number" => s.get("minimum").filter(|m| m.is_number()).cloned().unwrap_or(Value::from(0.0)),
            "boolean" => Value::Bool(true),
            "file" => Value::String("<binary>".into()),
            _ => Value::Null,
        };
        stack.truncate(stack.len() - refs.len());
        Some(value)
    }

    fn base_urls(&self) -> Vec<String> {
        if self.swagger2 {
            let host = self.root.get("host").map(text_of).unwrap_or_default();
            let base = self.root.get("basePath").map(text_of).unwrap_or_default();
            let base = base.trim_end_matches('/');
            if host.is_empty() {
                return if base.is_empty() { Vec::new() } else { vec![base.to_owned()] };
            }
            let schemes: Vec<String> = self.root.get("schemes").and_then(Value::as_array).map(|s| s.iter().map(text_of).collect()).unwrap_or_default();
            let schemes = if schemes.is_empty() { vec!["https".to_owned()] } else { schemes };
            return schemes.into_iter().map(|s| format!("{s}://{host}{base}")).collect();
        }
        let mut out: Vec<String> = Vec::new();
        for server in self.root.get("servers").and_then(Value::as_array).into_iter().flatten() {
            let mut url = server.get("url").map(text_of).unwrap_or_default();
            for (name, var) in server.get("variables").and_then(Value::as_object).into_iter().flatten() {
                url = url.replace(&format!("{{{name}}}"), &var.get("default").map(text_of).unwrap_or_default());
            }
            let url = url.trim_end_matches('/').to_owned();
            if !url.is_empty() && !out.contains(&url) {
                out.push(url);
            }
        }
        out
    }

    fn security(&self) -> Vec<SecurityScheme> {
        let schemes = if self.swagger2 { self.root.get("securityDefinitions") } else { self.root.pointer("/components/securitySchemes") };
        let flow = |kind: &str, f: &Map<String, Value>| OAuthFlow {
            kind: kind.to_owned(),
            auth_url: str_field(f, "authorizationUrl").to_owned(),
            token_url: str_field(f, "tokenUrl").to_owned(),
            refresh_url: str_field(f, "refreshUrl").to_owned(),
            scopes: f.get("scopes").and_then(Value::as_object).map(|m| m.iter().map(|(k, v)| (k.clone(), text_of(v))).collect()).unwrap_or_default(),
        };
        let mut out = Vec::new();
        for (name, s) in schemes.and_then(Value::as_object).into_iter().flatten() {
            let Some(s) = self.deref(s).as_object() else { continue };
            let name = name.clone();
            out.push(match str_field(s, "type") {
                "basic" => SecurityScheme::Basic { name },
                "http" => match str_field(s, "scheme").to_lowercase().as_str() {
                    "basic" => SecurityScheme::Basic { name },
                    "bearer" => SecurityScheme::Bearer { name, format: str_field(s, "bearerFormat").to_owned() },
                    _ => continue,
                },
                "apiKey" => SecurityScheme::ApiKey { name, key: str_field(s, "name").to_owned(), location: str_field(s, "in").to_owned() },
                "oauth2" if self.swagger2 => {
                    let kind = match str_field(s, "flow") {
                        "application" => "clientCredentials",
                        "accessCode" => "authorizationCode",
                        other => other,
                    };
                    SecurityScheme::OAuth2 { name, flows: vec![flow(kind, s)] }
                }
                "oauth2" => {
                    let flows = s.get("flows").and_then(Value::as_object).into_iter().flatten().filter_map(|(k, f)| Some(flow(k, f.as_object()?))).collect();
                    SecurityScheme::OAuth2 { name, flows }
                }
                _ => continue,
            });
        }
        out
    }
}

fn required_list(s: &Map<String, Value>) -> HashSet<String> {
    s.get("required").and_then(Value::as_array).map(|r| r.iter().filter_map(Value::as_str).map(str::to_owned).collect()).unwrap_or_default()
}

/// `from` into `into`: properties and `required` add up, other keys replace.
fn merge(into: &mut Map<String, Value>, from: Map<String, Value>) {
    for (k, v) in from {
        match (k.as_str(), into.get_mut(&k), v) {
            ("properties", Some(Value::Object(old)), Value::Object(new)) => old.extend(new),
            ("required", Some(Value::Array(old)), Value::Array(new)) => old.extend(new.into_iter().filter(|n| !old.contains(n)).collect::<Vec<_>>()),
            (_, _, v) => {
                into.insert(k, v);
            }
        }
    }
}

/// `base`, else `base_2`, `base_3`… (method names and resource keys are looked up by name).
fn unique(base: &str, used: &mut HashSet<String>) -> String {
    let mut name = base.to_owned();
    let mut n = 2;
    while used.contains(&name) {
        name = format!("{base}_{n}");
        n += 1;
    }
    used.insert(name.clone());
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    const PETSTORE_30: &str = r##"{
      "openapi": "3.0.3",
      "info": { "title": "Petstore", "version": "1.2.0" },
      "servers": [ { "url": "https://{env}.petstore.io/v1/", "variables": { "env": { "default": "api" } } }, { "url": "/local" } ],
      "tags": [ { "name": "pets", "description": "Everything about pets" }, { "name": "Store Front" } ],
      "paths": {
        "/pets": {
          "get": {
            "operationId": "listPets", "tags": ["pets"], "summary": "List all pets", "description": "Paginated.",
            "parameters": [
              { "name": "status", "in": "query", "schema": { "type": "string", "enum": ["available", "pending", "sold"] } },
              { "name": "limit", "in": "query", "required": true, "description": "How many", "schema": { "type": "integer", "maximum": 100 } },
              { "name": "tags", "in": "query", "schema": { "type": "array", "items": { "type": "string" } } },
              { "name": "filter", "in": "query", "style": "deepObject", "schema": { "type": "object", "properties": { "name": { "type": "string" } } } },
              { "name": "X-Request-Id", "in": "header", "required": true, "description": "Trace id", "schema": { "type": "string" } },
              { "name": "session", "in": "cookie", "schema": { "type": "string" } }
            ],
            "responses": {
              "200": { "description": "ok", "content": { "application/json": { "schema": { "type": "array", "items": { "$ref": "#/components/schemas/Pet" } } } } },
              "default": { "description": "error", "content": { "application/json": { "schema": { "type": "object" } } } }
            }
          },
          "post": {
            "operationId": "createPet", "tags": ["pets"],
            "requestBody": { "$ref": "#/components/requestBodies/NewPet" },
            "responses": {
              "201": { "description": "created", "content": { "application/json": {
                "examples": { "cat": { "value": { "id": 1, "name": "Tom" } }, "dog": { "$ref": "#/components/examples/Dog" } } } } }
            }
          }
        },
        "/pets/{petId}": {
          "parameters": [ { "$ref": "#/components/parameters/PetId" } ],
          "get": { "operationId": "showPetById", "tags": ["pets"], "responses": { "200": { "description": "ok", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Pet" } } } } } },
          "delete": { "summary": "Delete a pet", "responses": { "204": { "description": "gone" } } },
          "patch": { "operationId": "updatePet", "tags": ["pets"], "deprecated": true,
            "parameters": [ { "name": "petId", "in": "path", "required": true, "description": "Overridden", "schema": { "type": "string" } } ],
            "requestBody": { "content": { "application/json": { "schema": { "type": "array", "items": { "type": "string" } } } } },
            "responses": { "200": { "description": "ok" } } },
          "head": { "operationId": "headPet", "responses": { "200": { "description": "ok" } } }
        },
        "/pets/{pet-id}/photo": {
          "post": { "operationId": "uploadPhoto", "tags": ["pets"],
            "parameters": [ { "name": "pet-id", "in": "path", "required": true, "schema": { "type": "integer" } } ],
            "requestBody": { "content": { "multipart/form-data": { "schema": { "type": "object", "required": ["file"], "properties": {
              "file": { "type": "string", "format": "binary" }, "caption": { "type": "string" },
              "meta": { "type": "object", "properties": { "width": { "type": "integer" } } } } } } } },
            "responses": { "200": { "description": "ok" } } }
        },
        "/store/inventory": {
          "get": { "operationId": "getInventory", "tags": ["Store Front"], "responses": { "200": { "description": "ok",
            "content": { "application/json": { "schema": { "type": "object", "additionalProperties": { "type": "integer" } } } } } } }
        },
        "/v2/health": { "get": { "responses": { "200": { "description": "ok", "content": { "text/plain": { "example": "OK" } } } } } },
        "/nodes": {
          "get": { "operationId": "listNodes", "tags": ["nodes"], "responses": { "200": { "description": "ok", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Node" } } } } } },
          "post": { "operationId": "createNode", "tags": ["nodes"], "requestBody": { "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Node" } } } }, "responses": { "201": { "description": "ok" } } }
        }
      },
      "components": {
        "parameters": { "PetId": { "name": "petId", "in": "path", "required": true, "description": "The pet", "schema": { "type": "integer", "format": "int64" } } },
        "requestBodies": { "NewPet": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/NewPet" }, "example": { "name": "Rex" } } } } },
        "examples": { "Dog": { "value": { "id": 2, "name": "Rex" } } },
        "schemas": {
          "NewPet": { "type": "object", "required": ["name", "owner"], "properties": {
            "name": { "type": "string", "description": "Its name", "minLength": 1 },
            "tag": { "type": "string", "nullable": true },
            "owner": { "type": "object", "required": ["login"], "properties": {
              "login": { "type": "string" }, "address": { "type": "object", "properties": { "city": { "type": "string", "example": "Brussels" } } } } },
            "born_at": { "type": "string", "format": "date-time" },
            "kind": { "type": "string", "enum": ["cat", "dog"] },
            "vaccinated": { "type": "boolean" },
            "ratings": { "type": "array", "items": { "type": "integer" } },
            "toys": { "type": "array", "items": { "type": "object", "properties": { "n": { "type": "string" } } } },
            "price": { "type": "number" },
            "secret": { "type": "string", "writeOnly": true } } },
          "Pet": { "allOf": [ { "$ref": "#/components/schemas/NewPet" }, { "required": ["id"], "properties": { "id": { "type": "integer", "format": "int64", "readOnly": true } } } ] },
          "Node": { "type": "object", "properties": { "name": { "type": "string" }, "children": { "type": "array", "items": { "$ref": "#/components/schemas/Node" } }, "parent": { "$ref": "#/components/schemas/Node" } } }
        },
        "securitySchemes": {
          "basicAuth": { "type": "http", "scheme": "basic" },
          "jwt": { "type": "http", "scheme": "bearer", "bearerFormat": "JWT" },
          "api_key": { "type": "apiKey", "name": "X-API-Key", "in": "header" },
          "oauth": { "type": "oauth2", "flows": {
            "clientCredentials": { "tokenUrl": "https://auth.example.com/token", "scopes": { "read": "Read things", "write": "Write things" } },
            "authorizationCode": { "authorizationUrl": "https://auth.example.com/authorize", "tokenUrl": "https://auth.example.com/token", "refreshUrl": "https://auth.example.com/refresh", "scopes": {} } } },
          "oidc": { "type": "openIdConnect", "openIdConnectUrl": "https://auth.example.com/.well-known/openid-configuration" }
        }
      }
    }"##;

    const USERS_31: &str = r##"
openapi: 3.1.0
info:
  title: Users
  version: 2
servers:
  - url: https://api.example.com/
paths:
  /users/{user_id}:
    get:
      tags: [users]
      operationId: getUser
      description: |
        Gets one user.
        Second line.
      parameters:
        - name: user_id
          in: path
          required: true
          schema: { type: integer }
        - name: fields
          in: query
          schema:
            type: array
            items: { type: string, enum: [login, email] }
      responses:
        200:
          description: ok
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/User'
  /users:
    post:
      tags: [users]
      requestBody:
        content:
          application/x-www-form-urlencoded:
            schema:
              type: object
              properties:
                nickname: { type: [string, "null"] }
                age: { type: integer, minimum: 18 }
                active: { type: boolean }
                team: { oneOf: [ { $ref: '#/components/schemas/Team' }, { type: "null" } ] }
      responses:
        '201': { description: created }
components:
  schemas:
    User:
      type: object
      properties:
        id: { type: integer, examples: [17] }
        login: { type: string }
        nickname: { type: [string, "null"] }
        created_at: { type: string, format: date-time }
        team: { $ref: '#/components/schemas/Team' }
    Team:
      type: object
      properties:
        name: { type: string }
  securitySchemes:
    bearer: { type: http, scheme: bearer }
"##;

    const PETSTORE_SWAGGER: &str = r##"{
      "swagger": "2.0",
      "info": { "title": "Swagger Petstore", "version": "1.0.7" },
      "host": "petstore.swagger.io", "basePath": "/v2/", "schemes": ["https", "http"],
      "tags": [ { "name": "pet", "description": "Everything about your Pets" } ],
      "paths": {
        "/pet": { "post": { "tags": ["pet"], "operationId": "addPet", "consumes": ["application/json"],
          "parameters": [ { "in": "body", "name": "body", "required": true, "schema": { "$ref": "#/definitions/Pet" } } ],
          "responses": { "200": { "description": "ok", "schema": { "$ref": "#/definitions/Pet" } } } } },
        "/pet/findByStatus": { "get": { "tags": ["pet"], "operationId": "findPetsByStatus",
          "parameters": [ { "name": "status", "in": "query", "required": true, "type": "array", "items": { "type": "string", "enum": ["available", "pending", "sold"] }, "collectionFormat": "multi" } ],
          "responses": { "200": { "description": "ok", "schema": { "type": "array", "items": { "$ref": "#/definitions/Pet" } },
            "examples": { "application/json": [ { "id": 7, "name": "doggie" } ] } } } } },
        "/pet/{petId}": { "get": { "tags": ["pet"], "operationId": "getPetById",
          "parameters": [ { "name": "petId", "in": "path", "required": true, "type": "integer", "format": "int64", "description": "ID of pet" } ],
          "responses": { "200": { "description": "ok", "schema": { "$ref": "#/definitions/Pet" } }, "404": { "description": "nope" } } } },
        "/pet/{petId}/uploadImage": { "post": { "tags": ["pet"], "operationId": "uploadFile", "consumes": ["multipart/form-data"],
          "parameters": [
            { "name": "petId", "in": "path", "required": true, "type": "integer" },
            { "name": "additionalMetadata", "in": "formData", "type": "string" },
            { "name": "file", "in": "formData", "required": true, "type": "file" } ],
          "responses": { "200": { "description": "ok" } } } }
      },
      "definitions": {
        "Category": { "type": "object", "properties": { "id": { "type": "integer" }, "name": { "type": "string" } } },
        "Pet": { "type": "object", "required": ["name"], "properties": {
          "id": { "type": "integer", "format": "int64" }, "category": { "$ref": "#/definitions/Category" },
          "name": { "type": "string", "example": "doggie" }, "photoUrls": { "type": "array", "items": { "type": "string" } },
          "status": { "type": "string", "enum": ["available", "pending", "sold"] } } }
      },
      "securityDefinitions": {
        "petstore_auth": { "type": "oauth2", "flow": "implicit", "authorizationUrl": "https://petstore.swagger.io/oauth/authorize", "scopes": { "write:pets": "modify pets" } },
        "app": { "type": "oauth2", "flow": "application", "tokenUrl": "https://petstore.swagger.io/oauth/token", "scopes": {} },
        "api_key": { "type": "apiKey", "name": "api_key", "in": "header" },
        "basic": { "type": "basic" }
      }
    }"##;

    fn method<'d>(doc: &'d ApiDoc, name: &str) -> &'d Method {
        doc.resources.iter().flat_map(|r| &r.methods).find(|m| m.name == name).unwrap_or_else(|| panic!("no method {name}"))
    }

    /// A param anywhere in the tree, groups included.
    fn param<'p>(params: &'p [Param], full_name: &str) -> &'p Param {
        fn all<'p>(ps: &'p [Param], out: &mut Vec<&'p Param>) {
            for p in ps {
                out.push(p);
                all(&p.children, out);
            }
        }
        let mut every = Vec::new();
        all(params, &mut every);
        every.into_iter().find(|p| p.full_name == full_name).unwrap_or_else(|| panic!("no param {full_name}"))
    }

    fn response(m: &Method, i: usize) -> Value {
        serde_json::from_str(m.examples[i].response.as_deref().unwrap()).unwrap()
    }

    #[test]
    fn openapi_30() {
        let imp = import(PETSTORE_30).unwrap();
        assert_eq!((imp.title.as_str(), imp.version.as_str()), ("Petstore", "1.2.0"));
        assert_eq!(imp.base_urls, ["https://api.petstore.io/v1", "/local"]);
        let doc = &imp.doc;
        // Sorted by name; untagged `/v2/health` → `health`; the untagged DELETE /pets/{petId} joins `pets`.
        let keys: Vec<&str> = doc.resources.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["health", "nodes", "pets", "store_front"]);
        let pets = doc.resources.iter().find(|r| r.key == "pets").unwrap();
        assert_eq!(pets.description, "Everything about pets");
        let names: Vec<&str> = pets.methods.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["listPets", "createPet", "showPetById", "updatePet", "delete_pets_petid", "uploadPhoto"], "HEAD left out");
        assert_eq!(doc.endpoint_count, 10);

        let list = method(doc, "listPets");
        assert_eq!((list.routes[0].verb.as_str(), list.routes[0].path.as_str(), list.routes[0].summary.as_str()), ("GET", "/pets", "List all pets"));
        assert!(list.description.starts_with("List all pets\n\nPaginated."));
        assert!(list.description.contains("X-Request-Id (header, required): Trace id"), "{}", list.description);
        assert!(list.description.contains("session (cookie)"));
        assert!(!list.params.iter().any(|p| p.name == "X-Request-Id" || p.name == "session"));
        let status = param(&list.params, "status");
        assert_eq!(status.kind, ParamKind::Enum);
        assert_eq!(status.options, ["available", "pending", "sold"]);
        let limit = param(&list.params, "limit");
        assert!(limit.required && limit.kind == ParamKind::Integer && limit.description == "How many");
        assert!(limit.validator.contains("≤ 100"), "{}", limit.validator);
        assert_eq!(param(&list.params, "tags").kind, ParamKind::Array);
        let filter = param(&list.params, "filter");
        assert!(filter.is_group());
        assert_eq!(filter.children[0].full_name, "filter[name]");
        // A sample made from the schema: allOf merged, readOnly kept in responses, writeOnly left out.
        assert_eq!(list.examples.len(), 1, "only 2xx");
        assert_eq!(list.examples[0].code, "200");
        let pet = &response(list, 0)[0];
        assert_eq!(pet["id"], 0);
        assert_eq!(pet["owner"]["address"]["city"], "Brussels");
        assert_eq!(pet["born_at"], "2026-01-01T00:00:00Z");
        assert_eq!(pet["kind"], "cat");
        assert!(pet.get("secret").is_none());

        // Body fields, apipie style; a request example; named response examples ($ref'ed too).
        let create = method(doc, "createPet");
        let name = param(&create.params, "name");
        assert!(name.required && name.description == "Its name");
        assert!(name.validator.contains("min length 1"));
        assert!(param(&create.params, "tag").allow_nil);
        let owner = param(&create.params, "owner");
        assert!(owner.is_group() && owner.required);
        assert!(param(&create.params, "owner[login]").required);
        assert!(!param(&create.params, "owner[address][city]").required);
        assert_eq!(param(&create.params, "born_at").kind, ParamKind::DateTime);
        assert_eq!(param(&create.params, "kind").kind, ParamKind::Enum);
        assert_eq!(param(&create.params, "vaccinated").kind, ParamKind::Bool);
        assert_eq!(param(&create.params, "ratings").kind, ParamKind::IntArray);
        assert_eq!(param(&create.params, "toys").kind, ParamKind::NestedArray);
        assert_eq!(param(&create.params, "price").kind, ParamKind::Float);
        assert_eq!(create.examples.len(), 2);
        assert_eq!(create.examples[0].code, "201");
        assert_eq!(response(create, 0)["name"], "Tom");
        assert_eq!(response(create, 1)["name"], "Rex");
        assert_eq!(serde_json::from_str::<Value>(create.examples[0].request.as_deref().unwrap()).unwrap()["name"], "Rex");

        // Path params: a path-level $ref, an operation-level override, `{pet-id}` → `:pet_id`.
        let show = method(doc, "showPetById");
        assert_eq!(show.routes[0].path, "/pets/:petId");
        assert_eq!(show.params.len(), 1);
        let id = param(&show.params, "petId");
        assert!(id.required && id.kind == ParamKind::Integer && id.description == "The pet");
        let update = method(doc, "updatePet");
        assert!(update.routes[0].deprecated);
        assert_eq!(update.params.len(), 1, "a list body has no fields");
        assert_eq!((update.params[0].description.as_str(), update.params[0].kind), ("Overridden", ParamKind::String));
        let delete = method(doc, "delete_pets_petid");
        assert_eq!(delete.routes[0].summary, "Delete a pet");
        assert!(delete.examples.is_empty(), "204 without content");

        let upload = method(doc, "uploadPhoto");
        assert_eq!(upload.routes[0].path, "/pets/:pet_id/photo");
        assert_eq!(param(&upload.params, "pet_id").kind, ParamKind::Integer);
        let file = param(&upload.params, "file");
        assert!(file.kind == ParamKind::File && file.required);
        assert_eq!(param(&upload.params, "meta[width]").kind, ParamKind::Integer);

        assert_eq!(response(method(doc, "getInventory"), 0), serde_json::json!({ "key": 0 }));
        let health = &doc.resources[0].methods[0];
        assert_eq!(health.name, "get_v2_health");
        assert_eq!(health.examples[0].response.as_deref(), Some("OK"));

        // A recursive schema stops at the first repeat.
        assert_eq!(response(method(doc, "listNodes"), 0), serde_json::json!({ "name": "string", "children": [] }));
        let create_node = method(doc, "createNode");
        let parent = param(&create_node.params, "parent");
        assert!(parent.kind == ParamKind::Hash && parent.children.is_empty());
        assert_eq!(param(&create_node.params, "children").kind, ParamKind::NestedArray);

        // Search and route matching work on the imported doc.
        assert_eq!(doc.resources[doc.search("photo")[0].resource].key, "pets");
        let r = doc.match_route("GET", "/pets/12").unwrap();
        assert_eq!((r.resource.as_str(), r.method.as_str()), ("pets", "showPetById"));

        // Security schemes (openIdConnect left out).
        assert_eq!(imp.security.len(), 4);
        assert!(matches!(&imp.security[0], SecurityScheme::Basic { name } if name == "basicAuth"));
        assert!(matches!(&imp.security[1], SecurityScheme::Bearer { name, format } if name == "jwt" && format == "JWT"));
        assert!(matches!(&imp.security[2], SecurityScheme::ApiKey { key, location, .. } if key == "X-API-Key" && location == "header"));
        let SecurityScheme::OAuth2 { name, flows } = &imp.security[3] else { panic!("{:?}", imp.security[3]) };
        assert_eq!(name, "oauth");
        assert_eq!(flows.len(), 2);
        assert_eq!((flows[0].kind.as_str(), flows[0].token_url.as_str()), ("clientCredentials", "https://auth.example.com/token"));
        assert_eq!(flows[0].scopes, [("read".to_owned(), "Read things".to_owned()), ("write".to_owned(), "Write things".to_owned())]);
        assert_eq!(flows[1].kind, "authorizationCode");
        assert_eq!(flows[1].auth_url, "https://auth.example.com/authorize");
        assert_eq!(flows[1].refresh_url, "https://auth.example.com/refresh");
    }

    #[test]
    fn openapi_31_yaml() {
        let imp = import(USERS_31).unwrap();
        assert_eq!((imp.title.as_str(), imp.version.as_str()), ("Users", "2"));
        assert_eq!(imp.base_urls, ["https://api.example.com"]);
        assert!(matches!(&imp.security[..], [SecurityScheme::Bearer { name, format }] if name == "bearer" && format.is_empty()));
        let doc = &imp.doc;
        assert_eq!(doc.resources.len(), 1);
        let get = method(doc, "getUser");
        assert_eq!(get.routes[0].path, "/users/:user_id");
        assert_eq!(get.routes[0].summary, "Gets one user.", "first line of the description");
        let fields = param(&get.params, "fields");
        assert_eq!(fields.kind, ParamKind::Array);
        assert_eq!(fields.options, ["login", "email"]);
        // Integer YAML keys (`200:`) are codes; 3.1 `examples` lists and `type: [string, "null"]`.
        assert_eq!(get.examples[0].code, "200");
        let user = response(get, 0);
        assert_eq!(user["id"], 17);
        assert_eq!(user["nickname"], "string");
        assert_eq!(user["team"]["name"], "string");

        let post = method(doc, "post_users");
        let nick = param(&post.params, "nickname");
        assert!(nick.kind == ParamKind::String && nick.allow_nil);
        assert!(param(&post.params, "age").validator.contains("≥ 18"));
        let team = param(&post.params, "team");
        assert!(team.is_group() && team.allow_nil, "oneOf with null");
        assert_eq!(team.children[0].full_name, "team[name]");
    }

    #[test]
    fn swagger_20() {
        let imp = import(PETSTORE_SWAGGER).unwrap();
        assert_eq!(imp.title, "Swagger Petstore");
        assert_eq!(imp.base_urls, ["https://petstore.swagger.io/v2", "http://petstore.swagger.io/v2"]);
        let doc = &imp.doc;
        assert_eq!(doc.resources.len(), 1);
        assert_eq!(doc.resources[0].description, "Everything about your Pets");
        assert_eq!(doc.endpoint_count, 4);

        let add = method(doc, "addPet");
        assert!(param(&add.params, "name").required);
        assert!(param(&add.params, "category").is_group());
        assert_eq!(param(&add.params, "category[id]").kind, ParamKind::Integer);
        assert_eq!(param(&add.params, "photoUrls").kind, ParamKind::Array);
        assert_eq!(param(&add.params, "status").options, ["available", "pending", "sold"]);
        assert_eq!(response(add, 0)["name"], "doggie");

        let find = method(doc, "findPetsByStatus");
        let status = param(&find.params, "status");
        assert!(status.required && status.kind == ParamKind::Array);
        assert_eq!(status.options.len(), 3);
        assert_eq!(response(find, 0)[0]["id"], 7, "the given example wins");

        let get = method(doc, "getPetById");
        assert_eq!(get.routes[0].path, "/pet/:petId");
        let id = param(&get.params, "petId");
        assert!(id.kind == ParamKind::Integer && id.description == "ID of pet");
        assert_eq!(get.examples.len(), 1);

        let upload = method(doc, "uploadFile");
        let file = param(&upload.params, "file");
        assert!(file.kind == ParamKind::File && file.required);
        assert_eq!(param(&upload.params, "additionalMetadata").kind, ParamKind::String);

        assert_eq!(imp.security.len(), 4);
        let SecurityScheme::OAuth2 { flows, .. } = &imp.security[0] else { panic!() };
        assert_eq!((flows[0].kind.as_str(), flows[0].auth_url.as_str()), ("implicit", "https://petstore.swagger.io/oauth/authorize"));
        assert_eq!(flows[0].scopes[0].0, "write:pets");
        let SecurityScheme::OAuth2 { flows, .. } = &imp.security[1] else { panic!() };
        assert_eq!(flows[0].kind, "clientCredentials");
        assert!(matches!(&imp.security[2], SecurityScheme::ApiKey { key, location, .. } if key == "api_key" && location == "header"));
        assert!(matches!(&imp.security[3], SecurityScheme::Basic { .. }));
    }

    #[test]
    fn not_a_definition() {
        assert!(import("{ \"bad\"").err().unwrap().starts_with("invalid JSON"));
        assert!(import("a: [").err().unwrap().starts_with("invalid YAML"));
        assert!(import("hello: world").err().unwrap().contains("not an OpenAPI"));
        assert!(import("swagger: 2.0\ninfo: {title: T, version: '1'}\npaths: {}").is_ok(), "an unquoted 2.0 is a number");
    }

    /// A definition the app wrote: the test doc as the app exports it (Generate docs › OpenAPI), read back.
    #[test]
    fn round_trip_of_an_export() {
        let doc = ApiDoc::load().unwrap();
        let opts = crate::docgen::Options { title: "Shop".into(), examples: true, data_map: None };
        let (text, routes) = crate::docgen::generate_api(&doc, &[], &opts, crate::docgen::Format::OpenApi);
        let t = std::time::Instant::now();
        let imp = import(&text).unwrap();
        eprintln!("{routes} routes exported, {} imported in {:?}", imp.doc.endpoint_count, t.elapsed());
        assert!(imp.doc.endpoint_count * 10 >= routes * 9, "{} of {routes}", imp.doc.endpoint_count);
        let r = imp.doc.match_route("GET", "/v1/stores/12/users").unwrap();
        let (_, m) = imp.doc.lookup(&r).unwrap();
        assert_eq!(m.routes[r.route].path, "/v1/stores/:store_id/users");
        assert!(m.params.iter().any(|p| p.name == "store_id" && p.required));
        assert!(!m.examples.is_empty());
    }

    /// `ApiDoc::from_resources` builds the test doc to its known counts (sorted, hay, index).
    #[test]
    fn test_doc_counts() {
        let doc = ApiDoc::load().unwrap();
        let methods: Vec<&Method> = doc.resources.iter().flat_map(|r| &r.methods).collect();
        let mut leaves = Vec::new();
        for m in &methods {
            crate::docs::param_leaves(&m.params, &mut leaves);
        }
        let examples: usize = methods.iter().map(|m| m.examples.len()).sum();
        let hay: usize = methods.iter().flat_map(|m| &m.routes).map(|r| r.hay.len()).sum();
        assert_eq!((doc.resources.len(), doc.endpoint_count, methods.len(), leaves.len(), examples, hay), (11, 47, 33, 165, 8, 2093));
        assert_eq!(doc.resources[0].key, "categories");
        assert_eq!(doc.resources[10].key, "visits");
        let refs = doc.resources.iter().map(|r| crate::model::DocRef { resource: r.key.clone(), method: r.methods[0].name.clone(), route: 0 });
        assert!(refs.collect::<Vec<_>>().iter().all(|d| doc.lookup(d).is_some()), "index made");
    }
}
