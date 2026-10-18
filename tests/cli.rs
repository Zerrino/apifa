//! The command line, run as a user would: the built binary, a data folder of its own
//! (`APIFA_DIR`), the mock server instead of a real API.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A data folder with a workspace, removed at the end.
struct Data(PathBuf);

impl Data {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("ft_cli_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("workspace")).unwrap();
        let d = Data(dir);
        // An API definition to work with: the apipie export kept as a fixture.
        let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/apidoc.json");
        let r = d.run(&["import-api", fixture, "--name", "Fixture"]);
        assert!(r.status.success(), "{}", text(&r));
        // …which made its API profile, active (its data map: datamap.fixture.json).
        assert!(text(&d.run(&["profiles"])).contains("Fixture (active)"));
        d
    }

    fn file(&self, rel: &str, text: &str) -> PathBuf {
        let p = self.0.join("workspace").join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
        p
    }

    fn run(&self, args: &[&str]) -> Output {
        // No keychain: the test's secrets stay in its own folder.
        Command::new(env!("CARGO_BIN_EXE_apifa")).args(args).env("APIFA_DIR", &self.0).env("APIFA_KEYCHAIN", "0").output().unwrap()
    }
}

impl Drop for Data {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

const FLOW: &str = r#"{
  "name": "Find user",
  "inputs": [ { "type": "string", "name": "username", "value": "jdoe" } ],
  "blocks": [
    { "type": "request", "save_as": "user", "verb": "GET", "path": "/v1/users/{{username}}" },
    { "type": "extract", "from": "user.body.id", "into": "user_id" },
    { "type": "log", "message": "found {{user_id}}" }
  ],
  "outputs": [ { "name": "id", "value": "{{user_id}}" } ]
}"#;

#[test]
fn check_list_and_run_a_flow_on_the_mock_server() {
    let d = Data::new("run");
    d.file("Users/Find user.flow.json", FLOW);
    d.file(
        "Users/Me.request.json",
        r#"{"name": "Me", "spec": {"verb": "GET", "path": "/v1/users/:id", "path_values": {"id": "jdoe"},
            "mock": {"status": 200, "body": "{\"id\": 77, \"username\": \"{{path.id}}\"}"},
            "assertions": [{"expr": "res.body.id", "op": "eq", "value": "77"}]}}"#,
    );

    let check = d.run(&["check"]);
    assert!(check.status.success(), "{}", text(&check));
    assert!(text(&check).contains("OK"), "{}", text(&check));

    let list = d.run(&["list"]);
    assert!(text(&list).contains("Users/Find user") && text(&list).contains("Users/Me"), "{}", text(&list));

    // The saved request's mock answers the flow's call: same verb, the path fits.
    let run = d.run(&["run", "Find user", "-i", "username=jdoe", "--mock", "--vars"]);
    let out = text(&run);
    assert!(run.status.success(), "{out}");
    assert!(out.contains("found 77"), "{out}");
    assert!(out.contains("\"user_id\": 77"), "--vars prints the variables: {out}");

    let test = d.run(&["test", "--mock", "--junit", d.0.join("junit.xml").to_str().unwrap()]);
    assert!(test.status.success(), "{}", text(&test));
    let junit = std::fs::read_to_string(d.0.join("junit.xml")).unwrap();
    assert!(junit.contains("<testsuite") && junit.contains("Users/Me"), "{junit}");
}

