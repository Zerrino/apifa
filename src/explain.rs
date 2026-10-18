//! "Explained by AI": what an endpoint does, written once by the assistant's model and kept
//! (`ai_explanations.json`, keyed like the recorded examples). It's written from the endpoint's docs,
//! an example of what its GET returns and the data map — so it needs both: without an example
//! answer or a map, nothing is offered. A POST / PATCH / PUT / DELETE is explained from the GET of
//! the same path (what it creates, changes or removes is what that GET returns).
//!
//! What the model gets goes through `privacy.rs` like everything else: the example answer as its
//! structure (types and fingerprints), the map (structure only), the public docs.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::docs::ApiDoc;
use crate::model::DocRef;

const FILE: &str = "ai_explanations.json";

#[derive(Clone, Serialize, Deserialize)]
pub struct Explanation {
    pub text: String,
    #[serde(default)]
    pub model: String,
    /// Unix seconds.
    #[serde(default)]
    pub at: u64,
    /// The GET whose example it was written from (`GET /v1/users/:id`).
    #[serde(default)]
    pub from: String,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Explanations {
    pub map: BTreeMap<String, Explanation>,
    /// Being written now (not saved).
    #[serde(skip)]
    pub busy: HashSet<String>,
    /// The last failure per endpoint (not saved).
    #[serde(skip)]
    pub errors: BTreeMap<String, String>,
}

pub type Shared = Arc<Mutex<Explanations>>;

pub fn key(d: &DocRef) -> String {
    format!("{}/{}/{}", d.resource, d.method, d.route)
}

impl Explanations {
    pub fn load() -> Self {
        crate::storage::load(FILE)
    }

    pub fn save(&self) {
        crate::storage::save(FILE, self);
    }

