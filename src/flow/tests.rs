use super::*;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::docs::ApiDoc;
use crate::net::{self, Credentials, ResponseData};
use serde_json::json;

#[test]
fn flattening() {
    let mut out = Vec::new();
    flatten("", &json!({ "user": { "login": "jdoe", "ids": [1, 2], "x": {} } }), &mut out);
    assert_eq!(out[0], ("user.login".to_owned(), "\"jdoe\"".to_owned()));
    assert_eq!(out[2].0, "user.ids[1]");
    assert_eq!(out[3], ("user.x".to_owned(), "{}".to_owned()));
}

#[test]
fn quiet_conditions() {
    let b = Block::new_kind(BlockKind::from_id("if"));
    assert!(!b.logs());
    assert!(Block::new_kind(BlockKind::from_id("log")).logs());
    let json = serde_json::to_string(&b).unwrap();
    assert!(!json.contains("\"log\""), "default not serialized: {json}");
}

fn vars() -> Map<String, Value> {
    json!({
        "login": "jdoe",
        "user": { "status": 200, "body": { "id": 17, "login": "jdoe", "orders": [{ "total": 3.5 }, { "total": 7.1 }] } },
        "users": { "body": [{ "id": 1, "login": "a" }, { "id": 2, "login": "b" }] }
    })
    .as_object()
    .unwrap()
    .clone()
}

#[test]
fn paths() {
    let v = vars();
    assert_eq!(lookup(&v, "user.body.id"), Some(json!(17)));
    assert_eq!(lookup(&v, "{{ user.body.orders[-1].total }}"), Some(json!(7.1)));
    assert_eq!(lookup(&v, "users.body.length"), Some(json!(2)));
    assert_eq!(lookup(&v, "users.body.login"), Some(json!(["a", "b"])));
    assert_eq!(lookup(&v, "users.body[1].id"), Some(json!(2)));
    assert_eq!(lookup(&v, "nope"), None);
    assert!(resolve(&v, "{{now-1d}}").is_some());
    assert!(render("since={{now}}", &v).unwrap().starts_with("since=20"));
}

