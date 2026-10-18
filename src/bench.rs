//! Timings of the hot paths, to measure a change instead of guessing:
//!
//! ```sh
//! cargo test --release bench_ -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Ignored by `cargo test`. Each prints the time of one round (best of a few).

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::docs::ApiDoc;
use crate::flow::{self, Flow, RunEvent, RunSettings};
use crate::workspace::{Folder, Node, SavedRequest};

/// Best time of `rounds` runs of `f`.
fn time(label: &str, rounds: usize, mut f: impl FnMut()) {
    let best = (0..rounds)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .min()
        .unwrap_or_default();
    println!("BENCH {label:<46} {:>10.2} ms", best.as_secs_f64() * 1000.0);
}

/// A list response of `n` users (about 1 KB each), like `GET /v1/stores/:id/users` with all pages.
fn users(n: usize) -> Value {
    let one = |i: usize| {
        json!({ "id": i, "login": format!("user{i}"), "displayname": format!("User Number {i}"), "email": format!("user{i}@example.com"),
                "image": { "link": "https://cdn.example.com/users/x.jpg", "versions": { "large": "l", "medium": "m", "small": "s", "micro": "u" } },
                "plan": "monthly", "signup_year": "2024", "location": null, "credits": i % 400, "points": i % 12,
                "staff?": false, "active?": true, "alumni?": false, "created_at": "2024-09-01T08:00:00.000Z", "updated_at": "2026-10-01T08:00:00.000Z",
                "orders": [ { "total": 4.2, "status": "shipped", "store": { "id": 21, "name": "Downtown", "slug": "downtown" } } ] })
    };
    Value::Array((0..n).map(one).collect())
}

fn run(flow: Flow) -> bool {
    let settings = RunSettings {
        delay: Duration::ZERO,
        parallel: 4,
        parallel_rate: 0.0,
        max_pages: 1,
        doc: Arc::new(ApiDoc::empty()),
        flows: Arc::new(Vec::new()),
        subs: Arc::default(),
        subs_cache: Arc::default(),
        checkpoint: None,
        stop_after: None,
        profile: String::new(),
    };
    let creds = crate::net::Credentials::default();
    let handle = flow::start(flow, crate::net::agent(), Default::default(), creds, settings, || {});
    for ev in handle.rx.iter() {
        if let RunEvent::Finished { ok, .. } = ev {
            return ok;
        }
    }
    false
}

#[test]
#[ignore]
fn bench_templates_on_a_big_response() {
    let mut vars = Map::new();
    vars.insert("users".into(), json!({ "status": 200, "body": users(5000) }));
    time("2000 x {{users.body.length}} + [100].login (5 MB)", 3, || {
        for _ in 0..2000 {
            let out = flow::render("{{users.body.length}} {{users.body[100].login}}", &vars).unwrap();
            assert_eq!(out, "5000 user100");
        }
    });
    time("20 x {{users.body.signup_year.unique}} (5000 items)", 3, || {
        for _ in 0..20 {
            flow::render("{{users.body.signup_year.unique}}", &vars).unwrap();
        }
    });
}

#[test]
#[ignore]
fn bench_parallel_loop_with_a_big_variable() {
    let big = users(3000).to_string();
    let flow: Flow = serde_json::from_value(json!({
        "uid": 1, "name": "Bench",
        "blocks": [
            { "type": "set", "name": "users", "value": big },
            { "type": "set", "name": "list", "value": serde_json::to_string(&(0..400).collect::<Vec<_>>()).unwrap() },
            { "type": "for_each", "list": "list", "item": "n", "parallel": true, "log": false, "blocks": [
                { "type": "set", "name": "x", "value": "{{n}}", "log": false }
            ] }
        ]
    }))
    .unwrap();
    time("parallel for_each, 400 items, 3 MB of variables", 3, || assert!(run(flow.clone())));
}

#[test]
#[ignore]
fn bench_endpoint_search() {
    let doc = ApiDoc::load().unwrap();
    // The sidebar asks every frame: 300 frames with the same search, then 300 different ones.
    time("300 x search(\"users\") (same text, as per frame)", 3, || {
        for _ in 0..300 {
            assert!(!doc.search("users").is_empty());
        }
    });
    let words = ["user", "store", "product", "order", "category", "team", "event", "review", "visit", "item"];
    time("300 x search(<different words>)", 3, || {
        for i in 0..300 {
            doc.search(&format!("{} {}", words[i % 10], i % 3));
        }
    });
}

#[test]
#[ignore]
fn bench_workspace_save() {
    let (_lock, tmp) = crate::storage::test_data_dir("bench");
    let mut root = Folder::new("Workspace");
    for f in 0..10 {
        let mut folder = Folder::new(format!("Folder {f}"));
        for r in 0..50 {
            let spec = crate::model::RequestSpec { path: format!("/v1/users/{r}"), description: "x".repeat(2000), ..Default::default() };
            folder.items.push(Node::Request(SavedRequest { uid: crate::workspace::new_uid(), name: format!("Request {r}"), spec }));
        }
        root.items.push(Node::Folder(folder));
    }
    let mut index = crate::workspace_fs::load().map(|l| l.index).unwrap_or_default();
    crate::workspace_fs::save(&root, &mut index).unwrap();
    // As after a reload: what the app knows of the files comes from reading them.
    let mut index = crate::workspace_fs::load().unwrap().index;
    time("save, 500 files, nothing changed", 5, || crate::workspace_fs::save(&root, &mut index).unwrap());
    time("signature (the watcher's walk), 500 files", 5, || {
        crate::workspace_fs::signature();
    });
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
#[ignore]
fn bench_privacy() {
    let body = users(1000).to_string();
    time("privacy::body, 1000 users (~1 MB)", 5, || {
        std::hint::black_box(crate::privacy::body(&body));
    });
    let text = "Liste les commandes de user12, user500 et user999 du magasin Downtown, puis écris un flow. ".repeat(50);
    time("privacy::mask_text, 4 KB of chat", 5, || {
        std::hint::black_box(crate::privacy::mask_text(&text));
    });
}
