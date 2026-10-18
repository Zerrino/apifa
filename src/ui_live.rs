//! WebSocket and gRPC tabs: a saved request whose `protocol` isn't HTTP. The top card edits it
//! (URL, headers / metadata, auth, messages), the lower panel shows what happened.

use std::sync::mpsc::Receiver;
use std::time::SystemTime;

use egui::{vec2, Button, CollapsingHeader, ComboBox, FontId, Margin, RichText, ScrollArea, TextEdit, Ui};

use crate::app::{Action, TabId};
use crate::model::Protocol;
use crate::theme;
use crate::ui_request::ReqState;
use crate::widgets::*;

/// An open WebSocket and its events.
pub type Connection = (crate::ws::WsHandle, Receiver<crate::ws::WsEvent>);

/// What a live tab holds while it's open (not saved).
#[derive(Default)]
pub struct LiveState {
    // WebSocket
    pub ws: Option<crate::ws::WsHandle>,
    pub ws_rx: Option<Receiver<crate::ws::WsEvent>>,
    /// The handle, coming from the thread that got the auth headers and connected.
    pub ws_handle_rx: Option<Receiver<Result<Connection, String>>>,
    pub ws_log: Vec<crate::ws::WsEvent>,
    pub draft: String,
    pub binary: bool,
    // gRPC
    pub protos: Option<Result<crate::grpc::Protos, String>>,
    pub protos_rx: Option<Receiver<Result<crate::grpc::Protos, String>>>,
    pub call_rx: Option<Receiver<GrpcEvent>>,
    pub streamed: Vec<String>,
    pub result: Option<Result<crate::grpc::CallResult, String>>,
    pub started: Option<std::time::Instant>,
}

pub enum GrpcEvent {
    Message(String),
    Done(Result<crate::grpc::CallResult, String>),
}

impl LiveState {
    /// Takes what the threads sent; true when something changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.ws_handle_rx {
            if let Ok(r) = rx.try_recv() {
                self.ws_handle_rx = None;
                changed = true;
                match r {
                    Ok((h, events)) => {
                        self.ws = Some(h);
                        self.ws_rx = Some(events);
                    }
                    Err(e) => self.ws_log.push(crate::ws::WsEvent::Error(e)),
                }
            }
        }
        if let Some(rx) = &self.ws_rx {
            while let Ok(ev) = rx.try_recv() {
                changed = true;
                let closed = matches!(ev, crate::ws::WsEvent::Closed { .. });
                self.ws_log.push(ev);
                if closed {
                    self.ws = None;
                }
            }
        }
        if let Some(rx) = &self.protos_rx {
            if let Ok(r) = rx.try_recv() {
                self.protos = Some(r);
                self.protos_rx = None;
                changed = true;
            }
        }
        if let Some(rx) = &self.call_rx {
            while let Ok(ev) = rx.try_recv() {
                changed = true;
                match ev {
                    GrpcEvent::Message(m) => self.streamed.push(m),
                    GrpcEvent::Done(r) => {
                        self.result = Some(r);
                        self.call_rx = None;
                        break;
                    }
                }
            }
        }
        changed
    }

    pub fn busy(&self) -> bool {
        self.ws_handle_rx.is_some() || self.call_rx.is_some() || self.protos_rx.is_some()
    }

    pub fn connected(&self) -> bool {
        self.ws.as_ref().is_some_and(|h| h.is_open())
    }
}

/// The protocol picker of the URL bar: the HTTP verbs, then WebSocket and gRPC.
pub fn protocol_picker(ui: &mut Ui, tab: &mut ReqState) {
    let (label, color) = match tab.spec.protocol {
        Protocol::Http => (tab.spec.verb.clone(), theme::verb_color(&tab.spec.verb)),
        Protocol::WebSocket => ("WS".to_owned(), theme::CYAN),
        Protocol::Grpc => ("gRPC".to_owned(), theme::GREEN),
    };
    ComboBox::from_id_salt(("verb", tab.id)).width(96.0).selected_text(RichText::new(label).family(theme::bold()).color(color)).show_ui(ui, |ui| {
        card_popup(ui);
        for v in crate::model::VERBS {
            if ui.selectable_label(tab.spec.protocol.is_http() && tab.spec.verb == v, RichText::new(v).family(theme::bold()).color(theme::verb_color(v))).clicked() {
                tab.spec.protocol = Protocol::Http;
                tab.spec.verb = v.to_owned();
            }
        }
        ui.separator();
        if ui.selectable_label(tab.spec.protocol == Protocol::WebSocket, RichText::new("WebSocket").family(theme::bold()).color(theme::CYAN)).clicked() {
            tab.spec.protocol = Protocol::WebSocket;
        }
        if ui.selectable_label(tab.spec.protocol == Protocol::Grpc, RichText::new("gRPC").family(theme::bold()).color(theme::GREEN)).clicked() {
            tab.spec.protocol = Protocol::Grpc;
        }
    });
}

