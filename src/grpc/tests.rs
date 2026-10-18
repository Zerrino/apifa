use super::*;
use std::sync::OnceLock;

const COMMON: &str = r#"
syntax = "proto3";
package common;
enum Color { COLOR_UNSPECIFIED = 0; RED = 1; GREEN = 2; }
message Money { string currency = 1; int64 units = 2; }
"#;

const SHOP: &str = r#"
syntax = "proto3";
package shop;
import "common.proto";
import "google/protobuf/timestamp.proto";

message Item {
  string name = 1;
  repeated string tags = 2;
  map<string, int32> stock = 3;
  common.Money price = 4;
  common.Color color = 5;
  oneof id { int32 num = 6; string code = 7; }
  message Dimensions { double width = 1; Dimensions inner = 2; }
  Dimensions dims = 8;
  google.protobuf.Timestamp at = 9;
  optional bool flag = 10;
  uint64 big = 11;
  repeated common.Money prices = 12;
}

service Shop {
  rpc Get(Item) returns (Item);
  rpc Watch(Item) returns (stream Item);
  rpc Upload(stream Item) returns (Item);
  rpc Chat(stream Item) returns (stream Item);
}
"#;

/// A fresh temp folder with these files.
fn temp_dir(files: &[(&str, &str)]) -> PathBuf {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("apifa-grpc-test-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, text) in files {
        std::fs::write(dir.join(name), text).unwrap();
    }
    dir
}

fn shop() -> Protos {
    let dir = temp_dir(&[("common.proto", COMMON), ("shop.proto", SHOP)]);
    let p = Protos::from_files(&[dir.join("shop.proto")], &[]).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    p
}

#[test]
fn proto_files_services_and_methods() {
    let p = shop();
    let services = p.services();
    assert_eq!(services.len(), 1);
    assert_eq!(services[0].name, "shop.Shop");
    let kinds: Vec<(&str, &str)> = services[0].methods.iter().map(|m| (m.name.as_str(), m.kind())).collect();
    assert_eq!(kinds, [("Get", "unary"), ("Watch", "server streaming"), ("Upload", "client streaming"), ("Chat", "bidi streaming")]);
    let get = p.method("/shop.Shop/Get").unwrap();
    assert_eq!((get.full_path.as_str(), get.input.as_str(), get.output.as_str()), ("/shop.Shop/Get", "shop.Item", "shop.Item"));
    assert!(p.method("shop.Shop.Chat").is_some_and(|m| m.client_streaming && m.server_streaming));
    assert!(p.method("/shop.Shop/Nope").is_none());
    assert!(p.messages().contains(&"common.Money".to_string()));
    // Encoded and decoded again: the same services.
    assert_eq!(Protos::from_descriptor_set(&p.encode()).unwrap().services()[0].methods.len(), 4);
}

#[test]
fn proto_errors_name_the_file() {
    let dir = temp_dir(&[("bad.proto", "syntax = \"proto3\";\nmessage A {\n  strin x = 1;\n}\n"), ("imp.proto", "syntax = \"proto3\";\nimport \"missing.proto\";\n")]);
    let e = Protos::from_files(&[dir.join("bad.proto")], &[]).unwrap_err();
    assert!(e.contains("bad.proto"), "{e}");
    assert!(e.contains(":3:"), "line of the error: {e}");
    let e = Protos::from_files(&[dir.join("imp.proto")], &[]).unwrap_err();
    assert!(e.contains("missing.proto"), "{e}");
    let e = Protos::from_files(&[dir.join("nope.proto")], &[]).unwrap_err();
    assert!(e.contains("no such file"), "{e}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn include_dirs() {
    let dir = temp_dir(&[("shop.proto", SHOP)]);
    let lib = temp_dir(&[("common.proto", COMMON)]);
    assert!(Protos::from_files(&[dir.join("shop.proto")], &[]).is_err());
    let p = Protos::from_files(&[dir.join("shop.proto")], std::slice::from_ref(&lib)).unwrap();
    assert_eq!(p.services().len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&lib);
}

#[test]
fn example_json_skeleton() {
    let p = shop();
    let text = p.example_json("shop.Item").unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["name"], "");
    assert_eq!(v["tags"], serde_json::json!([""]));
    assert_eq!(v["stock"], serde_json::json!({"key": 0}));
    assert_eq!(v["price"], serde_json::json!({"currency": "", "units": "0"}));
    assert_eq!(v["prices"], serde_json::json!([{"currency": "", "units": "0"}]));
    assert_eq!(v["color"], "COLOR_UNSPECIFIED");
    assert_eq!(v["num"], 0);
    assert!(v.get("code").is_none(), "one field per oneof");
    assert_eq!(v["dims"], serde_json::json!({"width": 0.0, "inner": {}}), "recursive message stops");
    assert_eq!(v["at"], "1970-01-01T00:00:00Z");
    assert_eq!(v["flag"], false);
    assert_eq!(v["big"], "0");
    // The skeleton is a valid message.
    p.normalize_json("shop.Item", &text).unwrap();
    assert!(p.example_json("shop.Nope").is_none());
}

