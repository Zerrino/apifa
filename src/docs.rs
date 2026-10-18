//! API documentation: the endpoints, their parameters and examples (from OpenAPI / Swagger, `openapi.rs`, or an apipie export),
//! embedded in the binary so the app works fully offline.

use std::collections::{BTreeMap, HashMap};

use parking_lot::Mutex;
use serde::Deserialize;

use crate::model::DocRef;

#[derive(Deserialize)]
struct RawRoot {
    docs: RawDocs,
}

#[derive(Deserialize)]
struct RawDocs {
    #[serde(default)]
    resources: BTreeMap<String, RawResource>,
}

#[derive(Deserialize)]
struct RawResource {
    name: String,
    #[serde(default)]
    short_description: Option<String>,
    #[serde(default)]
    full_description: Option<String>,
    #[serde(default)]
    methods: Vec<RawMethod>,
}

#[derive(Deserialize)]
struct RawMethod {
    name: String,
    #[serde(default)]
    apis: Vec<RawApi>,
    #[serde(default)]
    full_description: Option<String>,
    #[serde(default)]
    params: Vec<RawParam>,
    #[serde(default)]
    examples: Option<Vec<RawExample>>,
}

#[derive(Deserialize)]
struct RawApi {
    api_url: String,
    http_method: String,
    #[serde(default)]
    short_description: Option<String>,
    #[serde(default)]
    deprecated: Option<bool>,
}

#[derive(Deserialize)]
struct RawParam {
    name: String,
    full_name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    required: Option<bool>,
    #[serde(default)]
    allow_nil: Option<bool>,
    #[serde(default)]
    validator: Option<String>,
    #[serde(default)]
    expected_type: Option<String>,
    #[serde(default)]
    show: Option<bool>,
    #[serde(default)]
    params: Vec<RawParam>,
}

#[derive(Deserialize)]
struct RawExample {
    #[serde(default)]
    verb: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    request_data: Option<serde_json::Value>,
    #[serde(default)]
    response_data: Option<serde_json::Value>,
    #[serde(default)]
    code: Option<serde_json::Value>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ParamKind {
    String,
    Integer,
    Float,
    Bool,
    DateTime,
    Enum,
    Hash,
    Array,
    IntArray,
    NestedArray,
    File,
}

impl ParamKind {
    pub fn label(self) -> &'static str {
        match self {
            ParamKind::String => "string",
            ParamKind::Integer => "integer",
            ParamKind::Float => "float",
            ParamKind::Bool => "boolean",
            ParamKind::DateTime => "datetime",
            ParamKind::Enum => "enum",
            ParamKind::Hash => "hash",
            ParamKind::Array => "array",
            ParamKind::IntArray => "integer[]",
            ParamKind::NestedArray => "object[]",
            ParamKind::File => "file",
        }
    }
}

/// Every documented field of `params`, groups (`user[…]`) opened.
pub fn param_leaves<'a>(params: &'a [Param], out: &mut Vec<&'a Param>) {
    for p in params {
        if p.is_group() {
            param_leaves(&p.children, out);
        } else {
            out.push(p);
        }
    }
}

pub struct Param {
    pub name: String,
    pub full_name: String,
    pub description: String,
    pub required: bool,
    pub allow_nil: bool,
    pub kind: ParamKind,
    pub validator: String,
    pub options: Vec<String>,
    pub children: Vec<Param>,
}

impl Param {
    pub fn is_group(&self) -> bool {
        self.kind == ParamKind::Hash && !self.children.is_empty()
    }

    pub fn matches(&self, needle: &str) -> bool {
        needle.is_empty()
            || self.full_name.to_lowercase().contains(needle)
            || self.description.to_lowercase().contains(needle)
            || self.children.iter().any(|c| c.matches(needle))
    }

    /// Number of leaf values filled under (and including) this param.
    pub fn count_set(&self, values: &BTreeMap<String, String>) -> usize {
        let own = values.get(&self.full_name).is_some_and(|v| !v.trim().is_empty()) as usize;
        own + if self.is_group() { self.children.iter().map(|c| c.count_set(values)).sum() } else { 0 }
    }

