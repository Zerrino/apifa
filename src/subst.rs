//! Substitutions: type a login where an id is expected, the app looks the id up before sending.
//! A rule says which fields it watches (`user_id`), the GET that finds the value (`/v1/users?filter[login]={{value}}`)
//! and where the answer is (`body[0].id`). Stored in `substitutions.json` as folders and rules.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{self, BodyMode, Kv, Prepared, RequestSpec};
use crate::net::ResponseData;
use crate::workspace::new_uid;

fn yes() -> bool {
    true
}

/// When a watched field is replaced.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum When {
    /// Only when what was typed isn't already a number (`jdoe` is looked up, `12345` is sent as is).
    #[default]
    NotNumber,
    Always,
}

impl When {
    pub fn label(self) -> &'static str {
        match self {
            When::NotNumber => "when the value isn't a number",
            When::Always => "always",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Rule {
    #[serde(default = "new_uid")]
    pub uid: u64,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Field names it applies to: `user_id` also matches `:user_id`, `filter[user_id]`, `user[user_id]`.
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default)]
    pub when: When,
    /// The GET that finds the value; `{{value}}` is what was typed.
    pub path: String,
    #[serde(default)]
    pub query: Vec<Kv>,
    /// Where the value is in `{status, headers, body}`: `body[0].id`.
    pub extract: String,
}

impl Default for Rule {
    fn default() -> Self {
        Self {
            uid: new_uid(),
            name: "New substitution".into(),
            description: String::new(),
            enabled: true,
            fields: Vec::new(),
            when: When::NotNumber,
            path: "/v1/".into(),
            query: Vec::new(),
            extract: "body[0].id".into(),
        }
    }
}

/// The last segment of a field name: `filter[user_id]` → `user_id`, `:user_id` → `user_id`.
fn short_name(key: &str) -> String {
    let k = key.trim().trim_start_matches(':');
    k.trim_end_matches(']').rsplit('[').next().unwrap_or(k).to_lowercase()
}

impl Rule {
    pub fn watches(&self, key: &str) -> bool {
        let short = short_name(key);
        let full = key.trim().trim_start_matches(':').to_lowercase();
        self.fields.iter().map(|f| f.trim().to_lowercase()).any(|f| !f.is_empty() && (f == short || f == full))
    }

    /// Whether one value (one item of a comma list) gets replaced.
    pub fn applies_to(&self, value: &str) -> bool {
        let v = value.trim();
        !v.is_empty() && !v.contains("{{") && (self.when == When::Always || !v.parse::<f64>().is_ok_and(f64::is_finite))
    }

    /// `GET /v1/users?filter[login]=…` for `value`.
    pub fn lookup(&self, value: &str) -> Prepared {
        let fill = |s: &str| s.replace("{{value}}", value);
        let path = fill(&self.path.trim().replace("{{value}}", &model::encode_component(value)));
        let path = if path.starts_with('/') || !model::split_origin(&path).0.is_empty() { path } else { format!("/{path}") };
        let query: Vec<(String, String)> = model::file_rows(&self.query).map(|(k, v)| (fill(k), fill(v))).collect();
        let q = query.iter().map(|(k, v)| format!("{}={}", model::encode_component(k), model::encode_component(v))).collect::<Vec<_>>().join("&");
        let url = model::resolve_url(&if q.is_empty() { path.clone() } else { format!("{path}?{q}") });
        Prepared {
            verb: "GET".into(),
            display_url: format!(
                "{}{}{}",
                model::resolve_url(&path),
                if query.is_empty() { "" } else { "?" },
                query.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&")
            ),
            url,
            path,
            query,
            headers: Vec::new(),
            body: None,
            files: Vec::new(),
            missing_path: Vec::new(),
            missing_required: Vec::new(),
            body_error: None,
            ..Default::default()
        }
    }

    /// The value found in the lookup's response.
    pub fn extract_from(&self, value: &str, r: &ResponseData) -> Result<String, String> {
        if r.status >= 400 {
            return Err(format!("substitution “{}”: HTTP {} while looking “{value}” up", self.name, r.status));
        }
        let body = serde_json::from_str::<Value>(&r.body).unwrap_or(Value::Null);
        let mut vars = serde_json::Map::new();
        vars.insert("status".into(), Value::from(r.status));
        vars.insert("body".into(), body);
        match crate::flow::lookup(&vars, &self.extract) {
            Some(Value::Null) | None => Err(format!("substitution “{}”: nothing found for “{value}”", self.name)),
            Some(v) => Ok(crate::flow::to_text(&v)),
        }
    }

