//! Optional services: mock server, MCP server, AI assistant.

use std::sync::mpsc::{self, Sender, TryRecvError};
use std::time::{Duration, Instant};

use egui::{vec2, Align2, Button, Frame, Margin, RichText, ScrollArea, TextEdit, TextStyle, Ui};

use crate::flow::Flow;
use crate::model::{self, RequestSpec};
use crate::net;
use crate::subst;
use crate::theme;
use crate::workspace::{self, Folder, Node};
use crate::{ai, ui_ai};

use super::*;

/// Said in front of what the AI gets, so it doesn't take fingerprints for real values.
const ANONYMIZED: &str = "Anonymized (no value is ever given: each one is its type and a fingerprint, equal values = equal fingerprints):\n";

impl App {
    /// Starts / stops the mock server to follow the settings, and points the app's requests at it.
    pub(super) fn mock_sync(&mut self, ctx: &egui::Context) {
        let want = self.config.mock_enabled;
        match (&self.mock.server, want) {
            (Some(s), true) if s.port == self.config.mock_port => {}
            (None, false) => {}
            (_, false) => self.mock.server = None,
            (_, true) => {
                self.mock.server = None;
                let ctx2 = ctx.clone();
                match crate::mock::start(self.config.mock_port, self.mock.mocks.clone(), move |_| ctx2.request_repaint()) {
                    Ok(s) => {
                        self.mock.server = Some(s);
                        self.mock.error = None;
                    }
                    Err(e) => {
                        self.mock.error = Some(e);
                        self.config.mock_enabled = false;
                    }
                }
            }
        }
        let target = self.mock.server.as_ref().filter(|_| self.config.mock_target).map(|s| s.url());
        if target != net::mock_target() {
            net::set_mock_target(target);
        }
    }