    /// Required itself, or a group containing required fields.
    pub fn has_required(&self) -> bool {
        self.required || self.children.iter().any(Param::has_required)
    }

    pub fn leaf_count(&self) -> usize {
        if self.is_group() {
            self.children.iter().map(Param::leaf_count).sum()
        } else {
            1
        }
    }
}

pub struct Route {
    pub verb: String,
    pub path: String,
    pub summary: String,
    pub deprecated: bool,
    /// Lowercase text searched by `ApiDoc::search` (resource, method, verb, path, summary), made once
    /// by `ApiDoc::from_resources`.
    pub(crate) hay: String,
}

pub struct Example {
    pub verb: String,
    pub path: String,
    pub query: String,
    pub code: String,
    pub request: Option<String>,
    pub response: Option<String>,
}

pub struct Method {
    pub name: String,
    pub description: String,
    pub routes: Vec<Route>,
    pub params: Vec<Param>,
    pub examples: Vec<Example>,
}

pub struct Resource {
    pub key: String,
    pub name: String,
    pub description: String,
    pub methods: Vec<Method>,
    /// The imported API definition it comes from (empty: unnamed, the tests' doc).
    pub api: String,
}

/// A resource and its matching routes (method index, route index), in display order.
#[derive(Clone)]
pub struct Hit {
    pub resource: usize,
    pub routes: Vec<(usize, usize)>,
}

fn singular(s: &str) -> &str {
    s.strip_suffix('s').unwrap_or(s)
}

/// 0 = the resource itself (`user`, `users`), 1 = starts with it (`order_items` for
/// `order`), 2 = one of its words (`order_items` for `item`), 3 = contains it, 4 = only
/// its routes match.
fn resource_score(res: &Resource, q: &str) -> u8 {
    let key = res.key.to_lowercase();
    let name = res.name.to_lowercase();
    if key == q || name == q || singular(&key) == singular(q) || singular(&name) == singular(q) {
        0
    } else if key.starts_with(q) || name.starts_with(q) {
        1
    } else if key.split('_').any(|w| w.starts_with(q)) || name.split_whitespace().any(|w| w.starts_with(q)) {
        2
    } else if key.contains(q) || name.contains(q) {
        3
    } else {
        4
    }
}

/// Routes under the resource's own root first (`/users…` for Users, a `/v1` or `/api` prefix aside), then by depth.
fn route_rank(key: &str, r: &Route) -> (bool, usize, String, usize) {
    // Optional `(/on/:field)` groups don't count in the depth.
    let main = r.path.split('(').next().unwrap_or(&r.path);
    let version = |s: &str| s == "api" || (s.len() > 1 && s.starts_with('v') && s[1..].chars().all(|c| c.is_ascii_digit() || c == '.'));
    let segs: Vec<&str> = main.split('/').filter(|s| !s.is_empty()).skip_while(|s| version(s)).collect();
    let root = segs.first().copied().unwrap_or_default();
    let verb = ["GET", "POST", "PUT", "PATCH", "DELETE"].iter().position(|v| *v == r.verb).unwrap_or(9);
    (root != key, segs.len(), r.path.clone(), verb)
}

pub struct ApiDoc {
    pub resources: Vec<Resource>,
    pub endpoint_count: usize,
    index: HashMap<String, usize>,
    /// The last search and its hits: the sidebar and the endpoint picker ask again every frame.
    last_search: Mutex<Option<(String, Vec<Hit>)>>,
    /// Where values of a field come from, learnt from the data map (`datamap::relations`).
    relations: parking_lot::RwLock<crate::datamap::Relations>,
}

impl ApiDoc {
    /// The API definitions the user imported (`apis.rs`). Tests: the apipie fixture.
    #[cfg(not(test))]
    pub fn load() -> Result<Self, String> {
        Ok(crate::apis::load_doc())
    }

    #[cfg(test)]
    pub fn load() -> Result<Self, String> {
        Self::from_apipie(include_str!("../tests/fixtures/apidoc.json"))
    }