#[test]
fn templates() {
    let v = vars();
    assert_eq!(render("/v1/users/{{user.body.id}}", &v).unwrap(), "/v1/users/17");
    assert_eq!(render("filter[id]={{users.body.id}}", &v).unwrap(), "filter[id]=1,2");
    assert!(render("{{missing}}", &v).is_err());
    let body = render_json(r#"{"user_id": {{user.body.id}}, "note": "hi {{login}}", "ids": {{users.body.id}}}"#, &v).unwrap().unwrap();
    assert_eq!(body, json!({ "user_id": 17, "note": "hi jdoe", "ids": [1, 2] }));
}

#[test]
fn path_vars_from_variables() {
    let mut v = vars();
    v.insert("user_id".into(), json!(17));
    assert_eq!(resolve_path("/v1/users/:user_id/orders", &v).unwrap(), "/v1/users/17/orders");
    assert!(resolve_path("/v1/users/:nope", &v).is_err());
    // A full URL keeps its origin (and its port isn't a variable).
    assert_eq!(resolve_path("http://localhost:8080/v1/users/:user_id", &v).unwrap(), "http://localhost:8080/v1/users/17");
}

fn run_offline(main: Flow, library: Vec<Flow>) -> (bool, Value, Vec<String>) {
    run_offline_with(main, library, None)
}

fn run_offline_with(main: Flow, library: Vec<Flow>, checkpoint: Option<crate::resume::Checkpoint>) -> (bool, Value, Vec<String>) {
    run_offline_until(main, library, checkpoint, None)
}

fn run_offline_until(main: Flow, library: Vec<Flow>, checkpoint: Option<crate::resume::Checkpoint>, stop_after: Option<u64>) -> (bool, Value, Vec<String>) {
    let settings = RunSettings {
        delay: Duration::ZERO,
        parallel: 4,
        parallel_rate: 0.0,
        max_pages: 1,
        doc: Arc::new(ApiDoc::empty()),
        flows: Arc::new(library),
        subs: Arc::default(),
        subs_cache: Arc::default(),
        checkpoint: checkpoint.map(Arc::new),
        stop_after,
        profile: String::new(),
    };
    let creds = Credentials::default();
    let handle = start(main, net::agent(), Default::default(), creds, settings, || {});
    let mut lines = Vec::new();
    for ev in handle.rx.iter() {
        match ev {
            RunEvent::Log(l) => lines.push(l.text),
            RunEvent::Rate(_) | RunEvent::Progress(_) | RunEvent::Tests(_) => {}
            RunEvent::Finished { ok, vars } => return (ok, vars, lines),
        }
    }
    panic!("no Finished event");
}

#[test]
fn sub_flows() {
    let sub: Flow = serde_json::from_value(json!({
        "uid": 7, "name": "Greet",
        "inputs": [ { "type": "integer", "name": "n", "value": "1" }, { "type": "string", "name": "who", "value": "world" } ],
        "blocks": [ { "type": "set", "name": "msg", "value": "hello {{who}} x{{n}}" } ],
        "outputs": [ { "name": "message", "value": "{{msg}}" }, { "name": "n", "value": "n" }, { "name": "missing", "value": "{{nope}}" } ]
    }))
    .unwrap();
    let main: Flow = serde_json::from_value(json!({
        "uid": 8, "name": "Main",
        "inputs": [ { "type": "integer", "name": "k", "value": "21" } ],
        "blocks": [ { "type": "flow", "flow": "greet", "inputs": { "n": "{{k}}" }, "save_as": "g" } ],
        "outputs": [ { "name": "text", "value": "{{g.message}}" } ]
    }))
    .unwrap();
    let (ok, vars, lines) = run_offline(main.clone(), vec![sub.clone()]);
    assert!(ok, "{lines:?}");
    // The input keeps its type through {{k}}; the default fills `who`; a missing output is null.
    assert_eq!(vars["g"], json!({ "message": "hello world x21", "n": 21, "missing": null }));
    assert!(lines.iter().any(|l| l == "output text = \"hello world x21\""), "{lines:?}");

    // A wrong input type stops the run.
    let mut bad = main.clone();
    if let BlockKind::Flow(c) = &mut bad.blocks[0].kind {
        c.inputs.insert("n".into(), "abc".into());
    }
    let (ok, _, lines) = run_offline(bad, vec![sub.clone()]);
    assert!(!ok);
    assert!(lines.last().unwrap().contains("must be an integer"), "{lines:?}");

    // A flow that ends up running itself is refused.
    let mut loop_sub = sub.clone();
    loop_sub.blocks.push(serde_json::from_value(json!({ "type": "flow", "flow": "Main", "save_as": "x" })).unwrap());
    let (ok, _, lines) = run_offline(main.clone(), vec![loop_sub, main]);
    assert!(!ok);
    assert!(lines.last().unwrap().contains("ends up running itself"), "{lines:?}");
}

#[test]
fn parallel_for_each() {
    // Items wait different times so they finish out of order: the console still shows them in order.
    let main: Flow = serde_json::from_value(json!({
        "uid": 9, "name": "Par",
        "blocks": [
            { "type": "set", "name": "list", "value": "[30, 0, 20, 0, 10, 0]" },
            { "type": "for_each", "list": "list", "item": "ms", "parallel": true, "blocks": [
                { "type": "if", "left": "{{ms}}", "op": "gt", "right": "0", "then_blocks": [ { "type": "wait", "ms": 30 } ], "else_blocks": [] },
                { "type": "set", "name": "seen", "value": "{{index}}" },
                { "type": "log", "message": "item {{index}}" }
            ] }
        ]
    }))
    .unwrap();
    let text = serde_json::to_string(&main).unwrap();
    assert!(text.contains(r#""parallel":true"#));
    let (ok, vars, lines) = run_offline(main.clone(), vec![]);
    assert!(ok, "{lines:?}");
    let items: Vec<&String> = lines.iter().filter(|l| l.starts_with("item ")).collect();
    assert_eq!(items, ["item 0", "item 1", "item 2", "item 3", "item 4", "item 5"], "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("6 items, 4 at a time")), "{lines:?}");
    // The last item's variables are kept.
    assert_eq!(vars["seen"], json!(5));
    assert_eq!(vars["index"], json!(5));

    // The first failing item (in order) stops the loop; the items before it are shown.
    let mut bad = main;
    if let BlockKind::ForEach { blocks, .. } = &mut bad.blocks[1].kind {
        blocks.push(
            serde_json::from_value(json!({ "type": "if", "left": "{{index}}", "op": "eq", "right": "2",
            "then_blocks": [ { "type": "fail", "message": "boom {{index}}" } ], "else_blocks": [] }))
            .unwrap(),
        );
    }
    let (ok, _, lines) = run_offline(bad, vec![]);
    assert!(!ok);
    assert_eq!(lines.last().unwrap(), "✖ boom 2", "{lines:?}");
    let pos = |t: &str| lines.iter().position(|l| l == t);
    assert!(pos("item 0") < pos("item 1") && pos("item 1") < pos("item 2"), "{lines:?}");
}

#[test]
fn nested_loops_and_aborts() {
    // The outer {{index}} comes back after an inner loop.
    let nested: Flow = serde_json::from_value(json!({
        "uid": 11, "name": "Nested",
        "blocks": [
            { "type": "set", "name": "rows", "value": "[[1,2,3],[4,5,6]]" },
            { "type": "for_each", "list": "rows", "item": "row", "blocks": [
                { "type": "for_each", "list": "row", "item": "x", "blocks": [] },
                { "type": "log", "message": "outer {{index}}" }
            ] }
        ]
    }))
    .unwrap();
    let (ok, _, lines) = run_offline(nested, vec![]);
    assert!(ok, "{lines:?}");
    let outer: Vec<&String> = lines.iter().filter(|l| l.starts_with("outer ")).collect();
    assert_eq!(outer, ["outer 0", "outer 1"], "{lines:?}");

    // Items stopped inside a sub-flow while another one fails: the real error is the one shown.
    let slow: Flow = serde_json::from_value(json!({ "uid": 12, "name": "Slow", "blocks": [ { "type": "wait", "ms": 200 } ] })).unwrap();
    let main: Flow = serde_json::from_value(json!({
        "uid": 13, "name": "Par",
        "blocks": [
            { "type": "set", "name": "list", "value": "[0, 1, 2, 3]" },
            { "type": "for_each", "list": "list", "item": "i", "parallel": true, "blocks": [
                { "type": "if", "left": "{{i}}", "op": "eq", "right": "2",
                  "then_blocks": [ { "type": "fail", "message": "boom" } ],
                  "else_blocks": [ { "type": "flow", "flow": "Slow", "save_as": "s" } ] }
            ] }
        ]
    }))
    .unwrap();
    let (ok, _, lines) = run_offline(main, vec![slow]);
    assert!(!ok);
    assert_eq!(lines.last().unwrap(), "✖ boom", "{lines:?}");
}

#[test]
fn parallel_items_start_from_the_variables_before_the_loop() {
    // Each item overwrites `big` and adds `mine`: the next item on the same thread must still see the
    // values from before the loop (the copy is reused, what an item set is put back).
    let main: Flow = serde_json::from_value(json!({
        "uid": 14, "name": "Iso",
        "blocks": [
            { "type": "set", "name": "big", "value": "[1, 2, 3]" },
            { "type": "set", "name": "list", "value": "[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]" },
            { "type": "for_each", "list": "list", "item": "n", "parallel": true, "blocks": [
                { "type": "if", "left": "{{mine}}", "op": "not_empty", "right": "",
                  "then_blocks": [ { "type": "fail", "message": "item {{n}} sees mine = {{mine}}" } ], "else_blocks": [] },
                { "type": "if", "left": "{{big.length}}", "op": "ne", "right": "3",
                  "then_blocks": [ { "type": "fail", "message": "item {{n}} sees big = {{big}}" } ], "else_blocks": [] },
                { "type": "set", "name": "big", "value": "[{{n}}, {{n}}]" },
                { "type": "set", "name": "mine", "value": "{{n}}" },
                { "type": "for_each", "list": "big", "item": "inner", "parallel": true, "blocks": [ { "type": "set", "name": "deep", "value": "{{inner}}" } ] }
            ] }
        ]
    }))
    .unwrap();
    let (ok, vars, lines) = run_offline(main, vec![]);
    assert!(ok, "{lines:?}");
    // The last item's variables are kept after the loop, the others' are not.
    assert_eq!(vars["mine"], json!(9));
    assert_eq!(vars["big"], json!([9, 9]));
    assert_eq!(vars["deep"], json!(9));
    assert_eq!(vars["index"], json!(9));
    assert_eq!(vars["list"], json!([0, 1, 2, 3, 4, 5, 6, 7, 8, 9]));
}

#[test]
fn lookup_borrows_until_the_end() {
    let vars: Map<String, Value> = serde_json::from_value(json!({ "u": { "body": [ { "login": "a" }, { "login": "b" }, { "login": "a" } ] } })).unwrap();
    assert_eq!(lookup(&vars, "u.body[-1].login"), Some(json!("a")));
    assert_eq!(lookup(&vars, "u.body.login.unique"), Some(json!(["a", "b"])));
    assert_eq!(lookup(&vars, "u.body.login.unique.length"), Some(json!(2)));
    assert_eq!(lookup(&vars, "u.body.login[1]"), Some(json!("b")));
    assert_eq!(lookup(&vars, "u.body.1.login"), Some(json!("b")));
    assert_eq!(lookup(&vars, "u.body[9]"), None);
    // Same object, keys in another order: one.
    let objs: Map<String, Value> = serde_json::from_str(r#"{"l": [{"id": 1, "login": "a"}, {"login": "a", "id": 1}, {"id": 1.5}]}"#).unwrap();
    assert_eq!(lookup(&objs, "l.unique.length"), Some(json!(2)));
}

#[test]
fn resume_replays_kept_responses() {
    use crate::resume::Checkpoint;
    let flow = |stop_at: i64| -> Flow {
        serde_json::from_value(json!({
            "uid": 10, "name": "Resume",
            "blocks": [
                { "type": "request", "save_as": "users", "verb": "GET", "path": "/v1/users", "log": false },
                { "type": "for_each", "list": "users.body", "item": "u", "blocks": [
                    { "type": "wait", "ms": 3000 },
                    { "type": "request", "save_as": "one", "verb": "GET", "path": "/v1/users/{{u.id}}", "log": false },
                    { "type": "if", "left": "{{index}}", "op": "eq", "right": stop_at.to_string(), "then_blocks": [ { "type": "fail", "message": "stopped at {{index}}" } ], "else_blocks": [] },
                    { "type": "set", "name": "last", "value": "{{one.body.login}}" }
                ] }
            ]
        }))
        .unwrap()
    };
    let dir = std::env::temp_dir().join(format!("ft_flow_resume_{}", std::process::id()));
    let file = dir.join("10.resume.jsonl");
    // What a first run received before it was stopped (requests are found by position + what was asked).
    let cp = Checkpoint::create(file.clone(), &crate::resume::header(&flow(-1))).unwrap();
    let resp = |body: Value| ResponseData {
        status: 200,
        status_text: String::new(),
        elapsed: Duration::ZERO,
        size: 0,
        headers: Vec::new(),
        body: body.to_string(),
        is_json: true,
        notes: Vec::new(),
        script: None,
    };
    cp.put("0|GET /v1/users?#", &resp(json!([{ "id": 1 }, { "id": 2 }, { "id": 3 }])));
    for (i, login) in ["ann", "bob", "cid"].iter().enumerate() {
        cp.put(&format!("1/i{i}/1|GET /v1/users/{}?#", i + 1), &resp(json!({ "login": login })));
    }
    drop(cp);

    // Fails at item 1: nothing was sent (all from the file) and the file is kept.
    let started = Instant::now();
    let (ok, _, lines) = run_offline_with(flow(1), vec![], Some(Checkpoint::open(file.clone()).unwrap().0));
    assert!(!ok);
    assert!(lines.iter().any(|l| l.contains("Resuming: 4 responses")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("3 responses reused")), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("Resume goes on from here")), "{lines:?}");
    assert!(file.exists());

    // Resumed to the end: the waits already passed are skipped, variables are rebuilt, the file goes.
    let (ok, vars, lines) = run_offline_with(flow(-1), vec![], Some(Checkpoint::open(file.clone()).unwrap().0));
    assert!(ok, "{lines:?}");
    assert_eq!(vars["last"], json!("cid"));
    assert!(started.elapsed() < Duration::from_secs(2), "waits were made again");
    assert!(!file.exists());
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn dates_and_unique() {
    assert!(compare(CompareOp::Gt, "2026-10-14T08:00:00.000+02:00", "2026-10-12"));
    assert!(!compare(CompareOp::Gt, "2026-10-11T23:00:00Z", "2026-10-12T00:00:00Z"));
    assert!(!compare(CompareOp::Gt, "", "2026-10-12"), "a missing field is never greater");
    assert!(compare(CompareOp::Gt, "10", "9"), "numbers stay numbers");
    let v = vars();
    let mut with_dupes = v.clone();
    with_dupes.insert("l".into(), json!([{ "u": "a" }, { "u": "b" }, { "u": "a" }]));
    assert_eq!(lookup(&with_dupes, "l.u.unique"), Some(json!(["a", "b"])));
    assert_eq!(lookup(&with_dupes, "l.u.unique.length"), Some(json!(2)));
    assert_eq!(human_duration(75.0), "1 min");
    assert_eq!(human_duration(3900.0), "1 h 05");
}

#[test]
fn preview_is_capped() {
    let big = Value::Array((0..100_000).map(|i| json!({ "id": i, "login": "someone" })).collect());
    let p = preview(&big);
    assert!(p.len() <= 4010 && p.ends_with('…'), "{}", p.len());
    assert_eq!(preview(&json!({ "a": 1 })), "{\n  \"a\": 1\n}");
}

#[test]
fn json_merge() {
    let mut form = json!({ "user": { "login": "a", "kind": "student" }, "x": 1 });
    merge_json(&mut form, json!({ "user": { "login": "b" }, "y": [1] }));
    assert_eq!(form, json!({ "user": { "login": "b", "kind": "student" }, "x": 1, "y": [1] }));
}

#[test]
fn values_roundtrip() {
    let b: RequestBlock = serde_json::from_value(json!({ "verb": "PATCH", "path": "/v1/users/{{login}}", "values": { "user[kind]": "admin" } })).unwrap();
    assert_eq!(b.values.get("user[kind]").map(String::as_str), Some("admin"));
    // Empty values aren't written.
    let text = serde_json::to_string(&RequestBlock::default()).unwrap();
    assert!(!text.contains("values"), "{text}");
}

#[test]
fn typed_inputs() {
    let input = |json: Value| serde_json::from_value::<FlowInput>(json).unwrap();
    // Files written before types existed keep guessing.
    let old = input(json!({ "name": "n", "value": "17" }));
    assert_eq!(old.kind, InputType::Auto);
    assert_eq!(old.to_value().unwrap(), json!(17));
    assert_eq!(input(json!({ "type": "string", "name": "n", "value": "17" })).to_value().unwrap(), json!("17"));
    assert_eq!(input(json!({ "type": "integer", "name": "n", "value": "7" })).to_value().unwrap(), json!(7));
    assert!(input(json!({ "type": "integer", "name": "n", "value": "x" })).to_value().is_err());
    assert_eq!(input(json!({ "type": "boolean", "name": "b", "value": "true" })).to_value().unwrap(), json!(true));
    assert_eq!(input(json!({ "type": "json", "name": "j", "value": "[1,2]" })).to_value().unwrap(), json!([1, 2]));
    assert!(input(json!({ "type": "date", "name": "d", "value": "{{now-7d}}" })).to_value().is_ok());
    assert!(input(json!({ "type": "date", "name": "d", "value": "soon" })).to_value().is_err());
    let e = input(json!({ "type": "enum", "name": "e", "value": "b", "options": ["a", "b"] }));
    assert_eq!(e.to_value().unwrap(), json!("b"));
    assert!(FlowInput { value: "c".into(), ..e.clone() }.to_value().is_err());
    assert_eq!(input(json!({ "type": "integer", "name": "n", "value": "" })).to_value().unwrap(), Value::Null);
    // `type` is written first; `options` only when there are some.
    let text = serde_json::to_string(&FlowInput { kind: InputType::File, name: "p".into(), ..Default::default() }).unwrap();
    assert_eq!(text, r#"{"type":"file","name":"p","value":"","description":""}"#);
}

#[test]
fn flow_json_roundtrip() {
    let mut f = Flow::new("t");
    f.blocks.push(Block::new_kind(BlockKind::from_id("for_each")));
    if let BlockKind::ForEach { blocks, .. } = &mut f.blocks[0].kind {
        blocks.push(Block::new_kind(BlockKind::from_id("request")));
    }
    let text = serde_json::to_string(&f).unwrap();
    assert!(text.contains(r#""type":"for_each""#));
    let back: Flow = serde_json::from_str(&text).unwrap();
    let BlockKind::ForEach { blocks, .. } = &back.blocks[0].kind else { panic!() };
    assert!(matches!(blocks[0].kind, BlockKind::Request(_)));
}

#[test]
fn comparisons() {
    assert!(compare(CompareOp::Eq, "200", "200.0"));
    assert!(compare(CompareOp::Gt, "10", "9"));
    assert!(compare(CompareOp::Contains, "finished", "fin"));
    assert!(compare(CompareOp::Empty, "[]", ""));
}

#[test]
fn run_to_here_and_time_per_block() {
    let flow = |parallel: bool| -> Flow {
        serde_json::from_value(json!({
            "uid": 15, "name": "To here",
            "blocks": [
                { "type": "set", "name": "list", "value": "[10, 20, 30]" },
                { "type": "for_each", "list": "list", "item": "n", "parallel": parallel, "blocks": [
                    { "type": "set", "name": "seen", "value": "{{n}}" },
                    { "type": "wait", "ms": 5 }
                ] },
                { "type": "log", "message": "after the loop" }
            ]
        }))
        .unwrap()
    };
    for parallel in [false, true] {
        let f = flow(parallel);
        // The wait inside the loop: the run stops the first time it's done, with that item's variables.
        let BlockKind::ForEach { blocks, .. } = &f.blocks[1].kind else { unreachable!() };
        let wait = blocks[1].uid;
        let (ok, vars, lines) = run_offline_until(f, vec![], None, Some(wait));
        assert!(ok, "a pause is not a failure: {lines:?}");
        assert!(lines.last().unwrap().starts_with("‖ Stopped after the chosen block"), "{lines:?}");
        assert!(!lines.iter().any(|l| l == "after the loop"), "nothing after it runs: {lines:?}");
        assert!(vars["seen"].is_number(), "the variables at that point (parallel: {parallel}): {vars}");
        assert!(lines.iter().any(|l| l.starts_with("◷ Time per block")), "{lines:?}");
    }
    // Without a stop the whole flow runs, and the waits are counted together.
    let (ok, _, lines) = run_offline(flow(false), vec![]);
    assert!(ok && lines.iter().any(|l| l == "after the loop"), "{lines:?}");
}

/// One flow, two APIs: it reads from the “shop” profile (its base URL and token), writes to the
/// “crm” one with what it read; a post-response script checks the answer and makes a variable;
/// a WebSocket block talks to an echo server.
#[test]
fn a_flow_across_profiles_with_scripts_and_a_websocket() {
    use std::io::{Read, Write};
    // An HTTP server for both APIs: it says what it got.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        for s in listener.incoming() {
            let mut s = s.unwrap();
            let mut buf = vec![0u8; 8192];
            let mut got = String::new();
            // Headers, then the body (Content-Length).
            loop {
                let n = s.read(&mut buf).unwrap();
                got.push_str(&String::from_utf8_lossy(&buf[..n]));
                if let Some(end) = got.find("\r\n\r\n") {
                    let len: usize = got.lines().find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap())).unwrap_or(0);
                    if got.len() >= end + 4 + len || n == 0 {
                        break;
                    }
                }
            }
            let body = if got.starts_with("GET /shop/orders/9") { r#"{"id": 9, "email": "jdoe@example.com"}"# } else { r#"{"created": true}"# };
            seen2.lock().push(got.to_lowercase());
            let _ =
                s.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
        }
    });
    // A WebSocket echo server.
    let ws_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let ws_url = format!("ws://{}", ws_listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for s in ws_listener.incoming() {
            let mut ws = tungstenite::accept(s.unwrap()).unwrap();
            while let Ok(m) = ws.read() {
                if m.is_text() && ws.send(m).is_err() {
                    break;
                }
            }
        }
    });
    let bearer = |t: &str| crate::auth::Auth::Bearer { token: t.into(), prefix: "Bearer".into() };
    let mut ps = crate::profiles::Profiles::default();
    ps.add(crate::profiles::Profile { id: "shop".into(), name: "Shop".into(), base_url: format!("{base}/shop"), auth: bearer("shop-token"), ..Default::default() });
    ps.add(crate::profiles::Profile { id: "crm".into(), name: "CRM".into(), base_url: format!("{base}/crm"), auth: bearer("crm-token"), ..Default::default() });
    crate::profiles::set_current(&ps);
    let flow: Flow = serde_json::from_value(json!({
        "name": "Across",
        "blocks": [
            { "type": "request", "save_as": "order", "verb": "GET", "path": "/orders/9", "profile": "shop",
              "post_script": "pm.test('has an email', () => pm.expect(pm.response.json()).to.have.property('email'));\npm.variables.set('email', pm.response.json().email);" },
            { "type": "request", "save_as": "contact", "verb": "POST", "path": "/contacts", "profile": "crm",
              "body": "{ \"email\": \"{{email}}\", \"order\": {{order.body.id}} }" },
            { "type": "web_socket", "save_as": "ws", "url": ws_url, "messages": ["{\"ping\": \"{{email}}\"}"], "max_messages": 1, "wait_ms": 3000 },
            { "type": "log", "message": "echo {{ws.received[0].ping}}" }
        ]
    }))
    .unwrap();
    let (ok, vars, lines) = run_offline(flow, Vec::new());
    crate::profiles::set_current(&crate::profiles::Profiles::default());
    assert!(ok, "{lines:#?}");
    let seen = seen.lock();
    assert!(seen[0].starts_with("get /shop/orders/9") && seen[0].contains("authorization: bearer shop-token"), "{}", seen[0]);
    assert!(seen[1].starts_with("post /crm/contacts") && seen[1].contains("authorization: bearer crm-token"), "{}", seen[1]);
    assert!(seen[1].contains(r#""email":"jdoe@example.com""#) || seen[1].contains(r#""email": "jdoe@example.com""#), "the shop's answer went to the CRM: {}", seen[1]);
    assert_eq!(vars["email"], "jdoe@example.com", "made by the post-response script");
    assert!(lines.iter().any(|l| l == "echo jdoe@example.com"), "{lines:#?}");
    assert_eq!(vars["ws"]["connected"], true);
}