fn url_bar(ui: &mut Ui, tab: &mut ReqState, hint: &str, button: (&str, egui::Color32, bool), action: Action, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        protocol_picker(ui, tab);
        let w = (ui.available_width() - 120.0).max(200.0);
        ui.add(TextEdit::singleline(&mut tab.spec.path).font(FontId::monospace(14.0)).desired_width(w).hint_text(hint_rt(hint)).margin(Margin::symmetric(8, 6)));
        let (label, fill, enabled) = button;
        let b = Button::new(RichText::new(label).family(theme::bold()).size(15.0).color(theme::WHITE)).fill(fill).corner_radius(4).min_size(vec2(110.0, 32.0));
        if ui.add_enabled(enabled, b).clicked() {
            actions.push(action);
        }
    });
}

fn auth_and_headers(ui: &mut Ui, tab: &mut ReqState, headers_title: &str, actions: &mut Vec<Action>) {
    let n = tab.spec.headers.iter().filter(|h| h.enabled && !h.key.trim().is_empty()).count();
    CollapsingHeader::new(RichText::new(if n > 0 { format!("{headers_title} ({n})") } else { headers_title.to_owned() }).family(theme::bold()))
        .id_salt(("live_h", tab.id))
        .default_open(n > 0)
        .show(ui, |ui| kv_editor(ui, &mut tab.spec.headers, ("live_headers", tab.id)));
    let label = if tab.spec.auth.is_inherit() { "Auth".to_owned() } else { format!("Auth · {}", tab.spec.auth.label()) };
    CollapsingHeader::new(RichText::new(label).family(theme::bold())).id_salt(("live_auth", tab.id)).show(ui, |ui| {
        let show_id = egui::Id::unique(("auth_show", tab.id));
        let mut show = ui.data(|d| d.get_temp::<bool>(show_id)).unwrap_or(false);
        let mut get = false;
        crate::ui_auth::auth_editor(ui, &mut tab.spec.auth, Some("the workspace's authentication (Settings)"), &mut show, &mut get, ("live_auth_ed", tab.id));
        ui.data_mut(|d| d.insert_temp(show_id, show));
        if get {
            actions.push(Action::FetchToken(tab.spec.auth.clone()));
        }
    });
}

// ---------------------------------------------------------------- WebSocket