    /// An apipie export (`/apipie.json`, Rails' apipie-rails gem).
    pub fn from_apipie(text: &str) -> Result<Self, String> {
        let raw: RawRoot = serde_json::from_str(text).map_err(|e| format!("not an apipie export: {e}"))?;
        let mut resources = Vec::new();
        for (key, r) in raw.docs.resources {
            let methods: Vec<Method> = r.methods.into_iter().filter(|m| !m.apis.is_empty()).map(convert_method).collect();
            if methods.is_empty() {
                continue;
            }
            let description = html_to_text(r.short_description.as_deref().filter(|s| !s.trim().is_empty()).or(r.full_description.as_deref()).unwrap_or_default());
            resources.push(Resource { key, name: r.name, description, methods, api: String::new() });
        }
        Ok(Self::from_resources(resources))
    }

    /// The doc of these resources: sorted by name, search text and index made.
    pub fn from_resources(mut resources: Vec<Resource>) -> Self {
        resources.sort_by_key(|r| r.name.to_lowercase());
        for res in &mut resources {
            for m in &mut res.methods {
                for r in &mut m.routes {
                    r.hay = format!("{} {} {} {} {} {}", res.name, res.key, m.name, r.verb, r.path, r.summary).to_lowercase();
                }
            }
        }
        let index = resources.iter().enumerate().map(|(i, r)| (r.key.clone(), i)).collect();
        let endpoint_count = resources.iter().flat_map(|r| &r.methods).map(|m| m.routes.len()).sum();
        Self { resources, endpoint_count, index, last_search: Mutex::new(None), relations: Default::default() }
    }

    /// Used only if the embedded doc fails to parse: custom requests still work.
    pub fn empty() -> Self {
        Self { resources: Vec::new(), endpoint_count: 0, index: HashMap::new(), last_search: Mutex::new(None), relations: Default::default() }
    }

    /// Resources and routes matching every word of `query`, best first:
    /// the closest category (`order` → Orders, then Order items…), and inside each one the routes
    /// rooted at that category first, shortest first (`/users`, `/users/:id`, `/users/:id/x`…).
    pub fn search(&self, query: &str) -> Vec<Hit> {
        let q = query.trim().to_lowercase();
        let mut last = self.last_search.lock();
        if let Some((_, hits)) = last.as_ref().filter(|(prev, _)| *prev == q) {
            return hits.clone();
        }
        let hits = self.search_uncached(&q);
        *last = Some((q, hits.clone()));
        hits
    }

    fn search_uncached(&self, q: &str) -> Vec<Hit> {
        let terms: Vec<&str> = q.split_whitespace().collect();
        let first = terms.first().copied().unwrap_or("");
        let mut hits: Vec<(u8, Hit)> = Vec::new();
        for (ri, res) in self.resources.iter().enumerate() {
            let mut routes: Vec<(usize, usize)> = Vec::new();
            for (mi, m) in res.methods.iter().enumerate() {
                for (pi, r) in m.routes.iter().enumerate() {
                    if terms.iter().all(|t| r.hay.contains(t)) {
                        routes.push((mi, pi));
                    }
                }
            }
            if routes.is_empty() {
                continue;
            }
            routes.sort_by_cached_key(|&(mi, pi)| route_rank(&res.key, &res.methods[mi].routes[pi]));
            let score = if first.is_empty() { 0 } else { resource_score(res, first) };
            hits.push((score, Hit { resource: ri, routes }));
        }
        if !first.is_empty() {
            hits.sort_by_key(|(score, h)| (*score, self.resources[h.resource].key.len(), self.resources[h.resource].key.clone()));
        }
        hits.into_iter().map(|(_, h)| h).collect()
    }

    /// Learns from a data map where the values of each field come from (the links it found).
    pub fn set_data_map(&self, map: Option<&crate::datamap::DataMap>) {
        *self.relations.write() = map.map(|m| crate::datamap::relations(m, self)).unwrap_or_default();
    }