#[test]
fn json_round_trip() {
    let p = shop();
    let input = r#"{"name": "pen", "tags": ["a", "b"], "stock": {"brussels": 3}, "price": {"currency": "EUR", "units": 12},
        "color": "GREEN", "code": "X1", "dims": {"width": 1.5, "inner": {"width": 2}}, "at": "2026-10-18T10:00:00Z",
        "flag": true, "big": "18446744073709551615", "prices": [{"units": "-3"}]}"#;
    let out = p.normalize_json("shop.Item", input).unwrap();
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["name"], "pen");
    assert_eq!(v["tags"], serde_json::json!(["a", "b"]));
    assert_eq!(v["stock"]["brussels"], 3);
    assert_eq!(v["price"]["units"], "12");
    assert_eq!(v["color"], "GREEN");
    assert_eq!(v["code"], "X1");
    assert_eq!(v["dims"]["inner"]["width"], 2.0);
    assert_eq!(v["at"], "2026-10-18T10:00:00Z");
    assert_eq!(v["big"], "18446744073709551615");
    assert_eq!(v["prices"][0]["units"], "-3");
    // Proto field names work too, defaults are shown.
    let out = p.normalize_json("shop.Item", r#"{"name": "x"}"#).unwrap();
    assert!(out.contains("\"tags\": []"), "{out}");
    let again = p.normalize_json("shop.Item", &out).unwrap();
    assert_eq!(out, again);
}

