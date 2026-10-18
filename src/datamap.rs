//! "Understand": the structure of an API's data, rebuilt from real GET responses (and only GETs).
//!
//! Every saved GET request is sent; its answer (and the examples recorded from real GETs) tells
//! which entities exist (`users`, `orders`…), the fields of each (types, formats, optional,
//! nullable) and how they link: an object embedded in another (`orders[].user`), a list of
//! them, or a reference (`store_id`). A link is *confirmed* when the ids found there are ids seen on
//! the target's own endpoints — compared by fingerprints (privacy.rs), never kept.
//!
//! The result keeps no value at all: `datamap.json` (data folder), `DATA_MAP.md` next to the
//! workspace for agents, a section of the generated docs, the assistant's `data_map` tool, and the
//! graph of the "Data map" window.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::docs::ApiDoc;
use crate::privacy::{fingerprint_u64, string_format};

pub const FILE: &str = "datamap.json";
pub const AGENT_FILE: &str = "DATA_MAP.md";

/// Fingerprints kept per field to tell a closed set (a status, a kind) from free values.
const DISTINCT_CAP: usize = 24;

/// The types seen at one place of the data, merged over every value seen there.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Shape {
    /// Values seen here (for a field: the objects that had it).
    pub seen: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub nulls: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub booleans: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub integers: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub numbers: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub strings: u64,
    /// String formats seen (`date-time`, `url`…), with how often.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub formats: BTreeMap<String, u64>,
    /// Distinct values seen, when few among many: a closed set (no value is kept, only the count).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distinct: Option<u64>,
    #[serde(skip)]
    fingerprints: HashSet<u64>,
    #[serde(skip)]
    overflow: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub array: Option<Box<ArrayShape>>,
    /// Objects seen here, and their fields.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub objects: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, Shape>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ArrayShape {
    pub min_len: u64,
    pub max_len: u64,
    pub items: Shape,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl Shape {
    /// Adds a value. `depth` stops at very deep data (a free-form JSON field).
    pub fn add(&mut self, v: &Value, depth: usize) {
        self.seen += 1;
        match v {
            Value::Null => self.nulls += 1,
            Value::Bool(_) => self.booleans += 1,
            Value::Number(n) => {
                if n.is_i64() || n.is_u64() {
                    self.integers += 1;
                } else {
                    self.numbers += 1;
                }
                self.note_value(&n.to_string());
            }
            Value::String(s) => {
                self.strings += 1;
                if let Some(f) = string_format(s) {
                    *self.formats.entry(f.to_owned()).or_default() += 1;
                }
                self.note_value(s);
            }
            Value::Array(a) => {
                let arr = self.array.get_or_insert_with(|| Box::new(ArrayShape { min_len: a.len() as u64, max_len: a.len() as u64, items: Shape::default() }));
                arr.min_len = arr.min_len.min(a.len() as u64);
                arr.max_len = arr.max_len.max(a.len() as u64);
                if depth < 12 {
                    for x in a {
                        arr.items.add(x, depth + 1);
                    }
                }
            }
            Value::Object(o) => {
                self.objects += 1;
                if depth < 12 {
                    match crate::privacy::data_keys(o) {
                        // A dictionary keyed by data (dates → counts): one "field" for all its keys.
                        Some(label) => {
                            for x in o.values() {
                                self.fields.entry(label.to_owned()).or_default().add(x, depth + 1);
                            }
                        }
                        None => {
                            for (k, x) in o {
                                self.fields.entry(k.clone()).or_default().add(x, depth + 1);
                            }
                        }
                    }
                }
            }
        }
    }

    fn note_value(&mut self, v: &str) {
        if self.overflow {
            return;
        }
        self.fingerprints.insert(fingerprint_u64(v));
        if self.fingerprints.len() > DISTINCT_CAP {
            self.overflow = true;
            self.fingerprints.clear();
        }
    }

    /// Ends the counting: a closed set is said (`distinct`), the fingerprints are dropped.
    fn finish(&mut self) {
        let scalars = self.integers + self.numbers + self.strings;
        self.distinct = (!self.overflow && scalars >= 20 && (self.fingerprints.len() as u64) * 4 <= scalars).then_some(self.fingerprints.len() as u64);
        self.fingerprints.clear();
        if let Some(a) = &mut self.array {
            a.items.finish();
        }
        self.fields.values_mut().for_each(Shape::finish);
    }

    /// `integer`, `string (date-time)`, `array of object`, `string | null`…
    pub fn describe(&self) -> String {
        let mut kinds = Vec::new();
        if self.integers > 0 {
            kinds.push("integer".to_owned());
        }
        if self.numbers > 0 {
            kinds.push("number".to_owned());
        }
        if self.strings > 0 {
            // The format when most strings have it.
            let main = self.formats.iter().filter(|(f, n)| f.as_str() != "empty" && **n * 2 >= self.strings).max_by_key(|(_, n)| **n).map(|(f, _)| f.clone());
            kinds.push(match main {
                Some(f) => format!("string ({f})"),
                None => "string".to_owned(),
            });
        }
        if self.booleans > 0 {
            kinds.push("boolean".to_owned());
        }
        if self.objects > 0 {
            kinds.push("object".to_owned());
        }
        if let Some(a) = &self.array {
            let items = a.items.describe();
            kinds.push(if a.max_len == 0 { "array (always empty)".to_owned() } else { format!("array of {items}") });
        }
        if kinds.is_empty() && self.nulls > 0 {
            return "null (always)".into();
        }
        let mut out = kinds.join(" | ");
        if self.nulls > 0 {
            out.push_str(" | null");
        }
        out
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    /// The whole object, inside another (`orders[].user`).
    Embedded,
    /// A list of them (`users[].orders`).
    EmbeddedList,
    /// An id pointing to one (`store_id`).
    Reference,
}

impl LinkKind {
    pub fn label(&self) -> &'static str {
        match self {
            LinkKind::Embedded => "embeds",
            LinkKind::EmbeddedList => "embeds a list of",
            LinkKind::Reference => "refers to",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entity {
    /// The API resource (`users`), or the field name it was found under when the docs have none.
    pub name: String,
    pub documented: bool,
    /// The GETs that return it (route templates).
    #[serde(default)]
    pub endpoints: BTreeSet<String>,
    /// As its own endpoints return it.
    #[serde(default)]
    pub full: Shape,
    /// As other entities embed it (often fewer fields).
    #[serde(default)]
    pub embedded: Shape,
    /// Other field names it was found under (`corrector`, `user`…).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub aliases: BTreeSet<String>,
}

impl Entity {
    fn new(name: &str, documented: bool) -> Self {
        Entity { name: name.to_owned(), documented, endpoints: BTreeSet::new(), full: Shape::default(), embedded: Shape::default(), aliases: BTreeSet::new() }
    }

    /// Every field seen, full or embedded.
    pub fn field_names(&self) -> BTreeSet<&str> {
        self.full.fields.keys().chain(self.embedded.fields.keys()).map(String::as_str).collect()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Link {
    pub from: String,
    /// Where in `from` (`orders[].product`, `store_id`).
    pub field: String,
    pub to: String,
    pub kind: LinkKind,
    /// Times seen.
    pub seen: u64,
    /// Distinct ids found there, and how many of them are ids of `to` seen on its own endpoints.
    pub ids: u64,
    pub confirmed: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Source {
    /// Where it came from: a saved request (its place in the workspace) or a recorded example.
    pub name: String,
    /// `GET /v1/users/:id` (the documented route, or the path as saved).
    pub route: String,
    pub status: Option<u16>,
    /// Elements read (1 for an object).
    pub items: u64,
    /// Why it gave nothing (not sent, not JSON, HTTP error…).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DataMap {
    /// The API profile it maps (empty: the workspace without profile).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub profile: String,
    /// Unix seconds.
    pub built_at: u64,
    /// Every GET was tried; false while Understand runs (live view, a partial save) or when stopped.
    #[serde(default)]
    pub complete: bool,
    pub sources: Vec<Source>,
    pub entities: BTreeMap<String, Entity>,
    pub links: Vec<Link>,
}

/// Builds a map from responses, one `observe` at a time.
#[derive(Clone)]
pub struct Builder<'a> {
    doc: &'a ApiDoc,
    /// The API's path words: a path the docs don't have is kept with its values anonymized.
    vocabulary: HashSet<String>,
    map: DataMap,
    /// Fingerprints of the ids each entity's own endpoints returned.
    own_ids: HashMap<String, HashSet<u64>>,
    /// Per link (from, field, to, kind): times seen and the fingerprints of the ids found there.
    links: BTreeMap<LinkKey, (u64, HashSet<u64>)>,
    /// A few real ids per entity, to fill the `:category_id` of the next GETs (Understand unlocks
    /// endpoints with them). In memory only: never in the map, never written, never sent to an AI.
    ids: HashMap<String, Vec<String>>,
}

/// A link while it's counted: (from, field, to, kind).
type LinkKey = (String, String, String, LinkKind);

/// Real ids kept per entity to try endpoints with.
const IDS_KEPT: usize = 6;

impl<'a> Builder<'a> {
    pub fn new(doc: &'a ApiDoc) -> Self {
        Builder { doc, vocabulary: crate::privacy::vocabulary(doc), map: DataMap::default(), own_ids: HashMap::new(), links: BTreeMap::new(), ids: HashMap::new() }
    }

    /// The resource a field name is about (`user` → `users`, `category` → `categories`), or the name itself.
    fn entity_name(&self, field: &str) -> (String, bool) {
        let base = field.trim_end_matches("[]");
        // `parent` / `children` of a project are projects: the caller (`related`) knows which.
        let mut candidates = crate::docs::plurals(base);
        candidates.push(base.trim_end_matches('s').to_owned());
        for c in &candidates {
            if self.doc.resources.iter().any(|r| &r.key == c) {
                return (c.clone(), true);
            }
        }
        (base.to_owned(), false)
    }

    /// The entity a field of `current` is about: `parent` / `children` are `current` itself (a
    /// project's sub-projects), the others by their name.
    fn related(&self, field: &str, current: &str) -> (String, bool) {
        let base = field.trim_end_matches("[]");
        if matches!(base, "parent" | "children" | "parents" | "child") {
            return (current.to_owned(), self.map.entities.get(current).is_some_and(|e| e.documented));
        }
        self.entity_name(field)
    }

    fn entity(&mut self, name: &str, documented: bool) -> &mut Entity {
        self.map.entities.entry(name.to_owned()).or_insert_with(|| Entity::new(name, documented))
    }

    /// A GET's answer. `path` is what was called (template or real path): its documented route gives the entity.
    pub fn observe(&mut self, source: &str, path: &str, status: Option<u16>, body: &Value) {
        let path_only = path.split('?').next().unwrap_or(path);
        let route = self.doc.match_route("GET", path_only);
        self.observe_at(source, path_only, route, status, body);
    }

    /// The same for a documented GET whose route is known (a template like `…/graph(/on/:field)`
    /// isn't found back from its text).
    pub fn observe_route(&mut self, source: &str, route: &crate::model::DocRef, template: &str, status: Option<u16>, body: &Value) {
        self.observe_at(source, template, Some(route.clone()), status, body);
    }

    fn observe_at(&mut self, source: &str, path_only: &str, route: Option<crate::model::DocRef>, status: Option<u16>, body: &Value) {
        let (entity, documented, route_text) = match &route {
            Some(d) => {
                let template = self.doc.lookup(d).map(|(_, m)| m.routes[d.route].path.clone()).unwrap_or_else(|| path_only.to_owned());
                (d.resource.clone(), true, format!("GET {template}"))
            }
            None => {
                // Named after the last resource of the API in it (never a value: `/v1/users/jdoe/x` → `users`),
                // else its last word of the API.
                let words: Vec<&str> = path_only.split('/').filter(|s| self.vocabulary.contains(*s)).collect();
                let last = words.iter().rev().find(|w| self.entity_name(w).1).or(words.last()).copied().unwrap_or("data");
                let (name, documented) = self.entity_name(last);
                (name, documented, format!("GET {}", crate::privacy::path(path_only, &self.vocabulary)))
            }
        };
        let items = match body {
            Value::Array(a) => {
                for x in a {
                    if let Value::Object(o) = x {
                        self.visit(&entity, documented, o, true);
                    }
                }
                a.len() as u64
            }
            // Statistics keyed by data (`…/graph`: dates → counts) are no element of the entity.
            Value::Object(o) if crate::privacy::data_keys(o).is_some() => {
                let label = crate::privacy::data_keys(o).unwrap_or("{key}");
                self.map.sources.push(Source {
                    name: source.to_owned(),
                    route: route_text,
                    status,
                    items: 0,
                    problem: Some(format!("statistics ({label} → value), not {entity} objects")),
                });
                return;
            }
            Value::Object(o) => {
                self.visit(&entity, documented, o, true);
                1
            }
            _ => 0,
        };
        self.entity(&entity, documented).endpoints.insert(route_text.clone());
        self.map.sources.push(Source { name: source.to_owned(), route: route_text, status, items, problem: None });
    }

    /// A request that gave nothing to read.
    pub fn skipped(&mut self, source: &str, path: &str, status: Option<u16>, problem: String) {
        let route = format!("GET {}", crate::privacy::path(path.trim(), &self.vocabulary));
        self.map.sources.push(Source { name: source.to_owned(), route, status, items: 0, problem: Some(problem) });
    }

    /// Ids seen for `entity` (its own endpoints, embedded, referenced), to call the GETs that need one.
    pub fn known_ids(&self, entity: &str) -> &[String] {
        self.ids.get(entity).map(Vec::as_slice).unwrap_or_default()
    }

    fn keep_id(&mut self, entity: &str, id: &Value) {
        if !(id.is_number() || id.is_string()) {
            return;
        }
        let list = self.ids.entry(entity.to_owned()).or_default();
        let text = scalar_text(id);
        if list.len() < IDS_KEPT && !text.is_empty() && !list.contains(&text) {
            list.push(text);
        }
    }

    fn visit(&mut self, entity: &str, documented: bool, obj: &Map<String, Value>, own: bool) {
        if let Some(id) = obj.get("id") {
            self.keep_id(entity, id);
        }
        let value = Value::Object(obj.clone());
        let e = self.entity(entity, documented);
        if own {
            e.full.add(&value, 0)
        } else {
            e.embedded.add(&value, 0)
        }
        if own {
            if let Some(id) = obj.get("id").filter(|v| v.is_number() || v.is_string()) {
                self.own_ids.entry(entity.to_owned()).or_default().insert(fingerprint_u64(&scalar_text(id)));
            }
        }
        self.scan(entity, "", obj);
    }

    /// The links inside an object of `entity` (`prefix`: where we are in it, for objects without an id).
    fn scan(&mut self, entity: &str, prefix: &str, obj: &Map<String, Value>) {
        // A dictionary keyed by data: its keys are values, links are named after `{date}`, `{key}`…
        let label = crate::privacy::data_keys(obj);
        for (k, v) in obj {
            let k = label.unwrap_or(k);
            let field = format!("{prefix}{k}");
            match v {
                Value::Object(child) if child.contains_key("id") => {
                    let (to, documented) = self.related(k, entity);
                    self.link(entity, &field, &to, LinkKind::Embedded, child.get("id"));
                    self.visit(&to, documented, child, false);
                }
                Value::Object(child) => self.scan(entity, &format!("{field}."), child),
                Value::Array(a) if a.iter().any(|x| x.get("id").is_some()) => {
                    let (to, documented) = self.related(k, entity);
                    for x in a {
                        if let Value::Object(child) = x {
                            self.link(entity, &format!("{field}[]"), &to, LinkKind::EmbeddedList, child.get("id"));
                            self.visit(&to, documented, child, false);
                        }
                    }
                }
                Value::Array(a) if k.ends_with("_ids") => {
                    let (to, _) = self.related(k.trim_end_matches("_ids"), entity);
                    for x in a.iter().filter(|x| x.is_number() || x.is_string()) {
                        self.link(entity, &format!("{field}[]"), &to, LinkKind::Reference, Some(x));
                    }
                }
                Value::Array(a) => {
                    for x in a {
                        if let Value::Object(child) = x {
                            self.scan(entity, &format!("{field}[]."), child);
                        }
                    }
                }
                Value::Number(_) | Value::String(_) if k.ends_with("_id") && k.len() > 3 => {
                    let (to, _) = self.related(k.trim_end_matches("_id"), entity);
                    self.link(entity, &field, &to, LinkKind::Reference, Some(v));
                }
                _ => {}
            }
        }
    }

    fn link(&mut self, from: &str, field: &str, to: &str, kind: LinkKind, id: Option<&Value>) {
        if let (LinkKind::Reference, Some(id)) = (&kind, id) {
            self.keep_id(to, id);
        }
        let entry = self.links.entry((from.to_owned(), field.to_owned(), to.to_owned(), kind)).or_default();
        entry.0 += 1;
        if let Some(id) = id.filter(|v| v.is_number() || v.is_string()) {
            entry.1.insert(fingerprint_u64(&scalar_text(id)));
        }
    }

    /// The map: unknown names merged into the entity they are (same fields, same ids), links confirmed.
    /// The map so far, the run going on (`complete` false).
    pub fn snapshot(&self) -> DataMap {
        self.clone().finish(crate::storage::now())
    }

    pub fn finish(mut self, built_at: u64) -> DataMap {
        self.merge_aliases();
        for ((from, field, to, kind), (seen, ids)) in std::mem::take(&mut self.links) {
            let confirmed = self.own_ids.get(&to).map(|own| ids.intersection(own).count() as u64).unwrap_or(0);
            // A reference to something that never shows up is still a link the data has.
            self.map.entities.entry(to.clone()).or_insert_with(|| Entity::new(&to, self.doc.resources.iter().any(|r| r.key == to)));
            self.map.links.push(Link { from, field, to, kind, seen, ids: ids.len() as u64, confirmed });
        }
        for e in self.map.entities.values_mut() {
            e.full.finish();
            e.embedded.finish();
        }
        self.map.built_at = built_at;
        self.map
    }

    /// Fields most entities have (`id`, timestamps, a name…): they don't tell which entity an unknown
    /// object is. Found in the data: a field of at least a third of the entities seen whole (and of two).
    fn generic_fields(&self) -> HashSet<String> {
        let whole: Vec<&Entity> = self.map.entities.values().filter(|e| !e.full.fields.is_empty()).collect();
        let mut count: HashMap<&str, usize> = HashMap::new();
        for e in &whole {
            for k in e.full.fields.keys() {
                *count.entry(k.as_str()).or_default() += 1;
            }
        }
        count.into_iter().filter(|(_, n)| *n >= 2 && n * 3 >= whole.len()).map(|(k, _)| k.to_owned()).collect()
    }

    /// `corrector` (not a resource) embedded with the fields and the ids of `users`: it's a user.
    fn merge_aliases(&mut self) {
        let unknown: Vec<String> = self.map.entities.values().filter(|e| !e.documented && e.endpoints.is_empty()).map(|e| e.name.clone()).collect();
        let generic = self.generic_fields();
        for name in unknown {
            let fields: BTreeSet<String> = self.map.entities[&name].field_names().into_iter().map(str::to_owned).collect();
            let ids: HashSet<u64> = self.links.iter().filter(|((_, _, to, _), _)| to == &name).flat_map(|(_, (_, ids))| ids.iter().copied()).collect();
            // A word of its name naming an entity (`from_user` → users, `corrector`: none).
            let named: Vec<String> = name.split('_').filter_map(|w| Some(self.entity_name(w)).filter(|(_, documented)| *documented).map(|(n, _)| n)).collect();
            let best = self
                .map
                .entities
                .values()
                .filter(|e| e.name != name && !e.full.fields.is_empty())
                .map(|e| {
                    let theirs: BTreeSet<String> = e.full.fields.keys().filter(|k| !generic.contains(*k)).cloned().collect();
                    // How much of what it has, they have (it's often a smaller, embedded version), on the
                    // fields that tell entities apart (everyone has an id, a name, timestamps).
                    let own: BTreeSet<&String> = fields.iter().filter(|k| !generic.contains(*k)).collect();
                    let common = own.iter().filter(|k| theirs.contains(**k)).count();
                    let inside = common as f64 / own.len().max(1) as f64;
                    let shared_ids = self.own_ids.get(&e.name).map(|own| ids.intersection(own).count()).unwrap_or(0);
                    let by_name = named.contains(&e.name);
                    // Its name says so: a distinctive field in common is enough. Its ids are theirs (a weak clue
                    // alone: small ids like 1, 2, 3 are everyone's): most of its fields too. No clue: two
                    // distinctive fields at least, nearly all of its own. Else it keeps its name.
                    let fits = (by_name && common >= 1) || (shared_ids > 0 && common >= 1 && inside >= 0.6) || (common >= 2 && inside >= 0.8);
                    (e.name.clone(), fits, by_name, shared_ids > 0, inside, theirs.len())
                })
                .filter(|c| c.1)
                .max_by(|a, b| (a.2, a.3, a.4).partial_cmp(&(b.2, b.3, b.4)).unwrap_or(std::cmp::Ordering::Equal).then(a.5.cmp(&b.5)))
                .map(|(n, ..)| (n, 0.0, 0usize));
            let Some((target, _, _)) = best else { continue };
            let alias = self.map.entities.remove(&name).expect("listed above");
            let t = self.map.entities.get_mut(&target).expect("found above");
            t.aliases.insert(name.clone());
            t.aliases.extend(alias.aliases);
            merge_shape(&mut t.embedded, &alias.embedded);
            let moved: Vec<_> = self.links.keys().filter(|(from, _, to, _)| from == &name || to == &name).cloned().collect();
            for key in moved {
                let v = self.links.remove(&key).expect("listed");
                let rename = |s: &String| if s == &name { target.clone() } else { s.clone() };
                let e = self.links.entry((rename(&key.0), key.1.clone(), rename(&key.2), key.3)).or_default();
                e.0 += v.0;
                e.1.extend(v.1);
            }
        }
    }
}

/// Adds what `b` saw to `a` (an alias merged into its entity).
fn merge_shape(a: &mut Shape, b: &Shape) {
    a.seen += b.seen;
    a.nulls += b.nulls;
    a.booleans += b.booleans;
    a.integers += b.integers;
    a.numbers += b.numbers;
    a.strings += b.strings;
    for (f, n) in &b.formats {
        *a.formats.entry(f.clone()).or_default() += n;
    }
    a.overflow |= b.overflow;
    a.fingerprints.extend(b.fingerprints.iter().copied());
    if a.fingerprints.len() > DISTINCT_CAP {
        a.overflow = true;
        a.fingerprints.clear();
    }
    if let Some(bb) = &b.array {
        match &mut a.array {
            Some(aa) => {
                aa.min_len = aa.min_len.min(bb.min_len);
                aa.max_len = aa.max_len.max(bb.max_len);
                merge_shape(&mut aa.items, &bb.items);
            }
            None => a.array = Some(bb.clone()),
        }
    }
    a.objects += b.objects;
    for (k, s) in &b.fields {
        merge_shape(a.fields.entry(k.clone()).or_default(), s);
    }
}

fn scalar_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------- running it

/// What `understand` sends: the saved GET requests (workspace path, request) of `profile` — the
/// requests whose own or folder's profile it is, and those bound to none (they use the active
/// profile, the one Understand maps).
pub fn saved_gets(root: &crate::workspace::Folder, profile: &str) -> Vec<(String, crate::workspace::SavedRequest)> {
    type Out = Vec<(String, crate::workspace::SavedRequest)>;
    fn walk(f: &crate::workspace::Folder, prefix: &str, inherited: &str, want: &str, out: &mut Out) {
        let here = if f.profile.is_empty() { inherited } else { f.profile.as_str() };
        for n in &f.items {
            match n {
                crate::workspace::Node::Folder(sub) => walk(sub, &format!("{prefix}{}/", sub.name), here, want, out),
                crate::workspace::Node::Request(r) if r.spec.verb.trim().eq_ignore_ascii_case("GET") && r.spec.protocol.is_http() => {
                    let mut r = r.clone();
                    if r.spec.profile.is_empty() {
                        r.spec.profile = here.to_owned();
                    }
                    if want.is_empty() || r.spec.profile.is_empty() || r.spec.profile == want {
                        out.push((format!("{prefix}{}", r.name), r));
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(root, "", "", profile, &mut out);
    out
}

/// The entity a path or query parameter takes the id of: `:id` → the route's own resource,
/// `:category_id` / `filter[category_id]` → `categories`, `:user_id` → `users`.
fn param_entity(doc: &ApiDoc, name: &str, own: &str) -> Option<String> {
    let name = name.trim_start_matches(':').trim_end_matches(']').rsplit('[').next().unwrap_or(name);
    if name == "id" {
        return Some(own.to_owned());
    }
    let base = name.strip_suffix("_id")?;
    crate::docs::plurals(base).into_iter().find(|k| doc.resources.iter().any(|r| &r.key == k)).or_else(|| Some(format!("{base}s")))
}

/// What fills a required parameter: an id of an entity, or a value of its type (a date: now).
enum Fill {
    Id(String),
    Value(String),
}

/// A documented GET to try: its route, what it needs (path variables and required query fields: name,
/// in the path, what fills it).
struct Pending {
    doc: crate::model::DocRef,
    template: String,
    needs: Vec<(String, bool, Fill)>,
}

/// A required query field that isn't an id: a date is now, an enum its first choice, a boolean false.
fn fill_for(doc: &ApiDoc, p: &crate::docs::Param, own: &str) -> Fill {
    use crate::docs::ParamKind;
    if p.name.ends_with("_id") || p.name == "id" {
        return Fill::Id(param_entity(doc, &p.full_name, own).unwrap_or_else(|| p.name.clone()));
    }
    match p.kind {
        ParamKind::DateTime => Fill::Value("{{now}}".into()),
        ParamKind::Bool => Fill::Value("false".into()),
        _ if !p.options.is_empty() => Fill::Value(p.options[0].clone()),
        _ if p.name.ends_with("_at") => Fill::Value("{{now}}".into()),
        _ => Fill::Id(param_entity(doc, &p.full_name, own).unwrap_or_else(|| p.name.clone())),
    }
}

/// Sends the saved GETs, then every documented GET (`all`), each one once its required ids are known:
/// `/v1/categories` gives category ids, which unlock `/v1/categories/:category_id/products`, whose products unlock… until
/// a round unlocks nothing. Only GETs, spaced by `delay`; a 404 is tried again with another id.
/// The recorded examples are read too. `progress(done, total, what, map)` before each call, with the map so
/// far every second or so (`map`: to show it live); `stop` ends it early.
#[allow(clippy::too_many_arguments)]
pub fn understand(
    sender: &crate::testrun::Sender,
    requests: &[(String, crate::workspace::SavedRequest)],
    learned: &crate::learned::Shared,
    all: bool,
    delay: Duration,
    stop: &AtomicBool,
    profile: &str,
    mut progress: impl FnMut(usize, usize, &str, Option<DataMap>),
) -> DataMap {
    let doc = &sender.doc;
    let mut b = Builder::new(doc);
    b.map.profile = profile.to_owned();
    // A GET answered whose endpoint has no example response yet: this answer becomes it (Examples tab,
    // mock server, the AI's explanations).
    let keep = |d: &crate::model::DocRef, spec: &crate::model::RequestSpec, resp: &crate::net::ResponseData| {
        if let Some((_, m)) = doc.lookup(d) {
            let url = crate::model::prepare(spec, Some(m)).url;
            crate::learned::record_shared(learned, d, m, "GET", &url, resp);
        }
    };
    // The map so far, at most every LIVE_EVERY (building it costs a copy of everything seen).
    let mut last_live = std::time::Instant::now();
    let mut live = |b: &Builder| {
        (last_live.elapsed() >= LIVE_EVERY).then(|| {
            last_live = std::time::Instant::now();
            b.snapshot()
        })
    };
    // The documented GETs, with what each one needs.
    let mut pending: Vec<Pending> = Vec::new();
    if all {
        for res in &doc.resources {
            for m in &res.methods {
                for (i, r) in m.routes.iter().enumerate() {
                    if !r.verb.eq_ignore_ascii_case("GET") || r.deprecated || pending.iter().any(|p| p.template == r.path) {
                        continue;
                    }
                    let mut needs = Vec::new();
                    for v in crate::model::path_vars(&r.path).into_iter().filter(|v| !v.optional) {
                        needs.push((v.name.clone(), true, Fill::Id(param_entity(doc, &v.name, &res.key).unwrap_or_else(|| v.name.clone()))));
                    }
                    for p in crate::model::field_params(m, &r.path).into_iter().filter(|p| p.required && !p.is_group()) {
                        needs.push((p.full_name.clone(), false, fill_for(doc, p, &res.key)));
                    }
                    pending.push(Pending { doc: crate::model::DocRef { resource: res.key.clone(), method: m.name.clone(), route: i }, template: r.path.clone(), needs });
                }
            }
        }
    }
    let total = requests.len() + pending.len();
    let mut done = 0usize;
    let mut first = true;
    let mut pause = |stop: &AtomicBool| {
        if !first {
            std::thread::sleep(delay);
        }
        first = false;
        !stop.load(Ordering::Relaxed)
    };
    for (name, r) in requests {
        if !pause(stop) {
            break;
        }
        progress(done, total, name, live(&b));
        done += 1;
        let path = r.spec.path.trim();
        // Already covered: the documented route of a saved request isn't sent again.
        if let Some(d) = doc.match_route("GET", path) {
            if let Some((_, m)) = doc.lookup(&d) {
                let template = &m.routes[d.route].path;
                if let Some(at) = pending.iter().position(|p| &p.template == template) {
                    pending.remove(at);
                }
            }
        }
        match sender.send(&r.spec) {
            Ok(resp) if resp.status < 400 => match serde_json::from_str::<Value>(&resp.body) {
                Ok(v) => {
                    if let Some(d) = r.spec.doc.clone().or_else(|| doc.match_route("GET", path)) {
                        keep(&d, &r.spec, &resp);
                    }
                    b.observe(name, path, Some(resp.status), &v)
                }
                Err(_) => b.skipped(name, path, Some(resp.status), "not JSON".into()),
            },
            Ok(resp) => b.skipped(name, path, Some(resp.status), format!("HTTP {}", resp.status)),
            Err(e) => b.skipped(name, path, None, crate::privacy::message(&e)),
        }
    }
    let total = done + pending.len();
    // The recorded examples first: their ids unlock endpoints too.
    let recorded: Vec<crate::learned::Learned> = learned.lock().map.values().cloned().collect();
    for l in recorded.iter().filter(|l| l.verb.eq_ignore_ascii_case("GET")) {
        if let Ok(v) = serde_json::from_str::<Value>(&l.response) {
            b.observe("recorded example", &l.url, Some(l.status), &v);
        }
    }
    // Rounds: what can be filled now is sent; new ids may unlock more.
    'rounds: loop {
        let mut unlocked = false;
        let mut i = 0;
        while i < pending.len() {
            let ready = pending[i].needs.iter().all(|(_, _, f)| match f {
                Fill::Id(entity) => !b.known_ids(entity).is_empty(),
                Fill::Value(_) => true,
            });
            if !ready {
                i += 1;
                continue;
            }
            let p = pending.remove(i);
            unlocked = true;
            let mut outcome = None;
            // Another id when one isn't found (a deleted user, a store without that category…).
            for attempt in 0..3 {
                let mut spec = crate::model::RequestSpec {
                    verb: "GET".into(),
                    path: p.template.clone(),
                    doc: Some(p.doc.clone()),
                    profile: profile.to_owned(),
                    ..Default::default()
                };
                let mut complete = true;
                for (k, (name, in_path, fill)) in p.needs.iter().enumerate() {
                    let id = match fill {
                        Fill::Value(v) => Some(v.clone()),
                        // The first id changes from one attempt to the next.
                        Fill::Id(entity) => {
                            let ids = b.known_ids(entity);
                            ids.get(if k == 0 { attempt } else { 0 }).or(ids.first()).cloned()
                        }
                    };
                    let Some(id) = id else {
                        complete = false;
                        break;
                    };
                    if *in_path {
                        spec.path_values.insert(name.clone(), id.clone());
                    } else {
                        spec.query.push(crate::model::Kv { enabled: true, key: name.clone(), value: id.clone() });
                    }
                }
                let first_ids = match p.needs.first() {
                    Some((_, _, Fill::Id(e))) => b.known_ids(e).len(),
                    _ => 1,
                };
                if !complete || (attempt > 0 && first_ids <= attempt) {
                    break;
                }
                if !pause(stop) {
                    break 'rounds;
                }
                progress(done, total, &format!("GET {}", p.template), live(&b));
                let result = sender.send(&spec);
                if let Ok(resp) = &result {
                    if resp.status < 400 {
                        keep(&p.doc, &spec, resp);
                    }
                }
                let retry = matches!(&result, Ok(r) if r.status == 404) && matches!(p.needs.first(), Some((_, _, Fill::Id(_))));
                outcome = Some(result);
                if !retry {
                    break;
                }
            }
            done += 1;
            let route = p.template.as_str();
            match outcome {
                Some(Ok(resp)) if resp.status < 400 => match serde_json::from_str::<Value>(&resp.body) {
                    Ok(v) => b.observe_route("documented GET", &p.doc, route, Some(resp.status), &v),
                    Err(_) => b.skipped("documented GET", route, Some(resp.status), "not JSON".into()),
                },
                Some(Ok(resp)) => b.skipped("documented GET", route, Some(resp.status), format!("HTTP {}", resp.status)),
                Some(Err(e)) => b.skipped("documented GET", route, None, crate::privacy::message(&e)),
                None => b.skipped("documented GET", route, None, "stopped".into()),
            }
        }
        if !unlocked || pending.is_empty() {
            break;
        }
    }
    // What no GET gave an id for.
    for p in pending {
        let missing: Vec<String> = p
            .needs
            .iter()
            .filter_map(|(n, _, f)| match f {
                Fill::Id(e) if b.known_ids(e).is_empty() => Some(format!("{n} (no {e} id in what the GETs returned)")),
                _ => None,
            })
            .collect();
        let why = if stop.load(Ordering::Relaxed) { "stopped before it".to_owned() } else { format!("needs {}", missing.join(", ")) };
        b.skipped("documented GET", &p.template, None, why);
    }
    progress(total, total, "done", None);
    let stopped = stop.load(Ordering::Relaxed);
    let mut map = b.finish(crate::storage::now());
    map.complete = !stopped;
    map
}

/// A GET the user sent (a request tab) whose endpoint the map hasn't read yet: what it returned is
/// added to `map` (entities, fields, links), no value kept. Returns whether the map changed.
pub fn absorb(map: &mut DataMap, doc: &ApiDoc, source: &str, path: &str, body: &Value) -> bool {
    let path_only = path.split('?').next().unwrap_or(path);
    let route = doc.match_route("GET", path_only);
    let mut b = Builder::new(doc);
    match &route {
        Some(d) => {
            let template = doc.lookup(d).map(|(_, m)| m.routes[d.route].path.clone()).unwrap_or_else(|| path_only.to_owned());
            b.observe_route(source, d, &template, Some(200), body);
        }
        None => b.observe(source, path_only, Some(200), body),
    }
    let small = b.finish(crate::storage::now());
    let Some(src) = small.sources.first() else { return false };
    if src.problem.is_some() || map.sources.iter().any(|s| s.route == src.route && s.problem.is_none()) {
        return false;
    }
    // A route read before without success: replaced by this answer.
    map.sources.retain(|s| s.route != src.route);
    map.sources.extend(small.sources);
    for (name, e) in small.entities {
        match map.entities.get_mut(&name) {
            Some(mine) => {
                merge_shape(&mut mine.full, &e.full);
                merge_shape(&mut mine.embedded, &e.embedded);
                mine.endpoints.extend(e.endpoints);
                mine.aliases.extend(e.aliases);
            }
            None => {
                map.entities.insert(name, e);
            }
        }
    }
    for l in small.links {
        match map.links.iter_mut().find(|m| m.from == l.from && m.field == l.field && m.to == l.to && m.kind == l.kind) {
            Some(m) => {
                m.seen += l.seen;
                m.ids += l.ids;
                m.confirmed += l.confirmed;
            }
            None => map.links.push(l),
        }
    }
    true
}

/// How often the map so far is handed out while Understand runs.
const LIVE_EVERY: Duration = Duration::from_millis(1500);

/// Saves the map (data folder) and its Markdown next to the workspace (for agents).
pub fn save(map: &DataMap) {
    let (file, agent) = files(&map.profile);
    crate::storage::save(&file, map);
    let _ = std::fs::write(crate::workspace_fs::dir().join(agent), markdown(map));
}

/// The map's file and its Markdown's: `datamap.json` / `DATA_MAP.md`, or per profile
/// `datamap.<id>.json` / `DATA_MAP.<id>.md`.
pub fn files(profile: &str) -> (String, String) {
    if profile.is_empty() {
        (FILE.to_owned(), AGENT_FILE.to_owned())
    } else {
        (format!("datamap.{profile}.json"), format!("DATA_MAP.{profile}.md"))
    }
}

/// The active profile's map.
pub fn load() -> Option<DataMap> {
    load_for(&crate::profiles::active_id())
}

pub fn load_for(profile: &str) -> Option<DataMap> {
    crate::storage::load_checked::<DataMap>(&files(profile).0).ok().flatten()
}

/// Where the values of a field come from, as the data shows it: `category_id` → the `categories` entity and
/// the GET listing it. From the map's links: a reference (`category_id`, `category_ids`) names its field,
/// an embedded object (`category`) stands for its `_id` too; when two entities claim a name, the link
/// seen most wins.
pub type Relations = HashMap<String, (String, crate::model::DocRef)>;

pub fn relations(map: &DataMap, doc: &ApiDoc) -> Relations {
    // name → (references first, times seen, entity)
    let mut best: HashMap<String, (bool, u64, String)> = HashMap::new();
    let mut claim = |name: String, reference: bool, seen: u64, to: &str| {
        let e = best.entry(name).or_insert((reference, seen, to.to_owned()));
        if (reference, seen) > (e.0, e.1) {
            *e = (reference, seen, to.to_owned());
        }
    };
    for l in &map.links {
        let last = l.field.rsplit('.').next().unwrap_or(&l.field).trim_end_matches("[]");
        match l.kind {
            LinkKind::Reference => {
                claim(last.to_owned(), true, l.seen, &l.to);
                if let Some(base) = last.strip_suffix("_ids") {
                    claim(format!("{base}_id"), true, l.seen, &l.to);
                }
            }
            LinkKind::Embedded => claim(format!("{last}_id"), false, l.seen, &l.to),
            LinkKind::EmbeddedList => claim(format!("{last}_ids"), false, l.seen, &l.to),
        }
    }
    best.into_iter().filter_map(|(name, (_, _, to))| Some((name, (to.clone(), entity_get(map, doc, &to)?)))).collect()
}

/// The GET that lists `entity` best: one of its own endpoints, with the fewest `:variables`, then the shortest.
pub fn entity_get(map: &DataMap, doc: &ApiDoc, entity: &str) -> Option<crate::model::DocRef> {
    let e = map.entities.get(entity)?;
    e.endpoints.iter().filter_map(|ep| ep.strip_prefix("GET ").map(str::trim)).min_by_key(|p| (p.matches(':').count(), p.len())).and_then(|p| doc.match_route("GET", p))
}

/// The entity a documented route returns, when the map knows it.
pub fn returned_by<'a>(map: &'a DataMap, route: &str) -> Option<&'a Entity> {
    let key = format!("GET {}", route.trim());
    map.entities.values().find(|e| e.endpoints.contains(&key))
}

/// What the docs can say of an entity's links: (what it uses, what uses it), as `(entity, field, kind)`.
pub fn neighbours<'a>(map: &'a DataMap, entity: &str) -> (Vec<&'a Link>, Vec<&'a Link>) {
    let uses = map.links.iter().filter(|l| l.from == entity && l.to != entity).collect();
    let used = map.links.iter().filter(|l| l.to == entity && l.from != entity).collect();
    (uses, used)
}

/// One link in words, from `entity`'s side: `categories (field category_id, an id; 12/12 ids confirmed)`.
pub fn link_words(l: &Link, entity: &str) -> String {
    let other = if l.from == entity { &l.to } else { &l.from };
    let how = match l.kind {
        LinkKind::Embedded => "embedded",
        LinkKind::EmbeddedList => "a list, embedded",
        LinkKind::Reference => "its id",
    };
    let ok = if l.ids > 0 { format!("; {}/{} ids confirmed", l.confirmed, l.ids) } else { String::new() };
    format!("{other} (`{}`, {how}{ok})", l.field)
}

/// The dependency layers as Markdown lines (only entities a GET returns).
pub fn layers_md(map: &DataMap) -> String {
    let lay = layers(map);
    let mut s = String::new();
    if !lay.layers.is_empty() {
        s.push_str("Layer 0 is what nothing else points to; each entity sits above what it uses (what the others depend on is at the bottom).\n\n");
        for (i, l) in lay.layers.iter().enumerate() {
            s.push_str(&format!("- layer {i}: {}\n", l.iter().map(|e| format!("`{e}`")).collect::<Vec<_>>().join(", ")));
        }
    }
    s
}

/// What a map is exported as.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Export {
    /// The map itself: loaded again with `parse` (another computer, an older map kept aside).
    Json,
    Markdown,
    /// A page of its own (the generated documentation's look).
    Html,
}

impl Export {
    pub const ALL: [Export; 3] = [Export::Json, Export::Markdown, Export::Html];

    pub fn ext(self) -> &'static str {
        match self {
            Export::Json => "json",
            Export::Markdown => "md",
            Export::Html => "html",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Export::Json => "Data map (.json, can be loaded again)",
            Export::Markdown => "Markdown (.md)",
            Export::Html => "HTML page (.html)",
        }
    }

    /// From `json` / `md` / `markdown` / `html`, or a file's extension.
    pub fn from_name(name: &str) -> Option<Export> {
        match name.trim().trim_start_matches('.').to_lowercase().as_str() {
            "json" => Some(Export::Json),
            "md" | "markdown" => Some(Export::Markdown),
            "html" | "htm" => Some(Export::Html),
            _ => None,
        }
    }
}

/// The map as `format` (no value in any of them: it holds none).
pub fn render(map: &DataMap, format: Export) -> String {
    match format {
        Export::Json => serde_json::to_string_pretty(map).unwrap_or_default(),
        Export::Markdown => markdown(map),
        Export::Html => crate::docgen::data_map_page(map),
    }
}

/// A map exported as JSON.
pub fn parse(text: &str) -> Result<DataMap, String> {
    let map: DataMap = serde_json::from_str(text).map_err(|e| format!("not a data map ({e})"))?;
    if map.entities.is_empty() && map.sources.is_empty() {
        return Err("this data map is empty".into());
    }
    Ok(map)
}

/// Makes `map` the current one (the Data map tab, `DATA_MAP.md`, the `data_map` tool).
pub fn replace(map: &DataMap) {
    save(map);
}

// ---------------------------------------------------------------- showing it: a tree

/// The shape to show for an entity: as its own endpoints return it, else as others embed it.
pub fn shape_of<'a>(map: &'a DataMap, entity: &str) -> Option<&'a Shape> {
    let e = map.entities.get(entity)?;
    Some(if e.full.objects > 0 { &e.full } else { &e.embedded })
}

/// What opens under a field of the tree.
pub enum Open<'a> {
    Nothing,
    /// Its own fields (an object, a list of objects), still part of the same entity (`prefix`).
    Fields(&'a Shape, String),
    /// The entity it links to (its fields are that entity's).
    Entity(String),
}

/// One field of the tree: its name (`order_items[]`), type, presence, the link it is, what opens under it.
pub struct Item<'a> {
    pub name: String,
    pub desc: String,
    /// % of the objects that have it.
    pub presence: u64,
    pub link: Option<&'a Link>,
    pub open: Open<'a>,
}

/// The fields of `shape` (an object of `entity`, at `prefix` in it) as tree items, links first.
pub fn level<'a>(map: &'a DataMap, entity: &str, shape: &'a Shape, prefix: &str) -> Vec<Item<'a>> {
    let link_at = |field: &str| map.links.iter().find(|l| l.from == entity && l.field == field);
    let mut items: Vec<Item> = shape
        .fields
        .iter()
        .map(|(k, f)| {
            let presence = (f.seen * 100).checked_div(shape.objects).map_or(100, |p| p.min(100));
            let mut desc = f.describe();
            if let Some(n) = f.distinct {
                desc.push_str(&format!(", {n} distinct values"));
            }
            let items_shape = f.array.as_ref().map(|a| &a.items).filter(|i| i.objects > 0 && !i.fields.is_empty());
            let name = if items_shape.is_some() || (f.array.is_some() && link_at(&format!("{prefix}{k}[]")).is_some()) { format!("{k}[]") } else { k.clone() };
            let link = link_at(&format!("{prefix}{name}")).or_else(|| link_at(&format!("{prefix}{k}")));
            let open = match (link, items_shape) {
                (Some(l), _) => Open::Entity(l.to.clone()),
                (None, Some(items)) => Open::Fields(items, format!("{prefix}{k}[].")),
                (None, None) if f.objects > 0 && !f.fields.is_empty() => Open::Fields(f, format!("{prefix}{k}.")),
                _ => Open::Nothing,
            };
            Item { name, desc, presence, link, open }
        })
        .collect();
    // Links first (where to go next), then the rest by name.
    items.sort_by_key(|i| (i.link.is_none(), i.name.clone()));
    items
}

/// `→ users (embeds, 3 of 3 ids confirmed)`.
pub fn link_text(l: &Link) -> String {
    let confirmed = if l.ids == 0 { String::new() } else { format!(", {} of {} ids confirmed", l.confirmed, l.ids) };
    format!("→ {} ({}{confirmed})", l.to, l.kind.label())
}

/// The tree of an entity as text (`├─ └─`): its fields, nested objects open, linked entities open
/// `depth` levels (then "see …"), never back into one already on the way.
pub fn tree_text(map: &DataMap, entity: &str, depth: usize) -> String {
    #[allow(clippy::too_many_arguments)]
    fn walk(map: &DataMap, entity: &str, shape: &Shape, prefix: &str, indent: &str, depth: usize, path: &mut Vec<String>, out: &mut String) {
        let items = level(map, entity, shape, prefix);
        let n = items.len();
        for (i, it) in items.into_iter().enumerate() {
            let last = i + 1 == n;
            let (branch, next) = if last { ("└─ ", "   ") } else { ("├─ ", "│  ") };
            let presence = if it.presence < 100 { format!(", in {}%", it.presence) } else { String::new() };
            let link = it.link.map(|l| format!("  {}", link_text(l))).unwrap_or_default();
            out.push_str(&format!("{indent}{branch}{}  {}{presence}{link}\n", it.name, it.desc));
            let inner = format!("{indent}{next}");
            match it.open {
                Open::Fields(s, p) => walk(map, entity, s, &p, &inner, depth, path, out),
                Open::Entity(to) if path.contains(&to) => out.push_str(&format!("{inner}└─ ↻ {to} (above)\n")),
                Open::Entity(to) if depth == 0 => out.push_str(&format!("{inner}└─ … (fields: see {to})\n")),
                Open::Entity(to) => {
                    let embedded = map.entities.get(&to).map(|e| &e.embedded).filter(|s| s.objects > 0);
                    if let Some(s) = embedded.or_else(|| shape_of(map, &to)).filter(|s| !s.fields.is_empty()) {
                        path.push(to.clone());
                        walk(map, &to, s, "", &inner, depth - 1, path, out);
                        path.pop();
                    }
                }
                Open::Nothing => {}
            }
        }
    }
    let mut out = String::new();
    if let Some(shape) = shape_of(map, entity) {
        out.push_str(&format!("{entity}\n"));
        walk(map, entity, shape, "", "", depth, &mut vec![entity.to_owned()], &mut out);
    }
    out
}

/// The map in Markdown: for each entity, where it comes from and its tree; then the links.
pub fn markdown(map: &DataMap) -> String {
    let mut s = String::from("# Data map\n\n");
    let when = chrono::DateTime::from_timestamp(map.built_at as i64, 0).map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string()).unwrap_or_default();
    let read = map.sources.iter().filter(|x| x.problem.is_none()).count();
    s.push_str(&format!(
        "Built {when} by **Understand** from {read} GET responses (the saved GET requests, every documented GET it could fill, \
         examples recorded from real GETs; nothing but GETs). \
         Only the structure is kept: types, formats, how often a field is there, how the data links. No value.{}\n\n\
         Each tree shows an entity's fields; a field that links to another entity (`→`) opens on that entity's fields \
         (as it is embedded there), one level deep.\n",
        if map.complete { "" } else { "\n\n**Partial**: Understand was still running, or was stopped, when this was written." }
    ));
    let layers = layers_md(map);
    if !layers.is_empty() {
        s.push_str(&format!("\n## How the data depends on itself\n\n{layers}"));
    }
    let (own, embedded): (Vec<&Entity>, Vec<&Entity>) = map.entities.values().partition(|e| !e.endpoints.is_empty());
    for (title, list) in [("Entities returned by a GET", own), ("Entities only seen inside others", embedded)] {
        if list.is_empty() {
            continue;
        }
        s.push_str(&format!("\n## {title}\n"));
        for e in list {
            s.push_str(&format!("\n<a id=\"entity-{0}\"></a>\n### {0}\n\n", e.name));
            let (uses, used) = neighbours(map, &e.name);
            if !uses.is_empty() {
                s.push_str(&format!("Uses: {}\n\n", uses.iter().map(|l| link_words(l, &e.name)).collect::<Vec<_>>().join(", ")));
            }
            if !used.is_empty() {
                s.push_str(&format!("Used by: {}\n\n", used.iter().map(|l| link_words(l, &e.name)).collect::<Vec<_>>().join(", ")));
            }
            if !e.endpoints.is_empty() {
                s.push_str(&format!("Returned by: {}\n\n", e.endpoints.iter().map(|x| format!("`{x}`")).collect::<Vec<_>>().join(", ")));
            }
            if !e.aliases.is_empty() {
                s.push_str(&format!("Also found as: {}\n\n", e.aliases.iter().map(|a| format!("`{a}`")).collect::<Vec<_>>().join(", ")));
            }
            if !e.documented {
                s.push_str("Not a resource of the API docs: named after the field it was found in.\n\n");
            }
            let tree = tree_text(map, &e.name, 1);
            if !tree.is_empty() {
                s.push_str(&format!("```text\n{tree}```\n\n"));
            }
            // The same entity embedded elsewhere, with fewer fields.
            if !e.full.fields.is_empty() && !e.embedded.fields.is_empty() {
                let missing: Vec<&String> = e.full.fields.keys().filter(|k| !e.embedded.fields.contains_key(*k)).collect();
                s.push_str(&format!(
                    "Embedded in another entity, it has {} of its {} fields{}.\n",
                    e.embedded.fields.keys().filter(|k| e.full.fields.contains_key(*k)).count(),
                    e.full.fields.len(),
                    if missing.is_empty() { String::new() } else { format!(" (not: {})", missing.iter().map(|m| format!("`{m}`")).collect::<Vec<_>>().join(", ")) }
                ));
            }
        }
    }
    s.push_str("\n## Links\n\n| From | Field | Link | To | Seen | Confirmed ids |\n|---|---|---|---|---|---|\n");
    for l in &map.links {
        let confirmed = if l.ids == 0 { "—".to_owned() } else { format!("{} of {}", l.confirmed, l.ids) };
        s.push_str(&format!("| {} | `{}` | {} | {} | {} | {confirmed} |\n", l.from, l.field, l.kind.label(), l.to, l.seen));
    }
    s.push_str("\n*Confirmed ids*: ids found in that field that are also ids returned by the target's own endpoints.\n");
    let problems: Vec<&Source> = map.sources.iter().filter(|x| x.problem.is_some()).collect();
    if !problems.is_empty() {
        s.push_str("\n## Not read\n\n");
        for p in problems {
            s.push_str(&format!("- {} (`{}`): {}\n", p.name, p.route, p.problem.as_deref().unwrap_or_default()));
        }
    }
    s
}

