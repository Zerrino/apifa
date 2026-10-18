//! Example responses recorded from real calls, for the endpoints whose documentation has none.
//! A successful GET to such an endpoint (sent by you or by the assistant) is kept as a small sample in
//! `learned_examples.json`: shown in the Examples tab, and given to the assistant with the endpoint's docs.

use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::docs::Method;
use crate::model::DocRef;
use crate::net::ResponseData;

const FILE: &str = "learned_examples.json";
/// Items kept from each list of the sample.
const LIST_ITEMS: usize = 2;
const TEXT_CHARS: usize = 200;
const MAX_SAMPLE: usize = 8_000;

#[derive(Clone, Serialize, Deserialize)]
pub struct Learned {
    pub verb: String,
    /// What was called (path and query), e.g. `/v1/stores/12/users?page[size]=1`.
    pub url: String,
    pub status: u16,
    /// Unix seconds.
    pub recorded_at: u64,
    /// How many items the list had (`X-Total`, or the length of what came back).
    #[serde(default)]
    pub total: Option<u64>,
    /// Pretty JSON, shrunk: lists keep their first items, long texts are cut.
    pub response: String,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Examples {
    /// `resource/method/route` → sample.
    pub map: BTreeMap<String, Learned>,
}

pub type Shared = Arc<Mutex<Examples>>;

fn key(d: &DocRef) -> String {
    format!("{}/{}/{}", d.resource, d.method, d.route)
}

/// The documentation already shows a response for this method.
pub fn documented(m: &Method) -> bool {
    m.examples.iter().any(|e| e.response.as_deref().is_some_and(|r| !r.trim().is_empty()))
}

fn shrink(v: &Value) -> Value {
    match v {
        Value::Array(a) => Value::Array(a.iter().take(LIST_ITEMS).map(shrink).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, v)| (k.clone(), shrink(v))).collect()),
        Value::String(s) if s.chars().count() > TEXT_CHARS => Value::String(format!("{}…", s.chars().take(TEXT_CHARS).collect::<String>())),
        other => other.clone(),
    }
}

/// A small sample of a JSON body (None if it isn't JSON).
pub fn sample(body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    let text = serde_json::to_string_pretty(&shrink(&v)).ok()?;
    // Cut text isn't JSON any more (the mock server serves these samples): too big = not recorded.
    (text.len() <= MAX_SAMPLE).then_some(text)
}

impl Examples {
    pub fn load() -> Self {
        crate::storage::load(FILE)
    }

    pub fn save(&self) {
        crate::storage::save(FILE, self);
    }

    pub fn get(&self, d: &DocRef) -> Option<&Learned> {
        self.map.get(&key(d))
    }

    pub fn forget(&mut self, d: &DocRef) -> bool {
        self.map.remove(&key(d)).is_some()
    }

    /// Records `r` for `d` if it's a successful JSON GET and neither the docs nor a previous call have an example.
    /// Returns whether it was recorded.
    pub fn record(&mut self, d: &DocRef, m: &Method, verb: &str, url: &str, r: &ResponseData) -> bool {
        if !verb.eq_ignore_ascii_case("GET") || !(200..300).contains(&r.status) || documented(m) || self.get(d).is_some() {
            return false;
        }
        let Some(response) = sample(&r.body) else { return false };
        // An empty list (a filter matching nothing) shows nothing: wait for a call that returns something.
        if matches!(serde_json::from_str::<Value>(&response), Ok(Value::Array(a)) if a.is_empty()) {
            return false;
        }
        let len = serde_json::from_str::<Value>(&r.body).ok().and_then(|v| v.as_array().map(|a| a.len() as u64));
        let url = crate::model::path_of(url).to_owned();
        self.map
            .insert(key(d), Learned { verb: "GET".into(), url, status: r.status, recorded_at: crate::storage::now(), total: r.header_u64("x-total").or(len), response });
        true
    }
}

/// Records and saves; returns whether something new was kept.
pub fn record_shared(shared: &Shared, d: &DocRef, m: &Method, verb: &str, url: &str, r: &ResponseData) -> bool {
    // An answer of the mock server is no example of the real API.
    if crate::net::mock_target().is_some() {
        return false;
    }
    let mut ex = shared.lock();
    let new = ex.record(d, m, verb, url, r);
    if new {
        ex.save();
    }
    new
}

/// `2026-10-16 13:40` (local time).
pub fn when(l: &Learned) -> String {
    use chrono::TimeZone;
    chrono::Local.timestamp_opt(l.recorded_at as i64, 0).single().map(|d| d.format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default()
}

/// The examples, for widgets drawn this frame.
pub fn set(ctx: &egui::Context, shared: Shared) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::unique("learned_examples"), shared));
}

pub fn get(ctx: &egui::Context) -> Option<Shared> {
    ctx.data(|d| d.get_temp::<Shared>(egui::Id::unique("learned_examples")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn resp(status: u16, body: &str) -> ResponseData {
        ResponseData {
            status,
            status_text: String::new(),
            elapsed: Duration::ZERO,
            size: 0,
            headers: vec![("X-Total".into(), "4145".into())],
            body: body.into(),
            is_json: true,
            notes: Vec::new(),
            script: None,
        }
    }

    #[test]
    fn records_once_and_only_when_undocumented() {
        let doc = crate::docs::ApiDoc::load().unwrap();
        let mut ex = Examples::default();
        let long = "x".repeat(500);
        let body = format!(r#"[{{"login":"a","bio":"{long}"}},{{"login":"b"}},{{"login":"c"}}]"#);
        // A documented GET keeps its own example.
        let documented_get = doc.resources.iter().flat_map(|r| r.methods.iter().map(move |m| (r, m))).find(|(_, m)| documented(m) && m.routes[0].verb == "GET");
        let (r, m) = documented_get.expect("some GET has an example");
        let d = DocRef { resource: r.key.clone(), method: m.name.clone(), route: 0 };
        assert!(!ex.record(&d, m, "GET", "/x", &resp(200, &body)));
        // One without.
        let (r, m) = doc
            .resources
            .iter()
            .flat_map(|r| r.methods.iter().map(move |m| (r, m)))
            .find(|(_, m)| !documented(m) && m.routes[0].verb == "GET")
            .expect("some GET has no example");
        let d = DocRef { resource: r.key.clone(), method: m.name.clone(), route: 0 };
        assert!(!ex.record(&d, m, "GET", "/x", &resp(404, &body)), "errors aren't examples");
        assert!(!ex.record(&d, m, "POST", "/x", &resp(200, &body)), "only GETs");
        assert!(ex.record(&d, m, "GET", "https://shop.example.com/v1/stores/12/users?page[size]=1", &resp(200, &body)));
        let l = ex.get(&d).unwrap();
        assert_eq!(l.url, "/v1/stores/12/users?page[size]=1");
        assert_eq!(l.total, Some(4145));
        let v: Value = serde_json::from_str(&l.response).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 2, "lists are cut");
        assert!(v[0]["bio"].as_str().unwrap().chars().count() <= TEXT_CHARS + 1, "long texts are cut");
        assert!(!ex.record(&d, m, "GET", "/again", &resp(200, &body)), "the first sample stays");
        assert!(ex.forget(&d) && ex.get(&d).is_none());
    }
}