    /// The GET where values of a parameter can be found: what the data map's links say first
    /// (`category_id` → the GET listing categories), else by its name (`store_id` → the resource `stores`'s
    /// list); `id` → the list of `context` (the current resource). Also says when the data map told it.
    pub fn value_source(&self, param: &str, context: Option<&str>) -> Option<(DocRef, bool)> {
        let name = param.trim().trim_start_matches(':').trim_end_matches(']').rsplit('[').next().unwrap_or_default().to_lowercase();
        if name == "id" {
            return self.list_route(context?).map(|d| (d, false));
        }
        if let Some((_, d)) = self.relations.read().get(&name) {
            return Some((d.clone(), true));
        }
        let base = name.strip_suffix("_ids").or_else(|| name.strip_suffix("_id"))?;
        let key = plurals(base).into_iter().find(|k| self.index.contains_key(k))?;
        self.list_route(&key).map(|d| (d, false))
    }

    /// A resource's list: its `index` GET with no `:variable` (else the fewest), the shortest path.
    fn list_route(&self, key: &str) -> Option<DocRef> {
        let res = &self.resources[*self.index.get(key)?];
        res.methods
            .iter()
            .filter(|m| m.name == "index")
            .flat_map(|m| m.routes.iter().enumerate().filter(|(_, r)| r.verb == "GET").map(move |(i, r)| (m, i, r)))
            .min_by_key(|(_, _, r)| (r.path.matches(':').count(), r.path.len()))
            .map(|(m, i, _)| DocRef { resource: key.to_owned(), method: m.name.clone(), route: i })
    }

    pub fn lookup(&self, r: &DocRef) -> Option<(&Resource, &Method)> {
        let res = &self.resources[*self.index.get(&r.resource)?];
        let method = res.methods.iter().find(|m| m.name == r.method)?;
        Some((res, method))
    }

    /// Whether `r` documents `verb path` (`path` as typed in a flow: `/v1/users/{{login}}`, `:user_id`…).
    pub fn route_fits(&self, r: &DocRef, verb: &str, path: &str) -> bool {
        self.lookup(r)
            .and_then(|(_, m)| m.routes.get(r.route))
            .is_some_and(|route| route.verb.eq_ignore_ascii_case(verb.trim()) && route_score(&route.path, path).is_some())
    }

    /// The documented endpoint for `verb path`: variables (`{{x}}`, `:x`, a literal id) fill `:name` segments,
    /// and the route with the most literal segments in common wins.
    pub fn match_route(&self, verb: &str, path: &str) -> Option<DocRef> {
        let verb = verb.trim();
        let mut best: Option<(usize, DocRef)> = None;
        for res in &self.resources {
            for m in &res.methods {
                for (i, route) in m.routes.iter().enumerate() {
                    if !route.verb.eq_ignore_ascii_case(verb) {
                        continue;
                    }
                    if let Some(score) = route_score(&route.path, path) {
                        if best.as_ref().is_none_or(|(b, _)| score > *b) {
                            best = Some((score, DocRef { resource: res.key.clone(), method: m.name.clone(), route: i }));
                        }
                    }
                }
            }
        }
        best.map(|(_, r)| r)
    }
}

/// `user` → user, users, useres; `category` → … categories: the names a resource of that thing may have.
pub fn plurals(base: &str) -> Vec<String> {
    let mut out = vec![base.to_owned(), format!("{base}s"), format!("{base}es")];
    if let Some(stem) = base.strip_suffix('y') {
        out.push(format!("{stem}ies"));
    }
    out
}

fn path_segments(path: &str) -> Vec<&str> {
    let path = path.trim();
    let path = crate::model::split_origin(path).1;
    let path = path.split('?').next().unwrap_or_default();
    path.split('/').filter(|s| !s.is_empty()).collect()
}

/// Number of literal segments shared by a documented route and a typed path, when they fit.
fn route_score(route: &str, path: &str) -> Option<usize> {
    let typed = path_segments(path);
    // A route with optional groups fits both without them and with them filled.
    let without: String = {
        let mut depth = 0;
        route
            .chars()
            .filter(|c| {
                match c {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => return depth == 0,
                }
                false
            })
            .collect()
    };
    let with: String = route.chars().filter(|c| !matches!(c, '(' | ')')).collect();
    [without, with]
        .iter()
        .filter_map(|variant| {
            let doc = path_segments(variant);
            if doc.len() != typed.len() {
                return None;
            }
            let mut score = 0;
            for (d, t) in doc.iter().zip(&typed) {
                let typed_var = t.contains("{{") || t.starts_with(':');
                if d.starts_with(':') {
                    continue;
                }
                if typed_var || d != t {
                    return None;
                }
                score += 1;
            }
            Some(score)
        })
        .max()
}