/// What the `data_map` tool gives: without `entity` / `field`, the overview (every entity on one line,
/// its links, the dependency layers); `entity` = one entity's tree two levels deep and its links
/// (`all`: the whole map); `field` = where fields of that name are.
pub fn markdown_for(map: &DataMap, entity: &str, field: &str) -> String {
    let want = entity.trim();
    if !field.trim().is_empty() {
        return find_field(map, field.trim());
    }
    if want.is_empty() {
        return overview(map);
    }
    if want == "all" || want == "*" {
        return markdown(map);
    }
    let Some(e) = map.entities.get(want).or_else(|| map.entities.values().find(|e| e.aliases.contains(want))) else {
        return format!("No entity “{want}” in the map. Entities: {}", map.entities.keys().cloned().collect::<Vec<_>>().join(", "));
    };
    let mut s = format!("# {}\n\n", e.name);
    if !e.endpoints.is_empty() {
        s.push_str(&format!("Returned by: {}\n\n", e.endpoints.iter().cloned().collect::<Vec<_>>().join(", ")));
    }
    s.push_str(&format!("```text\n{}```\n\nLinked from: ", tree_text(map, &e.name, 2)));
    let from: Vec<String> = map.links.iter().filter(|l| l.to == e.name).map(|l| format!("{}.{} ({})", l.from, l.field, l.kind.label())).collect();
    s.push_str(&if from.is_empty() { "nothing".to_owned() } else { from.join(", ") });
    s.push('\n');
    s
}

