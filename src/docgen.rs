//! API documentation generated from the workspace (folder menu › Generate documentation, or
//! `apifa docs`): every saved request and flow of a folder, in one of three formats:
//! - HTML: one self-contained page (no network), searchable, light / dark;
//! - Markdown: for a README or a wiki;
//! - OpenAPI 3.0 (JSON): for Swagger UI, Redoc, code generators, or importing in other clients.
//!
//! A request gets: your notes (Docs tab), the API documentation of its endpoint, its parameters (path
//! variables, query, body) with the values saved as examples, a cURL command, its assertions (what a
//! good response looks like) and an example response (its mock, else the docs' example, else the one
//! recorded from a real call — `examples: false` leaves them out). A flow gets its description,
//! inputs, outputs and steps.
//!
//! `generate_api` documents the imported APIs instead (every documented route, grouped by resource,
//! with all its parameters): Endpoints tab › Generate documentation, or `apifa docs --api`.

use std::collections::HashMap;

use serde_json::{json, Map, Value};

use crate::docs::{ApiDoc, Method, ParamKind};
use crate::flow::{Block, Flow, InputType};
use crate::model::{self, BodyMode, DocRef, RequestSpec};
use crate::testrun::Item;
use crate::workspace::Node;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Html,
    Markdown,
    OpenApi,
}

impl Format {
    pub const ALL: [Format; 3] = [Format::Html, Format::Markdown, Format::OpenApi];

    pub fn label(self) -> &'static str {
        match self {
            Format::Html => "HTML page",
            Format::Markdown => "Markdown",
            Format::OpenApi => "OpenAPI 3 (JSON)",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Html => "html",
            Format::Markdown => "md",
            Format::OpenApi => "openapi.json",
        }
    }

    pub fn parse(s: &str) -> Option<Format> {
        match s.to_lowercase().as_str() {
            "html" => Some(Format::Html),
            "md" | "markdown" => Some(Format::Markdown),
            "openapi" | "oas" | "swagger" | "json" => Some(Format::OpenApi),
            _ => None,
        }
    }
}

pub struct Options {
    pub title: String,
    /// Example responses (mocks, docs, recorded calls): real data may be in them.
    pub examples: bool,
    /// The structure of the data (Understand), as a section / OpenAPI schemas.
    pub data_map: Option<crate::datamap::DataMap>,
}

/// Where a parameter goes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum In {
    Path,
    Query,
    Body,
}

impl In {
    fn label(self) -> &'static str {
        match self {
            In::Path => "path",
            In::Query => "query",
            In::Body => "body",
        }
    }
}

struct ParamDoc {
    name: String,
    at: In,
    required: bool,
    kind: Option<ParamKind>,
    description: String,
    example: String,
    /// Where its values come from (data map links, else its name): `GET /v1/categories`, and the entity.
    source: Option<(String, Option<String>)>,
}

/// What the data map says of an endpoint: the entity it returns (a GET) or works on (the GET of
/// the same path), what that entity uses and what uses it.
struct Shapes {
    entity: String,
    returned: bool,
    uses: Vec<String>,
    used_by: Vec<String>,
}