    /// One line for tooltips: `user_id ← GET /v1/users?filter[login]={{value}} → body[0].id`.
    pub fn summary(&self) -> String {
        let q: Vec<String> = model::file_rows(&self.query).map(|(k, v)| format!("{k}={v}")).collect();
        let q = if q.is_empty() { String::new() } else { format!("?{}", q.join("&")) };
        format!("{} ← GET {}{q} → {}", self.fields.join(", "), self.path.trim(), self.extract.trim())
    }
}

/// The first enabled rule watching `key` that would replace (part of) `value`.
pub fn rule_for<'a>(rules: &'a [Rule], key: &str, value: &str) -> Option<&'a Rule> {
    rules.iter().find(|r| r.enabled && r.watches(key) && value.split(',').any(|v| r.applies_to(v)))
}

/// Replaces what the rules watch in `value` (each item of a comma list); `None` when nothing applies.
fn substitute(
    rules: &[Rule],
    key: &str,
    value: &str,
    resolve: &mut dyn FnMut(&Rule, &str) -> Result<String, String>,
    notes: &mut Vec<String>,
) -> Result<Option<String>, String> {
    let Some(rule) = rule_for(rules, key, value) else { return Ok(None) };
    let mut parts = Vec::new();
    for part in value.split(',') {
        if rule.applies_to(part) {
            let found = resolve(rule, part.trim())?;
            notes.push(format!("≈ {}: {} → {found}  ({})", key.trim_start_matches(':'), part.trim(), rule.name));
            parts.push(found);
        } else {
            parts.push(part.trim().to_owned());
        }
    }
    Ok(Some(parts.join(",")))
}