/// The map in a few lines per entity: what returns it, what it links to and what links to it, and the
/// layers of dependency (layer 0: what nothing else points to; each entity above what it uses).
pub fn overview(map: &DataMap) -> String {
    let when = chrono::DateTime::from_timestamp(map.built_at as i64, 0).map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string()).unwrap_or_default();
    let read = map.sources.iter().filter(|x| x.problem.is_none()).count();
    let not_read = map.sources.len() - read;
    let mut s = format!(
        "# Data map (overview)\n\nBuilt {when} from {read} GET responses ({not_read} not read){}. Structure only, no value. \
         {} entities, {} links.\nMore: data_map with `entity` (its fields as a tree), `field` (where a field is), `entity`=\"all\" (everything).\n",
        if map.complete { "" } else { ", **partial**" },
        map.entities.len(),
        map.links.len()
    );
    let lay = layers(map);
    if !lay.layers.is_empty() {
        s.push_str("\n## Dependency layers (only entities a GET returns)\n\nLayer 0 is what nothing points to; each entity sits above what it uses.\n\n");
        for (i, l) in lay.layers.iter().enumerate() {
            s.push_str(&format!("- layer {i}: {}\n", l.join(", ")));
        }
    }
    let line = |l: &Link, other: &str| {
        let ok = if l.ids > 0 { format!(", {}/{} ids confirmed", l.confirmed, l.ids) } else { String::new() };
        format!("{other} (`{}`, {}{ok})", l.field, l.kind.label())
    };
    let (own, embedded): (Vec<&Entity>, Vec<&Entity>) = map.entities.values().partition(|e| !e.endpoints.is_empty());
    for (title, list) in [("Entities returned by a GET", own), ("Entities only seen inside others", embedded)] {
        if list.is_empty() {
            continue;
        }
        s.push_str(&format!("\n## {title}\n\n"));
        for e in list {
            let fields = shape_of(map, &e.name).map_or(0, |sh| sh.fields.len());
            let eps: Vec<&String> = e.endpoints.iter().collect();
            let mut row = format!("- **{}** — {fields} fields", e.name);
            if !eps.is_empty() {
                let shown: Vec<String> = eps.iter().take(3).map(|x| format!("`{x}`")).collect();
                row.push_str(&format!(" — {}{}", shown.join(", "), if eps.len() > 3 { format!(" (+{})", eps.len() - 3) } else { String::new() }));
            }
            let uses: Vec<String> = map.links.iter().filter(|l| l.from == e.name && l.to != e.name).map(|l| line(l, &l.to)).collect();
            let used: Vec<String> = map.links.iter().filter(|l| l.to == e.name && l.from != e.name).map(|l| line(l, &l.from)).collect();
            if !uses.is_empty() {
                row.push_str(&format!("\n  - uses: {}", uses.join("; ")));
            }
            if !used.is_empty() {
                row.push_str(&format!("\n  - used by: {}", used.join("; ")));
            }
            s.push_str(&row);
            s.push('\n');
        }
    }
    s
}