fn convert_method(m: RawMethod) -> Method {
    let routes = m
        .apis
        .into_iter()
        .map(|a| Route {
            verb: a.http_method.to_uppercase(),
            path: a.api_url,
            summary: a.short_description.map(|s| html_to_text(&s)).unwrap_or_default(),
            deprecated: a.deprecated.unwrap_or(false),
            hay: String::new(),
        })
        .collect();
    let examples = m
        .examples
        .unwrap_or_default()
        .into_iter()
        .map(|e| Example {
            verb: e.verb.unwrap_or_default(),
            path: e.path.unwrap_or_default(),
            query: e.query.unwrap_or_default(),
            code: match e.code {
                Some(serde_json::Value::String(s)) => s,
                Some(v) => v.to_string(),
                None => String::new(),
            },
            request: e.request_data.filter(|v| !v.is_null()).map(|v| pretty(&v)),
            response: e.response_data.filter(|v| !v.is_null()).map(|v| pretty(&v)),
        })
        .collect();
    let mut params: Vec<Param> = convert_params(m.params).into_iter().map(expand_filter).collect();
    // apipie list endpoints (with sort / filter) are paginated even when page isn't documented.
    let is_list = params.iter().any(|p| p.name == "sort" || p.name == "filter");
    if is_list && !params.iter().any(|p| p.name == "page") {
        params.push(page_param());
    }
    Method { name: m.name, description: html_to_text(m.full_description.as_deref().unwrap_or_default()), routes, params, examples }
}

/// The docs describe `filter` / `range` as "one of: id, login, …" (the field names), but the API
/// expects `filter[login]=…`: turn them into a group with one field per allowed name.
fn expand_filter(p: Param) -> Param {
    let is_keyed = matches!(p.name.as_str(), "filter" | "range") && p.kind == ParamKind::Enum && !p.options.is_empty();
    if !is_keyed {
        return p;
    }
    let range = p.name == "range";
    let children = p
        .options
        .iter()
        .map(|field| Param {
            name: field.clone(),
            full_name: format!("{}[{field}]", p.full_name),
            description: if range {
                format!("Keep results whose {field} is between min and max (inclusive): min,max")
            } else {
                format!("Keep results whose {field} equals the value; several values separated by commas match any of them")
            },
            required: false,
            allow_nil: false,
            kind: ParamKind::String,
            validator: if range { "min,max".into() } else { "value or value1,value2".into() },
            options: Vec::new(),
            children: Vec::new(),
        })
        .collect();
    Param {
        description: if range {
            "Select on a range: range[field]=min,max".into()
        } else {
            "Filter on one or more fields: filter[field]=value (comma-separated values = OR)".into()
        },
        kind: ParamKind::Hash,
        validator: "Must be a Hash".into(),
        options: Vec::new(),
        children,
        ..p
    }
}

fn page_param() -> Param {
    let leaf = |name: &str, description: &str| Param {
        name: name.into(),
        full_name: format!("page[{name}]"),
        description: description.into(),
        required: false,
        allow_nil: false,
        kind: ParamKind::Integer,
        validator: "Must be Fixnum".into(),
        options: Vec::new(),
        children: Vec::new(),
    };
    Param {
        name: "page".into(),
        full_name: "page".into(),
        description: "The pagination params, as a hash (added by the client: the API paginates every list).".into(),
        required: false,
        allow_nil: false,
        kind: ParamKind::Hash,
        validator: "Must be a Hash".into(),
        options: Vec::new(),
        children: vec![leaf("number", "The current page"), leaf("size", "The number of items per page, defaults to 30, maximum 100")],
    }
}