pub fn ws_editor(ui: &mut Ui, tab: &mut ReqState, tab_id: TabId, actions: &mut Vec<Action>) {
    let connected = tab.live.connected();
    let connecting = tab.live.ws_handle_rx.is_some();
    let button = if connected {
        ("Disconnect", theme::RED, true)
    } else if connecting {
        ("Connecting…", theme::CYAN, false)
    } else {
        ("Connect", theme::CYAN, true)
    };
    let action = if connected { Action::WsDisconnect(tab_id) } else { Action::WsConnect(tab_id) };
    url_bar(ui, tab, "wss://echo.websocket.org   (or /path: the base URL with ws:// / wss://)", button, action, actions);
    ui.add_space(4.0);
    auth_and_headers(ui, tab, "Headers", actions);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Subprotocols").color(theme::MUTED));
        let mut text = tab.spec.ws_subprotocols.join(", ");
        if ui.add(TextEdit::singleline(&mut text).hint_text("graphql-ws, mqtt…").desired_width(300.0)).changed() {
            tab.spec.ws_subprotocols = text.split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect();
        }
    });
    ui.add_space(6.0);
    section(ui, "Message", "Text (JSON is fine) or, with Binary, hex bytes. Ctrl+Enter sends. Sent messages are kept below (saved with the request).");
    ui.add(
        TextEdit::multiline(&mut tab.live.draft)
            .id(egui::Id::unique(("ws_draft", tab.id)))
            .code_editor()
            .desired_rows(5)
            .desired_width(f32::INFINITY)
            .hint_text(hint_rt("{\"type\": \"subscribe\", \"channel\": \"…\"}")),
    );
    let send_key = ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
    ui.horizontal(|ui| {
        let send = Button::new(RichText::new("Send").family(theme::bold()).color(theme::WHITE)).fill(theme::CYAN);
        if (ui.add_enabled(connected, send).clicked() || (send_key && connected)) && !tab.live.draft.is_empty() {
            actions.push(Action::WsSend(tab_id, tab.live.draft.clone(), tab.live.binary));
            let m = tab.live.draft.clone();
            tab.spec.ws_messages.retain(|x| *x != m);
            tab.spec.ws_messages.insert(0, m);
            tab.spec.ws_messages.truncate(30);
        }
        ui.checkbox(&mut tab.live.binary, "Binary (hex)");
        if ui.small_button("Ping").clicked() {
            if let Some(h) = &tab.live.ws {
                h.ping();
            }
        }
        if serde_json::from_str::<serde_json::Value>(tab.live.draft.trim()).is_ok() && ui.small_button("Beautify").clicked() {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(tab.live.draft.trim()) {
                tab.live.draft = serde_json::to_string_pretty(&v).unwrap_or_default();
            }
        }
    });
    if !tab.spec.ws_messages.is_empty() {
        CollapsingHeader::new(RichText::new(format!("Saved messages ({})", tab.spec.ws_messages.len())).family(theme::bold())).id_salt(("ws_saved", tab.id)).show(
            ui,
            |ui| {
                let mut remove = None;
                for (i, m) in tab.spec.ws_messages.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.small_button("Use").clicked() {
                            tab.live.draft = m.clone();
                        }
                        if ui.small_button("✕").on_hover_text("Forget it").clicked() {
                            remove = Some(i);
                        }
                        ui.label(RichText::new(crate::widgets::truncate(&m.replace('\n', " "), 90)).monospace().size(12.0).color(theme::MUTED));
                    });
                }
                if let Some(i) = remove {
                    tab.spec.ws_messages.remove(i);
                }
            },
        );
    }
}

fn when(t: &SystemTime) -> String {
    let local: chrono::DateTime<chrono::Local> = (*t).into();
    local.format("%H:%M:%S%.3f").to_string()
}

fn show_data(d: &crate::ws::WsData) -> String {
    match d {
        crate::ws::WsData::Text(t) => match serde_json::from_str::<serde_json::Value>(t) {
            Ok(v) if t.len() < 20_000 => serde_json::to_string_pretty(&v).unwrap_or_else(|_| t.clone()),
            _ => t.clone(),
        },
        crate::ws::WsData::Binary(b) => format!("{} bytes: {}", b.len(), b.iter().take(64).map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")),
    }
}

pub fn ws_log(ui: &mut Ui, tab: &mut ReqState) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Messages").family(theme::bold()).size(17.0).color(theme::WHITE));
        if tab.live.connected() {
            pill(ui, "CONNECTED", theme::WHITE, theme::GREEN);
        } else if tab.live.ws_handle_rx.is_some() {
            ui.add(egui::Spinner::new().color(theme::CYAN));
        }
        let (sent, got) = tab.live.ws_log.iter().fold((0, 0), |(s, g), e| match e {
            crate::ws::WsEvent::Sent { .. } => (s + 1, g),
            crate::ws::WsEvent::Received { .. } => (s, g + 1),
            _ => (s, g),
        });
        stat(ui, "Sent", &sent.to_string());
        stat(ui, "Received", &got.to_string());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Clear").clicked() {
                tab.live.ws_log.clear();
            }
        });
    });
    if tab.live.ws_log.is_empty() {
        ui.add_space(20.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("Connect, then send messages: both directions show here.").color(theme::SILVER));
        });
        return;
    }
    ScrollArea::vertical().id_salt(("ws_log", tab.id)).auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
        for e in &tab.live.ws_log {
            let (arrow, color, text, at) = match e {
                crate::ws::WsEvent::Connected { status, protocol, .. } => {
                    ("●", theme::GREEN, format!("Connected (HTTP {status}){}", protocol.as_ref().map(|p| format!(" · protocol {p}")).unwrap_or_default()), None)
                }
                crate::ws::WsEvent::Sent { data, at } => ("↑", theme::CYAN, show_data(data), Some(at)),
                crate::ws::WsEvent::Received { data, at } => ("↓", theme::GREEN, show_data(data), Some(at)),
                crate::ws::WsEvent::Closed { code, reason } => (
                    "■",
                    theme::ORANGE,
                    format!("Closed{}{}", code.map(|c| format!(" · {c}")).unwrap_or_default(), if reason.is_empty() { String::new() } else { format!(" · {reason}") }),
                    None,
                ),
                crate::ws::WsEvent::Error(e) => ("✖", theme::RED, e.clone(), None),
            };
            ui.horizontal_top(|ui| {
                ui.label(RichText::new(arrow).family(theme::bold()).color(color));
                if let Some(at) = at {
                    ui.label(RichText::new(when(at)).monospace().size(11.5).color(theme::CONCRETE));
                }
                ui.add(egui::Label::new(RichText::new(text).monospace().size(12.5).color(theme::CLOUDS)).wrap().selectable(true));
            });
            ui.add_space(2.0);
        }
    });
}