struct ReqDoc {
    name: String,
    folder: String,
    anchor: String,
    verb: String,
    path: String,
    notes: String,
    /// The API documentation of the endpoint: (resource, method description).
    api: Option<(String, String)>,
    params: Vec<ParamDoc>,
    /// Documented parameters left out (not required, not filled).
    more_params: usize,
    body: Option<String>,
    curl: String,
    asserts: Vec<String>,
    /// (status, body, where it comes from)
    response: Option<(u16, String, &'static str)>,
    data: Option<Shapes>,
    /// “Explained by AI” (Explain with AI in its tab), when there is one.
    explained: Option<String>,
}

struct FlowDoc {
    name: String,
    folder: String,
    anchor: String,
    description: String,
    inputs: Vec<(String, InputType, String, String)>,
    outputs: Vec<(String, String, String)>,
    /// (depth, block type, summary, note, enabled)
    steps: Vec<(usize, &'static str, String, String, bool)>,
}

enum Entry {
    Request(Box<ReqDoc>),
    Flow(FlowDoc),
}

impl Entry {
    fn folder(&self) -> &str {
        match self {
            Entry::Request(r) => &r.folder,
            Entry::Flow(f) => &f.folder,
        }
    }
}

fn slug(s: &str, taken: &mut Vec<String>) -> String {
    let mut base: String = s.to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
    while base.contains("--") {
        base = base.replace("--", "-");
    }
    let base = base.trim_matches('-').to_owned();
    let base = if base.is_empty() { "item".to_owned() } else { base };
    let mut out = base.clone();
    let mut n = 2;
    while taken.contains(&out) {
        out = format!("{base}-{n}");
        n += 1;
    }
    taken.push(out.clone());
    out
}

/// `Users/Sub/Name` → `Users/Sub`.
fn folder_of(path: &str, name: &str) -> String {
    path.strip_suffix(name).unwrap_or(path).trim_end_matches('/').to_owned()
}

fn pretty(text: &str) -> String {
    serde_json::from_str::<Value>(text).ok().and_then(|v| serde_json::to_string_pretty(&v).ok()).unwrap_or_else(|| text.to_owned())
}

/// A long example is cut: a doc page isn't a data dump.
fn cut(text: &str) -> String {
    const MAX: usize = 6000;
    if text.len() <= MAX {
        return text.to_owned();
    }
    let mut end = MAX;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… ({} more bytes)", &text[..end], text.len() - end)
}

/// `all_params`: every documented parameter (API docs), not only the required / filled ones.
#[allow(clippy::too_many_arguments)]
fn request_doc(
    name: &str,
    folder: &str,
    anchor: String,
    spec: &RequestSpec,
    doc: &ApiDoc,
    learned: &crate::learned::Examples,
    explained: &crate::explain::Explanations,
    opts: &Options,
    all_params: bool,
) -> ReqDoc {
    let verb = spec.verb.trim().to_uppercase();
    let found = spec.doc.clone().or_else(|| doc.match_route(&verb, &spec.path));
    let docs = found.as_ref().and_then(|d| doc.lookup(d));
    let method: Option<&Method> = docs.map(|(_, m)| m);
    let mut documented = Vec::new();
    if let Some(m) = method {
        crate::docs::param_leaves(&m.params, &mut documented);
    }
    let described = |name: &str| documented.iter().find(|p| p.full_name == name || p.name == name).copied();

    let mut params = Vec::new();
    for seg in spec.path.split('/') {
        let seg = seg.trim_matches(|c| c == '(' || c == ')');
        if let Some(var) = seg.strip_prefix(':') {
            let p = described(var);
            params.push(ParamDoc {
                name: var.to_owned(),
                at: In::Path,
                required: !spec.path.contains(&format!("(/:{var}")),
                kind: p.map(|p| p.kind),
                description: p.map(|p| p.description.clone()).unwrap_or_default(),
                example: spec.path_values.get(var).cloned().unwrap_or_default(),
                source: None,
            });
        }
    }
    let body_side = model::has_body(&verb);
    let mut left_out = 0;
    for p in &documented {
        if params.iter().any(|x| x.at == In::Path && x.name == p.full_name) {
            continue;
        }
        let value = spec.values.get(&p.full_name).cloned().unwrap_or_default();
        if !all_params && !p.required && value.trim().is_empty() {
            left_out += 1;
            continue;
        }
        params.push(ParamDoc {
            name: p.full_name.clone(),
            at: if body_side { In::Body } else { In::Query },
            required: p.required,
            kind: Some(p.kind),
            description: p.description.clone(),
            example: value,
            source: None,
        });
    }
    for q in spec.query.iter().filter(|q| q.enabled && !q.key.trim().is_empty()) {
        if params.iter().any(|x| x.name == q.key.trim()) {
            continue;
        }
        let p = described(q.key.trim());
        params.push(ParamDoc {
            name: q.key.trim().to_owned(),
            at: In::Query,
            required: false,
            kind: p.map(|p| p.kind),
            description: p.map(|p| p.description.clone()).unwrap_or_default(),
            example: q.value.clone(),
            source: None,
        });
    }
    // Where values come from, and what the data is: the data map's links (else the parameter's name).
    let resource = found.as_ref().map(|d| d.resource.clone());
    let template = found.as_ref().and_then(|d| Some(doc.lookup(d)?.1.routes.get(d.route)?.path.clone()));
    for p in &mut params {
        let Some((src, _)) = doc.value_source(&p.name, resource.as_deref()) else { continue };
        let Some(path) = doc.lookup(&src).and_then(|(_, m)| m.routes.get(src.route)).map(|r| r.path.clone()) else { continue };
        // The parameter of the list itself (`filter[id]` of GET /v1/categories) points nowhere new.
        if template.as_deref() == Some(path.as_str()) && verb == "GET" {
            continue;
        }
        let entity = opts.data_map.as_ref().and_then(|m| crate::datamap::returned_by(m, &path)).map(|e| e.name.clone());
        p.source = Some((format!("GET {path}"), entity));
    }
    let data = opts.data_map.as_ref().zip(template.as_ref()).and_then(|(map, t)| {
        let (e, returned) = match crate::datamap::returned_by(map, t) {
            Some(e) if verb == "GET" => (e, true),
            _ => (crate::datamap::returned_by(map, t)?, false),
        };
        let (uses, used) = crate::datamap::neighbours(map, &e.name);
        Some(Shapes {
            entity: e.name.clone(),
            returned,
            uses: uses.iter().map(|l| crate::datamap::link_words(l, &e.name)).collect(),
            used_by: used.iter().map(|l| crate::datamap::link_words(l, &e.name)).collect(),
        })
    });
    let body = (body_side && spec.body_mode == BodyMode::Raw && !spec.raw_body.trim().is_empty()).then(|| pretty(&spec.raw_body));
    let prepared = model::prepare(spec, method);
    let response = opts.examples.then(|| {
        if let Some(m) = &spec.mock {
            return Some((m.status, pretty(&m.body), "its mock"));
        }
        let m = method?;
        if let Some((code, text)) = m
            .examples
            .iter()
            .filter(|e| e.verb.eq_ignore_ascii_case(&verb))
            .find_map(|e| Some((e.code.parse().unwrap_or(200), e.response.clone().filter(|r| !r.trim().is_empty())?)))
        {
            return Some((code, pretty(&text), "the API docs"));
        }
        let l = learned.get(found.as_ref()?)?;
        Some((l.status, l.response.clone(), "a real call"))
    });
    ReqDoc {
        name: name.to_owned(),
        folder: folder.to_owned(),
        anchor,
        verb,
        path: spec.path.trim().to_owned(),
        notes: spec.description.trim().to_owned(),
        api: docs.map(|(r, m)| (r.name.clone(), m.description.trim().to_owned())),
        params,
        more_params: left_out,
        body,
        curl: model::curl_snippet(&prepared),
        asserts: spec.assertions.iter().filter(|a| a.enabled).map(|a| a.text()).collect(),
        response: response.flatten().map(|(s, b, w)| (s, cut(&b), w)),
        data,
        explained: found.as_ref().and_then(|d| explained.get(d)).map(|e| e.text.clone()),
    }
}

fn flow_doc(f: &Flow, folder: &str, anchor: String) -> FlowDoc {
    fn walk(blocks: &[Block], depth: usize, out: &mut Vec<(usize, &'static str, String, String, bool)>) {
        for b in blocks {
            out.push((depth, b.kind.title(), b.kind.summary(), b.note.trim().to_owned(), b.enabled));
            let branches = b.kind.children();
            let labelled = branches.iter().filter(|(_, c)| !c.is_empty()).count() > 1 || branches.iter().any(|(l, c)| *l == "else" && !c.is_empty());
            for (label, children) in branches.into_iter().filter(|(_, c)| !c.is_empty()) {
                // `then:` / `else:` only when the else has blocks.
                if labelled && !label.is_empty() {
                    out.push((depth + 1, "", format!("{label}:"), String::new(), b.enabled));
                }
                walk(children, depth + 1 + (labelled && !label.is_empty()) as usize, out);
            }
        }
    }
    let mut steps = Vec::new();
    walk(&f.blocks, 0, &mut steps);
    FlowDoc {
        name: f.name.clone(),
        folder: folder.to_owned(),
        anchor,
        description: f.description.trim().to_owned(),
        inputs: f.inputs.iter().filter(|i| !i.name.trim().is_empty()).map(|i| (i.name.trim().to_owned(), i.kind, i.value.clone(), i.description.clone())).collect(),
        outputs: f.outputs.iter().filter(|o| !o.name.trim().is_empty()).map(|o| (o.name.trim().to_owned(), o.value.clone(), o.description.clone())).collect(),
        steps,
    }
}

fn entries(items: &[Item], doc: &ApiDoc, opts: &Options) -> Vec<Entry> {
    if opts.data_map.is_some() {
        doc.set_data_map(opts.data_map.as_ref());
    }
    let learned = crate::learned::Examples::load();
    let explained = crate::explain::Explanations::load();
    let mut taken = Vec::new();
    items
        .iter()
        .filter_map(|it| match &it.node {
            Node::Request(r) => {
                let folder = folder_of(&it.path, &r.name);
                Some(Entry::Request(Box::new(request_doc(&r.name, &folder, slug(&it.path, &mut taken), &r.spec, doc, &learned, &explained, opts, false))))
            }
            Node::Flow(f) => {
                let folder = folder_of(&it.path, &f.name);
                Some(Entry::Flow(flow_doc(f, &folder, slug(&it.path, &mut taken))))
            }
            Node::Folder(_) => None,
        })
        .collect()
}

pub fn generate(items: &[Item], doc: &ApiDoc, opts: &Options, format: Format) -> String {
    render(&entries(items, doc, opts), opts, format)
}

/// Name of a route in the API docs: its summary, else the method's description when short, else
/// `Resource method`.
fn route_name(res: &crate::docs::Resource, m: &Method, route: &crate::docs::Route) -> String {
    let desc = m.description.lines().next().unwrap_or_default().trim().trim_end_matches('.');
    if !route.summary.trim().is_empty() {
        route.summary.trim().trim_end_matches('.').to_owned()
    } else if !desc.is_empty() && desc.chars().count() <= 80 {
        desc.to_owned()
    } else {
        format!("{} {}", res.name, m.name)
    }
}

/// The imported APIs: every documented route of the resources matching one of `filter` (key or
/// name, case-insensitive; empty = all), grouped by resource. Returns the page and how many routes.
pub fn generate_api(doc: &ApiDoc, filter: &[String], opts: &Options, format: Format) -> (String, usize) {
    if opts.data_map.is_some() {
        doc.set_data_map(opts.data_map.as_ref());
    }
    let learned = crate::learned::Examples::load();
    let explained = crate::explain::Explanations::load();
    let filter: Vec<String> = filter.iter().map(|f| f.trim().to_lowercase()).filter(|f| !f.is_empty()).collect();
    let mut taken = Vec::new();
    let mut list = Vec::new();
    for res in &doc.resources {
        if !filter.is_empty() && !filter.iter().any(|f| *f == res.key.to_lowercase() || *f == res.name.to_lowercase()) {
            continue;
        }
        for m in &res.methods {
            for (i, route) in m.routes.iter().enumerate() {
                let spec = RequestSpec {
                    verb: route.verb.clone(),
                    path: route.path.clone(),
                    doc: Some(DocRef { resource: res.key.clone(), method: m.name.clone(), route: i }),
                    ..Default::default()
                };
                let name = route_name(res, m, route);
                let anchor = slug(&format!("{} {} {}", res.key, route.verb, route.path), &mut taken);
                let mut r = request_doc(&name, &res.name, anchor, &spec, doc, &learned, &explained, opts, true);
                if route.deprecated {
                    r.notes = "Deprecated.".into();
                }
                list.push(Entry::Request(Box::new(r)));
            }
        }
    }
    let n = list.len();
    (render(&list, opts, format), n)
}

fn render(list: &[Entry], opts: &Options, format: Format) -> String {
    match format {
        Format::Html => html(list, opts),
        Format::Markdown => markdown(list, opts),
        Format::OpenApi => serde_json::to_string_pretty(&openapi(list, opts)).unwrap_or_default(),
    }
}

// ==================================================================== Markdown

fn md_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

fn markdown(list: &[Entry], opts: &Options) -> String {
    let mut out = format!("# {}\n\n", opts.title);
    out.push_str(&format!("{}{}. Each cURL example shows the call with its authentication.\n\n", counts(list, ", "), base_note()));
    out.push_str("## Contents\n\n");
    let mut folder = None;
    for e in list {
        if folder != Some(e.folder()) {
            folder = Some(e.folder());
            if !e.folder().is_empty() {
                out.push_str(&format!("- **{}**\n", e.folder()));
            }
        }
        let indent = if e.folder().is_empty() { "" } else { "  " };
        match e {
            Entry::Request(r) => out.push_str(&format!("{indent}- [{}](#{}) — `{} {}`\n", r.name, r.anchor, r.verb, r.path)),
            Entry::Flow(f) => out.push_str(&format!("{indent}- [{}](#{}) — flow\n", f.name, f.anchor)),
        }
    }
    out.push('\n');
    let mut folder = None;
    for e in list {
        if folder != Some(e.folder()) {
            folder = Some(e.folder());
            if !e.folder().is_empty() {
                out.push_str(&format!("## {}\n\n", e.folder()));
            }
        }
        match e {
            Entry::Request(r) => md_request(&mut out, r),
            Entry::Flow(f) => md_flow(&mut out, f),
        }
    }
    if let Some(map) = &opts.data_map {
        out.push('\n');
        out.push_str(&crate::datamap::markdown_section(map));
        out.push('\n');
    }
    out
}

fn md_request(out: &mut String, r: &ReqDoc) {
    out.push_str(&format!("<a id=\"{}\"></a>\n### {}\n\n`{} {}`\n\n", r.anchor, r.name, r.verb, r.path));
    if !r.notes.is_empty() {
        out.push_str(&format!("{}\n\n", r.notes));
    }
    if let Some((res, desc)) = &r.api {
        let desc = if desc.is_empty() { String::new() } else { format!(": {desc}") };
        out.push_str(&format!("> API docs — {res}{desc}\n\n"));
    }
    if let Some(d) = &r.data {
        md_shapes(out, d);
    }
    if let Some(text) = &r.explained {
        out.push_str(&format!("<details><summary>Explained by AI</summary>\n\n{}\n\n</details>\n\n", text.trim()));
    }
    if !r.params.is_empty() {
        out.push_str("| Parameter | In | Required | Example | Description | Values from |\n|---|---|---|---|---|---|\n");
        for p in &r.params {
            let from = match &p.source {
                Some((get, Some(e))) => format!("`{get}` ([{e}](#entity-{e}))"),
                Some((get, None)) => format!("`{get}`"),
                None => String::new(),
            };
            out.push_str(&format!(
                "| `{}` | {} | {} | {} | {} | {from} |\n",
                md_cell(&p.name),
                p.at.label(),
                if p.required { "yes" } else { "" },
                if p.example.is_empty() { String::new() } else { format!("`{}`", md_cell(&p.example)) },
                md_cell(&p.description)
            ));
        }
        out.push('\n');
    }
    if r.more_params > 0 {
        out.push_str(&format!("{} more optional parameter(s) are documented by the API.\n\n", r.more_params));
    }
    if let Some(b) = &r.body {
        out.push_str(&format!("Body:\n\n```json\n{b}\n```\n\n"));
    }
    if !r.asserts.is_empty() {
        out.push_str("Expected (assertions):\n\n");
        for a in &r.asserts {
            out.push_str(&format!("- `{a}`\n"));
        }
        out.push('\n');
    }
    out.push_str(&format!("```sh\n{}\n```\n\n", r.curl.trim_end()));
    if let Some((status, body, from)) = &r.response {
        out.push_str(&format!("Example response ({status}, from {from}):\n\n```json\n{body}\n```\n\n"));
    }
}

fn md_shapes(out: &mut String, d: &Shapes) {
    let what = if d.returned { "Returns" } else { "Works on" };
    out.push_str(&format!("**{what}** [`{0}`](#entity-{0}) (data map).", d.entity));
    if !d.uses.is_empty() {
        out.push_str(&format!(" It uses {}.", d.uses.join(", ")));
    }
    if !d.used_by.is_empty() {
        out.push_str(&format!(" Used by {}.", d.used_by.join(", ")));
    }
    out.push_str("\n\n");
}

fn md_flow(out: &mut String, f: &FlowDoc) {
    out.push_str(&format!("<a id=\"{}\"></a>\n### {} (flow)\n\n", f.anchor, f.name));
    if !f.description.is_empty() {
        out.push_str(&format!("{}\n\n", f.description));
    }
    if !f.inputs.is_empty() {
        out.push_str("| Input | Type | Default | Description |\n|---|---|---|---|\n");
        for (n, t, v, d) in &f.inputs {
            out.push_str(&format!(
                "| `{}` | {} | {} | {} |\n",
                md_cell(n),
                t.label(),
                if v.is_empty() { String::new() } else { format!("`{}`", md_cell(v)) },
                md_cell(d)
            ));
        }
        out.push('\n');
    }
    if !f.outputs.is_empty() {
        out.push_str("| Output | Value | Description |\n|---|---|---|\n");
        for (n, v, d) in &f.outputs {
            out.push_str(&format!("| `{}` | `{}` | {} |\n", md_cell(n), md_cell(v), md_cell(d)));
        }
        out.push('\n');
    }
    out.push_str("Steps:\n\n```text\n");
    for (depth, kind, text, note, enabled) in &f.steps {
        let off = if *enabled { "" } else { " (disabled)" };
        let note = if note.is_empty() { String::new() } else { format!("   # {note}") };
        if kind.is_empty() {
            out.push_str(&format!("{}{text}\n", "  ".repeat(*depth)));
        } else {
            out.push_str(&format!("{}{kind:<14} {text}{off}{note}\n", "  ".repeat(*depth)));
        }
    }
    out.push_str("```\n\n");
    out.push_str(&format!("Run it: `apifa run \"{}\"`\n\n", f.name));
}

// ==================================================================== HTML

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Paragraphs and `code` of plain notes (no full Markdown).
fn prose(s: &str) -> String {
    s.split("\n\n")
        .filter(|p| !p.trim().is_empty())
        .map(|p| {
            let mut html = String::new();
            for (i, part) in esc(p.trim()).split('`').enumerate() {
                if i % 2 == 1 {
                    html.push_str(&format!("<code>{part}</code>"));
                } else {
                    html.push_str(&part.replace('\n', "<br>"));
                }
            }
            format!("<p>{html}</p>")
        })
        .collect()
}

const CSS: &str = r#"
:root{--bg:#f5f7f8;--card:#fff;--text:#2c3e50;--muted:#7f8c8d;--line:#e3e8ea;--code:#1e2a36;--codetext:#ecf0f1;--accent:#00babc;--side:#2c3e50;--sidetext:#ecf0f1}
@media (prefers-color-scheme:dark){:root{--bg:#141c24;--card:#1c2733;--text:#ecf0f1;--muted:#95a5a6;--line:#2c3a48;--code:#0f161d;--side:#0f161d}}
*{box-sizing:border-box}body{margin:0;font:15px/1.55 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif;background:var(--bg);color:var(--text)}
nav{position:fixed;top:0;bottom:0;left:0;width:290px;overflow:auto;background:var(--side);color:var(--sidetext);padding:18px 14px}
nav h1{font-size:19px;margin:0 0 4px}nav .sub{color:#95a5a6;font-size:12px;margin-bottom:12px}
nav input{width:100%;padding:7px 9px;border-radius:5px;border:0;margin-bottom:12px;font:inherit}
nav .folder{font-size:11px;text-transform:uppercase;letter-spacing:.06em;color:#95a5a6;margin:14px 0 4px}
nav a{display:flex;gap:7px;align-items:center;color:var(--sidetext);text-decoration:none;padding:3px 5px;border-radius:4px;font-size:13.5px}
nav a:hover{background:rgba(255,255,255,.08)}
main{margin-left:290px;padding:28px 36px;max-width:1100px}
section.item{background:var(--card);border:1px solid var(--line);border-radius:8px;padding:20px 24px;margin:0 0 22px}
h2.folder{margin:30px 0 12px;font-size:14px;text-transform:uppercase;letter-spacing:.06em;color:var(--muted)}
h3{margin:0 0 6px;font-size:20px}.route{font:14px ui-monospace,SFMono-Regular,Menlo,monospace;margin-bottom:10px;word-break:break-all}
.verb{display:inline-block;min-width:54px;text-align:center;padding:1px 6px;border-radius:4px;color:#fff;font:bold 11.5px ui-monospace,Menlo,monospace}
.GET{background:#27ae60}.POST{background:#2980b9}.PUT,.PATCH{background:#e67e22}.DELETE{background:#c0392b}.FLOW{background:#8e44ad}
.api{border-left:3px solid var(--accent);padding:4px 12px;color:var(--muted);margin:10px 0}
table{border-collapse:collapse;width:100%;margin:10px 0;font-size:13.5px}th,td{text-align:left;padding:6px 8px;border-bottom:1px solid var(--line);vertical-align:top}
th{color:var(--muted);font-weight:600}td code,p code,li code{background:rgba(127,140,141,.15);padding:1px 4px;border-radius:3px;font-size:12.5px}
.req{color:#c0392b;font-weight:600}
pre{background:var(--code);color:var(--codetext);padding:12px 14px;border-radius:6px;overflow:auto;font:12.5px/1.5 ui-monospace,SFMono-Regular,Menlo,monospace;max-height:420px}
details summary{cursor:pointer;color:var(--muted);margin:8px 0}h4{margin:16px 0 4px;font-size:14px}
.off{opacity:.45;text-decoration:line-through}.note{color:var(--muted)}footer{color:var(--muted);font-size:12px;margin:30px 0}
@media (max-width:800px){nav{position:static;width:auto}main{margin:0;padding:16px}}
"#;

const JS: &str = r##"
const q=document.getElementById('q');q.addEventListener('input',()=>{const t=q.value.toLowerCase();
document.querySelectorAll('section.item').forEach(s=>{const hit=!t||s.textContent.toLowerCase().includes(t);s.style.display=hit?'':'none';
const a=document.querySelector('nav a[href="#'+s.id+'"]');if(a)a.style.display=hit?'':'none'});});
"##;

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// `3 requests · 1 flow`, without `0 flows` (API docs).
fn counts(list: &[Entry], sep: &str) -> String {
    let requests = list.iter().filter(|e| matches!(e, Entry::Request(_))).count();
    let flows = list.len() - requests;
    if flows == 0 {
        plural(requests, "request")
    } else {
        format!("{}{sep}{}", plural(requests, "request"), plural(flows, "flow"))
    }
}

fn html(list: &[Entry], opts: &Options) -> String {
    let title = esc(&opts.title);
    let mut nav = String::new();
    let mut body = String::new();
    // `GET /v1/categories` → its section, for the “values from” links.
    let anchors: HashMap<String, String> = list
        .iter()
        .filter_map(|e| match e {
            Entry::Request(r) => Some((format!("{} {}", r.verb, r.path), r.anchor.clone())),
            Entry::Flow(_) => None,
        })
        .rev()
        .collect();
    let mut folder = None;
    for e in list {
        if folder != Some(e.folder()) {
            folder = Some(e.folder());
            if !e.folder().is_empty() {
                nav.push_str(&format!("<div class=\"folder\">{}</div>", esc(e.folder())));
                body.push_str(&format!("<h2 class=\"folder\">{}</h2>", esc(e.folder())));
            }
        }
        match e {
            Entry::Request(r) => {
                nav.push_str(&format!("<a href=\"#{}\"><span class=\"verb {}\">{}</span>{}</a>", r.anchor, esc(&r.verb), esc(&r.verb), esc(&r.name)));
                html_request(&mut body, r, &anchors);
            }
            Entry::Flow(f) => {
                nav.push_str(&format!("<a href=\"#{}\"><span class=\"verb FLOW\">FLOW</span>{}</a>", f.anchor, esc(&f.name)));
                html_flow(&mut body, f);
            }
        }
    }
    if let Some(map) = &opts.data_map {
        nav.push_str("<a href=\"#data-map\"><span class=\"verb FLOW\">MAP</span>Data map</a>");
        body.push_str(&crate::datamap::html_section(map));
    }
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{title}</title><style>{CSS}</style></head><body>\
<nav><h1>{title}</h1><div class=\"sub\">{}</div><input id=\"q\" placeholder=\"Search…\" autocomplete=\"off\">{nav}</nav>\
<main><p class=\"note\">{}{}. Each cURL example shows the call with its authentication.</p>{body}\
<footer>Generated by apifa on {}.</footer></main><script>{JS}</script></body></html>\n",
        counts(list, " · "),
        esc(&counts(list, ", ")),
        esc(&base_note()),
        chrono::Local::now().format("%Y-%m-%d %H:%M")
    )
}

/// ` on https://api.example.com` (the base URL now), or nothing.
fn base_note() -> String {
    let b = model::base_url_for("");
    if b.is_empty() {
        String::new()
    } else {
        format!(" on `{b}`")
    }
}

/// The data map as a page of its own (Data map tab › Export, `apifa datamap -f html`).
pub fn data_map_page(map: &crate::datamap::DataMap) -> String {
    let nav: String = map.entities.keys().map(|e| format!("<a href=\"#entity-{0}\"><span class=\"verb FLOW\">MAP</span>{0}</a>", esc(e))).collect();
    format!(
        "<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Data map</title><style>{CSS}</style></head><body>\
<nav><h1>Data map</h1><div class=\"sub\">{} entities · {} links</div><input id=\"q\" placeholder=\"Search…\" autocomplete=\"off\">{nav}</nav>\
<main>{}<footer>Generated by apifa on {}.</footer></main><script>{JS}</script></body></html>\n",
        map.entities.len(),
        map.links.len(),
        crate::datamap::html_section(map),
        chrono::Local::now().format("%Y-%m-%d %H:%M")
    )
}

fn html_request(out: &mut String, r: &ReqDoc, anchors: &HashMap<String, String>) {
    out.push_str(&format!("<section class=\"item\" id=\"{}\"><h3>{}</h3>", r.anchor, esc(&r.name)));
    out.push_str(&format!("<div class=\"route\"><span class=\"verb {0}\">{0}</span> {1}</div>", esc(&r.verb), esc(&r.path)));
    out.push_str(&prose(&r.notes));
    if let Some((res, desc)) = &r.api {
        out.push_str(&format!("<div class=\"api\"><b>API docs — {}</b>{}</div>", esc(res), if desc.is_empty() { String::new() } else { prose(desc) }));
    }
    if let Some(d) = &r.data {
        let what = if d.returned { "Returns" } else { "Works on" };
        out.push_str(&format!("<div class=\"api\"><b>{what}</b> <a href=\"#entity-{0}\"><code>{0}</code></a> <span class=\"note\">(data map)</span>", esc(&d.entity)));
        if !d.uses.is_empty() {
            out.push_str(&format!("<br>It uses {}.", esc(&d.uses.join(", "))));
        }
        if !d.used_by.is_empty() {
            out.push_str(&format!("<br>Used by {}.", esc(&d.used_by.join(", "))));
        }
        out.push_str("</div>");
    }
    if let Some(text) = &r.explained {
        out.push_str(&format!("<details><summary>Explained by AI</summary>{}</details>", prose(text)));
    }
    if !r.params.is_empty() {
        out.push_str("<table><tr><th>Parameter</th><th>In</th><th>Example</th><th>Description</th><th>Values from</th></tr>");
        for p in &r.params {
            let kind = p.kind.map(|k| format!(" <span class=\"note\">{}</span>", esc(k.label()))).unwrap_or_default();
            let from = match &p.source {
                Some((get, entity)) => {
                    let target = anchors.get(get).map(|a| format!("#{a}")).or_else(|| entity.as_ref().map(|e| format!("#entity-{e}")));
                    match target {
                        Some(t) => format!("<a href=\"{}\"><code>{}</code></a>", esc(&t), esc(get)),
                        None => format!("<code>{}</code>", esc(get)),
                    }
                }
                None => String::new(),
            };
            out.push_str(&format!(
                "<tr><td><code>{}</code>{}{kind}</td><td>{}</td><td>{}</td><td>{}</td><td>{from}</td></tr>",
                esc(&p.name),
                if p.required { " <span class=\"req\">required</span>" } else { "" },
                p.at.label(),
                if p.example.is_empty() { String::new() } else { format!("<code>{}</code>", esc(&p.example)) },
                esc(&p.description)
            ));
        }
        out.push_str("</table>");
    }
    if r.more_params > 0 {
        out.push_str(&format!("<p class=\"note\">{} more optional parameter(s) are documented by the API.</p>", r.more_params));
    }
    if let Some(b) = &r.body {
        out.push_str(&format!("<h4>Body</h4><pre>{}</pre>", esc(b)));
    }
    if !r.asserts.is_empty() {
        out.push_str("<h4>Expected</h4><ul>");
        for a in &r.asserts {
            out.push_str(&format!("<li><code>{}</code></li>", esc(a)));
        }
        out.push_str("</ul>");
    }
    out.push_str(&format!("<details><summary>cURL</summary><pre>{}</pre></details>", esc(r.curl.trim_end())));
    if let Some((status, body, from)) = &r.response {
        out.push_str(&format!("<details open><summary>Example response · {status} · from {from}</summary><pre>{}</pre></details>", esc(body)));
    }
    out.push_str("</section>");
}

fn html_flow(out: &mut String, f: &FlowDoc) {
    out.push_str(&format!("<section class=\"item\" id=\"{}\"><h3>{}</h3><div class=\"route\"><span class=\"verb FLOW\">FLOW</span></div>", f.anchor, esc(&f.name)));
    out.push_str(&prose(&f.description));
    if !f.inputs.is_empty() {
        out.push_str("<h4>Inputs</h4><table><tr><th>Name</th><th>Type</th><th>Default</th><th>Description</th></tr>");
        for (n, t, v, d) in &f.inputs {
            out.push_str(&format!(
                "<tr><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td></tr>",
                esc(n),
                t.label(),
                if v.is_empty() { String::new() } else { format!("<code>{}</code>", esc(v)) },
                esc(d)
            ));
        }
        out.push_str("</table>");
    }
    if !f.outputs.is_empty() {
        out.push_str("<h4>Outputs</h4><table><tr><th>Name</th><th>Value</th><th>Description</th></tr>");
        for (n, v, d) in &f.outputs {
            out.push_str(&format!("<tr><td><code>{}</code></td><td><code>{}</code></td><td>{}</td></tr>", esc(n), esc(v), esc(d)));
        }
        out.push_str("</table>");
    }
    out.push_str("<h4>Steps</h4><pre>");
    for (depth, kind, text, note, enabled) in &f.steps {
        let line = if kind.is_empty() { format!("{}{text}", "  ".repeat(*depth)) } else { format!("{}{kind:<14} {text}", "  ".repeat(*depth)) };
        let note = if note.is_empty() { String::new() } else { format!("   # {note}") };
        if *enabled {
            out.push_str(&format!("{}{}\n", esc(&line), esc(&note)));
        } else {
            out.push_str(&format!("<span class=\"off\">{}</span>\n", esc(&line)));
        }
    }
    out.push_str(&format!("</pre><p class=\"note\">Run it: <code>apifa run \"{}\"</code></p></section>", esc(&f.name)));
}

// ==================================================================== OpenAPI

fn schema(kind: Option<ParamKind>) -> Value {
    match kind {
        Some(ParamKind::Integer) => json!({ "type": "integer" }),
        Some(ParamKind::Float) => json!({ "type": "number" }),
        Some(ParamKind::Bool) => json!({ "type": "boolean" }),
        Some(ParamKind::DateTime) => json!({ "type": "string", "format": "date-time" }),
        Some(ParamKind::IntArray) => json!({ "type": "array", "items": { "type": "integer" } }),
        Some(ParamKind::Array) => json!({ "type": "array", "items": { "type": "string" } }),
        Some(ParamKind::File) => json!({ "type": "string", "format": "binary" }),
        _ => json!({ "type": "string" }),
    }
}

/// `/v1/users/:id(/x)` → `/v1/users/{id}/x`.
fn oas_path(path: &str) -> String {
    path.replace(['(', ')'], "")
        .split('/')
        .map(|s| match s.strip_prefix(':') {
            Some(v) => format!("{{{v}}}"),
            None => s.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn openapi(list: &[Entry], opts: &Options) -> Value {
    let mut paths: Map<String, Value> = Map::new();
    let mut tags: Vec<String> = Vec::new();
    let mut flows = Vec::new();
    for e in list {
        let r = match e {
            Entry::Request(r) => r,
            Entry::Flow(f) => {
                flows.push(json!({ "name": f.name, "description": f.description, "inputs": f.inputs.iter().map(|(n, t, v, d)| json!({ "name": n, "type": t.label(), "default": v, "description": d })).collect::<Vec<_>>() }));
                continue;
            }
        };
        let tag = if r.folder.is_empty() { "Workspace".to_owned() } else { r.folder.clone() };
        if !tags.contains(&tag) {
            tags.push(tag.clone());
        }
        let path = oas_path(r.path.split('?').next().unwrap_or_default());
        let item = paths.entry(path).or_insert_with(|| json!({}));
        let verb = r.verb.to_lowercase();
        // Two saved requests on the same route: the first one documents it, the others are mentioned.
        if let Some(op) = item.get_mut(&verb) {
            let d = op["description"].as_str().unwrap_or_default().to_owned();
            op["description"] = Value::String(format!("{d}\n\nAlso saved as “{}”.", r.name).trim().to_owned());
            continue;
        }
        let mut description = r.notes.clone();
        if let Some((res, desc)) = &r.api {
            description.push_str(&format!("\n\nAPI docs — {res}{}", if desc.is_empty() { String::new() } else { format!(": {desc}") }));
        }
        if !r.asserts.is_empty() {
            description.push_str(&format!("\n\nExpected: {}", r.asserts.iter().map(|a| format!("`{a}`")).collect::<Vec<_>>().join(", ")));
        }
        let parameters: Vec<Value> = r
            .params
            .iter()
            .filter(|p| p.at != In::Body)
            .map(|p| {
                let mut v = json!({ "name": p.name, "in": p.at.label(), "required": p.at == In::Path || p.required, "schema": schema(p.kind) });
                if !p.description.is_empty() {
                    v["description"] = Value::String(p.description.clone());
                }
                if !p.example.is_empty() {
                    v["example"] = Value::String(p.example.clone());
                }
                if let Some((get, entity)) = &p.source {
                    v["x-values-from"] = json!({ "endpoint": get, "entity": entity });
                }
                v
            })
            .collect();
        let mut op = json!({ "summary": r.name, "operationId": r.anchor, "tags": [tag], "parameters": parameters });
        if let Some(text) = &r.explained {
            op["x-explained-by-ai"] = Value::String(text.clone());
        }
        if let Some(d) = &r.data {
            op["x-data-map"] = json!({ if d.returned { "returns" } else { "works_on" }: d.entity, "uses": d.uses, "used_by": d.used_by });
        }
        if !description.trim().is_empty() {
            op["description"] = Value::String(description.trim().to_owned());
        }
        // Body: the raw JSON when there is one, else the documented fields as a form object.
        let body_fields: Vec<&ParamDoc> = r.params.iter().filter(|p| p.at == In::Body).collect();
        if let Some(b) = &r.body {
            let example = serde_json::from_str::<Value>(b).unwrap_or(Value::String(b.clone()));
            op["requestBody"] = json!({ "content": { "application/json": { "schema": { "type": "object" }, "example": example } } });
        } else if !body_fields.is_empty() {
            let mut props = Map::new();
            let mut required = Vec::new();
            for p in &body_fields {
                let mut s = schema(p.kind);
                if !p.description.is_empty() {
                    s["description"] = Value::String(p.description.clone());
                }
                if !p.example.is_empty() {
                    s["example"] = Value::String(p.example.clone());
                }
                props.insert(p.name.clone(), s);
                if p.required {
                    required.push(p.name.clone());
                }
            }
            let mut s = json!({ "type": "object", "properties": props });
            if !required.is_empty() {
                s["required"] = json!(required);
            }
            op["requestBody"] = json!({ "content": { "application/json": { "schema": s } } });
        }
        let mut responses = Map::new();
        match &r.response {
            Some((status, body, from)) => {
                let example = serde_json::from_str::<Value>(body).unwrap_or(Value::String(body.clone()));
                responses.insert(status.to_string(), json!({ "description": format!("Example from {from}"), "content": { "application/json": { "example": example } } }));
            }
            None => {
                responses.insert("200".into(), json!({ "description": "Success" }));
            }
        }
        responses.insert("401".into(), json!({ "description": "Missing or expired token" }));
        op["responses"] = Value::Object(responses);
        item[verb] = op;
    }
    let mut out = json!({
        "openapi": "3.0.3",
        "info": { "title": opts.title, "version": chrono::Local::now().format("%Y-%m-%d").to_string(), "description": "Generated by apifa." },
        "servers": if model::base_url().is_empty() { json!([]) } else { json!([{ "url": model::base_url() }]) },
        "tags": tags.iter().map(|t| json!({ "name": t })).collect::<Vec<_>>(),
        "paths": paths,
        "components": {},
    });
    if !flows.is_empty() {
        out["x-flows"] = Value::Array(flows);
    }
    // The workspace's authentication, as a security scheme.
    let scheme = match crate::auth::default_auth() {
        crate::auth::Auth::Basic { .. } => Some(json!({ "type": "http", "scheme": "basic" })),
        crate::auth::Auth::Bearer { .. } => Some(json!({ "type": "http", "scheme": "bearer" })),
        crate::auth::Auth::ApiKey { key, location, .. } => {
            Some(json!({ "type": "apiKey", "name": key, "in": if location == crate::auth::KeyLocation::Query { "query" } else { "header" } }))
        }
        crate::auth::Auth::OAuth2(o) => {
            let scopes: Map<String, Value> = o.scope.split_whitespace().map(|s| (s.to_owned(), Value::String(String::new()))).collect();
            let flow = match o.grant {
                crate::auth::Grant::ClientCredentials => json!({ "clientCredentials": { "tokenUrl": o.token_url, "scopes": scopes } }),
                crate::auth::Grant::Password => json!({ "password": { "tokenUrl": o.token_url, "scopes": scopes } }),
                crate::auth::Grant::AuthorizationCode => json!({ "authorizationCode": { "authorizationUrl": o.auth_url, "tokenUrl": o.token_url, "scopes": scopes } }),
            };
            Some(json!({ "type": "oauth2", "flows": flow }))
        }
        crate::auth::Auth::Inherit | crate::auth::Auth::None => None,
    };
    if let Some(scheme) = scheme {
        out["components"]["securitySchemes"] = json!({ "workspace": scheme });
        out["security"] = json!([{ "workspace": [] }]);
    }
    // The structure Understand found: one schema per entity, the links beside.
    if let Some(map) = &opts.data_map {
        let schemas: serde_json::Map<String, Value> = map
            .entities
            .values()
            .filter_map(|e| {
                let shape = if e.full.objects > 0 { &e.full } else { &e.embedded };
                (shape.objects > 0).then(|| (e.name.clone(), crate::datamap::json_schema(shape)))
            })
            .collect();
        out["components"]["schemas"] = Value::Object(schemas);
        out["x-data-map-links"] = serde_json::to_value(&map.links).unwrap_or_default();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assert::{Assertion, Op};
    use crate::model::Kv;
    use crate::workspace::SavedRequest;

    fn items() -> Vec<Item> {
        let spec = RequestSpec {
            verb: "GET".into(),
            path: "/v1/stores/:store_id/users".into(),
            path_values: [("store_id".to_owned(), "12".to_owned())].into(),
            query: vec![Kv { enabled: true, key: "page[size]".into(), value: "100".into() }],
            description: "Users of a store. Use `12` for Downtown.".into(),
            assertions: vec![Assertion::new("res.status", Op::Eq, "200")],
            mock: Some(crate::mock::Mock::from_response(200, r#"[{"id":1,"login":"jdoe"}]"#)),
            ..Default::default()
        };
        let post = RequestSpec {
            verb: "POST".into(),
            path: "/v1/users/:id/notes".into(),
            body_mode: BodyMode::Raw,
            raw_body: r#"{"note":{"content":"<hi>"}}"#.into(),
            ..Default::default()
        };
        let flow: Flow = serde_json::from_value(json!({
            "name": "Shipped orders", "description": "Looks a login up",
            "inputs": [ { "type": "string", "name": "login", "value": "jdoe", "description": "A login" } ],
            "blocks": [ { "type": "request", "save_as": "u", "verb": "GET", "path": "/v1/users/{{login}}" },
                        { "type": "if", "left": "{{u.status}}", "op": "eq", "right": "200", "then_blocks": [ { "type": "log", "message": "ok" } ], "else_blocks": [] } ],
            "outputs": [ { "name": "id", "value": "{{u.body.id}}" } ]
        }))
        .unwrap();
        vec![
            Item { path: "Stores/Users of a store".into(), node: Node::Request(SavedRequest { uid: 1, name: "Users of a store".into(), spec }) },
            Item { path: "Stores/Add a note".into(), node: Node::Request(SavedRequest { uid: 2, name: "Add a note".into(), spec: post }) },
            Item { path: "Shipped orders".into(), node: Node::Flow(flow) },
        ]
    }

    fn opts() -> Options {
        Options { title: "My <Shop> API".into(), examples: true, data_map: None }
    }

    #[test]
    fn html_page() {
        let doc = ApiDoc::load().unwrap();
        let page = generate(&items(), &doc, &opts(), Format::Html);
        assert!(page.starts_with("<!doctype html>"));
        assert!(page.contains("<title>My &lt;Shop&gt; API</title>"), "escaped");
        assert!(page.contains("href=\"#stores-users-of-a-store\""));
        assert!(page.contains("Use <code>12</code> for Downtown"));
        assert!(page.contains("<code>store_id</code>"));
        assert!(page.contains("from its mock"));
        assert!(page.contains("&lt;hi&gt;"), "bodies are escaped");
        assert!(page.contains("res.status eq 200"));
        assert!(page.contains("FLOW</span>Shipped orders"));
        assert!(page.contains("\n  LOG"), "nested steps are indented");
        assert!(!page.contains("http://") || !page.contains("<script src"), "self-contained");
        // Example responses can be left out (real data may be in them).
        let without = generate(&items(), &doc, &Options { examples: false, ..opts() }, Format::Html);
        assert!(!without.contains("Example response"));
    }

    #[test]
    fn markdown_and_openapi() {
        let doc = ApiDoc::load().unwrap();
        let md = generate(&items(), &doc, &opts(), Format::Markdown);
        assert!(md.contains("## Stores"));
        assert!(md.contains("- [Users of a store](#stores-users-of-a-store) — `GET /v1/stores/:store_id/users`"));
        assert!(md.contains("| `store_id` | path | yes | `12` |"), "{md}");
        assert!(md.contains("| `login` | string | `jdoe` | A login |"));

        let oas: Value = serde_json::from_str(&generate(&items(), &doc, &opts(), Format::OpenApi)).unwrap();
        assert_eq!(oas["openapi"], "3.0.3");
        let get = &oas["paths"]["/v1/stores/{store_id}/users"]["get"];
        assert_eq!(get["summary"], "Users of a store");
        assert_eq!(get["tags"][0], "Stores");
        assert!(get["parameters"].as_array().unwrap().iter().any(|p| p["name"] == "store_id" && p["in"] == "path" && p["required"] == true));
        assert_eq!(get["responses"]["200"]["content"]["application/json"]["example"][0]["login"], "jdoe");
        assert_eq!(oas["paths"]["/v1/users/{id}/notes"]["post"]["requestBody"]["content"]["application/json"]["example"]["note"]["content"], "<hi>");
        assert_eq!(oas["x-flows"][0]["name"], "Shipped orders");
    }

    #[test]
    fn api_docs() {
        let doc = ApiDoc::load().unwrap();
        let (md, n) = generate_api(&doc, &["users".into()], &opts(), Format::Markdown);
        assert!(n > 5, "{n}");
        assert!(md.contains("## Users"));
        assert!(md.contains("`GET /v1/users`"), "{md}");
        assert!(!md.contains("flows"), "no `0 flows`");
        assert!(md.contains("`page[size]`"), "optional parameters are listed: {md}");
        let (all, total) = generate_api(&doc, &[], &opts(), Format::OpenApi);
        assert_eq!(total, doc.endpoint_count);
        let oas: Value = serde_json::from_str(&all).unwrap();
        assert!(oas["paths"]["/v1/users/{id}"]["get"].is_object());
        assert_eq!(generate_api(&doc, &["nope".into()], &opts(), Format::Html).1, 0);
    }
}