#[test]
fn json_errors_name_the_field() {
    let p = shop();
    let err = |text: &str| p.normalize_json("shop.Item", text).unwrap_err();
    let e = err(r#"{"price": {"cents": 1}}"#);
    assert!(e.contains("`price.cents`") && e.contains("currency, units"), "{e}");
    let e = err(r#"{"price": {"units": "abc"}}"#);
    assert!(e.contains("`price.units`") && e.contains("int64") && e.contains("\"abc\""), "{e}");
    let e = err(r#"{"tags": "a"}"#);
    assert!(e.contains("`tags`") && e.contains("list"), "{e}");
    let e = err(r#"{"tags": ["a", 2]}"#);
    assert!(e.contains("`tags[1]`") && e.contains("string"), "{e}");
    let e = err(r#"{"color": "BLUE"}"#);
    assert!(e.contains("`color`") && e.contains("RED"), "{e}");
    let e = err(r#"{"stock": {"a": "many"}}"#);
    assert!(e.contains("stock[\"a\"]") && e.contains("int32"), "{e}");
    let e = err(r#"{"num": 1, "code": "x"}"#);
    assert!(e.contains("oneof `id`"), "{e}");
    let e = err(r#"{"num": 3000000000}"#);
    assert!(e.contains("`num`") && e.contains("range"), "{e}");
    let e = err(r#"{"dims": {"inner": {"width": true}}}"#);
    assert!(e.contains("`dims.inner.width`"), "{e}");
    let e = err(r#"{"name": "#);
    assert!(e.contains("not valid JSON"), "{e}");
    let e = err(r#"[1]"#);
    assert!(e.contains("expected an object"), "{e}");
    // Well-known types: prost-reflect's message, with the path.
    let e = err(r#"{"at": "yesterday"}"#);
    assert!(e.starts_with("`at`: ") && e.contains("Timestamp"), "{e}");
}

// --- Calls against in-process servers -------------------------------------------------------------

struct Servers {
    /// Health + reflection v1.
    v1: u16,
    /// Health + reflection v1alpha only.
    v1alpha: u16,
}

/// Starts the test servers once, on a runtime in a background thread.
fn servers() -> &'static Servers {
    static S: OnceLock<Servers> = OnceLock::new();
    S.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async move {
                let (reporter, health) = tonic_health::server::health_reporter();
                reporter.set_service_status("shop.Shop", tonic_health::ServingStatus::Serving).await;
                let v1 = tonic_reflection::server::Builder::configure().register_encoded_file_descriptor_set(tonic_health::pb::FILE_DESCRIPTOR_SET).build_v1().unwrap();
                let v1alpha =
                    tonic_reflection::server::Builder::configure().register_encoded_file_descriptor_set(tonic_health::pb::FILE_DESCRIPTOR_SET).build_v1alpha().unwrap();
                let l1 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let l2 = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                tx.send(Servers { v1: l1.local_addr().unwrap().port(), v1alpha: l2.local_addr().unwrap().port() }).unwrap();
                let s1 =
                    tonic::transport::Server::builder().add_service(health.clone()).add_service(v1).serve_with_incoming(tonic::transport::server::TcpIncoming::from(l1));
                let s2 =
                    tonic::transport::Server::builder().add_service(health).add_service(v1alpha).serve_with_incoming(tonic::transport::server::TcpIncoming::from(l2));
                let t = tokio::spawn(s2);
                s1.await.unwrap();
                let _ = t.await;
            });
        });
        rx.recv().unwrap()
    })
}

fn url(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

fn health() -> Protos {
    Protos::from_reflection(&url(servers().v1), &[], Duration::from_secs(5)).unwrap()
}

fn req(port: u16, method: &str, messages: &[&str], timeout_ms: u64) -> CallRequest {
    CallRequest {
        url: url(port),
        method_path: method.into(),
        messages: messages.iter().map(|s| s.to_string()).collect(),
        metadata: vec![("x-test".into(), "yes".into())],
        timeout: Duration::from_millis(timeout_ms),
    }
}

#[test]
fn reflection_lists_services() {
    let p = health();
    let names: Vec<String> = p.services().into_iter().map(|s| s.name).collect();
    assert!(names.contains(&"grpc.health.v1.Health".to_string()), "{names:?}");
    assert!(names.contains(&"grpc.reflection.v1.ServerReflection".to_string()), "{names:?}");
    let watch = p.method("/grpc.health.v1.Health/Watch").unwrap();
    assert!(watch.server_streaming && !watch.client_streaming);
    assert!(p.example_json("grpc.health.v1.HealthCheckRequest").unwrap().contains("\"service\": \"\""));
}

#[test]
fn reflection_v1alpha_fallback() {
    let p = Protos::from_reflection(&url(servers().v1alpha), &[], Duration::from_secs(5)).unwrap();
    let names: Vec<String> = p.services().into_iter().map(|s| s.name).collect();
    assert!(names.contains(&"grpc.health.v1.Health".to_string()), "{names:?}");
    assert!(names.contains(&"grpc.reflection.v1alpha.ServerReflection".to_string()), "{names:?}");
}

#[test]
fn unary_ok() {
    let p = health();
    let mut seen = Vec::new();
    let r = call(&p, &req(servers().v1, "/grpc.health.v1.Health/Check", &[r#"{"service": "shop.Shop"}"#], 5000), &mut |m| seen.push(m.to_string())).unwrap();
    assert_eq!((r.status_code, r.status_name.as_str()), (0, "OK"));
    assert_eq!(r.messages.len(), 1);
    assert!(r.messages[0].contains("\"status\": \"SERVING\""), "{}", r.messages[0]);
    assert_eq!(seen, r.messages);
    assert!(r.headers.iter().any(|(k, v)| k == "content-type" && v.starts_with("application/grpc")), "{:?}", r.headers);
    assert!(r.trailers.iter().any(|(k, v)| k == "grpc-status" && v == "0"), "{:?}", r.trailers);
    assert!(r.elapsed > Duration::ZERO);
    // No message = an empty one (the server's overall health).
    let r = call(&p, &req(servers().v1, "grpc.health.v1.Health/Check", &[], 5000), &mut |_| {}).unwrap();
    assert_eq!(r.status_code, 0);
}

#[test]
fn unary_not_found() {
    let p = health();
    let r = call(&p, &req(servers().v1, "/grpc.health.v1.Health/Check", &[r#"{"service": "nope.Nope"}"#], 5000), &mut |_| {}).unwrap();
    assert_eq!((r.status_code, r.status_name.as_str()), (5, "NOT_FOUND"));
    assert!(r.messages.is_empty());
    assert!(r.trailers.iter().any(|(k, v)| k == "grpc-status" && v == "5"), "{:?}", r.trailers);
}

#[test]
fn server_streaming_until_deadline() {
    let p = health();
    let mut seen = Vec::new();
    let r = call(&p, &req(servers().v1, "/grpc.health.v1.Health/Watch", &[r#"{"service": "shop.Shop"}"#], 300), &mut |m| seen.push(m.to_string())).unwrap();
    assert_eq!(r.status_name, "DEADLINE_EXCEEDED", "{r:?}");
    assert!(!seen.is_empty() && seen[0].contains("SERVING"), "{seen:?}");
    assert_eq!(r.messages, seen);
}

#[test]
fn bidi_streaming() {
    let p = health();
    let list = r#"{"listServices": ""}"#;
    let file = r#"{"fileByFilename": "health.proto"}"#;
    let r = call(&p, &req(servers().v1, "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo", &[list, file, list], 5000), &mut |_| {}).unwrap();
    assert_eq!(r.status_code, 0, "{r:?}");
    assert_eq!(r.messages.len(), 3);
    assert!(r.messages[0].contains("grpc.health.v1.Health"), "{}", r.messages[0]);
    assert!(r.messages[1].contains("fileDescriptorResponse") || r.messages[1].contains("errorResponse"), "{}", r.messages[1]);
}

#[test]
fn local_errors() {
    let p = health();
    let port = servers().v1;
    let e = call(&p, &req(port, "/grpc.health.v1.Health/Check", &["{}", "{}"], 1000), &mut |_| {}).unwrap_err();
    assert!(e.contains("one message"), "{e}");
    let e = call(&p, &req(port, "/grpc.health.v1.Health/Check", &[r#"{"svc": "x"}"#], 1000), &mut |_| {}).unwrap_err();
    assert!(e.contains("`svc`") && e.contains("service"), "{e}");
    let e = call(&p, &req(port, "/grpc.health.v1.Health/Nope", &[], 1000), &mut |_| {}).unwrap_err();
    assert!(e.contains("unknown method"), "{e}");
    let mut bad = req(port, "/grpc.health.v1.Health/Check", &[], 1000);
    bad.metadata = vec![("bad key".into(), "x".into())];
    assert!(call(&p, &bad, &mut |_| {}).unwrap_err().contains("metadata"));
    let mut bad = req(port, "/grpc.health.v1.Health/Check", &[], 1000);
    bad.url = "127.0.0.1:1".into();
    assert!(call(&p, &bad, &mut |_| {}).unwrap_err().contains("http://"));
    // Nothing listens there.
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let e = call(&p, &req(free, "/grpc.health.v1.Health/Check", &[], 2000), &mut |_| {}).unwrap_err();
    assert!(e.contains("can't connect"), "{e}");
    assert!(Protos::from_reflection(&url(free), &[], Duration::from_secs(2)).is_err());
    // TLS to a plaintext server: the handshake fails.
    let mut tls = req(port, "/grpc.health.v1.Health/Check", &[], 2000);
    tls.url = format!("https://127.0.0.1:{port}");
    assert!(call(&p, &tls, &mut |_| {}).is_err());
}

#[test]
fn status_names() {
    assert_eq!(code_name(0), "OK");
    assert_eq!(code_name(16), "UNAUTHENTICATED");
    assert_eq!(code_name(99), "UNKNOWN_CODE");
}

#[test]
fn status_details_text() {
    let p = shop();
    let money = p.pool.get_message_by_name("common.Money").unwrap();
    let mut m = DynamicMessage::new(money);
    m.set_field_by_name("currency", prost_reflect::Value::String("EUR".into()));
    let st =
        RpcStatus { code: 3, message: "bad".into(), details: vec![prost_types::Any { type_url: "type.googleapis.com/common.Money".into(), value: m.encode_to_vec() }] };
    let text = details_text(&p, &st.encode_to_vec());
    assert!(text.starts_with("common.Money: ") && text.contains("EUR"), "{text}");
    assert!(details_text(&p, &[0xff, 0xff]).contains("bytes"));
}