    pub fn get(&self, d: &DocRef) -> Option<&Explanation> {
        self.map.get(&key(d))
    }
}

/// The GET an endpoint is explained from — itself, or the GET of its path — and that GET's example
/// answer (the docs', else one recorded from a real call). None: nothing to explain it from.
pub fn source(doc: &ApiDoc, learned: &crate::learned::Examples, d: &DocRef) -> Option<(DocRef, String)> {
    let (_, m) = doc.lookup(d)?;
    let route = m.routes.get(d.route)?;
    let get = if route.verb.eq_ignore_ascii_case("GET") {
        d.clone()
    } else {
        let g = doc.match_route("GET", &route.path)?;
        // The same path exactly, not a neighbour that happens to fit.
        (doc.lookup(&g)?.1.routes.get(g.route)?.path == route.path).then_some(g)?
    };
    let (_, gm) = doc.lookup(&get)?;
    let example = gm
        .examples
        .iter()
        .filter(|e| e.verb.eq_ignore_ascii_case("GET"))
        .find_map(|e| e.response.clone().filter(|r| !r.trim().is_empty()))
        .or_else(|| learned.get(&get).map(|l| l.response.clone()))?;
    Some((get, example))
}

/// What the model is asked, with nothing of the user's data in it.
pub fn prompt(doc: &ApiDoc, d: &DocRef, get: &DocRef, example: &str, map: &crate::datamap::DataMap) -> Option<String> {
    let (res, m) = doc.lookup(d)?;
    let route = m.routes.get(d.route)?;
    let get_path = doc.lookup(get).and_then(|(_, gm)| gm.routes.get(get.route)).map(|r| r.path.clone()).unwrap_or_default();
    let mut leaves = Vec::new();
    crate::docs::param_leaves(&m.params, &mut leaves);
    let params: Vec<String> = leaves
        .iter()
        .map(|p| {
            format!(
                "- `{}` ({}{}){}",
                p.full_name,
                p.kind.label(),
                if p.required { ", required" } else { "" },
                if p.description.is_empty() { String::new() } else { format!(": {}", p.description) }
            )
        })
        .collect();
    let entity = crate::datamap::returned_by(map, &get_path).map(|e| e.name.clone());
    let map_part = match &entity {
        Some(e) => crate::datamap::markdown_for(map, e, ""),
        None => crate::datamap::overview(map),
    };
    let same = get == d;
    Some(format!(
        "Explain this endpoint of the API to a developer who will use it.\n\n\
         Endpoint: {} {} — resource “{}”, method “{}”.\n\
         Its documentation: {}\n\nParameters:\n{}\n\n\
         {}\n\nExample of what GET {get_path} returns (anonymized: each value is its type and a fingerprint; equal values, equal fingerprints):\n```json\n{}\n```\n\n\
         The data map (built from real GET responses: entities, fields, how they link; structure only):\n{}\n\n\
         Write, in Markdown, short and concrete:\n\
         1. What it does, in one or two sentences.\n\
         2. What it {} — the entity, its main fields and what they mean.\n\
         3. How it relates to the rest of the API (which ids come from where, what you'd call before or after it), from the data map.\n\
         4. The parameters that matter and a typical use.\n\
         Don't invent fields: only use what the documentation, the example and the map show. No values (there are none).",
        route.verb,
        route.path,
        res.name,
        m.name,
        if m.description.trim().is_empty() { "(none)" } else { m.description.trim() },
        if params.is_empty() { "(none)".to_owned() } else { params.join("\n") },
        if same {
            "It is a GET: the example below is its own answer.".to_owned()
        } else {
            format!("It is a {} on the same path as GET {get_path}: what it creates, changes or removes is what that GET returns (example below).", route.verb)
        },
        crate::ai::clip(&crate::privacy::body(example), 8000),
        crate::ai::clip(&map_part, 12_000),
        if same { "returns" } else { "acts on" },
    ))
}

/// Asks the model (blocking: run it on a thread).
pub fn write(cfg: &crate::ai::AiConfig, prompt: &str) -> Result<String, String> {
    let system = "You document an HTTP API inside a desktop API client (apifa). \
                  Be accurate and brief; use Markdown headings sparingly. You never see real data: values are given as type#fingerprint.";
    let reply = crate::ai::complete(cfg, system, &[], &[crate::ai::Message::user(vec![crate::ai::Part::Text(prompt.to_owned())])])?;
    let text: String = reply
        .message
        .parts
        .iter()
        .filter_map(|p| if let crate::ai::Part::Text(t) = p { Some(t.as_str()) } else { None })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned();
    if text.is_empty() {
        return Err(reply.warning.unwrap_or_else(|| "the model gave no text".into()));
    }
    Ok(text)
}

/// For widgets drawn this frame: the explanations, and whether the feature is on (an AI key and a
/// data map).
pub fn set(ctx: &egui::Context, shared: Shared, ready: bool) {
    ctx.data_mut(|d| {
        d.insert_temp(egui::Id::unique("ai_explanations"), shared);
        d.insert_temp(egui::Id::unique("ai_explanations_ready"), ready);
    });
}

pub fn get(ctx: &egui::Context) -> Option<Shared> {
    ctx.data(|d| d.get_temp::<Shared>(egui::Id::unique("ai_explanations")))
}

pub fn ready(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(egui::Id::unique("ai_explanations_ready"))).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explained_from_the_get_of_the_same_path() {
        let doc = ApiDoc::load().unwrap();
        let learned = crate::learned::Examples::default();
        // A GET with an example in the docs: itself.
        let routes: Vec<DocRef> = doc
            .resources
            .iter()
            .flat_map(|r| r.methods.iter().flat_map(move |m| (0..m.routes.len()).map(move |i| DocRef { resource: r.key.clone(), method: m.name.clone(), route: i })))
            .collect();
        let get = routes.iter().find(|d| source(&doc, &learned, d).is_some_and(|(g, _)| &g == *d)).expect("a GET with an example");
        // Its PATCH / DELETE (when there is one) are explained from it.
        let path = doc.lookup(get).unwrap().1.routes[get.route].path.clone();
        for verb in ["PATCH", "PUT", "DELETE"] {
            if let Some(other) = doc.match_route(verb, &path).filter(|o| doc.lookup(o).unwrap().1.routes[o.route].path == path) {
                assert!(source(&doc, &learned, &other).map(|(g, _)| g).as_ref() == Some(get), "{verb} {path}");
            }
        }
        // No example anywhere: nothing.
        let none = routes.iter().find(|d| source(&doc, &learned, d).is_none());
        assert!(none.is_some());
    }

    #[test]
    fn the_prompt_holds_no_value() {
        let doc = ApiDoc::load().unwrap();
        let mut b = crate::datamap::Builder::new(&doc);
        b.observe("Users", "/v1/users", Some(200), &serde_json::json!([ { "id": 1, "username": "jdoe" } ]));
        let map = b.finish(0);
        let d = doc.match_route("GET", "/v1/users").unwrap();
        let p = prompt(&doc, &d, &d, r#"[{"id": 98765, "username": "zkowalsk", "email": "zk@example.com"}]"#, &map).unwrap();
        assert!(!p.contains("zkowalsk") && !p.contains("98765") && !p.contains("example.com") && !p.contains("jdoe"), "{p}");
        assert!(p.contains("GET /v1/users") && p.contains("string#"), "{p}");
    }
}
