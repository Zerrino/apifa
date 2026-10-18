//! WebSocket and gRPC tabs: connecting, sending, loading services, calling — on threads, the
//! results polled by `App::poll` (`LiveState::poll`).

use std::sync::mpsc;
use std::time::Duration;

use crate::model::{self, Kv};
use crate::ui_live::GrpcEvent;

use super::*;

/// The request's headers with `{{values}}` filled, and its auth's (an OAuth token is fetched here:
/// call it on a thread). `Err` when the auth fails.
fn headers_with_auth(agent: &ureq::Agent, tokens: &net::SharedAuth, default: &crate::auth::Auth, spec: &RequestSpec) -> Result<Vec<(String, String)>, String> {
    let fill = |v: &str| crate::dates::expand(&crate::env::expand(v));
    let mut headers: Vec<(String, String)> =
        spec.headers.iter().filter(|h: &&Kv| h.enabled && !h.key.trim().is_empty()).map(|h| (h.key.trim().to_owned(), fill(&h.value))).collect();
    let auth = spec.auth.or(default).expand(&fill);
    let (extra, _query) = crate::auth::apply(agent, tokens, &auth, &headers, &crate::auth::Refresh::IfExpired)?;
    headers.extend(extra);
    Ok(headers)
}

/// The URL of a live tab: `{{values}}` filled, a path made absolute with the base URL (as
/// `ws://` / `wss://` for a WebSocket).
fn live_url(spec: &RequestSpec, ws: bool) -> String {
    let url = model::resolve_url(&crate::env::expand(spec.path.trim()));
    if ws {
        if let Some(rest) = url.strip_prefix("https://") {
            return format!("wss://{rest}");
        }
        if let Some(rest) = url.strip_prefix("http://") {
            return format!("ws://{rest}");
        }
    }
    url
}

impl App {
    fn live_tab(&mut self, id: TabId) -> Option<&mut ReqState> {
        let i = self.pos(id)?;
        match &mut self.tabs.get_mut(i)?.body {
            TabBody::Request(r) => Some(r),
            _ => None,
        }
    }

    pub(super) fn ws_connect(&mut self, id: TabId, ctx: &egui::Context) {
        let (agent, tokens, default) = (self.agent.clone(), self.auth.clone(), self.config.auth.clone());
        let Some(tab) = self.live_tab(id) else { return };
        let spec = tab.spec.clone();
        let (tx, rx) = mpsc::channel();
        tab.live.ws_handle_rx = Some(rx);
        tab.live.ws_log.clear();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = headers_with_auth(&agent, &tokens, &default, &spec).map(|headers| {
                let (etx, erx) = mpsc::channel();
                let wake_ctx = ctx.clone();
                let cfg =
                    crate::ws::WsConfig { url: live_url(&spec, true), headers, subprotocols: spec.ws_subprotocols.clone(), connect_timeout: Duration::from_secs(10) };
                (crate::ws::connect(cfg, etx, Box::new(move || wake_ctx.request_repaint())), erx)
            });
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    pub(super) fn ws_send(&mut self, id: TabId, text: String, binary: bool) {
        let Some(tab) = self.live_tab(id) else { return };
        let Some(h) = &tab.live.ws else { return };
        if binary {
            let clean: String = text.chars().filter(|c| c.is_ascii_hexdigit()).collect();
            let bytes: Vec<u8> = (0..clean.len() / 2).filter_map(|i| u8::from_str_radix(&clean[i * 2..i * 2 + 2], 16).ok()).collect();
            h.send_binary(bytes);
        } else {
            h.send_text(crate::env::expand(&text));
        }
    }

    pub(super) fn ws_disconnect(&mut self, id: TabId) {
        if let Some(tab) = self.live_tab(id) {
            if let Some(h) = &tab.live.ws {
                h.close(1000, String::new());
            }
        }
    }

    pub(super) fn grpc_add_protos(&mut self, id: TabId) {
        let Some(files) = rfd::FileDialog::new().add_filter("Protocol Buffers", &["proto"]).pick_files() else { return };
        let Some(tab) = self.live_tab(id) else { return };
        for f in files {
            let f = f.display().to_string();
            if !tab.spec.proto_files.contains(&f) {
                tab.spec.proto_files.push(f);
            }
        }
        tab.live.protos = None;
    }

    pub(super) fn grpc_load(&mut self, id: TabId, ctx: &egui::Context) {
        let (agent, tokens, default) = (self.agent.clone(), self.auth.clone(), self.config.auth.clone());
        let Some(tab) = self.live_tab(id) else { return };
        let spec = tab.spec.clone();
        let (tx, rx) = mpsc::channel();
        tab.live.protos_rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = if spec.proto_files.is_empty() {
                headers_with_auth(&agent, &tokens, &default, &spec)
                    .and_then(|h| crate::grpc::Protos::from_reflection(&live_url(&spec, false), &h, Duration::from_secs(15)))
            } else {
                let files: Vec<std::path::PathBuf> = spec.proto_files.iter().map(|f| model::expand_home(f)).collect();
                let includes: Vec<std::path::PathBuf> = spec.proto_includes.iter().map(|f| model::expand_home(f)).collect();
                crate::grpc::Protos::from_files(&files, &includes)
            };
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    pub(super) fn grpc_call(&mut self, id: TabId, ctx: &egui::Context) {
        let (agent, tokens, default) = (self.agent.clone(), self.auth.clone(), self.config.auth.clone());
        let Some(tab) = self.live_tab(id) else { return };
        let Some(Ok(protos)) = tab.live.protos.clone() else { return };
        let spec = tab.spec.clone();
        let (tx, rx) = mpsc::channel();
        tab.live.call_rx = Some(rx);
        tab.live.streamed.clear();
        tab.live.result = None;
        tab.live.started = Some(std::time::Instant::now());
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let body = crate::env::expand(&spec.raw_body);
            let streaming = protos.method(&spec.grpc_method).is_some_and(|m| m.client_streaming);
            // Client / bidi streaming: a JSON array is one message per element.
            let messages = match serde_json::from_str::<serde_json::Value>(body.trim()) {
                Ok(serde_json::Value::Array(a)) if streaming => a.iter().map(|v| v.to_string()).collect(),
                _ if body.trim().is_empty() => Vec::new(),
                _ => vec![body],
            };
            let result = headers_with_auth(&agent, &tokens, &default, &spec).and_then(|metadata| {
                let req =
                    crate::grpc::CallRequest { url: live_url(&spec, false), method_path: spec.grpc_method.clone(), messages, metadata, timeout: Duration::from_secs(60) };
                let tx2 = tx.clone();
                let wake = ctx.clone();
                crate::grpc::call(&protos, &req, &mut |m| {
                    let _ = tx2.send(GrpcEvent::Message(m.to_owned()));
                    wake.request_repaint();
                })
            });
            let _ = tx.send(GrpcEvent::Done(result));
            ctx.request_repaint();
        });
    }
}