#[test]
fn a_failing_flow_and_a_broken_file_exit_with_1() {
    let d = Data::new("fail");
    d.file("Boom.flow.json", r#"{"name": "Boom", "blocks": [ { "type": "fail", "message": "boom {{now}}" } ]}"#);
    let run = d.run(&["run", "Boom", "--mock"]);
    assert_eq!(run.status.code(), Some(1), "{}", text(&run));
    assert!(text(&run).contains("boom"), "{}", text(&run));

    let bad = d.file("Bad.flow.json", r#"{"name": "Bad", "inputs": [ { "type": "integer", "name": "n", "value": "abc" } ], "blocks": []}"#);
    let check = d.run(&["check", bad.to_str().unwrap()]);
    assert_eq!(check.status.code(), Some(1), "{}", text(&check));
}

#[test]
fn docs_of_the_workspace() {
    let d = Data::new("docs");
    d.file("Users/Find user.flow.json", FLOW);
    let out = d.0.join("docs.md");
    let docs = d.run(&["docs", "-f", "md", "-o", out.to_str().unwrap()]);
    assert!(docs.status.success(), "{}", text(&docs));
    let md = std::fs::read_to_string(Path::new(&out)).unwrap();
    assert!(md.contains("Find user"), "{md}");
}

#[test]
fn environments_fill_requests_and_flows() {
    let d = Data::new("env");
    std::fs::write(
        d.0.join("environments.json"),
        r#"{"active": "dev", "list": [
            {"name": "dev", "vars": [{"key": "store_id", "value": "12"}]},
            {"name": "prod", "vars": [{"key": "store_id", "value": "1"}]}]}"#,
    )
    .unwrap();
    // The mock answers the store asked for: the flow shows which one the environment gave.
    d.file(
        "Store.request.json",
        r#"{"name": "Store", "spec": {"verb": "GET", "path": "/v1/stores/:id", "path_values": {"id": ""},
            "mock": {"status": 200, "body": "{\"id\": \"{{path.id}}\"}"}}}"#,
    );
    d.file(
        "Which store.flow.json",
        r#"{"name": "Which store", "blocks": [
            { "type": "request", "save_as": "c", "verb": "GET", "path": "/v1/stores/{{store_id}}" },
            { "type": "log", "message": "store {{c.body.id}} (env {{store_id}})" } ]}"#,
    );
    let run = d.run(&["run", "Which store", "--mock"]);
    assert!(text(&run).contains("store 12 (env 12)"), "the active environment: {}", text(&run));
    let run = d.run(&["run", "Which store", "--mock", "--env", "prod"]);
    assert!(text(&run).contains("store 1 (env 1)"), "--env: {}", text(&run));
    let bad = d.run(&["run", "Which store", "--mock", "--env", "staging"]);
    assert_eq!(bad.status.code(), Some(1));
    assert!(text(&bad).contains("No environment named “staging”"), "{}", text(&bad));
}