// ---------------------------------------------------------------- gRPC

pub fn grpc_editor(ui: &mut Ui, tab: &mut ReqState, tab_id: TabId, actions: &mut Vec<Action>) {
    let calling = tab.live.call_rx.is_some();
    let ready = tab.live.protos.as_ref().is_some_and(|p| p.is_ok()) && !tab.spec.grpc_method.is_empty();
    let button = if calling { ("Calling…", theme::CYAN, false) } else { ("Invoke", theme::GREEN, ready) };
    url_bar(ui, tab, "https://grpc.example.com:443   (http:// for plaintext h2c)", button, Action::GrpcCall(tab_id), actions);
    ui.add_space(4.0);
    // Where the services come from: .proto files, or the server's reflection.
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Services from").family(theme::bold()));
        let reflection = tab.spec.proto_files.is_empty();
        ui.label(RichText::new(if reflection { "server reflection" } else { ".proto files" }).color(theme::MUTED));
        if ui.button("Add .proto files…").clicked() {
            actions.push(Action::GrpcAddProtos(tab_id));
        }
        let loading = tab.live.protos_rx.is_some();
        if ui
            .add_enabled(!loading, Button::new(if reflection { "Ask the server" } else { "Load" }))
            .on_hover_text(if reflection { "Lists the services with gRPC server reflection" } else { "Reads the .proto files" })
            .clicked()
        {
            actions.push(Action::GrpcLoad(tab_id));
        }
        if loading {
            ui.add(egui::Spinner::new().size(14.0).color(theme::CYAN_DARK));
        }
    });
    if !tab.spec.proto_files.is_empty() {
        let mut remove = None;
        for (i, f) in tab.spec.proto_files.iter().enumerate() {
            ui.horizontal(|ui| {
                if ui.small_button("✕").clicked() {
                    remove = Some(i);
                }
                ui.label(RichText::new(f).monospace().size(12.0).color(theme::MUTED));
            });
        }
        if let Some(i) = remove {
            tab.spec.proto_files.remove(i);
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new("Include folders").color(theme::MUTED));
            let mut text = tab.spec.proto_includes.join(", ");
            if ui.add(TextEdit::singleline(&mut text).hint_text("~/protos, ./third_party").desired_width(360.0)).changed() {
                tab.spec.proto_includes = text.split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect();
            }
        });
    }
    match &tab.live.protos {
        Some(Err(e)) => {
            ui.add(egui::Label::new(RichText::new(e).color(theme::RED)).wrap());
        }
        Some(Ok(protos)) => {
            let services = protos.services();
            let current = protos.method(&tab.spec.grpc_method);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Method").family(theme::bold()));
                let text = current.as_ref().map(|m| format!("{} · {}", m.full_path, m.kind())).unwrap_or_else(|| "Pick a method".into());
                ComboBox::from_id_salt(("grpc_m", tab.id)).selected_text(text).width(460.0).show_ui(ui, |ui| {
                    for s in &services {
                        ui.label(RichText::new(&s.name).family(theme::bold()).color(theme::MUTED));
                        for m in &s.methods {
                            if ui.selectable_label(tab.spec.grpc_method == m.full_path, format!("   {} ({})", m.name, m.kind())).clicked() {
                                tab.spec.grpc_method = m.full_path.clone();
                                if tab.spec.raw_body.trim().is_empty() {
                                    tab.spec.raw_body = protos.example_json(&m.input).unwrap_or_default();
                                }
                            }
                        }
                    }
                });
            });
            if let Some(m) = &current {
                ui.label(RichText::new(format!("{} → {}", m.input, m.output)).monospace().size(12.0).color(theme::MUTED));
                ui.horizontal(|ui| {
                    if ui.small_button("Example message").on_hover_text("Every field at its default").clicked() {
                        tab.spec.raw_body = protos.example_json(&m.input).unwrap_or_default();
                    }
                    if !m.client_streaming {
                        match protos.normalize_json(&m.input, &tab.spec.raw_body) {
                            Ok(_) => {
                                ui.label(RichText::new("✔ fits the message").size(12.0).color(theme::GREEN));
                            }
                            Err(e) if !tab.spec.raw_body.trim().is_empty() => {
                                ui.label(RichText::new(format!("✖ {e}")).size(12.0).color(theme::RED));
                            }
                            Err(_) => {}
                        }
                    }
                });
            }
        }
        None => {
            ui.label(RichText::new("Load the services (above) to pick a method; the message editor then knows its fields.").size(12.5).color(theme::MUTED));
        }
    }
    section(ui, "Message", "JSON (proto3 mapping). Client / bidi streaming: a JSON array, one element per message.");
    ui.add(
        TextEdit::multiline(&mut tab.spec.raw_body)
            .id(egui::Id::unique(("grpc_msg", tab.id)))
            .code_editor()
            .desired_rows(10)
            .desired_width(f32::INFINITY)
            .hint_text(hint_rt("{ \"name\": \"world\" }")),
    );
    auth_and_headers(ui, tab, "Metadata", actions);
}