/// Where fields whose name contains `needle` are: `entity: path (type)`, a few levels deep.
pub fn find_field(map: &DataMap, needle: &str) -> String {
    fn walk(shape: &Shape, path: &str, needle: &str, depth: usize, out: &mut Vec<String>, entity: &str) {
        for (k, f) in &shape.fields {
            let p = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
            if k.to_lowercase().contains(needle) {
                out.push(format!("{entity}: `{p}`"));
            }
            if depth < 3 {
                walk(f, &p, needle, depth + 1, out, entity);
                if let Some(a) = &f.array {
                    walk(&a.items, &format!("{p}[]"), needle, depth + 1, out, entity);
                }
            }
        }
    }
    let needle = needle.to_lowercase();
    let mut out = Vec::new();
    for e in map.entities.values() {
        if let Some(shape) = shape_of(map, &e.name) {
            walk(shape, "", &needle, 0, &mut out, &e.name);
        }
    }
    if out.is_empty() {
        return format!("No field “{needle}” in the map.");
    }
    let more = out.len().saturating_sub(120);
    out.truncate(120);
    format!("Fields matching “{needle}” (entity: path):\n{}{}", out.join("\n"), if more > 0 { format!("\n… {more} more") } else { String::new() })
}

/// The map as a part of a bigger Markdown document (its headings one level down).
pub fn markdown_section(map: &DataMap) -> String {
    let mut in_code = false;
    markdown(map)
        .lines()
        .map(|l| {
            if l.starts_with("```") {
                in_code = !in_code;
            }
            if !in_code && l.starts_with('#') {
                format!("#{l}")
            } else {
                l.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The map as HTML (generated docs): each entity as a tree that opens (`<details>`), then the links.
pub fn html_section(map: &DataMap) -> String {
    fn esc(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
    }
    fn walk(map: &DataMap, entity: &str, shape: &Shape, prefix: &str, depth: usize, path: &mut Vec<String>, out: &mut String) {
        out.push_str("<ul class=\"tree\">");
        for it in level(map, entity, shape, prefix) {
            let presence = if it.presence < 100 { format!(" <span class=\"opt\">in {}%</span>", it.presence) } else { String::new() };
            let link = it.link.map(|l| format!(" <a class=\"to\" href=\"#entity-{}\">{}</a>", esc(&l.to), esc(&link_text(l)))).unwrap_or_default();
            let head = format!("<code>{}</code> <span class=\"ty\">{}</span>{presence}{link}", esc(&it.name), esc(&it.desc));
            let mut inner = String::new();
            match it.open {
                Open::Fields(s, p) => walk(map, entity, s, &p, depth, path, &mut inner),
                Open::Entity(to) if !path.contains(&to) && depth > 0 => {
                    let embedded = map.entities.get(&to).map(|e| &e.embedded).filter(|s| s.objects > 0);
                    if let Some(s) = embedded.or_else(|| shape_of(map, &to)).filter(|s| !s.fields.is_empty()) {
                        path.push(to.clone());
                        walk(map, &to, s, "", depth - 1, path, &mut inner);
                        path.pop();
                    }
                }
                _ => {}
            }
            if inner.is_empty() {
                out.push_str(&format!("<li>{head}</li>"));
            } else {
                out.push_str(&format!("<li><details><summary>{head}</summary>{inner}</details></li>"));
            }
        }
        out.push_str("</ul>");
    }
    let read = map.sources.iter().filter(|x| x.problem.is_none()).count();
    let mut s = format!(
        "<section class=\"entry\" id=\"data-map\"><style>\
         ul.tree{{list-style:none;margin:2px 0 2px 6px;padding-left:14px;border-left:1px solid #d5dbdb}}\
         ul.tree li{{margin:2px 0}} ul.tree summary{{cursor:pointer}} .ty{{color:#5d6d7e}} .opt{{color:#e67e22;font-size:12px}}\
         a.to{{color:#00babc;text-decoration:none;font-size:13px}}</style>\
         <h2>Data map</h2><p class=\"note\">Built from {read} GET responses (saved GET requests and examples recorded from real GETs). \
         Only the structure: types, formats, presence, links. No value. A field that links to another entity opens on its fields.</p>"
    );
    let entity_link = |e: &str| format!("<a class=\"to\" href=\"#entity-{0}\">{0}</a>", esc(e));
    let lay = layers(map);
    if !lay.layers.is_empty() {
        s.push_str(
            "<h3>How the data depends on itself</h3><p class=\"note\">Layer 0 is what nothing else points to; each entity sits above what it uses \
                    (what the others depend on is at the bottom). Only entities a GET returns.</p><ul>",
        );
        for (i, l) in lay.layers.iter().enumerate() {
            s.push_str(&format!("<li>layer {i}: {}</li>", l.iter().map(|e| entity_link(e)).collect::<Vec<_>>().join(", ")));
        }
        s.push_str("</ul>");
    }
    let words = |links: &[&Link], me: &str| {
        links
            .iter()
            .map(|l| {
                let other = if l.from == me { &l.to } else { &l.from };
                esc(&link_words(l, me)).replacen(&esc(other), &entity_link(other), 1)
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    for e in map.entities.values() {
        let Some(shape) = shape_of(map, &e.name) else { continue };
        s.push_str(&format!("<h3 id=\"entity-{0}\">{0}</h3>", esc(&e.name)));
        let (uses, used) = neighbours(map, &e.name);
        if !uses.is_empty() {
            s.push_str(&format!("<p><b>Uses</b>: {}</p>", words(&uses, &e.name)));
        }
        if !used.is_empty() {
            s.push_str(&format!("<p><b>Used by</b>: {}</p>", words(&used, &e.name)));
        }
        if !e.endpoints.is_empty() {
            s.push_str(&format!("<p>Returned by: {}</p>", e.endpoints.iter().map(|x| format!("<code>{}</code>", esc(x))).collect::<Vec<_>>().join(", ")));
        } else {
            s.push_str("<p>Only seen inside other entities.</p>");
        }
        if !e.aliases.is_empty() {
            s.push_str(&format!("<p>Also found as: {}</p>", e.aliases.iter().map(|x| format!("<code>{}</code>", esc(x))).collect::<Vec<_>>().join(", ")));
        }
        walk(map, &e.name, shape, "", 2, &mut vec![e.name.clone()], &mut s);
    }
    s.push_str("<h3>Links</h3><table><tr><th>From</th><th>Field</th><th>Link</th><th>To</th><th>Seen</th><th>Confirmed ids</th></tr>");
    for l in &map.links {
        let confirmed = if l.ids == 0 { "—".to_owned() } else { format!("{} of {}", l.confirmed, l.ids) };
        s.push_str(&format!(
            "<tr><td><a href=\"#entity-{0}\">{0}</a></td><td><code>{1}</code></td><td>{2}</td><td><a href=\"#entity-{3}\">{3}</a></td><td>{4}</td><td>{confirmed}</td></tr>",
            esc(&l.from),
            esc(&l.field),
            l.kind.label(),
            esc(&l.to),
            l.seen
        ));
    }
    s.push_str("</table></section>");
    s
}

// ---------------------------------------------------------------- showing it: layers

/// The entities a GET returned, in layers of dependency: layer 0 is what nothing else points to;
/// an entity sits below everything that embeds or refers to it, so what all the rest depends on
/// (`users`, `categories`, `stores`) ends at the bottom.
pub struct Layered {
    /// Entity names per layer, ordered to cross few links.
    pub layers: Vec<Vec<String>>,
    /// Links between two of them (`back`: against the layers, a cycle's closing link, drawn upward).
    pub edges: Vec<LayerEdge>,
}

pub struct LayerEdge {
    pub from: String,
    pub field: String,
    pub to: String,
    pub kind: LinkKind,
    pub confirmed: bool,
    pub back: bool,
}

pub fn layers(map: &DataMap) -> Layered {
    // Only what a GET returned (what we have).
    let nodes: Vec<&str> = map.entities.values().filter(|e| !e.endpoints.is_empty()).map(|e| e.name.as_str()).collect();
    let index: HashMap<&str, usize> = nodes.iter().enumerate().map(|(i, n)| (*n, i)).collect();
    // One edge per (from, to); the fields are on the nodes.
    let mut out: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); nodes.len()];
    for l in &map.links {
        if let (Some(&a), Some(&b)) = (index.get(l.from.as_str()), index.get(l.to.as_str())) {
            if a != b {
                out[a].insert(b);
            }
        }
    }
    // Cycles (users ↔ teams): a depth-first walk marks the links that close one; they don't
    // count for the layers.
    let mut back: HashSet<(usize, usize)> = HashSet::new();
    let mut state = vec![0u8; nodes.len()];
    fn dfs(n: usize, out: &[BTreeSet<usize>], state: &mut [u8], back: &mut HashSet<(usize, usize)>) {
        state[n] = 1;
        for &m in &out[n] {
            match state[m] {
                0 => dfs(m, out, state, back),
                1 => {
                    back.insert((n, m));
                }
                _ => {}
            }
        }
        state[n] = 2;
    }
    // From what nothing points to first, so the cycles break at their lowest link.
    let pointed: HashSet<usize> = out.iter().flatten().copied().collect();
    let starts: Vec<usize> = (0..nodes.len()).filter(|n| !pointed.contains(n)).chain(0..nodes.len()).collect();
    for n in starts {
        if state[n] == 0 {
            dfs(n, &out, &mut state, &mut back);
        }
    }
    // Layer: the longest way down from a root.
    let mut layer = vec![0usize; nodes.len()];
    let mut changed = true;
    let mut rounds = 0;
    while changed && rounds <= nodes.len() {
        changed = false;
        rounds += 1;
        for a in 0..nodes.len() {
            for &b in &out[a] {
                if !back.contains(&(a, b)) && layer[b] < layer[a] + 1 {
                    layer[b] = layer[a] + 1;
                    changed = true;
                }
            }
        }
    }
    let depth = layer.iter().copied().max().map_or(0, |d| d + 1);
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); depth];
    for (n, &l) in layer.iter().enumerate() {
        layers[l].push(n);
    }
    // Fewer crossings: each node goes to the average position of its neighbours in the layer above, then
    // below, a few times.
    let pos_of = |layers: &[Vec<usize>]| {
        let mut p = vec![0f32; nodes.len()];
        for l in layers {
            for (i, &n) in l.iter().enumerate() {
                p[n] = i as f32 / l.len().max(1) as f32;
            }
        }
        p
    };
    let parents: Vec<Vec<usize>> = (0..nodes.len()).map(|b| (0..nodes.len()).filter(|&a| out[a].contains(&b)).collect()).collect();
    for sweep in 0..6 {
        let p = pos_of(&layers);
        for l in layers.iter_mut() {
            let key = |n: &usize| {
                let near: Vec<usize> = if sweep % 2 == 0 { parents[*n].clone() } else { out[*n].iter().copied().collect() };
                if near.is_empty() {
                    p[*n]
                } else {
                    near.iter().map(|m| p[*m]).sum::<f32>() / near.len() as f32
                }
            };
            l.sort_by(|a, b| key(a).partial_cmp(&key(b)).unwrap_or(std::cmp::Ordering::Equal).then(nodes[*a].cmp(nodes[*b])));
        }
    }
    let edges = map
        .links
        .iter()
        .filter_map(|l| {
            let (a, b) = (*index.get(l.from.as_str())?, *index.get(l.to.as_str())?);
            (a != b).then(|| LayerEdge {
                from: l.from.clone(),
                field: l.field.clone(),
                to: l.to.clone(),
                kind: l.kind.clone(),
                confirmed: l.confirmed > 0,
                back: back.contains(&(a, b)) || layer[b] <= layer[a],
            })
        })
        .collect();
    Layered { layers: layers.into_iter().map(|l| l.into_iter().map(|n| nodes[n].to_owned()).collect()).collect(), edges }
}

/// JSON Schema of a shape (OpenAPI `components.schemas`): types, formats, nullable, required when always there.
pub fn json_schema(shape: &Shape) -> Value {
    let mut types: Vec<Value> = Vec::new();
    let mut schema = Map::new();
    if shape.integers > 0 {
        types.push("integer".into());
    } else if shape.numbers > 0 {
        types.push("number".into());
    }
    if shape.strings > 0 {
        types.push("string".into());
        let main = shape.formats.iter().filter(|(f, n)| f.as_str() != "empty" && **n * 2 >= shape.strings).max_by_key(|(_, n)| **n).map(|(f, _)| f.as_str());
        if let Some(f) = main.and_then(|f| match f {
            "date-time" | "date" | "email" | "uuid" => Some(f),
            "url" => Some("uri"),
            _ => None,
        }) {
            schema.insert("format".into(), f.into());
        }
    }
    if shape.booleans > 0 {
        types.push("boolean".into());
    }
    if shape.objects > 0 {
        types.push("object".into());
        let props: Map<String, Value> = shape.fields.iter().map(|(k, f)| (k.clone(), json_schema(f))).collect();
        let required: Vec<Value> = shape.fields.iter().filter(|(_, f)| f.seen == shape.objects).map(|(k, _)| Value::from(k.as_str())).collect();
        schema.insert("properties".into(), Value::Object(props));
        if !required.is_empty() {
            schema.insert("required".into(), Value::Array(required));
        }
    }
    if let Some(a) = &shape.array {
        types.push("array".into());
        schema.insert("items".into(), json_schema(&a.items));
    }
    match types.len() {
        0 => {}
        1 => {
            schema.insert("type".into(), types.remove(0));
        }
        _ => {
            schema.insert("anyOf".into(), Value::Array(types.into_iter().map(|t| serde_json::json!({ "type": t })).collect()));
        }
    }
    if shape.nulls > 0 {
        schema.insert("nullable".into(), true.into());
    }
    Value::Object(schema)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn doc() -> ApiDoc {
        ApiDoc::load().unwrap()
    }

    #[test]
    fn entities_fields_and_links_from_responses() {
        let doc = doc();
        let mut b = Builder::new(&doc);
        let users = json!([
            { "id": 1, "username": "jdoe", "email": "jdoe@example.com", "created_at": "2024-09-01T08:00:00Z", "location": null, "staff?": false },
            { "id": 2, "username": "asmith", "email": "asmith@example.com", "created_at": "2024-09-01T08:00:00Z", "location": "aisle-4", "staff?": true }
        ]);
        b.observe("Users", "/v1/stores/12/users", Some(200), &users);
        let orders = json!([
            { "id": 10, "status": "shipped", "total": 100, "user": { "id": 1, "username": "jdoe" }, "product": { "id": 5, "name": "Watering can" },
              "category_ids": [21], "teams": [ { "id": 7, "users": [ { "id": 1, "username": "jdoe" } ] } ] },
            { "id": 11, "status": "pending", "total": null, "user": { "id": 3, "username": "x" }, "product": { "id": 6, "name": "Rake" }, "category_ids": [21], "teams": [] }
        ]);
        b.observe("Orders of a user", "/v1/users/1/orders", Some(200), &orders);
        // A path the docs don't have: kept, its values anonymized.
        b.observe("Odd", "/v1/nothing_documented/jdoe", Some(200), &json!({ "id": 1 }));
        b.skipped("Broken", "/v1/users/jdoe/nope", Some(404), "HTTP 404".into());
        b.observe("Reviews", "/v1/reviews", Some(200), &json!([ { "id": 3, "author": { "id": 2, "username": "asmith", "email": "x@y.z" } } ]));
        b.observe(
            "Products",
            "/v1/products",
            Some(200),
            &json!([ { "id": 5, "name": "Watering can", "slug": "watering-can", "parent_id": null, "children": [ { "id": 8, "name": "x", "slug": "x" } ] } ]),
        );
        b.observe("Categories", "/v1/categories", Some(200), &json!([ { "id": 21, "name": "Garden", "slug": "garden" } ]));
        b.observe("Stats", "/v1/users/stats", Some(200), &json!({ "2017-11-13": 4, "2017-11-14": 1 }));
        let map = b.finish(0);

        let u = &map.entities["users"];
        assert!(!u.full.fields.keys().any(|k| k.starts_with('{')), "statistics aren't users: {:?}", u.full.fields.keys());
        assert!(u.documented && u.endpoints.iter().any(|e| e.contains(":store_id/users")), "{:?}", u.endpoints);
        assert_eq!(u.full.fields["email"].describe(), "string (email)");
        assert_eq!(u.full.fields["created_at"].describe(), "string (date-time)");
        assert_eq!(u.full.fields["location"].describe(), "string | null");
        assert!(map.entities.contains_key("orders") && map.entities.contains_key("products"));
        assert!(map.entities["orders"].full.fields["total"].describe().contains("null"));

        let link =
            |from: &str, field: &str| map.links.iter().find(|l| l.from == from && l.field == field).unwrap_or_else(|| panic!("no {from}.{field}: {:#?}", map.links));
        let user = link("orders", "user");
        assert_eq!((user.to.as_str(), &user.kind), ("users", &LinkKind::Embedded));
        assert_eq!((user.ids, user.confirmed), (2, 1), "user 1 is a user seen on its own endpoint, user 3 isn't");
        assert_eq!(link("orders", "category_ids[]").kind, LinkKind::Reference);
        assert_eq!(link("orders", "category_ids[]").to, "categories");
        assert_eq!(link("teams", "users[]").to, "users", "nested deeper");
        // A product's children are products (not categories, though they share id / name / slug).
        assert_eq!(link("products", "children[]").to, "products");
        // `author` isn't a resource: its fields and ids are those of users.
        let author = link("reviews", "author");
        assert_eq!(author.to, "users");
        assert!(map.entities["users"].aliases.contains("author"));
        assert!(!map.entities.contains_key("author"));

        // Not one value in what is kept.
        let text = serde_json::to_string(&map).unwrap() + &markdown(&map) + &html_section(&map);
        for value in ["jdoe", "asmith", "example.com", "Watering", "shipped", "aisle-4", "2024-09"] {
            assert!(!text.contains(value), "{value} kept: …{}…", text.find(value).map(|i| &text[i.saturating_sub(120)..(i + 40).min(text.len())]).unwrap_or(""));
        }
        // The tree: links first, the linked entity's fields under them, nested objects open.
        let tree = tree_text(&map, "orders", 1);
        assert!(tree.contains("├─ product  object  → products (embeds"), "{tree}");
        assert!(tree.contains("├─ user  object  → users (embeds, 1 of 2 ids confirmed)"), "{tree}");
        assert!(tree.contains("│  ├─ id  integer\n│  └─ username  string"), "the embedded user's fields under it: {tree}");
        assert!(tree.trim_end().ends_with("└─ total  integer | null"), "links first, then the rest: {tree}");
        assert!(tree_text(&map, "products", 3).contains("↻ products (above)"), "no loop back: {}", tree_text(&map, "products", 3));
    }

    #[test]
    fn layers_of_dependency() {
        let doc = doc();
        let mut b = Builder::new(&doc);
        b.observe("Users", "/v1/users", Some(200), &json!([ { "id": 1, "teams": [ { "id": 7, "store_id": 21 } ] } ]));
        b.observe("Teams", "/v1/teams", Some(200), &json!([ { "id": 7, "users": [ { "id": 1 } ], "store": { "id": 21 } } ]));
        b.observe("Stores", "/v1/stores", Some(200), &json!([ { "id": 21 } ]));
        b.observe("Orders", "/v1/orders", Some(200), &json!([ { "id": 9, "user": { "id": 1 }, "items": [ { "id": 3 } ] } ]));
        let l = layers(&b.finish(0));
        let at = |name: &str| l.layers.iter().position(|layer| layer.iter().any(|n| n == name)).unwrap_or_else(|| panic!("{name} not shown: {:?}", l.layers));
        // Nothing points to orders: on top. users ↔ teams is a cycle: still layered.
        assert_eq!(at("orders"), 0, "{:?}", l.layers);
        assert!(at("users") > at("orders") && at("stores") > at("teams"), "{:?}", l.layers);
        // items has no GET of its own: not a node.
        assert!(!l.layers.iter().flatten().any(|n| n == "items"));
        assert!(l.edges.iter().all(|e| e.to != "items"));
        assert!(l.edges.iter().any(|e| e.back), "the cycle's closing link goes up");
    }

    #[test]
    fn overview_and_field_search_for_agents() {
        let doc = doc();
        let mut b = Builder::new(&doc);
        b.observe("Users", "/v1/users", Some(200), &json!([ { "id": 1, "username": "jdoe", "teams": [ { "id": 7, "rating": 4.2, "store_id": 21 } ] } ]));
        b.observe("Stores", "/v1/stores", Some(200), &json!([ { "id": 21, "name": "Downtown" } ]));
        let map = b.finish(0);
        let o = markdown_for(&map, "", "");
        assert!(o.contains("## Dependency layers") && o.contains("**users**") && o.contains("uses:"), "{o}");
        assert!(!o.contains("jdoe") && !o.contains("Downtown") && !o.contains("4.2"), "no value: {o}");
        let f = markdown_for(&map, "", "rating");
        assert!(f.contains("`teams[].rating`"), "{f}");
        assert!(markdown_for(&map, "all", "").contains("## Links"));
        assert!(markdown_for(&map, "users", "").starts_with("# users"));
    }

    #[test]
    fn a_get_sent_by_hand_grows_the_map() {
        let doc = doc();
        let mut b = Builder::new(&doc);
        b.observe("Users", "/v1/users", Some(200), &json!([ { "id": 1, "username": "jdoe" } ]));
        let mut map = b.finish(0);
        let categories = json!([ { "id": 21, "name": "Garden" } ]);
        assert!(absorb(&mut map, &doc, "a request tab", "/v1/categories?page[size]=5", &categories));
        assert!(map.entities["categories"].endpoints.contains("GET /v1/categories"));
        assert!(!absorb(&mut map, &doc, "a request tab", "/v1/categories", &categories), "already read");
        assert!(absorb(&mut map, &doc, "a request tab", "/v1/users/jdoe/orders", &json!([ { "id": 7, "category_id": 21, "user": { "id": 1 } } ])));
        assert!(map.links.iter().any(|l| l.from == "orders" && l.to == "categories"));
        assert!(!serde_json::to_string(&map).unwrap().contains("jdoe"), "no value, not even in the route");
        // The links of the forms follow it.
        let rel = relations(&map, &doc);
        let (to, d) = &rel["category_id"];
        assert_eq!(to, "categories");
        assert_eq!(doc.lookup(d).unwrap().1.routes[d.route].path, "/v1/categories");
    }

    #[test]
    fn export_and_load_again() {
        let doc = doc();
        let mut b = Builder::new(&doc);
        b.observe("Users", "/v1/users", Some(200), &json!([ { "id": 1, "username": "jdoe", "stores": [ { "id": 12 } ] } ]));
        let map = b.finish(1_700_000_000);
        let back = parse(&render(&map, Export::Json)).unwrap();
        assert_eq!(serde_json::to_value(&back).unwrap(), serde_json::to_value(&map).unwrap());
        let page = render(&map, Export::Html);
        assert!(page.starts_with("<!doctype html>") && page.contains("id=\"data-map\""), "{page}");
        for f in Export::ALL {
            assert!(!render(&map, f).contains("jdoe"), "{f:?}: no value");
            assert_eq!(Export::from_name(f.ext()), Some(f));
        }
        assert!(parse("{}").is_err() && parse("[1]").is_err());
    }

    #[test]
    fn the_map_so_far_while_it_grows() {
        let doc = doc();
        let mut b = Builder::new(&doc);
        b.observe("Users", "/v1/users", Some(200), &json!([ { "id": 1, "username": "a" } ]));
        let live = b.snapshot();
        assert!(!live.complete, "a live map says it's partial");
        assert!(markdown(&live).contains("**Partial**"));
        assert_eq!(live.entities.len(), 1);
        // The run goes on after it.
        b.observe("Categories", "/v1/categories", Some(200), &json!([ { "id": 21, "name": "x" } ]));
        assert_eq!(b.snapshot().entities.len(), 2);
        assert_eq!(b.known_ids("users"), ["1"], "the ids to unlock endpoints are still there");
    }

    #[test]
    fn closed_sets_and_presence() {
        let doc = doc();
        let mut b = Builder::new(&doc);
        let items: Vec<Value> = (0..40)
            .map(|i| {
                let kind = ["a", "b", "c"][i % 3];
                if i % 2 == 0 {
                    json!({ "id": i, "kind": kind })
                } else {
                    json!({ "id": i })
                }
            })
            .collect();
        b.observe("x", "/v1/users", Some(200), &Value::Array(items));
        let map = b.finish(0);
        let items = level(&map, "users", &map.entities["users"].full, "");
        let kind = items.iter().find(|r| r.name == "kind").unwrap();
        assert_eq!(kind.presence, 50, "present in half");
        assert!(kind.desc.contains("3 distinct values"), "{}", kind.desc);
        assert!(!items.iter().find(|r| r.name == "id").unwrap().desc.contains("distinct"), "ids are all different");
    }
}