    pub(super) fn mock_settings(&mut self, ui: &mut Ui) -> bool {
        let mut changed = false;
        ui.label(RichText::new("MOCK SERVER").family(theme::bold()).size(12.5).color(theme::CYAN));
        ui.label(
            RichText::new("A local stand-in for your API: answers with your saved requests' mocks (response › Save as mock, or a request's Mock tab), else the docs' examples, else the examples recorded from real calls. Lists are paginated like the API. Work on scripts, pages and flows without the network or the rate limit. Local only (127.0.0.1).")
                .size(12.5)
                .color(theme::SILVER),
        );
        ui.horizontal(|ui| {
            changed |= ui.checkbox(&mut self.config.mock_enabled, "Turn on the mock server").changed();
            ui.add_space(12.0);
            ui.label("Port");
            let edit = self.mock.port_edit.get_or_insert(self.config.mock_port);
            ui.add(egui::DragValue::new(edit).range(1024..=65535).speed(1));
            if *edit != self.config.mock_port && ui.button("Apply").clicked() {
                self.config.mock_port = *edit;
                self.mock.error = None;
                changed = true;
            }
        });
        if let Some(e) = &self.mock.error {
            ui.add(egui::Label::new(RichText::new(format!("{e} — pick another port, then turn it on again")).size(12.0).color(theme::RED)).wrap());
        }
        let Some(server) = &self.mock.server else { return changed };
        let st = server.status.lock().clone();
        let url = server.url();
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("● {url}")).color(theme::GREEN));
            if ui.small_button("Copy").clicked() {
                ui.ctx().copy_text(url.clone());
            }
            let text = match &st.last {
                Some((at, line)) => format!("· {} answered, last: {line} {}s ago", st.served, at.elapsed().as_secs()),
                None => "· no request yet".into(),
            };
            ui.label(RichText::new(text).size(12.0).color(theme::SILVER));
        });
        changed |= ui
            .checkbox(&mut self.config.mock_target, "Send the app's requests to the mock server (tabs, flows, tests)")
            .on_hover_text("Nothing goes to the real API while this is on: no token, no rate limit. A MOCK badge shows in the title bar.")
            .changed();
        changed
    }

    /// Starts / stops the MCP server to follow the settings.
    pub(super) fn mcp_sync(&mut self, ctx: &egui::Context) {
        let want = self.config.mcp_enabled && !self.no_save;
        match (&self.mcp.server, want) {
            (Some(s), true) if s.port == self.config.mcp_port => {}
            (None, false) => {}
            (_, false) => self.mcp.server = None,
            (_, true) => {
                self.mcp.server = None;
                if self.config.mcp_token.is_empty() {
                    self.config.mcp_token = crate::mcp::new_token();
                    self.save_config();
                }
                let ctx2 = ctx.clone();
                match crate::mcp::start(self.config.mcp_port, self.config.mcp_token.clone(), self.mcp.tx.clone(), move || ctx2.request_repaint()) {
                    Ok(s) => {
                        self.mcp.server = Some(s);
                        self.mcp.error = None;
                    }
                    Err(e) => {
                        // Don't retry every frame: off until the user changes something.
                        self.mcp.error = Some(e);
                        self.config.mcp_enabled = false;
                    }
                }
            }
        }
    }

    /// Runs the agents' tool calls; requests to send wait for the user.
    pub(super) fn mcp_poll(&mut self, ctx: &egui::Context) {
        while let Ok(call) = self.mcp.rx.try_recv() {
            if !self.config.mcp_enabled {
                let _ = call.reply.send(("The MCP server was turned off in the app.".into(), true));
            } else if ai::NEEDS_CONFIRMATION.contains(&call.name.as_str()) {
                self.mcp.confirm.push_back(call);
            } else {
                let (text, is_error) = self.ai_tool(&call.name, &call.args);
                let _ = call.reply.send((text, is_error));
            }
        }
        // Nobody waits for these any more: sending them now would act without anyone getting the result.
        self.mcp.confirm.retain(|c| c.deadline > Instant::now());
        if !self.mcp.confirm.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    pub(super) fn mcp_confirm_window(&mut self, ctx: &egui::Context) {
        let Some(call) = self.mcp.confirm.front() else { return };
        let client = self.mcp.server.as_ref().and_then(|s| s.status.lock().client.clone()).unwrap_or_else(|| "An external agent".into());
        let mut decision = None;
        egui::Window::new(RichText::new("Send this request?").family(theme::bold()).size(17.0))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_width(520.0);
                ui.label(RichText::new(format!("{client} (MCP) wants to send this request with your credentials:")).color(theme::SILVER));
                if let Some(why) = call.args.get("why").and_then(serde_json::Value::as_str) {
                    ui.add(egui::Label::new(RichText::new(why).color(theme::CLOUDS)).wrap());
                }
                Frame::new().fill(theme::CODE_BG).corner_radius(4).inner_margin(Margin::same(8)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(egui::Label::new(RichText::new(ui_ai::confirm_text(&call.args)).monospace().color(theme::WHITE)).wrap().selectable(true));
                });
                if self.mcp.confirm.len() > 1 {
                    ui.label(RichText::new(format!("{} more waiting", self.mcp.confirm.len() - 1)).size(12.0).color(theme::SILVER));
                }
                ui.horizontal(|ui| {
                    if ui.add(Button::new(RichText::new("Send it").family(theme::bold()).color(theme::WHITE)).fill(theme::CYAN)).clicked() {
                        decision = Some(true);
                    }
                    if ui.button("Don't send").clicked() {
                        decision = Some(false);
                    }
                });
            });
        if let Some(ok) = decision {
            let call = self.mcp.confirm.pop_front().expect("shown above");
            if ok {
                self.spawn_send(&call.args, call.reply, ctx);
            } else {
                let _ = call.reply.send(("The user chose not to send this request.".into(), true));
            }
        }
    }

    pub(super) fn mcp_settings(&mut self, ui: &mut Ui) -> bool {
        let mut changed = false;
        ui.label(RichText::new("MCP SERVER (EXTERNAL AI AGENTS)").family(theme::bold()).size(12.5).color(theme::CYAN));
        ui.label(
            RichText::new("Lets AI agents on this computer (Claude Code, Cursor, Codex…) use the app like the built-in assistant: search the docs, read your workspace, open requests / flows / substitutions in tabs. Sending a request always asks you first. Local only (127.0.0.1), with an access key.")
                .size(12.5)
                .color(theme::SILVER),
        );
        ui.horizontal(|ui| {
            changed |= ui.checkbox(&mut self.config.mcp_enabled, "Turn on the MCP server").changed();
            ui.add_space(12.0);
            ui.label("Port");
            let edit = self.mcp.port_edit.get_or_insert(self.config.mcp_port);
            ui.add(egui::DragValue::new(edit).range(1024..=65535).speed(1)).on_hover_text("1024–65535. Agents already set up need the new address.");
            if *edit != self.config.mcp_port {
                if ui.button("Apply").on_hover_text("Restart the server on this port").clicked() {
                    self.config.mcp_port = *edit;
                    self.mcp.error = None;
                    changed = true;
                }
                if ui.small_button("Cancel").clicked() {
                    self.mcp.port_edit = Some(self.config.mcp_port);
                }
            } else if self.config.mcp_port != crate::mcp::DEFAULT_PORT
                && ui.small_button("Default").on_hover_text(format!("Back to {}", crate::mcp::DEFAULT_PORT)).clicked()
            {
                self.config.mcp_port = crate::mcp::DEFAULT_PORT;
                self.mcp.port_edit = Some(crate::mcp::DEFAULT_PORT);
                changed = true;
            }
        });
        if let Some(e) = &self.mcp.error {
            ui.add(egui::Label::new(RichText::new(format!("{e} — pick another port, then turn the server on again")).size(12.0).color(theme::RED)).wrap());
        }
        let Some(server) = &self.mcp.server else { return changed };
        let st = server.status.lock().clone();
        let port = server.port;
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("● listening on http://127.0.0.1:{port}/mcp")).color(theme::GREEN));
            match &st.last {
                Some((at, tool)) => ui.label(
                    RichText::new(format!(
                        "· {} call(s), last: {tool} {}s ago{}",
                        st.calls,
                        at.elapsed().as_secs(),
                        st.client.as_ref().map(|c| format!(" ({c})")).unwrap_or_default()
                    ))
                    .size(12.0)
                    .color(theme::SILVER),
                ),
                None => ui.label(RichText::new("· no call yet").size(12.0).color(theme::SILVER)),
            };
        });
        ui.horizontal(|ui| {
            ui.label("Access key");
            let shown = if self.mcp.show_token { self.config.mcp_token.clone() } else { "•".repeat(16) };
            ui.label(RichText::new(shown).monospace());
            if ui.small_button(if self.mcp.show_token { "Hide" } else { "Show" }).clicked() {
                self.mcp.show_token = !self.mcp.show_token;
            }
            if ui.small_button("Copy").clicked() {
                ui.ctx().copy_text(self.config.mcp_token.clone());
            }
            if ui.small_button("New key").on_hover_text("Agents set up with the old key stop working: copy their setup again").clicked() {
                self.config.mcp_token = crate::mcp::new_token();
                self.mcp.server = None; // restarted with the new key next frame
                changed = true;
            }
        });

        // Ready-made setup per client, the key written in.
        ui.add_space(4.0);
        ui.label(RichText::new("Connect an agent").family(theme::bold()));
        let token = self.config.mcp_token.clone();
        let setups = crate::mcp::clients(port, &token);
        self.mcp.client = self.mcp.client.min(setups.len() - 1);
        ui.horizontal_wrapped(|ui| {
            for (i, c) in setups.iter().enumerate() {
                if ui.selectable_label(self.mcp.client == i, c.name).clicked() {
                    self.mcp.client = i;
                }
            }
        });
        let setup = &setups[self.mcp.client];
        ui.label(RichText::new(setup.place).size(12.5).color(theme::SILVER));
        // Shown with the key hidden unless "Show"; copied with the key.
        let shown = if self.mcp.show_token { setup.snippet.clone() } else { setup.snippet.replace(&token, "••••••••") };
        Frame::new().fill(theme::CODE_BG).corner_radius(4).inner_margin(Margin::same(8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(RichText::new(shown).monospace().size(11.5).color(theme::CLOUDS)).wrap());
        });
        ui.horizontal(|ui| {
            let copy = Button::new(RichText::new(format!("Copy for {}", setup.name)).family(theme::bold()).color(theme::WHITE)).fill(theme::CYAN);
            if ui.add(copy).on_hover_text("Copied with your access key").clicked() {
                ui.ctx().copy_text(setup.snippet.clone());
                self.toast(format!("Copied the {} setup (with your key)", setup.name), theme::GREEN);
            }
            ui.label(RichText::new("Contains your access key: don't share it.").size(11.5).color(theme::CONCRETE));
        });
        changed
    }

    pub(super) fn ai_settings(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) -> bool {
        let mut changed = false;
        ui.label(RichText::new("AI ASSISTANT").family(theme::bold()).size(12.5).color(theme::CYAN));
        ui.label(
            RichText::new("Use your own API key. The AI button appears at the top once a key is set; the assistant asks before sending anything to an API.")
                .size(12.5)
                .color(theme::SILVER),
        );
        let ai = &mut self.config.ai;
        let provider = ai.provider();
        egui::Grid::new("aicfg").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
            ui.label("Provider");
            egui::ComboBox::from_id_salt("ai_provider").width(300.0).selected_text(provider.label()).show_ui(ui, |ui| {
                for p in ai::Provider::ALL {
                    if ui.selectable_label(p == provider, p.label()).clicked() && p != provider {
                        ai.provider = p.id().to_owned();
                        changed = true;
                        self.ai_setup.models.clear();
                        self.ai_setup.models_error = None;
                    }
                }
            });
            ui.end_row();
            let provider = ai.provider();
            ui.label("API key");
            ui.horizontal(|ui| {
                let key = ai.keys.entry(provider.id().to_owned()).or_default();
                changed |= ui
                    .add_sized([300.0, 24.0], TextEdit::singleline(key).font(TextStyle::Monospace).password(!self.ai_setup.show_key).hint_text(provider.key_help()))
                    .changed();
                if ui.small_button(if self.ai_setup.show_key { "Hide" } else { "Show" }).clicked() {
                    self.ai_setup.show_key = !self.ai_setup.show_key;
                }
            });
            ui.end_row();
            if provider == ai::Provider::Custom {
                ui.label("Base URL");
                changed |= ui.add_sized([300.0, 24.0], TextEdit::singleline(&mut ai.custom_url).font(TextStyle::Monospace).hint_text(provider.base_url())).changed();
                ui.end_row();
            }
            ui.label("Model");
            ui.horizontal(|ui| {
                let model = ai.models.entry(provider.id().to_owned()).or_insert_with(|| provider.default_model().to_owned());
                changed |= ui.add_sized([220.0, 24.0], TextEdit::singleline(model).font(TextStyle::Monospace).hint_text("pick one ›")).changed();
                if !self.ai_setup.models.is_empty() {
                    egui::ComboBox::from_id_salt("ai_models").width(30.0).selected_text("").show_ui(ui, |ui| {
                        ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                            for m in &self.ai_setup.models {
                                if ui.selectable_label(m == model, m).clicked() {
                                    *model = m.clone();
                                    changed = true;
                                }
                            }
                        });
                    });
                }
                let can_load = !ai.keys.get(provider.id()).is_none_or(|k| k.trim().is_empty()) || provider == ai::Provider::Custom;
                if self.ai_setup.models_rx.is_some() {
                    ui.add(egui::Spinner::new().color(theme::CYAN));
                } else if ui.add_enabled(can_load, Button::new("Load models")).on_hover_text("Ask the provider which models your key can use").clicked() {
                    actions.push(Action::AiLoadModels);
                }
            });
            ui.end_row();
        });
        if let Some(e) = &self.ai_setup.models_error {
            ui.add(egui::Label::new(RichText::new(e).size(12.0).color(theme::RED)).wrap());
        }
        if !self.config.ai.ready() && self.chat.open {
            self.chat.open = false;
        }
        changed
    }

    pub(super) fn ai_panel(&mut self, root: &mut Ui, actions: &mut Vec<Action>) {
        if !self.chat.open || !self.config.ai.ready() {
            return;
        }
        let (model, provider) = (self.config.ai.model(), self.config.ai.provider().label());
        egui::Panel::right("ai_chat")
            .resizable(true)
            .default_size(400.0)
            .size_range(300.0..=720.0)
            .frame(Frame::new().fill(theme::NAVY_DEEP).inner_margin(Margin::same(10)))
            .show(root, |ui| ui_ai::panel(ui, &mut self.chat, &model, provider, actions));
    }

    /// Sends the conversation to the model (on a thread).
    pub(super) fn ai_call(&mut self, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        self.chat.waiting = Some((rx, Instant::now()));
        let cfg = self.config.ai.clone();
        let history = self.chat.history.clone();
        let today = chrono::Local::now().format("%Y-%m-%d (%A)").to_string();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let r = ai::complete(&cfg, &ai::system_prompt(&today), &ai::tools(), &history);
            let _ = tx.send(r);
            ctx.request_repaint();
        });
    }

    /// Answers from the model, tool results, next call.
    pub(super) fn ai_poll(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.ai_setup.models_rx {
            if let Ok(r) = rx.try_recv() {
                self.ai_setup.models_rx = None;
                match r {
                    Ok(models) if models.is_empty() => self.ai_setup.models_error = Some("The provider returned no model".into()),
                    Ok(models) => self.ai_setup.models = models,
                    Err(e) => self.ai_setup.models_error = Some(e),
                }
            }
        }
        if let Some((rx, _)) = &self.chat.waiting {
            match rx.try_recv() {
                Ok(Ok(reply)) => {
                    self.chat.waiting = None;
                    self.chat.last_usage = reply.usage.or(self.chat.last_usage);
                    self.chat.queue = reply.message.tool_calls().into_iter().collect();
                    // An empty answer replayed later is refused by the providers ("non-empty content"), breaking the chat.
                    let empty_raw = reply.message.raw.as_ref().is_none_or(|(_, v)| v.as_array().is_some_and(|a| a.is_empty()) || v.is_null());
                    if !(reply.message.parts.is_empty() && empty_raw) {
                        self.chat.history.push(reply.message);
                    }
                    if let Some(w) = reply.warning {
                        self.chat.notice(w, theme::ORANGE);
                    }
                }
                Ok(Err(e)) => {
                    self.chat.waiting = None;
                    self.chat.notice(e, theme::RED);
                }
                Err(TryRecvError::Empty) => ctx.request_repaint_after(Duration::from_millis(250)),
                Err(TryRecvError::Disconnected) => {
                    self.chat.waiting = None;
                    self.chat.notice("The assistant stopped unexpectedly.", theme::RED);
                }
            }
        }
        if let Some((id, rx)) = &self.chat.tool_wait {
            if let Ok((content, is_error)) = rx.try_recv() {
                let id = id.clone();
                self.chat.tool_wait = None;
                self.chat.results.push(ai::Part::ToolResult { id, content, is_error });
            }
        }
        // Run the tool calls in order; a request to send waits for the user.
        while self.chat.confirm.is_none() && self.chat.tool_wait.is_none() {
            let Some((id, name, input)) = self.chat.queue.pop_front() else { break };
            if ai::NEEDS_CONFIRMATION.contains(&name.as_str()) {
                self.chat.confirm = Some((id, name, input));
                break;
            }
            let (content, is_error) = self.ai_tool(&name, &input);
            self.chat.results.push(ai::Part::ToolResult { id, content, is_error });
        }
        let idle = self.chat.waiting.is_none() && self.chat.confirm.is_none() && self.chat.tool_wait.is_none() && self.chat.queue.is_empty();
        if idle && !self.chat.results.is_empty() {
            let results = std::mem::take(&mut self.chat.results);
            self.chat.history.push(ai::Message::user(results));
            self.ai_call(ctx);
        }
    }

    /// A request described by the assistant (verb, path, path_values, query, body, files…).
    pub(super) fn ai_spec(&self, input: &serde_json::Value) -> RequestSpec {
        let s = |k: &str| input.get(k).and_then(serde_json::Value::as_str).unwrap_or_default().trim().to_owned();
        let verb = s("verb").to_uppercase();
        let verb = if model::VERBS.contains(&verb.as_str()) { verb } else { "GET".into() };
        let path = s("path");
        let path = path.to_owned();
        let kv = |pairs: Vec<(String, String)>| pairs.into_iter().map(|(key, value)| model::Kv { enabled: true, key, value }).collect::<Vec<_>>();
        let body = ai::body_text(input.get("body"));
        let protocol: model::Protocol = input.get("protocol").and_then(|p| serde_json::from_value(p.clone()).ok()).unwrap_or_default();
        let auth: crate::auth::Auth = input.get("auth").and_then(|a| serde_json::from_value(a.clone()).ok()).unwrap_or_default();
        let graphql = input.get("graphql").filter(|g| g.is_object());
        let (body_mode, body, graphql_variables) = match graphql {
            Some(g) => (
                model::BodyMode::GraphQl,
                g.get("query").and_then(serde_json::Value::as_str).unwrap_or_default().to_owned(),
                g.get("variables").filter(|v| !v.is_null()).map(|v| serde_json::to_string_pretty(v).unwrap_or_default()).unwrap_or_default(),
            ),
            None => (model::BodyMode::Raw, body, String::new()),
        };
        RequestSpec {
            protocol,
            auth,
            graphql_variables,
            headers: kv(ai::pairs(input.get("headers"))),
            pre_script: s("pre_script"),
            post_script: s("post_script"),
            grpc_method: s("grpc_method"),
            doc: self.doc.match_route(&verb, &path),
            verb,
            path,
            path_values: ai::string_map(input.get("path_values")),
            query: kv(ai::pairs(input.get("query"))),
            files: kv(ai::pairs(input.get("files"))),
            body_mode,
            raw_body: body,
            visualizer: s("visualizer"),
            ..Default::default()
        }
    }

    /// The confirmed send_request: runs on a thread, the result goes back to the model.
    pub(super) fn ai_send_request(&mut self, id: String, input: &serde_json::Value, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        self.chat.tool_wait = Some((id, rx));
        self.spawn_send(input, tx, ctx);
    }

    /// Sends a request described by an assistant or an agent (after the user's OK); the result text goes to `tx`.
    pub(super) fn spawn_send(&mut self, input: &serde_json::Value, tx: Sender<(String, bool)>, ctx: &egui::Context) {
        let spec = self.ai_spec(&crate::privacy::reveal(input));
        let method = spec.doc.as_ref().and_then(|d| self.doc.lookup(d)).map(|(_, m)| m);
        let prepared = model::prepare(&spec, method);
        if let Some(e) =
            prepared.body_error.clone().or_else(|| (!prepared.missing_path.is_empty()).then(|| format!("missing path variables: {}", prepared.missing_path.join(", "))))
        {
            let _ = tx.send((e, true));
            return;
        }
        let (agent, auth, creds) = (self.agent.clone(), self.auth.clone(), self.credentials());
        let (rules, cache, doc, ctx) = (self.subs_rules.clone(), self.subs_cache.clone(), self.doc.clone(), ctx.clone());
        let learned = self.learned.clone();
        std::thread::spawn(move || {
            let mut spec = spec;
            let mut exec = |p: &model::Prepared| net::execute(&agent, &auth, &creds, p);
            let result = subst::apply_spec(&mut spec, &rules, &mut |r, v| cache.resolve(r, v, &mut exec)).and_then(|notes| {
                let method = spec.doc.as_ref().and_then(|d| doc.lookup(d)).map(|(_, m)| m);
                let prepared = if notes.is_empty() { prepared } else { model::prepare(&spec, method) };
                let recorded = net::execute(&agent, &auth, &creds, &prepared).map(|r| {
                    let recorded = spec
                        .doc
                        .as_ref()
                        .and_then(|d| Some((d, doc.lookup(d)?.1)))
                        .is_some_and(|(d, m)| crate::learned::record_shared(&learned, d, m, &spec.verb, &prepared.url, &r));
                    (r, notes, recorded)
                });
                recorded
            });
            // What the AI gets: the status and the structure of the answer, never a value (privacy.rs).
            let out = match result {
                Ok((r, notes, recorded)) => {
                    let mut text = format!("HTTP {} {}\n", r.status, r.status_text);
                    if recorded {
                        text.push_str(
                            "(The docs had no example response for this endpoint: a sample of this one was saved; endpoint_details will show it from now on.)\n",
                        );
                    }
                    if !notes.is_empty() {
                        text.push_str(&format!("({} value(s) looked up by substitutions before sending)\n", notes.len()));
                    }
                    text.push_str(ANONYMIZED);
                    text.push_str(&ai::clip(&crate::privacy::body(&r.body), 20_000));
                    (text, r.status >= 400)
                }
                Err(e) => (crate::privacy::message(&e), true),
            };
            let _ = tx.send((crate::privacy::mask_text(&out.0), out.1));
            ctx.request_repaint();
        });
    }

    /// Runs one of the assistant's tools (except send_request); returns (result, is_error).
    /// The result never holds a value of the user's data: responses, examples, saved values are
    /// given as their structure (privacy.rs). The assistant and the MCP clients both get this.
    pub(super) fn ai_tool(&mut self, name: &str, input: &serde_json::Value) -> (String, bool) {
        // The fingerprints the AI writes back are values again here; what goes out hides the logins it
        // may still quote (a saved item's name, a folder).
        let (text, is_error) = self.run_ai_tool(name, &crate::privacy::reveal(input));
        (crate::privacy::mask_text(&text), is_error)
    }

    fn run_ai_tool(&mut self, name: &str, input: &serde_json::Value) -> (String, bool) {
        let vocab = crate::privacy::vocabulary(&self.doc);
        let s = |k: &str| input.get(k).and_then(serde_json::Value::as_str).unwrap_or_default().trim().to_owned();
        match name {
            "read_docs" => (ai::read_docs(&s("section")), false),
            "data_map" => match crate::datamap::load() {
                None => ("No data map yet: the user builds it with Understand (Data map tab, Workspace › Data map; or apifa understand).".into(), false),
                Some(map) => (ai::clip(&crate::datamap::markdown_for(&map, &s("entity"), &s("field")), 30_000), false),
            },
            "search_endpoints" => {
                let mut out = Vec::new();
                for hit in self.doc.search(&s("query")) {
                    let res = &self.doc.resources[hit.resource];
                    for (mi, ri) in hit.routes {
                        let m = &res.methods[mi];
                        let r = &m.routes[ri];
                        out.push(format!("{} {}  — {} › {}{}", r.verb, r.path, res.name, m.name, if r.deprecated { " (deprecated)" } else { "" }));
                    }
                }
                if out.is_empty() {
                    return ("No endpoint matches. Try other words (singular / plural, resource names).".into(), false);
                }
                let n = out.len();
                out.truncate(40);
                (format!("{n} endpoints{}:\n{}", if n > 40 { " (first 40)" } else { "" }, out.join("\n")), false)
            }
            "endpoint_details" => {
                let (verb, path) = (s("verb").to_uppercase(), s("path"));
                let Some(d) = self.doc.match_route(&verb, &path) else {
                    return (format!("{verb} {path} isn't documented. Use search_endpoints."), true);
                };
                let Some((res, m)) = self.doc.lookup(&d) else { return ("not found".into(), true) };
                let route = &m.routes[d.route];
                fn walk(params: &[&crate::docs::Param], depth: usize, out: &mut String) {
                    for p in params {
                        let desc: String = p.description.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(180).collect();
                        let opts = if p.options.is_empty() { String::new() } else { format!(" one of: {}", p.options.join(", ")) };
                        out.push_str(&format!(
                            "{}- {} ({}{}){opts} {desc}\n",
                            "  ".repeat(depth),
                            p.full_name,
                            p.kind.label(),
                            if p.required { ", REQUIRED" } else { "" }
                        ));
                        if p.is_group() {
                            walk(&p.children.iter().collect::<Vec<_>>(), depth + 1, out);
                        }
                    }
                }
                let mut out = format!("{} {}  — {} › {}\n", route.verb, route.path, res.name, m.name);
                let desc: String = m.description.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(400).collect();
                if !desc.is_empty() {
                    out.push_str(&format!("{desc}\n"));
                }
                let vars: Vec<String> = model::path_vars(&route.path).into_iter().map(|v| format!(":{}", v.name)).collect();
                if !vars.is_empty() {
                    out.push_str(&format!("Path variables: {}\n", vars.join(", ")));
                }
                out.push_str(if model::has_body(&route.verb) { "Body fields:\n" } else { "Query parameters:\n" });
                walk(&model::field_params(m, &route.path), 0, &mut out);
                // What a response looks like: the docs' example, or one recorded from a real call.
                let documented = m.examples.iter().find_map(|e| e.response.as_deref().filter(|r| !r.trim().is_empty()));
                match (documented, self.learned.lock().get(&d)) {
                    (Some(resp), _) => {
                        out.push_str(&format!("Example response (from the docs; {}):\n{}\n", ANONYMIZED.trim(), ai::clip(&crate::privacy::body(resp), 4000)))
                    }
                    (None, Some(l)) => out.push_str(&format!(
                        "Example response (recorded from a real call on {}: GET {}; lists cut to 2 items; {}):\n{}\n",
                        crate::learned::when(l),
                        crate::privacy::path(&l.url, &vocab),
                        ANONYMIZED.trim(),
                        ai::clip(&crate::privacy::body(&l.response), 6000)
                    )),
                    (None, None) => out.push_str(
                        "No example response yet: to know the returned fields, get one element (send_request, or open_request with page[size]=1 for the user to Send). It will be saved as this endpoint's example.\n",
                    ),
                }
                if let Some(e) = self.explanations.lock().get(&d) {
                    out.push_str(&format!("\nExplained by AI (written earlier from the docs, an example and the data map):\n{}\n", e.text.trim()));
                }
                (ai::clip(&out, 16_000), false)
            }
            "list_workspace" => {
                fn walk(f: &Folder, prefix: &str, out: &mut String, vocab: &std::collections::HashSet<String>) {
                    for n in &f.items {
                        match n {
                            Node::Folder(sub) => walk(sub, &format!("{prefix}{}/", sub.name), out, vocab),
                            Node::Request(r) => out.push_str(&format!("request  {prefix}{}  — {} {}\n", r.name, r.spec.verb, crate::privacy::path(&r.spec.path, vocab))),
                            Node::Flow(fl) => {
                                let ins: Vec<String> = fl.inputs.iter().map(|i| format!("{}:{}", i.name, i.kind.label())).collect();
                                let outs: Vec<&str> = fl.outputs.iter().map(|o| o.name.as_str()).collect();
                                out.push_str(&format!("flow     {prefix}{}  — inputs [{}] outputs [{}] {}\n", fl.name, ins.join(", "), outs.join(", "), fl.description));
                            }
                        }
                    }
                }
                let mut out = String::new();
                walk(&self.workspace, "", &mut out, &vocab);
                (if out.is_empty() { "The workspace is empty.".into() } else { out }, false)
            }
            "read_saved" => {
                let wanted = s("name").to_lowercase();
                fn find<'a>(f: &'a Folder, wanted: &str) -> Option<&'a Node> {
                    f.items.iter().find_map(|n| match n {
                        Node::Folder(sub) => find(sub, wanted),
                        other if other.name().to_lowercase() == wanted => Some(other),
                        _ => None,
                    })
                }
                match find(&self.workspace, &wanted) {
                    Some(node) => {
                        let v = serde_json::to_value(node).unwrap_or_default();
                        let text = serde_json::to_string_pretty(&crate::privacy::saved(&v, &vocab)).unwrap_or_default();
                        (format!("{}{}", ANONYMIZED, ai::clip(&text, 30_000)), false)
                    }
                    None => (format!("Nothing named “{}”. Use list_workspace.", s("name")), true),
                }
            }
            "current_tab" => match self.tabs.get(self.active) {
                None => ("No tab is open.".into(), false),
                Some(t) => match &t.body {
                    TabBody::Request(st) => {
                        let spec = crate::privacy::saved(&serde_json::to_value(&st.spec).unwrap_or_default(), &vocab);
                        let mut out = format!("Request tab “{}” ({}):\n{}\n", t.title, ANONYMIZED.trim(), serde_json::to_string_pretty(&spec).unwrap_or_default());
                        match (&st.response, &st.error) {
                            (Some(r), _) => out.push_str(&format!("Last response: HTTP {}\n{}", r.status, ai::clip(&crate::privacy::body(&r.body), 15_000))),
                            (None, Some(e)) => out.push_str(&format!("Last error: {}", crate::privacy::message(e))),
                            _ => out.push_str("Not sent yet."),
                        }
                        (out, false)
                    }
                    TabBody::DataMap => ("The Data map tab is open: read the map with the data_map tool.".into(), false),
                    TabBody::Flow(st) => {
                        let flow = crate::privacy::saved(&serde_json::to_value(&st.flow).unwrap_or_default(), &vocab);
                        // The console is free text full of values: how the run went, and its variables' structure.
                        let errors = st.log.iter().filter(|l| l.kind == crate::flow::LogKind::Error).count();
                        let run = match st.result {
                            Some((ok, d)) => format!(
                                "Last run: {} in {:.1}s, {} console lines, {errors} error line(s).",
                                if ok { "finished" } else { "failed" },
                                d.as_secs_f32(),
                                st.log.len()
                            ),
                            None => "Not run yet.".into(),
                        };
                        let vars = st
                            .last_vars
                            .as_ref()
                            .map(|v| {
                                format!(
                                    "\nVariables at the end (structure only):\n{}",
                                    ai::clip(&serde_json::to_string_pretty(&crate::privacy::json(v)).unwrap_or_default(), 15_000)
                                )
                            })
                            .unwrap_or_default();
                        (
                            ai::clip(
                                &format!("Flow tab “{}” ({}):\n{}\n{run}{vars}", t.title, ANONYMIZED.trim(), serde_json::to_string_pretty(&flow).unwrap_or_default()),
                                30_000,
                            ),
                            false,
                        )
                    }
                },
            },
            "open_request" => {
                let spec = self.ai_spec(input);
                let title = if s("title").is_empty() { format!("{} {}", spec.verb, spec.path) } else { s("title") };
                let doc_note = if spec.doc.is_some() { "" } else { " (warning: this path isn't a documented endpoint)" };
                self.push_request_tab(title.clone(), spec, None);
                (format!("Opened the tab “{title}”{doc_note}. The user can review it and press Send (Ctrl+Enter), then Save."), false)
            }
            "open_flow" => {
                let Some(v) = input.get("flow") else { return ("missing “flow”".into(), true) };
                let mut flow: Flow = match serde_json::from_value(v.clone()) {
                    Ok(f) => f,
                    Err(e) => return (format!("Not a valid flow: {e}. Follow the documented format."), true),
                };
                flow.refresh_uids();
                let mut problems = Vec::new();
                crate::cli::lint_flow(&flow, &mut problems);
                let name = flow.name.clone();
                self.push_flow_tab(flow, None);
                if problems.is_empty() {
                    (format!("Opened the flow “{name}” in a tab (edit mode). The user can check it, Save and Run it."), false)
                } else {
                    (format!("Opened the flow “{name}”, but it has problems to fix (open it again corrected):\n{}", problems.join("\n")), true)
                }
            }
            "list_substitutions" => {
                let rules = self.subs.rules();
                if rules.is_empty() {
                    return ("No substitution.".into(), false);
                }
                let summary = |r: &subst::Rule| {
                    let q: Vec<String> = model::file_rows(&r.query).map(|(k, v)| format!("{k}={}", crate::privacy::text_value(v))).collect();
                    let q = if q.is_empty() { String::new() } else { format!("?{}", q.join("&")) };
                    format!("{} ← GET {}{q} → {}", r.fields.join(", "), crate::privacy::path(r.path.trim(), &vocab), r.extract.trim())
                };
                (rules.iter().map(|r| format!("{}{}: {}", r.name, if r.enabled { "" } else { " (off)" }, summary(r))).collect::<Vec<_>>().join("\n"), false)
            }
            "open_substitution" => {
                let mut v = input.clone();
                if let Some(q) = v.get("query").cloned() {
                    v["query"] = serde_json::to_value(ai::pairs(Some(&q)).into_iter().map(|(key, value)| model::Kv { enabled: true, key, value }).collect::<Vec<_>>())
                        .unwrap_or_default();
                }
                match serde_json::from_value::<subst::Rule>(v) {
                    Ok(mut rule) => {
                        rule.uid = workspace::new_uid();
                        let name = rule.name.clone();
                        self.subst_edit = Some(SubstEditor::new(rule, None, true));
                        self.side = SideTab::Substitutions;
                        (format!("Opened “{name}” in the substitution editor. The user can test it and click Save."), false)
                    }
                    Err(e) => (format!("Not a valid substitution: {e}"), true),
                }
            }
            other => (format!("Unknown tool {other}"), true),
        }
    }
}