fn pretty(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => match serde_json::from_str::<serde_json::Value>(s) {
            Ok(inner) => serde_json::to_string_pretty(&inner).unwrap_or_else(|_| s.clone()),
            Err(_) => s.clone(),
        },
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

fn convert_params(raw: Vec<RawParam>) -> Vec<Param> {
    raw.into_iter()
        .filter(|p| p.show.unwrap_or(true))
        .map(|p| {
            let validator_html = p.validator.unwrap_or_default();
            let (kind, options) = classify(&validator_html, p.expected_type.as_deref());
            Param {
                name: p.name,
                full_name: p.full_name,
                description: html_to_text(p.description.as_deref().unwrap_or_default()),
                required: p.required.unwrap_or(false),
                allow_nil: p.allow_nil.unwrap_or(false),
                kind,
                validator: html_to_text(&validator_html),
                options,
                children: convert_params(p.params),
            }
        })
        .collect()
}

fn classify(validator: &str, expected: Option<&str>) -> (ParamKind, Vec<String>) {
    let v = validator.to_lowercase();
    if v.starts_with("must be one of") {
        let options = extract_codes(validator);
        if options.len() == 2 && options.iter().all(|o| o == "true" || o == "false") {
            return (ParamKind::Bool, Vec::new());
        }
        return (ParamKind::Enum, options);
    }
    let kind = if v.contains("array of nested") {
        ParamKind::NestedArray
    } else if v.contains("array of integer") {
        ParamKind::IntArray
    } else if v.contains("array") {
        ParamKind::Array
    } else if v.contains("hash") {
        ParamKind::Hash
    } else if v.contains("fixnum") || v.contains("integer") {
        ParamKind::Integer
    } else if v.contains("float") {
        ParamKind::Float
    } else if v.contains("datetime") || v.contains("date") {
        ParamKind::DateTime
    } else if v.contains("file") {
        ParamKind::File
    } else {
        match expected {
            Some("numeric") => ParamKind::Integer,
            Some("hash") => ParamKind::Hash,
            Some("array") => ParamKind::Array,
            _ => ParamKind::String,
        }
    };
    (kind, Vec::new())
}

fn extract_codes(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find("<code>") {
        rest = &rest[start + 6..];
        let Some(end) = rest.find("</code>") else { break };
        out.push(decode_entities(&rest[..end]));
        rest = &rest[end + 7..];
    }
    out
}

fn decode_entities(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&nbsp;", " ").replace("&amp;", "&")
}

/// Very small HTML → plain text conversion, enough for apipie descriptions.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut tag = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        if in_tag {
            if c == '>' {
                in_tag = false;
                let t = tag.trim().to_lowercase();
                if t.starts_with("br") || t.starts_with("/p") || t.starts_with("/li") || t.starts_with("/h") || t.starts_with("/pre") {
                    out.push('\n');
                } else if t.starts_with("li") {
                    out.push_str("\n• ");
                }
                tag.clear();
            } else {
                tag.push(c);
            }
        } else if c == '<' {
            in_tag = true;
        } else {
            out.push(c);
        }
    }
    let decoded = decode_entities(&out);
    let mut lines: Vec<String> = Vec::new();
    for line in decoded.lines() {
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.is_empty() {
            if lines.last().is_some_and(|l| !l.is_empty()) {
                lines.push(String::new());
            }
        } else {
            lines.push(collapsed);
        }
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_sources() {
        let doc = ApiDoc::load().unwrap();
        let path = |p: &str, ctx: Option<&str>| {
            doc.value_source(p, ctx).map(|(d, _)| {
                let (_, m) = doc.lookup(&d).unwrap();
                m.routes[d.route].path.clone()
            })
        };
        assert_eq!(path(":store_id", None).as_deref(), Some("/v1/stores"));
        assert_eq!(path("category_id", None).as_deref(), Some("/v1/categories"));
        assert_eq!(path("filter[user_id]", None).as_deref(), Some("/v1/users"));
        assert_eq!(path("product_ids", None).as_deref(), Some("/v1/products"));
        assert_eq!(path("filter[id]", Some("teams")).as_deref(), Some("/v1/teams"));
        assert_eq!(path("price", None), None);
        assert_eq!(path("username", None), None, "nothing about a particular API is written in the code");
    }

    #[test]
    fn filters_are_keyed() {
        let doc = ApiDoc::load().unwrap();
        let (_, m) = doc.lookup(&DocRef { resource: "users".into(), method: "index".into(), route: 1 }).unwrap();
        let filter = m.params.iter().find(|p| p.name == "filter").unwrap();
        assert!(filter.is_group());
        assert!(filter.children.iter().any(|c| c.full_name == "filter[username]"));
        let range = m.params.iter().find(|p| p.name == "range").unwrap();
        assert!(range.children.iter().any(|c| c.full_name == "range[created_at]"));
        // The query string uses the bracket syntax.
        let spec = crate::model::RequestSpec {
            doc: Some(DocRef { resource: "users".into(), method: "index".into(), route: 1 }),
            path: m.routes[1].path.clone(),
            values: [("filter[username]".to_owned(), "jdoe,jane".to_owned())].into(),
            path_values: [("store_id".to_owned(), "1".to_owned())].into(),
            ..Default::default()
        };
        let url = crate::model::prepare(&spec, Some(m)).display_url;
        assert!(url.ends_with("/v1/stores/1/users?filter[username]=jdoe,jane"), "{url}");
    }

    #[test]
    fn route_matching() {
        let doc = ApiDoc::load().unwrap();
        let path = |verb: &str, p: &str| {
            let r = doc.match_route(verb, p)?;
            let (_, m) = doc.lookup(&r)?;
            Some(m.routes[r.route].path.clone())
        };
        assert_eq!(path("PATCH", "/v1/users/{{username}}").as_deref(), Some("/v1/users/:id"));
        assert_eq!(path("GET", "/v1/users/:user_id/orders").as_deref(), Some("/v1/users/:user_id/orders"));
        assert_eq!(path("get", "https://shop.example.com/v1/stores/1/users?page[size]=5").as_deref(), Some("/v1/stores/:store_id/users"));
        // A literal segment beats a variable one.
        assert_eq!(path("GET", "/v1/users").as_deref(), Some("/v1/users"));
        assert_eq!(path("GET", "/v1/nothing/here/at/all"), None);
        let r = doc.match_route("PATCH", "/v1/users/7").unwrap();
        assert!(doc.route_fits(&r, "PATCH", "/v1/users/{{id}}"));
        assert!(!doc.route_fits(&r, "GET", "/v1/users/{{id}}"));
    }

    #[test]
    fn search_ranking() {
        let doc = ApiDoc::load().unwrap();
        let keys = |q: &str| doc.search(q).iter().map(|h| doc.resources[h.resource].key.clone()).collect::<Vec<_>>();
        let o = keys("order");
        assert_eq!(&o[..2], ["orders", "order_items"]);
        assert_eq!(keys("user")[0], "users");
        // Inside Users: /v1/users first, nested routes last.
        let hit = &doc.search("user")[0];
        let res = &doc.resources[hit.resource];
        let paths: Vec<&str> = hit.routes.iter().map(|&(m, r)| res.methods[m].routes[r].path.as_str()).collect();
        assert!(paths[0].starts_with("/v1/users"), "{paths:?}");
        let depth = |p: &str| p.split('(').next().unwrap_or(p).split('/').filter(|s| !s.is_empty()).count();
        let users_routes: Vec<&&str> = paths.iter().take_while(|p| p.starts_with("/v1/users")).collect();
        assert!(users_routes.windows(2).all(|w| depth(w[0]) <= depth(w[1])), "{users_routes:?}");
        assert_eq!(doc.search("").len(), doc.resources.len());
    }

    #[test]
    fn embedded_doc_loads() {
        let doc = ApiDoc::load().unwrap();
        assert_eq!(doc.resources.len(), 11, "the test API's resources");
        assert!(doc.endpoint_count > 40, "{} endpoints", doc.endpoint_count);
        let users = doc.lookup(&DocRef { resource: "users".into(), method: "index".into(), route: 0 }).unwrap();
        let page = users.1.params.iter().find(|p| p.name == "page").unwrap();
        assert!(page.is_group());
        assert_eq!(page.children[1].full_name, "page[size]");
        assert_eq!(page.children[1].kind, ParamKind::Integer);
        assert!(doc.resources.iter().flat_map(|r| &r.methods).flat_map(|m| &m.routes).any(|r| r.deprecated));
    }
}