#[test]
fn understand_maps_the_data_of_saved_gets_without_keeping_values() {
    let d = Data::new("understand");
    // Two saved GETs, answered by their mocks: users, and orders embedding a user.
    d.file(
        "Users.request.json",
        r#"{"name": "Users", "spec": {"verb": "GET", "path": "/v1/users",
            "mock": {"status": 200, "body": "[{\"id\": 1, \"username\": \"jdoe\", \"email\": \"jdoe@example.com\"}, {\"id\": 2, \"username\": \"asmith\", \"email\": \"a@example.com\"}]"}}}"#,
    );
    d.file(
        "Orders.request.json",
        r#"{"name": "Orders", "spec": {"verb": "GET", "path": "/v1/orders",
            "mock": {"status": 200, "body": "[{\"id\": 9, \"status\": \"shipped\", \"user\": {\"id\": 1, \"username\": \"jdoe\"}, \"product\": {\"id\": 5, \"name\": \"Watering can\"}}]"}}}"#,
    );
    // Not a GET: never sent by Understand.
    d.file("Change.request.json", r#"{"name": "Change", "spec": {"verb": "PATCH", "path": "/v1/users/:id", "path_values": {"id": "1"}}}"#);
    let run = d.run(&["understand", "--mock", "--saved-only"]);
    let out = text(&run);
    assert!(run.status.success(), "{out}");
    assert!(out.contains("[1/2]") && !out.contains("[3/") && !out.contains("Change"), "only the 2 saved GETs: {out}");

    let md = std::fs::read_to_string(d.0.join("workspace/DATA_MAP.fixture.md")).unwrap();
    assert!(md.contains("### users") && md.contains("### orders"), "{md}");
    assert!(md.contains("| orders | `user` | embeds | users | 1 | 1 of 1 |"), "the embedded user is a user seen on /v1/users: {md}");
    assert!(md.contains("string (email)"), "{md}");
    for value in ["jdoe", "asmith", "example.com", "shipped", "Watering"] {
        assert!(!md.contains(value), "{value} in DATA_MAP.md");
    }
    let saved = std::fs::read_to_string(d.0.join("datamap.fixture.json")).unwrap();
    assert!(!saved.contains("jdoe") && !saved.contains("Watering"), "{saved}");

    // The generated docs have it.
    for (format, needle) in [("md", "## Data map"), ("html", "id=\"data-map\""), ("openapi", "\"x-data-map-links\"")] {
        let file = d.0.join(format!("docs.{format}"));
        let docs = d.run(&["docs", "-f", format, "-o", file.to_str().unwrap()]);
        assert!(docs.status.success(), "{}", text(&docs));
        let content = std::fs::read_to_string(&file).unwrap();
        assert!(content.contains(needle), "{format}: no data map");
    }
    let oas: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(d.0.join("docs.openapi")).unwrap()).unwrap();
    assert_eq!(oas["components"]["schemas"]["users"]["properties"]["email"]["format"], "email");

    // Exported (the format from the extension), then loaded back as the current map.
    for (file, needle) in [("map.json", "\"entities\""), ("map.md", "### users"), ("map.html", "<!doctype html>")] {
        let path = d.0.join(file);
        let r = d.run(&["datamap", "-o", path.to_str().unwrap()]);
        assert!(r.status.success(), "{}", text(&r));
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains(needle) && !content.contains("jdoe"), "{file}");
    }
    std::fs::remove_file(d.0.join("datamap.fixture.json")).unwrap();
    assert!(!d.run(&["datamap"]).status.success(), "no map");
    let r = d.run(&["datamap", "--load", d.0.join("map.json").to_str().unwrap()]);
    assert!(r.status.success(), "{}", text(&r));
    assert!(text(&d.run(&["datamap"])).contains("### orders"));
    assert!(!d.run(&["datamap", "--load", d.0.join("map.md").to_str().unwrap()]).status.success(), "not a map");
}

#[test]
fn understand_unlocks_every_documented_get_with_the_ids_it_finds() {
    let d = Data::new("unlock");
    std::fs::write(d.0.join("config.json"), r#"{"flow_delay_ms": 0}"#).unwrap();
    // No saved request: every documented GET, the mock answering with the docs' examples.
    let run = d.run(&["understand", "--mock"]);
    assert!(run.status.success(), "{}", text(&run));
    let map: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(d.0.join("datamap.fixture.json")).unwrap()).unwrap();
    let sources = map["sources"].as_array().unwrap();
    let read = |route: &str| sources.iter().any(|s| s["route"] == route && s["problem"].is_null());
    assert!(read("GET /v1/categories"), "no parameter: sent first");
    // Needs a category id: unlocked by what /v1/categories returned; two ids: unlocked too.
    assert!(read("GET /v1/categories/:category_id/products"), "{sources:?}");
    assert!(read("GET /v1/stores/:store_id/categories/:category_id/products"), "{sources:?}");
    assert!(sources.iter().filter(|s| s["problem"].is_null()).count() > 15, "most GETs read");
    assert!(sources.iter().all(|s| s["route"].as_str().unwrap().starts_with("GET ")), "only GETs");
    let md = std::fs::read_to_string(d.0.join("workspace/DATA_MAP.fixture.md")).unwrap();
    for value in ["https://", "@", "Downtown", "2025-"] {
        assert!(!md.contains(value) && !map.to_string().contains(value), "{value} kept");
    }
}