fn substitute_json(rules: &[Rule], v: &mut Value, resolve: &mut dyn FnMut(&Rule, &str) -> Result<String, String>, notes: &mut Vec<String>) -> Result<(), String> {
    match v {
        Value::Object(o) => {
            for (k, child) in o.iter_mut() {
                match child {
                    Value::String(s) => {
                        if let Some(new) = substitute(rules, k, s, resolve, notes)? {
                            // An id goes back as a number.
                            *child = new.parse::<i64>().map(Value::from).unwrap_or(Value::String(new));
                        }
                    }
                    Value::Array(items) if rules.iter().any(|r| r.enabled && r.watches(k)) => {
                        for item in items.iter_mut() {
                            if let Value::String(s) = item {
                                if let Some(new) = substitute(rules, k, s, resolve, notes)? {
                                    *item = new.parse::<i64>().map(Value::from).unwrap_or(Value::String(new));
                                }
                            }
                        }
                    }
                    other => substitute_json(rules, other, resolve, notes)?,
                }
            }
        }
        Value::Array(a) => {
            for item in a {
                substitute_json(rules, item, resolve, notes)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Applies the rules to a request tab's spec (path variables, fields, query, raw JSON body). Returns what was replaced.
pub fn apply_spec(spec: &mut RequestSpec, rules: &[Rule], resolve: &mut dyn FnMut(&Rule, &str) -> Result<String, String>) -> Result<Vec<String>, String> {
    let mut notes = Vec::new();
    if !rules.iter().any(|r| r.enabled) {
        return Ok(notes);
    }
    let path_vars: Vec<String> = model::path_vars(&spec.path).into_iter().map(|v| v.name).collect();
    for (k, v) in spec.path_values.iter_mut().filter(|(k, _)| path_vars.contains(k)) {
        if let Some(new) = substitute(rules, &format!(":{k}"), v, resolve, &mut notes)? {
            *v = new;
        }
    }
    apply_map(&mut spec.values, rules, resolve, &mut notes)?;
    for kv in spec.query.iter_mut().filter(|kv| kv.enabled) {
        if let Some(new) = substitute(rules, &kv.key, &kv.value, resolve, &mut notes)? {
            kv.value = new;
        }
    }
    if spec.body_mode == BodyMode::Raw && model::has_body(&spec.verb) {
        if let Ok(mut body) = serde_json::from_str::<Value>(&spec.raw_body) {
            let before = notes.len();
            substitute_json(rules, &mut body, resolve, &mut notes)?;
            if notes.len() > before {
                spec.raw_body = body.to_string();
            }
        }
    }
    Ok(notes)
}

/// Applies the rules to `name → value` fields.
pub fn apply_map(
    values: &mut BTreeMap<String, String>,
    rules: &[Rule],
    resolve: &mut dyn FnMut(&Rule, &str) -> Result<String, String>,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    for (k, v) in values.iter_mut() {
        if let Some(new) = substitute(rules, k, v, resolve, notes)? {
            *v = new;
        }
    }
    Ok(())
}

/// Applies the rules to query pairs.
pub fn apply_pairs(
    pairs: &mut [(String, String)],
    rules: &[Rule],
    resolve: &mut dyn FnMut(&Rule, &str) -> Result<String, String>,
    notes: &mut Vec<String>,
) -> Result<(), String> {
    for (k, v) in pairs.iter_mut() {
        if let Some(new) = substitute(rules, k, v, resolve, notes)? {
            *v = new;
        }
    }
    Ok(())
}

/// Lookups already done this session: (rule uid, mock server or the API, typed value) → found value.
/// The target is part of the key: an id found on the mock server is fake, it must never reach the real API.
#[derive(Default)]
pub struct Cache(Mutex<HashMap<CacheKey, String>>);

/// (rule uid, mock server URL or None for the API, typed value).
type CacheKey = (u64, Option<String>, String);

impl Cache {
    /// Resolves through the cache, calling `exec` for the lookup request otherwise.
    pub fn resolve(&self, rule: &Rule, value: &str, exec: &mut dyn FnMut(&Prepared) -> Result<ResponseData, String>) -> Result<String, String> {
        let key = (rule.uid, crate::net::mock_target(), value.to_owned());
        if let Some(found) = self.0.lock().get(&key) {
            return Ok(found.clone());
        }
        let r = exec(&rule.lookup(value))?;
        let found = rule.extract_from(value, &r)?;
        self.0.lock().insert(key, found.clone());
        Ok(found)
    }

    pub fn clear(&self) {
        self.0.lock().clear();
    }
}

// ---------------------------------------------------------------- the tree (sidebar)

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    Rule(Rule),
    Folder {
        #[serde(default = "new_uid")]
        uid: u64,
        name: String,
        #[serde(default)]
        items: Vec<Entry>,
    },
}

impl Entry {
    pub fn uid(&self) -> u64 {
        match self {
            Entry::Rule(r) => r.uid,
            Entry::Folder { uid, .. } => *uid,
        }
    }

    /// New uids everywhere (an imported copy must not collide with what's there).
    pub fn refresh_uids(&mut self) {
        match self {
            Entry::Rule(r) => r.uid = new_uid(),
            Entry::Folder { uid, items, .. } => {
                *uid = new_uid();
                items.iter_mut().for_each(Entry::refresh_uids);
            }
        }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Substitutions {
    pub items: Vec<Entry>,
}

impl Substitutions {
    /// The rules a new install starts with, read from the API docs (nothing about the API is written
    /// here): for the ids most routes ask for (`:user_id`, `filter[store_id]`…), the list of that
    /// resource filtered by a field that names an item (`login`, `slug`, `name`…), when it has one.
    pub fn starter(doc: &crate::docs::ApiDoc) -> Self {
        // Words a field naming one item goes by, in order: plain words, about no API in particular.
        const NAMING: [&str; 6] = ["login", "username", "slug", "code", "name", "email"];
        let mut asked: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for res in &doc.resources {
            for m in &res.methods {
                let mut names: Vec<String> = Vec::new();
                for r in &m.routes {
                    names.extend(r.path.split('/').filter_map(|seg| seg.trim_matches(['(', ')']).strip_prefix(':')).map(str::to_owned));
                }
                let mut leaves = Vec::new();
                crate::docs::param_leaves(&m.params, &mut leaves);
                names.extend(leaves.iter().map(|p| p.name.clone()));
                for n in names.into_iter().filter(|n| n.ends_with("_id")) {
                    *asked.entry(n).or_default() += 1;
                }
            }
        }
        let mut asked: Vec<(String, usize)> = asked.into_iter().filter(|(_, n)| *n >= 5).collect();
        asked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut items = Vec::new();
        for (field, _) in asked {
            let Some((list, _)) = doc.value_source(&field, None) else { continue };
            let Some((res, m)) = doc.lookup(&list) else { continue };
            let Some(route) = m.routes.get(list.route).filter(|r| !r.path.contains(':')) else { continue };
            let mut leaves = Vec::new();
            crate::docs::param_leaves(&m.params, &mut leaves);
            let filters: Vec<&str> = leaves.iter().filter_map(|p| p.full_name.strip_prefix("filter[")?.strip_suffix(']')).collect();
            let Some(by) = NAMING.iter().find(|w| filters.contains(w)) else { continue };
            items.push(Entry::Rule(Rule {
                name: format!("{} id from {by}", res.name),
                description: format!("Type a {by} where a {field} is expected: its id is looked up in GET {} before sending", route.path),
                fields: vec![field.clone()],
                path: route.path.clone(),
                query: vec![Kv { enabled: true, key: format!("filter[{by}]"), value: "{{value}}".into() }],
                ..Default::default()
            }));
            if items.len() == 8 {
                break;
            }
        }
        if items.is_empty() {
            return Self::default();
        }
        Self { items: vec![Entry::Folder { uid: new_uid(), name: "Ids from names".into(), items }] }
    }

    /// `substitutions.json`, or the starter rules when there is none yet.
    pub fn load(doc: &crate::docs::ApiDoc) -> Self {
        if crate::storage::dir().join("substitutions.json").exists() {
            crate::storage::load("substitutions.json")
        } else {
            Self::starter(doc)
        }
    }
}

fn find_list(items: &mut [Entry], folder: u64) -> Option<&mut Vec<Entry>> {
    for e in items.iter_mut() {
        if let Entry::Folder { uid, items: inner, .. } = e {
            if *uid == folder {
                return Some(inner);
            }
            if let Some(found) = find_list(inner, folder) {
                return Some(found);
            }
        }
    }
    None
}

fn take(items: &mut Vec<Entry>, uid: u64) -> Option<Entry> {
    if let Some(i) = items.iter().position(|e| e.uid() == uid) {
        return Some(items.remove(i));
    }
    items.iter_mut().find_map(|e| match e {
        Entry::Folder { items, .. } => take(items, uid),
        _ => None,
    })
}

fn find_entry(items: &[Entry], uid: u64) -> Option<&Entry> {
    items.iter().find_map(|e| {
        if e.uid() == uid {
            return Some(e);
        }
        match e {
            Entry::Folder { items, .. } => find_entry(items, uid),
            _ => None,
        }
    })
}

impl Substitutions {
    /// Every rule, in folders too.
    pub fn rules(&self) -> Vec<Rule> {
        fn walk(items: &[Entry], out: &mut Vec<Rule>) {
            for e in items {
                match e {
                    Entry::Rule(r) => out.push(r.clone()),
                    Entry::Folder { items, .. } => walk(items, out),
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.items, &mut out);
        out
    }

    pub fn entry(&self, uid: u64) -> Option<&Entry> {
        find_entry(&self.items, uid)
    }

    pub fn rule(&self, uid: u64) -> Option<&Rule> {
        match self.entry(uid)? {
            Entry::Rule(r) => Some(r),
            _ => None,
        }
    }

    /// The list of `folder`, or the top level for `None` / an unknown folder.
    fn list(&mut self, folder: Option<u64>) -> &mut Vec<Entry> {
        let exists = folder.is_some_and(|uid| find_list(&mut self.items, uid).is_some());
        match folder {
            Some(uid) if exists => find_list(&mut self.items, uid).expect("checked above"),
            _ => &mut self.items,
        }
    }

    /// The folder holding `uid` (`None` = top level).
    pub fn parent_of(&self, uid: u64) -> Option<u64> {
        fn walk(items: &[Entry], uid: u64, parent: Option<u64>) -> Option<Option<u64>> {
            for e in items {
                if e.uid() == uid {
                    return Some(parent);
                }
                if let Entry::Folder { uid: f, items, .. } = e {
                    if let Some(p) = walk(items, uid, Some(*f)) {
                        return Some(p);
                    }
                }
            }
            None
        }
        walk(&self.items, uid, None).flatten()
    }

    /// Adds or replaces (same uid, same place) a rule.
    pub fn upsert(&mut self, rule: Rule, folder: Option<u64>) {
        fn replace(items: &mut [Entry], rule: &Rule) -> bool {
            items.iter_mut().any(|e| match e {
                Entry::Rule(r) if r.uid == rule.uid => {
                    *r = rule.clone();
                    true
                }
                Entry::Folder { items, .. } => replace(items, rule),
                _ => false,
            })
        }
        if !replace(&mut self.items, &rule) {
            self.list(folder).push(Entry::Rule(rule));
        }
    }

    pub fn insert(&mut self, entry: Entry, folder: Option<u64>) {
        self.list(folder).push(entry);
    }

    pub fn remove(&mut self, uid: u64) -> Option<Entry> {
        take(&mut self.items, uid)
    }

    pub fn add_folder(&mut self, parent: Option<u64>, name: &str) {
        let name = if name.trim().is_empty() { "New folder" } else { name.trim() };
        self.list(parent).push(Entry::Folder { uid: new_uid(), name: name.to_owned(), items: Vec::new() });
    }

    pub fn rename_folder(&mut self, uid: u64, name: &str) {
        fn walk(items: &mut [Entry], uid: u64, name: &str) {
            for e in items {
                if let Entry::Folder { uid: u, name: n, items } = e {
                    if *u == uid {
                        *n = name.to_owned();
                        return;
                    }
                    walk(items, uid, name);
                }
            }
        }
        if !name.trim().is_empty() {
            walk(&mut self.items, uid, name.trim());
        }
    }

    /// Moves a rule or a folder; refuses to put a folder inside itself.
    pub fn move_to(&mut self, uid: u64, to: Option<u64>) -> bool {
        if let Some(target) = to {
            let inside = match self.entry(uid) {
                Some(Entry::Folder { items, .. }) => target == uid || find_entry(items, target).is_some(),
                _ => false,
            };
            if inside {
                return false;
            }
        }
        match take(&mut self.items, uid) {
            Some(e) => {
                self.list(to).push(e);
                true
            }
            None => false,
        }
    }

    /// Every folder as (uid, "A / B"), the top level first as `None`.
    pub fn folder_paths(&self) -> Vec<(Option<u64>, String)> {
        fn walk(items: &[Entry], prefix: &str, out: &mut Vec<(Option<u64>, String)>) {
            for e in items {
                if let Entry::Folder { uid, name, items } = e {
                    let path = if prefix.is_empty() { name.clone() } else { format!("{prefix} / {name}") };
                    out.push((Some(*uid), path.clone()));
                    walk(items, &path, out);
                }
            }
        }
        let mut out = vec![(None, "(top level)".to_owned())];
        walk(&self.items, "", &mut out);
        out
    }
}

/// Sharing format: one rule, a folder, or a list of them.
pub fn export(entry: &Entry) -> String {
    serde_json::to_string_pretty(&serde_json::json!({ "format": "apifa-substitutions", "items": [entry] })).unwrap_or_default()
}

/// Reads what [`export`] wrote, a bare rule / folder, or a list; uids are renewed.
pub fn import(text: &str) -> Result<Vec<Entry>, String> {
    let v: Value = serde_json::from_str(text.trim()).map_err(|e| format!("not JSON: {e}"))?;
    let items = match v {
        Value::Object(ref o) if o.contains_key("items") => o["items"].clone(),
        Value::Array(_) => v,
        Value::Object(ref o) if o.contains_key("kind") => Value::Array(vec![v]),
        // A bare rule without "kind".
        Value::Object(mut o) => {
            o.insert("kind".into(), Value::String("rule".into()));
            Value::Array(vec![Value::Object(o)])
        }
        _ => return Err("expected a substitution, a folder or a list".into()),
    };
    let mut entries: Vec<Entry> = serde_json::from_value(items).map_err(|e| format!("not a substitution: {e}"))?;
    if entries.is_empty() {
        return Err("nothing to import".into());
    }
    entries.iter_mut().for_each(Entry::refresh_uids);
    Ok(entries)
}

/// The rules as the UI sees them this frame (widgets read them from the context).
pub fn set_rules(ctx: &egui::Context, rules: Arc<Vec<Rule>>) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique("subst_rules"), rules));
}

pub fn rules_in(ctx: &egui::Context) -> Arc<Vec<Rule>> {
    ctx.data(|d| d.get_temp::<Arc<Vec<Rule>>>(egui::Id::unique("subst_rules"))).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn response(body: &str) -> ResponseData {
        ResponseData {
            status: 200,
            status_text: "OK".into(),
            elapsed: Duration::ZERO,
            size: 0,
            headers: Vec::new(),
            body: body.into(),
            is_json: true,
            notes: Vec::new(),
            script: None,
        }
    }

    #[test]
    fn substitutes_fields_of_a_spec() {
        let rules = Substitutions::starter(&crate::docs::ApiDoc::load().unwrap()).rules();
        let user = rules.iter().find(|r| r.fields == ["user_id"]).unwrap();
        assert!(user.watches(":user_id") && user.watches("filter[user_id]") && !user.watches("id"));
        assert!(user.lookup("j doe").url.ends_with("/v1/users?filter%5Busername%5D=j%20doe"), "relative to the base URL: {}", user.lookup("j doe").url);

        let mut spec = RequestSpec {
            verb: "POST".into(),
            path: "/v1/users/:user_id/x".into(),
            body_mode: BodyMode::Raw,
            raw_body: r#"{"team":{"user_id":"jdoe","store_id":12}}"#.into(),
            ..Default::default()
        };
        spec.path_values.insert("user_id".into(), "jdoe".into());
        spec.query.push(Kv { enabled: true, key: "filter[user_id]".into(), value: "jdoe, 17 ,asmith".into() });
        let cache = Cache::default();
        let mut calls = 0;
        let mut exec = |p: &Prepared| {
            calls += 1;
            let login = p.query[0].1.clone();
            Ok(response(&format!(r#"[{{"id": {}}}]"#, if login == "jdoe" { 1 } else { 2 })))
        };
        let mut resolve = |r: &Rule, v: &str| cache.resolve(r, v, &mut exec);
        let notes = apply_spec(&mut spec, &rules, &mut resolve).unwrap();
        assert_eq!(spec.path_values["user_id"], "1");
        assert_eq!(spec.query[0].value, "1,17,2", "numbers stay, each name is looked up");
        assert_eq!(spec.raw_body, r#"{"team":{"user_id":1,"store_id":12}}"#);
        assert_eq!(notes.len(), 4);
        assert_eq!(calls, 2, "jdoe is looked up once (cache)");
    }

    #[test]
    fn nothing_found_is_an_error() {
        let rule = &Substitutions::starter(&crate::docs::ApiDoc::load().unwrap()).rules()[0];
        assert!(rule.extract_from("nobody", &response("[]")).unwrap_err().contains("nothing found for “nobody”"));
    }

    #[test]
    fn export_import_tree() {
        let mut s = Substitutions::starter(&crate::docs::ApiDoc::load().unwrap());
        let folder = s.items[0].clone();
        let back = import(&export(&folder)).unwrap();
        assert_eq!(back.len(), 1);
        assert_ne!(back[0].uid(), folder.uid(), "fresh uids");
        // A bare rule as written by hand.
        let bare = import(r#"{"name":"x","fields":["team_id"],"path":"/v1/teams","extract":"body[0].id"}"#).unwrap();
        assert!(matches!(&bare[0], Entry::Rule(r) if r.fields == ["team_id"]));
        let fid = s.items[0].uid();
        let rid = s.rules()[0].uid;
        assert!(s.move_to(rid, None));
        assert_eq!(s.parent_of(rid), None);
        assert!(!s.move_to(fid, Some(fid)));
        s.add_folder(Some(fid), "Sub");
        let sub = s.folder_paths()[2].0.unwrap();
        assert!(!s.move_to(fid, Some(sub)), "not inside itself");
    }
}