pub fn grpc_result(ui: &mut Ui, tab: &mut ReqState) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Response").family(theme::bold()).size(17.0).color(theme::WHITE));
        if let Some(Ok(r)) = &tab.live.result {
            let color = if r.status_code == 0 { theme::GREEN } else { theme::RED };
            pill(ui, &format!("{} {}", r.status_code, r.status_name), theme::WHITE, color);
            stat(ui, "Time", &format!("{} ms", r.elapsed.as_millis()));
            stat(ui, "Messages", &r.messages.len().to_string());
        }
        if tab.live.call_rx.is_some() {
            ui.add(egui::Spinner::new().color(theme::CYAN));
            if let Some(t) = tab.live.started {
                ui.label(RichText::new(format!("{:.1}s · {} message(s) so far", t.elapsed().as_secs_f32(), tab.live.streamed.len())).color(theme::SILVER));
            }
        }
    });
    let messages: Vec<String> = match &tab.live.result {
        Some(Ok(r)) => r.messages.clone(),
        _ => tab.live.streamed.clone(),
    };
    ScrollArea::vertical().id_salt(("grpc_res", tab.id)).auto_shrink([false, false]).show(ui, |ui| {
        if let Some(Err(e)) = &tab.live.result {
            ui.add(egui::Label::new(RichText::new(e).color(theme::RED)).wrap());
        }
        if let Some(Ok(r)) = &tab.live.result {
            if !r.status_message.is_empty() {
                ui.add(egui::Label::new(RichText::new(&r.status_message).color(if r.status_code == 0 { theme::CLOUDS } else { theme::RED })).wrap());
            }
            if !r.status_details.is_empty() {
                ui.add(egui::Label::new(RichText::new(&r.status_details).monospace().size(12.0).color(theme::SILVER)).wrap());
            }
        }
        for (i, m) in messages.iter().enumerate() {
            if messages.len() > 1 {
                ui.label(RichText::new(format!("message {}", i + 1)).size(11.5).color(theme::CONCRETE));
            }
            code_block(ui, crate::json_view::highlight(m).into());
        }
        if let Some(Ok(r)) = &tab.live.result {
            for (title, list) in [("Headers", &r.headers), ("Trailers", &r.trailers)] {
                if list.is_empty() {
                    continue;
                }
                CollapsingHeader::new(RichText::new(format!("{title} ({})", list.len())).color(theme::CLOUDS)).id_salt((title, tab.id)).show(ui, |ui| {
                    for (k, v) in list {
                        ui.label(RichText::new(format!("{k}: {v}")).monospace().size(12.0).color(theme::SILVER));
                    }
                });
            }
        }
        if messages.is_empty() && tab.live.result.is_none() && tab.live.call_rx.is_none() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("Pick a method, write its message and Invoke.").color(theme::SILVER));
            });
        }
    });
}
