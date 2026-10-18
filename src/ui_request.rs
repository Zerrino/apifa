//! Request editor (the white card) and response viewer (the dark code column).

use std::collections::BTreeMap;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use egui::{vec2, Align, Button, CollapsingHeader, ComboBox, FontId, Frame, Label, Layout, Margin, RichText, ScrollArea, Stroke, TextEdit, TextStyle, Ui};

use crate::app::{Action, TabId};
use crate::docs::{Method, Param, ParamKind, Resource};
use crate::model::{self, BodyMode, RequestSpec};
use crate::net::ResponseData;
use crate::widgets::*;
use crate::{json_view, theme};

const INPUT_W: f32 = 260.0;
const NAME_W: f32 = 250.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ReqTab {
    Params,
    Body,
    Headers,
    Docs,
    Examples,
    Visualize,
    Tests,
    Mock,
    Auth,
    Scripts,
    /// “Explained by AI” (shown when there is one).
    Explained,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RespTab {
    Body,
    Headers,
    Tests,
    /// The scripts' console (`console.log`).
    Console,
}

/// Everything a request tab holds besides its title.
pub struct ReqState {
    pub id: u64,
    pub spec: RequestSpec,
    pub sub: ReqTab,
    pub resp_tab: RespTab,
    pub pretty: bool,
    pub filter: String,
    pub pending: Option<Receiver<Result<ResponseData, String>>>,
    pub inflight: Option<(RequestSpec, String)>,
    pub sent_at: Option<Instant>,
    pub response: Option<ResponseData>,
    /// The assertions checked on that response.
    pub tests: Vec<crate::assert::Outcome>,
    pub error: Option<String>,
    /// The response body pretty-printed, made the first time the Pretty view needs it.
    pub pretty_body: Option<String>,
    /// Line starts of the body shown, keyed by (text address, length): built once per response.
    pub view: Option<((usize, usize), Vec<usize>)>,
    /// Search matches (byte offsets) for (text key, needle).
    pub find_cache: Option<((usize, usize), String, Vec<usize>)>,
    /// Ctrl+F in the response body.
    pub find: Find,
    /// GraphQL: the schema read by introspection, and the explorer's search.
    pub graphql: Option<Result<crate::graphql::Schema, String>>,
    pub schema_rx: Option<std::sync::mpsc::Receiver<Result<crate::graphql::Schema, String>>>,
    pub schema_filter: String,
    /// WebSocket / gRPC: the connection, the services, what came back.
    pub live: crate::ui_live::LiveState,
}

#[derive(Default)]
pub struct Find {
    pub open: bool,
    pub text: String,
    pub current: usize,
    /// Scroll to the current match on the next frame.
    pub scroll: bool,
    pub focus: bool,
}

impl ReqState {
    pub fn new(id: u64, spec: RequestSpec) -> Self {
        let sub = if model::has_body(&spec.verb) && spec.doc.is_some() { ReqTab::Body } else { ReqTab::Params };
        Self {
            id,
            spec,
            sub,
            resp_tab: RespTab::Body,
            pretty: true,
            filter: String::new(),
            pending: None,
            inflight: None,
            sent_at: None,
            response: None,
            tests: Vec::new(),
            error: None,
            view: None,
            find_cache: None,
            pretty_body: None,
            find: Find::default(),
            graphql: None,
            schema_rx: None,
            schema_filter: String::new(),
            live: Default::default(),
        }
    }
}

/// What parameter names can link to: the docs and the current resource.
pub struct Links<'a> {
    pub doc: &'a crate::docs::ApiDoc,
    pub resource: Option<&'a str>,
}

/// A parameter name; when its values can be listed somewhere, a link opening that GET in a tab.
fn param_name(ui: &mut Ui, text: &str, param: &str, links: &Links, actions: &mut Vec<Action>, truncate: bool) {
    match links.doc.value_source(param, links.resource) {
        Some((src, from_map)) => {
            let target = links.doc.lookup(&src).map(|(_, m)| m.routes[src.route].path.clone()).unwrap_or_default();
            let mut label = Label::new(RichText::new(text).monospace().color(theme::CYAN_DARK).underline()).sense(egui::Sense::click());
            if truncate {
                label = label.truncate();
            }
            let how = if from_map { " (the data map links this field to it)" } else { "" };
            let resp = ui.add(label).on_hover_text(format!("Where to find a value: click to open GET {target} in a tab{how}"));
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                actions.push(Action::Open(src));
            }
        }
        None => {
            let mut label = Label::new(RichText::new(text).monospace().color(theme::NAVY));
            if truncate {
                label = label.truncate();
            }
            ui.add(label).on_hover_text(param);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn request_ui(
    ui: &mut Ui,
    method: Option<(&Resource, &Method)>,
    doc: &crate::docs::ApiDoc,
    title: &mut String,
    tab: &mut ReqState,
    tab_id: TabId,
    saved: bool,
    fav: Option<bool>,
    actions: &mut Vec<Action>,
    token: Option<&str>,
) {
    let has_body = model::has_body(&tab.spec.verb);
    if tab.sub == ReqTab::Body && !has_body {
        tab.sub = ReqTab::Params;
    }

    // Name + breadcrumb + actions
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            star_button(ui, fav, tab_id, actions);
            ui.menu_button("More", |ui| {
                if ui.button("Save as…").clicked() {
                    actions.push(Action::SaveTabAs(tab_id));
                    ui.close();
                }
                if ui.button("Export to file…").clicked() {
                    actions.push(Action::ExportTab(tab_id));
                    ui.close();
                }
                if ui.button("Copy as JSON (share)").clicked() {
                    actions.push(Action::CopyTab(tab_id));
                    ui.close();
                }
                if ui.button("cURL snippet").clicked() {
                    actions.push(Action::ShowCode);
                    ui.close();
                }
                ui.separator();
                if ui.button("Use in a new flow").clicked() {
                    actions.push(Action::RequestToFlow(tab_id));
                    ui.close();
                }
            });
            let save_label = if saved { "Save" } else { "Save…" };
            if ui.button(save_label).on_hover_text("Save in your workspace (Ctrl+S)").clicked() {
                actions.push(Action::SaveTab(tab_id));
            }
            if saved {
                ui.label(RichText::new("saved in workspace").size(11.5).color(theme::GREEN));
            }
            // What is left of the row, after the buttons on the right (drawn first: they no longer cover it).
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                ui.add(
                    TextEdit::singleline(title)
                        .font(FontId::new(17.0, theme::bold()))
                        .desired_width(ui.available_width().min(320.0))
                        .frame(egui::Frame::NONE)
                        .hint_text(hint_rt("Request name")),
                )
                .on_hover_text("Click to rename");
                match method {
                    Some((res, m)) => {
                        ui.label(RichText::new(&res.name).color(theme::MUTED));
                        ui.label(RichText::new("›").color(theme::SILVER));
                        ui.label(RichText::new(&m.name).color(theme::MUTED));
                        if let Some(r) = tab.spec.doc.as_ref().and_then(|d| m.routes.get(d.route)) {
                            if r.deprecated {
                                pill(ui, "DEPRECATED", theme::WHITE, theme::REED);
                            }
                        }
                    }
                    None => {
                        ui.label(RichText::new("custom request").color(theme::MUTED));
                    }
                }
            });
        });
    });
    if let Some((_, m)) = method {
        if !m.description.is_empty() {
            ui.add(Label::new(RichText::new(first_lines(&m.description, 2)).color(theme::MUTED).size(13.0)).wrap()).on_hover_text(&m.description);
        }
    }
    ui.add_space(4.0);

    // WebSocket / gRPC: their own editor.
    match tab.spec.protocol {
        model::Protocol::WebSocket => {
            egui::ScrollArea::vertical().id_salt(("live_top", tab.id)).auto_shrink([false, false]).show(ui, |ui| crate::ui_live::ws_editor(ui, tab, tab_id, actions));
            return;
        }
        model::Protocol::Grpc => {
            egui::ScrollArea::vertical().id_salt(("live_top", tab.id)).auto_shrink([false, false]).show(ui, |ui| crate::ui_live::grpc_editor(ui, tab, tab_id, actions));
            return;
        }
        model::Protocol::Http => {}
    }

    // URL bar
    ui.horizontal(|ui| {
        crate::ui_live::protocol_picker(ui, tab);
        let busy = tab.pending.is_some();
        let url_w = (ui.available_width() - 120.0).max(200.0);
        ui.add(
            TextEdit::singleline(&mut tab.spec.path)
                .font(FontId::monospace(14.0))
                .desired_width(url_w)
                .hint_text(hint_rt("/v1/…  (use :name for path variables)"))
                .margin(Margin::symmetric(8, 6)),
        )
        .on_hover_text(format!("Base URL: {} (the environment's baseUrl, else Settings › Base URL); a full URL works too", model::base_url()));
        if busy {
            let cancel =
                Button::new(RichText::new("Cancel").family(theme::bold()).size(15.0).color(theme::WHITE)).fill(theme::RED).corner_radius(4).min_size(vec2(96.0, 32.0));
            if ui.add(cancel).on_hover_text("Stop waiting (Esc). The request may still reach the API: its answer is ignored").clicked() {
                actions.push(Action::CancelSend(tab_id));
            }
        } else {
            let send =
                Button::new(RichText::new("Send").family(theme::bold()).size(15.0).color(theme::WHITE)).fill(theme::CYAN).corner_radius(4).min_size(vec2(96.0, 32.0));
            if ui.add(send).on_hover_text("Ctrl+Enter").clicked() {
                actions.push(Action::Send(tab_id));
            }
        }
    });

    let prepared = model::prepare(&tab.spec, method.map(|(_, m)| m));
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(&prepared.display_url).monospace().size(12.0).color(theme::MUTED));
    });
    if !prepared.missing_path.is_empty() {
        notice(
            ui,
            theme::RED,
            &format!(
                "Required path variable{} empty: {}",
                if prepared.missing_path.len() > 1 { "s" } else { "" },
                prepared.missing_path.iter().map(|v| format!(":{v}")).collect::<Vec<_>>().join(", ")
            ),
        );
    }
    if !prepared.missing_required.is_empty() && (has_body || tab.spec.doc.is_some()) {
        notice(
            ui,
            theme::ORANGE,
            &format!("Required field{} not filled yet: {}", if prepared.missing_required.len() > 1 { "s" } else { "" }, prepared.missing_required.join(", ")),
        );
    }
    if let Some(e) = &prepared.body_error {
        notice(ui, theme::RED, e);
    }
    ui.add_space(6.0);

    // Sub-tabs
    let filled = tab.spec.values.values().filter(|v| !v.trim().is_empty()).count() + tab.spec.path_values.values().filter(|v| !v.trim().is_empty()).count();
    let n_headers = tab.spec.headers.iter().filter(|h| h.enabled && !h.key.is_empty()).count();
    let recorded = tab.spec.doc.as_ref().and_then(|d| crate::learned::get(ui.ctx()).map(|l| l.lock().get(d).is_some())).unwrap_or(false);
    let n_examples = method.map(|(_, m)| m.examples.len()).unwrap_or(0) + recorded as usize;
    // “Explained by AI”: the tab when there is one; the button when it can be written (an AI key, a
    // data map, an example of what the endpoint's GET returns).
    let explanations = crate::explain::get(ui.ctx());
    let (explained, busy, failed) = match (&tab.spec.doc, &explanations) {
        (Some(d), Some(ex)) => {
            let ex = ex.lock();
            let k = crate::explain::key(d);
            (ex.map.contains_key(&k), ex.busy.contains(&k), ex.errors.get(&k).cloned())
        }
        _ => (false, false, None),
    };
    let can_explain = crate::explain::ready(ui.ctx())
        && tab.spec.doc.as_ref().is_some_and(|d| crate::learned::get(ui.ctx()).is_some_and(|l| crate::explain::source(doc, &l.lock(), d).is_some()));
    if tab.sub == ReqTab::Explained && !explained {
        tab.sub = ReqTab::Docs;
    }
    ui.horizontal(|ui| {
        subtab(ui, &mut tab.sub, ReqTab::Params, &with_count("Params", if has_body { tab.spec.path_values.values().filter(|v| !v.is_empty()).count() } else { filled }));
        if has_body {
            subtab(ui, &mut tab.sub, ReqTab::Body, &with_count("Body", tab.spec.values.values().filter(|v| !v.trim().is_empty()).count()));
        }
        subtab(ui, &mut tab.sub, ReqTab::Headers, &with_count("Headers", n_headers));
        subtab(ui, &mut tab.sub, ReqTab::Auth, if tab.spec.auth.is_inherit() { "Auth" } else { "Auth ●" });
        let scripts = [&tab.spec.pre_script, &tab.spec.post_script].iter().filter(|s| !s.trim().is_empty()).count();
        subtab(ui, &mut tab.sub, ReqTab::Scripts, &with_count("Scripts", scripts));
        subtab(ui, &mut tab.sub, ReqTab::Docs, if tab.spec.description.trim().is_empty() { "Docs" } else { "Docs ●" });
        if method.is_some() {
            subtab(ui, &mut tab.sub, ReqTab::Examples, &with_count("Examples", n_examples));
        }
        subtab(ui, &mut tab.sub, ReqTab::Tests, &with_count("Tests", tab.spec.assertions.iter().filter(|a| a.enabled).count()));
        subtab(ui, &mut tab.sub, ReqTab::Mock, if tab.spec.mock.as_ref().is_some_and(|m| m.enabled) { "Mock ●" } else { "Mock" });
        subtab(ui, &mut tab.sub, ReqTab::Visualize, if tab.spec.visualizer.trim().is_empty() { "Visualize" } else { "Visualize ●" });
        if explained {
            subtab(ui, &mut tab.sub, ReqTab::Explained, "Explained by AI");
        }
        if busy {
            ui.add(egui::Spinner::new().size(14.0).color(theme::CYAN_DARK));
            ui.label(RichText::new("Explaining…").size(12.0).color(theme::MUTED));
        } else if can_explain && !explained {
            let resp = ui
                .button(RichText::new("Explain with AI").size(12.5).color(theme::CYAN_DARK))
                .on_hover_text("The assistant's model explains this endpoint from its docs, an example of what its GET returns and the data map — all anonymized. Kept, and shown in “Explained by AI”.");
            if resp.clicked() {
                if let Some(d) = &tab.spec.doc {
                    actions.push(Action::Explain(d.clone()));
                    tab.sub = ReqTab::Explained;
                }
            }
        }
        if let Some(e) = &failed {
            ui.label(RichText::new("explanation failed").size(12.0).color(theme::RED)).on_hover_text(e);
        }
    });
    ui.separator();

    let links = Links { doc, resource: method.map(|(r, _)| r.key.as_str()) };
    let mut area = ScrollArea::vertical().id_salt(("req", tab.id)).auto_shrink([false, false]);
    // The field filter changed: show the results from the top.
    let seen_id = egui::Id::unique(("req_filter_seen", tab.id));
    if ui.data(|d| d.get_temp::<String>(seen_id)).is_some_and(|seen| seen != tab.filter) {
        area = area.vertical_scroll_offset(0.0);
    }
    ui.data_mut(|d| d.insert_temp(seen_id, tab.filter.clone()));
    area.show(ui, |ui| {
        ui.set_width(ui.available_width());
        match tab.sub {
            ReqTab::Params => params_tab(ui, method.map(|(_, m)| m), tab, &links, actions),
            ReqTab::Body => body_tab(ui, method.map(|(_, m)| m), tab, &links, actions),
            ReqTab::Headers => headers_tab(ui, tab, token),
            ReqTab::Docs => {
                section(
                    ui,
                    "Your notes",
                    "What this request is for, how to fill it… Shown in the documentation you generate (folder menu › Generate documentation). Markdown-style text.",
                );
                ui.add(
                    TextEdit::multiline(&mut tab.spec.description)
                        .id(egui::Id::unique(("notes", tab.id)))
                        .desired_rows(3)
                        .desired_width(f32::INFINITY)
                        .hint_text(hint_rt("e.g. Lists the users of an account; set account_id first.")),
                );
                ui.add_space(8.0);
                if let Some((res, m)) = method {
                    docs_tab(ui, res, m, tab);
                }
            }
            ReqTab::Examples => {
                if let Some((_, m)) = method {
                    examples_tab(ui, m, tab.spec.doc.as_ref(), actions);
                }
            }
            ReqTab::Visualize => visualize_tab(ui, tab, tab_id, actions),
            ReqTab::Tests => {
                section(
                    ui,
                    "Assertions",
                    "Checked on every response (results in the response's Tests tab) and by the test runner (folder menu › Run tests, or apifa test). \
                     Expression: res.status, res.body, res.body[0].login, res.body.length, res.headers.x-total, res.time (ms). Values can use {{now}}.",
                );
                assertions_editor(ui, &mut tab.spec.assertions, ("asserts", tab.id));
            }
            ReqTab::Mock => mock_tab(ui, tab),
            ReqTab::Auth => {
                section(ui, "Authentication", "How this request proves who it is. Inherit: its folder's, else the workspace's (Settings).");
                let show_id = egui::Id::unique(("auth_show", tab.id));
                let mut show = ui.data(|d| d.get_temp::<bool>(show_id)).unwrap_or(false);
                let mut get = false;
                crate::ui_auth::auth_editor(
                    ui,
                    &mut tab.spec.auth,
                    Some("its folder's, else the workspace's authentication (Settings)"),
                    &mut show,
                    &mut get,
                    ("req_auth", tab.id),
                );
                ui.data_mut(|d| d.insert_temp(show_id, show));
                if get {
                    actions.push(Action::FetchToken(tab.spec.auth.clone()));
                }
            }
            ReqTab::Scripts => scripts_tab(ui, tab),
            ReqTab::Explained => {
                let Some(ex) = tab.spec.doc.as_ref().and_then(|d| explanations.as_ref()?.lock().map.get(&crate::explain::key(d)).cloned()) else { return };
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Explained by AI").family(theme::bold()).size(17.0));
                    let when = chrono::DateTime::from_timestamp(ex.at as i64, 0)
                        .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_default();
                    ui.label(RichText::new(format!("{} · {when} · from the docs, an example of {} and the data map", ex.model, ex.from)).size(12.0).color(theme::MUTED));
                    if can_explain && !busy && ui.small_button("Explain again").on_hover_text("Writes it again (after a new Understand, a new example…)").clicked() {
                        if let Some(d) = &tab.spec.doc {
                            actions.push(Action::Explain(d.clone()));
                        }
                    }
                });
                ui.add_space(6.0);
                ui.add(egui::Label::new(RichText::new(&ex.text).size(13.5)).wrap().selectable(true));
            }
        }
    });
}

/// What the mock server answers for this request (once it's saved).
fn mock_tab(ui: &mut Ui, tab: &mut ReqState) {
    section(
        ui,
        "Mock response",
        "The mock server (Settings › Mock server) answers this when it gets this verb and path: a path variable filled here must be the same \
         (/v1/users/:id with id = jdoe answers only jdoe; left empty, any id). Its query parameters must be there too (page[size] / page[number] aside: lists are paginated). \
         The body can use {{path.id}}, {{query.filter[login]}}, {{now}}. Save the request for the server to see it.",
    );
    let Some(m) = &mut tab.spec.mock else {
        ui.horizontal(|ui| {
            if ui.button("+ Add a mock response").clicked() {
                tab.spec.mock = Some(crate::mock::Mock::from_response(200, "{}"));
            }
            if let Some(r) = &tab.response {
                if ui.button(format!("Use the last response ({})", r.status)).clicked() {
                    tab.spec.mock = Some(crate::mock::Mock::from_response(r.status, &r.body));
                }
            }
        });
        return;
    };
    let mut remove = false;
    ui.horizontal_wrapped(|ui| {
        ui.checkbox(&mut m.enabled, "Enabled");
        ui.add_space(10.0);
        ui.label("Status");
        ui.add(egui::DragValue::new(&mut m.status).range(100..=599));
        ui.add_space(10.0);
        ui.label("Delay");
        ui.add(egui::DragValue::new(&mut m.delay_ms).range(0..=60_000).speed(10).suffix(" ms")).on_hover_text("Wait before answering, to try a slow API");
        // Too narrow for the buttons on the right: they go on the next line instead of over the fields.
        if left_on_line(ui) < 280.0 {
            ui.end_row();
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            remove = ui.button("Remove the mock").clicked();
            if let Some(r) = &tab.response {
                if ui.button("Use the last response").on_hover_text(format!("Status {} and its body", r.status)).clicked() {
                    *m = crate::mock::Mock::from_response(r.status, &r.body);
                }
            }
        });
    });
    if let Err(e) = serde_json::from_str::<serde_json::Value>(&m.body) {
        if !m.body.trim().is_empty() && !m.body.contains("{{") {
            ui.label(RichText::new(format!("Not JSON ({e}): sent as text")).size(12.0).color(theme::ORANGE));
        }
    }
    ui.add(TextEdit::multiline(&mut m.body).id(egui::Id::unique(("mock_body", tab.id))).code_editor().desired_rows(14).desired_width(f32::INFINITY));
    CollapsingHeader::new(with_count("Headers", m.headers.len())).id_salt(("mock_h", tab.id)).show(ui, |ui| {
        kv_editor(ui, &mut m.headers, ("mock_headers", tab.id));
    });
    if remove {
        tab.spec.mock = None;
    }
}

/// Rows of `expression · operator · value` (request tabs and flow request blocks).
pub fn assertions_editor(ui: &mut Ui, rows: &mut Vec<crate::assert::Assertion>, salt: impl std::hash::Hash + std::fmt::Debug) {
    use crate::assert::{Assertion, Op};
    let base = egui::Id::unique(salt);
    let mut remove = None;
    for (i, a) in rows.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.checkbox(&mut a.enabled, "").on_hover_text("Enabled");
            ui.add(TextEdit::singleline(&mut a.expr).id(base.with(("e", i))).desired_width(240.0).font(TextStyle::Monospace).hint_text(hint_rt("res.status")));
            egui::ComboBox::from_id_salt(base.with(("op", i))).width(120.0).selected_text(a.op.id()).show_ui(ui, |ui| {
                card_popup(ui);
                ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    for op in Op::ALL {
                        ui.selectable_value(&mut a.op, op, op.id()).on_hover_text(op.help());
                    }
                });
            });
            if !a.op.unary() {
                ui.add(TextEdit::singleline(&mut a.value).id(base.with(("v", i))).desired_width(220.0).font(TextStyle::Monospace).hint_text(hint_rt("200")));
            }
            if ui.small_button("×").on_hover_text("Remove").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        rows.remove(i);
    }
    ui.horizontal(|ui| {
        if ui.button("+ Add").clicked() {
            rows.push(Assertion::new("res.status", Op::Eq, "200"));
        }
        ui.menu_button("Common checks", |ui| {
            card_popup(ui);
            for (label, a) in [
                ("Status is 200", Assertion::new("res.status", Op::Eq, "200")),
                ("Status is a success (< 300)", Assertion::new("res.status", Op::Lt, "300")),
                ("Body is a non-empty list", Assertion::new("res.body", Op::IsNotEmpty, "")),
                ("Body is a list", Assertion::new("res.body", Op::IsArray, "")),
                ("First element has an id", Assertion::new("res.body[0].id", Op::IsNumber, "")),
                ("Answers within 1 s", Assertion::new("res.time", Op::Lt, "1000")),
                ("Paginated (has x-total)", Assertion::new("res.headers.x-total", Op::IsDefined, "")),
            ] {
                if ui.button(label).clicked() {
                    rows.push(a);
                    ui.close();
                }
            }
        });
    });
}

/// Checked assertions, on the dark response column or the test runner.
pub fn outcomes_ui(ui: &mut Ui, outcomes: &[crate::assert::Outcome]) {
    for o in outcomes {
        ui.horizontal_wrapped(|ui| {
            let (mark, color) = if o.passed { ("✓", theme::GREEN) } else { ("✗", theme::RED) };
            ui.label(RichText::new(mark).family(theme::bold()).color(color));
            ui.label(RichText::new(&o.text).monospace().color(theme::CLOUDS));
            if !o.passed {
                ui.label(RichText::new(format!("— {}", o.detail)).color(theme::RED));
            }
        });
    }
}

pub fn visualize_tab(ui: &mut Ui, tab: &mut ReqState, tab_id: TabId, actions: &mut Vec<Action>) {
    section(
        ui,
        "Visualization",
        "HTML + JavaScript opened in your browser with the response: response.status, response.headers, body (= response.body). Postman scripts work too: pm.response.json(), pm.visualizer.set(template, data) with a Handlebars template. Saved and exported with the request.",
    );
    ui.horizontal(|ui| {
        ui.menu_button("Start from a template", |ui| {
            for (name, script) in crate::visualize::TEMPLATES {
                if ui.button(name).clicked() {
                    tab.spec.visualizer = script.to_owned();
                    ui.close();
                }
            }
        });
        let has = tab.response.is_some();
        let open = Button::new(RichText::new("Open with the last response ↗").family(theme::bold()).color(theme::WHITE)).fill(theme::CYAN);
        if ui.add_enabled(has, open).on_disabled_hover_text("Send the request first").clicked() {
            actions.push(Action::Visualize(tab_id));
        }
        if tab.spec.visualizer.trim().is_empty() {
            ui.label(RichText::new("Empty: Visualize shows a table").size(12.0).color(theme::MUTED));
        } else if ui.small_button("Clear").clicked() {
            tab.spec.visualizer.clear();
        }
    });
    ui.add_space(4.0);
    ui.add(
        TextEdit::multiline(&mut tab.spec.visualizer)
            .id(egui::Id::unique(("visualizer", tab.id)))
            .code_editor()
            .desired_rows(20)
            .desired_width(f32::INFINITY)
            .hint_text(hint_rt("<h3 id=\"t\"></h3>\n<script>\n  document.getElementById(\"t\").textContent = body.length + \" items\";\n</script>")),
    );
}

pub fn params_tab(ui: &mut Ui, method: Option<&Method>, tab: &mut ReqState, links: &Links, actions: &mut Vec<Action>) {
    let vars = model::path_vars(&tab.spec.path);
    section(ui, "Path variables", "Every :variable in the URL. Required ones must be filled before sending.");
    if vars.is_empty() {
        ui.label(RichText::new("This URL has no path variable.").color(theme::MUTED));
    }
    for v in &vars {
        let doc_param = method.and_then(|m| m.params.iter().find(|p| p.name == v.name));
        row_frame(ui, |ui| {
            ui.horizontal(|ui| {
                fixed(ui, NAME_W, |ui| {
                    param_name(ui, &format!(":{}", v.name), &v.name, links, actions, false);
                });
                if v.optional {
                    pill(ui, "optional", theme::MUTED, theme::CLOUDS);
                } else {
                    pill(ui, "REQUIRED", theme::WHITE, theme::RED);
                }
                let value = tab.spec.path_values.entry(v.name.clone()).or_default();
                ui.add(
                    TextEdit::singleline(value)
                        .id(egui::Id::unique(("pathvar", tab.id, &v.name)))
                        .desired_width(INPUT_W)
                        .font(TextStyle::Monospace)
                        .hint_text(hint_rt("id or slug/login")),
                );
                subst_badge(ui, &format!(":{}", v.name), value);
                if let Some(p) = doc_param {
                    type_pill(ui, p);
                }
            });
            if let Some(p) = doc_param.filter(|p| !p.description.is_empty()) {
                description(ui, &p.description);
            }
        });
    }
    // Drop values of variables that were removed from the URL.
    tab.spec.path_values.retain(|k, _| vars.iter().any(|v| &v.name == k));

    if !model::has_body(&tab.spec.verb) {
        if let Some(m) = method {
            ui.add_space(10.0);
            doc_fields(ui, m, tab, "Query parameters", "Sent in the query string. Leave a field empty to not send it.", links, actions);
        }
    }
    ui.add_space(10.0);
    section(ui, "Custom query parameters", "Anything else to append to the query string (?key=value).");
    kv_editor(ui, &mut tab.spec.query, ("query", tab.id));
}

pub fn body_tab(ui: &mut Ui, method: Option<&Method>, tab: &mut ReqState, links: &Links, actions: &mut Vec<Action>) {
    if method.is_none() {
        // Without documentation there is no form to build the body from.
        tab.spec.body_mode = BodyMode::Raw;
    }
    ui.horizontal(|ui| {
        if method.is_some() {
            ui.radio_value(&mut tab.spec.body_mode, BodyMode::Form, "Form (from the documentation)");
        }
        ui.radio_value(&mut tab.spec.body_mode, BodyMode::Raw, "JSON");
        ui.radio_value(&mut tab.spec.body_mode, BodyMode::Text, "Text / XML");
        ui.radio_value(&mut tab.spec.body_mode, BodyMode::GraphQl, "GraphQL");
    });
    ui.add_space(6.0);
    match tab.spec.body_mode {
        BodyMode::Text => {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Content-Type").color(theme::MUTED));
                ComboBox::from_id_salt(("ctype", tab.id))
                    .selected_text(if tab.spec.content_type.is_empty() { "text/plain" } else { tab.spec.content_type.as_str() })
                    .show_ui(ui, |ui| {
                        for t in ["text/plain", "application/xml", "text/xml", "application/x-www-form-urlencoded", "text/csv", "text/html", "application/yaml"] {
                            if ui.selectable_label(tab.spec.content_type == t, t).clicked() {
                                tab.spec.content_type = t.to_owned();
                            }
                        }
                    });
                ui.add(TextEdit::singleline(&mut tab.spec.content_type).hint_text("or another type").desired_width(220.0));
            });
            ui.add(TextEdit::multiline(&mut tab.spec.raw_body).code_editor().desired_rows(16).desired_width(f32::INFINITY).hint_text(hint_rt("<note><to>…</to></note>")));
        }
        BodyMode::GraphQl => graphql_body(ui, tab, actions),
        BodyMode::Form => {
            let Some(m) = method else {
                ui.label(RichText::new("This request isn't linked to a documented endpoint: use Raw JSON.").color(theme::MUTED));
                return;
            };
            doc_fields(
                ui,
                m,
                tab,
                "Body fields",
                "Sent as a JSON body (application/json). Nested names like user[login] become {\"user\":{\"login\":…}}.",
                links,
                actions,
            );
            ui.add_space(8.0);
            CollapsingHeader::new(RichText::new("Preview JSON body").family(theme::bold())).id_salt(("preview", tab.id)).show(ui, |ui| {
                let body = serde_json::to_string_pretty(&model::form_body(m, &tab.spec)).unwrap_or_default();
                code_block(ui, json_view::highlight(&body).into());
                let files = model::form_files(m, &tab.spec);
                if !files.is_empty() {
                    ui.label(RichText::new("Sent as multipart/form-data: these fields as text parts, plus").color(theme::MUTED));
                    for (k, path) in files {
                        ui.label(RichText::new(format!("{k} ← {path}")).monospace());
                    }
                }
            });
        }
        BodyMode::Raw => {
            ui.horizontal(|ui| {
                if let Some(m) = method {
                    if ui.button("Generate from form").on_hover_text("Fill the editor with the JSON built from the documented fields").clicked() {
                        tab.spec.raw_body = serde_json::to_string_pretty(&model::form_body(m, &tab.spec)).unwrap_or_default();
                    }
                }
                let raw = tab.spec.raw_body.trim();
                if raw.is_empty() {
                    ui.label(RichText::new("Empty body (nothing will be sent)").color(theme::MUTED));
                } else {
                    match serde_json::from_str::<serde_json::Value>(raw) {
                        Ok(_) => {
                            ui.label(RichText::new("✔ valid JSON").color(theme::GREEN));
                            if ui.small_button("Beautify").clicked() {
                                if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) {
                                    tab.spec.raw_body = serde_json::to_string_pretty(&v).unwrap_or_default();
                                }
                            }
                        }
                        Err(e) => {
                            ui.label(RichText::new(format!("✖ {e}")).color(theme::RED));
                        }
                    }
                }
            });
            ui.add(
                TextEdit::multiline(&mut tab.spec.raw_body)
                    .code_editor()
                    .desired_rows(16)
                    .desired_width(f32::INFINITY)
                    .hint_text(hint_rt("{\n  \"user\": { \"login\": \"…\" }\n}")),
            );
            ui.add_space(8.0);
            let n = model::file_rows(&tab.spec.files).count();
            CollapsingHeader::new(RichText::new(if n > 0 { format!("Files ({n})") } else { "Files".into() }).family(theme::bold()))
                .id_salt(("files", tab.id))
                .default_open(n > 0)
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("Upload files (e.g. user[image]). The request is then sent as multipart/form-data, with the JSON above as text fields.")
                            .size(11.5)
                            .color(theme::MUTED),
                    );
                    files_editor(ui, &mut tab.spec.files, "~/Pictures/me.jpg");
                });
        }
    }
}

/// GraphQL: the operation and its variables, the schema's root fields to start from.
fn graphql_body(ui: &mut Ui, tab: &mut ReqState, actions: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        let busy = tab.schema_rx.is_some();
        if ui
            .add_enabled(!busy, Button::new(if tab.graphql.is_some() { "Fetch the schema again" } else { "Fetch the schema" }))
            .on_hover_text("Sends the introspection query to this URL (same auth and headers), for the explorer below")
            .clicked()
        {
            actions.push(Action::GraphQlSchema(TabId(tab.id)));
        }
        if busy {
            ui.add(egui::Spinner::new().size(14.0).color(theme::CYAN_DARK));
        }
        if ui.small_button("Prettify").clicked() {
            tab.spec.raw_body = crate::graphql::format_query(&tab.spec.raw_body);
        }
        let problems = if tab.spec.raw_body.trim().is_empty() { Vec::new() } else { crate::graphql::lint(&tab.spec.raw_body) };
        if problems.is_empty() {
            if !tab.spec.raw_body.trim().is_empty() {
                ui.label(RichText::new("✔ looks fine").size(12.0).color(theme::GREEN));
            }
        } else {
            ui.label(RichText::new(format!("✖ {}", problems[0])).size(12.0).color(theme::RED)).on_hover_text(problems.join("\n"));
        }
    });
    let explorer_w = if tab.graphql.is_some() { 300.0 } else { 0.0 };
    ui.horizontal_top(|ui| {
        let w = (ui.available_width() - explorer_w - 12.0).max(300.0);
        ui.vertical(|ui| {
            ui.set_width(w);
            ui.label(RichText::new("Query").family(theme::bold()));
            ui.add(
                TextEdit::multiline(&mut tab.spec.raw_body)
                    .id(egui::Id::unique(("gql_q", tab.id)))
                    .code_editor()
                    .desired_rows(14)
                    .desired_width(w)
                    .hint_text(hint_rt("query User($id: ID!) {\n  user(id: $id) { id name }\n}")),
            );
            ui.label(RichText::new("Variables (JSON)").family(theme::bold()));
            ui.add(
                TextEdit::multiline(&mut tab.spec.graphql_variables)
                    .id(egui::Id::unique(("gql_v", tab.id)))
                    .code_editor()
                    .desired_rows(5)
                    .desired_width(w)
                    .hint_text(hint_rt("{ \"id\": \"1\" }")),
            );
        });
        let Some(schema) = &tab.graphql else { return };
        ui.separator();
        ui.vertical(|ui| {
            ui.set_width(explorer_w);
            match schema {
                Err(e) => {
                    ui.add(egui::Label::new(RichText::new(e).color(theme::RED)).wrap());
                }
                Ok(schema) => {
                    ui.label(RichText::new("Schema").family(theme::bold()));
                    ui.add(TextEdit::singleline(&mut tab.schema_filter).hint_text("Find a field…").desired_width(explorer_w));
                    let needle = tab.schema_filter.trim().to_lowercase();
                    let mut chosen = None;
                    ScrollArea::vertical().id_salt(("gql_schema", tab.id)).max_height(340.0).show(ui, |ui| {
                        for op in ["query", "mutation", "subscription"] {
                            let fields = schema.root_fields(op);
                            if fields.is_empty() {
                                continue;
                            }
                            CollapsingHeader::new(RichText::new(op).family(theme::bold())).id_salt(("gql_op", op, tab.id)).default_open(op == "query").show(ui, |ui| {
                                for f in fields.iter().filter(|f| needle.is_empty() || f.name.to_lowercase().contains(&needle)) {
                                    let args = if f.args.is_empty() {
                                        String::new()
                                    } else {
                                        format!("({})", f.args.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", "))
                                    };
                                    let resp = ui
                                        .add(
                                            egui::Label::new(RichText::new(format!("{}{args}: {}", f.name, f.type_ref)).monospace().size(12.0).color(theme::CYAN_DARK))
                                                .sense(egui::Sense::click()),
                                        )
                                        .on_hover_text(if f.description.is_empty() { "Click: write this operation" } else { f.description.as_str() });
                                    if resp.clicked() {
                                        chosen = Some((op, f.name.clone()));
                                    }
                                }
                            });
                        }
                    });
                    if let Some((op, field)) = chosen {
                        if let Some((q, v)) = crate::graphql::operation_for(schema, op, &field, 2) {
                            tab.spec.raw_body = q;
                            tab.spec.graphql_variables = v;
                        }
                    }
                }
            }
        });
    });
}

pub fn doc_fields(ui: &mut Ui, m: &Method, tab: &mut ReqState, title: &str, help: &str, links: &Links, actions: &mut Vec<Action>) {
    let fields = model::field_params(m, &tab.spec.path);
    section(ui, title, help);
    if fields.is_empty() {
        ui.label(RichText::new("No documented parameter for this endpoint.").color(theme::MUTED));
        return;
    }
    let leaves: usize = fields.iter().map(|p| p.leaf_count()).sum();
    if leaves > 6 {
        ui.horizontal(|ui| {
            ui.add(TextEdit::singleline(&mut tab.filter).id(egui::Id::unique(("filter", tab.id))).hint_text(hint_rt("Filter fields…")).desired_width(INPUT_W));
            if !tab.filter.is_empty() && ui.small_button("×").clicked() {
                tab.filter.clear();
            }
            let set: usize = fields.iter().map(|p| p.count_set(&tab.spec.values)).sum();
            ui.label(RichText::new(format!("{set} of {leaves} filled")).color(theme::MUTED));
            // Always drawn: a widget appearing on the first keystroke would shift the ids of the fields below.
            if ui.add_enabled(set > 0, Button::new("Clear all").small()).clicked() {
                tab.spec.values.clear();
            }
        });
        ui.add_space(4.0);
    }
    let needle = tab.filter.trim().to_lowercase();
    let (required, optional): (Vec<&Param>, Vec<&Param>) = fields.into_iter().partition(|p| p.has_required());
    if !required.is_empty() {
        group_title(ui, "Required — you must add", theme::RED);
        for p in &required {
            param_ui(ui, p, &mut tab.spec.values, 0, &needle, tab.id, links, actions);
        }
    }
    if !optional.is_empty() {
        group_title(ui, "Optional — you can add", theme::CYAN);
        for p in &optional {
            param_ui(ui, p, &mut tab.spec.values, 0, &needle, tab.id, links, actions);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn param_ui(ui: &mut Ui, p: &Param, values: &mut BTreeMap<String, String>, depth: usize, needle: &str, salt: u64, links: &Links, actions: &mut Vec<Action>) {
    if !p.matches(needle) {
        return;
    }
    let indent = depth as f32 * 18.0;
    if p.is_group() {
        let set = p.count_set(values);
        let mut title = format!("{}   ·   {} field{}", p.full_name, p.leaf_count(), if p.leaf_count() > 1 { "s" } else { "" });
        if set > 0 {
            title.push_str(&format!("   ·   {set} set"));
        }
        let mut header = CollapsingHeader::new(RichText::new(title).monospace().color(theme::NAVY))
            .id_salt(("group", salt, &p.full_name))
            .default_open(p.has_required() || p.children.len() <= 6 || set > 0);
        if !needle.is_empty() {
            header = header.open(Some(true));
        }
        ui.horizontal(|ui| {
            ui.add_space(indent);
            ui.vertical(|ui| {
                header.show(ui, |ui| {
                    if !p.description.is_empty() {
                        description(ui, &p.description);
                    }
                    // A matching group shows all its children; otherwise only matching ones.
                    let child_needle = if p.full_name.to_lowercase().contains(needle) || p.description.to_lowercase().contains(needle) { "" } else { needle };
                    // What you must add first, then what you can add.
                    let mut children: Vec<&Param> = p.children.iter().collect();
                    children.sort_by_key(|c| !c.has_required());
                    for c in children {
                        param_ui(ui, c, values, depth + 1, child_needle, salt, links, actions);
                    }
                });
            });
        });
        return;
    }
    row_frame(ui, |ui| {
        ui.horizontal(|ui| {
            ui.add_space(indent);
            fixed(ui, (NAME_W - indent).max(120.0), |ui| {
                param_name(ui, &p.full_name, &p.full_name, links, actions, true);
            });
            if p.required {
                pill(ui, "REQUIRED", theme::WHITE, theme::RED);
            } else {
                pill(ui, "optional", theme::MUTED, theme::CLOUDS);
            }
            value_input(ui, p, values, salt);
            type_pill(ui, p);
        });
        ui.horizontal(|ui| {
            ui.add_space(indent);
            ui.vertical(|ui| {
                if !p.description.is_empty() {
                    description(ui, &p.description);
                }
                if p.kind == ParamKind::NestedArray && !p.children.is_empty() {
                    let fields: Vec<&str> = p.children.iter().map(|c| c.name.as_str()).collect();
                    description(ui, &format!("JSON array of objects with: {}", fields.join(", ")));
                }
                if p.kind == ParamKind::File {
                    description(ui, "Path of a file on this computer (~/ works). With a file, the request is sent as multipart/form-data.");
                }
            });
        });
    });
}

pub fn value_input(ui: &mut Ui, p: &Param, values: &mut BTreeMap<String, String>, salt: u64) {
    let v = values.entry(p.full_name.clone()).or_default();
    if p.kind == ParamKind::Bool {
        egui::ComboBox::from_id_salt(("bool", salt, &p.full_name)).width(INPUT_W - 8.0).selected_text(if v.is_empty() { "— not sent —" } else { v.as_str() }).show_ui(
            ui,
            |ui| {
                card_popup(ui);
                ui.selectable_value(v, String::new(), "— not sent —");
                ui.selectable_value(v, "true".into(), "true");
                ui.selectable_value(v, "false".into(), "false");
            },
        );
        return;
    }
    // A fixed id: fields keep the focus whatever appears above them while typing.
    ui.add(TextEdit::singleline(v).id(egui::Id::unique(("value", salt, &p.full_name))).desired_width(INPUT_W).font(TextStyle::Monospace).hint_text(hint_rt(hint(p))));
    subst_badge(ui, &p.full_name, v);
    if p.kind == ParamKind::File {
        browse_file(ui, v);
        let path = model::expand_home(v);
        if !v.trim().is_empty() && !v.contains("{{") {
            match std::fs::metadata(&path) {
                Ok(meta) if meta.is_file() => ui.label(RichText::new(human_size(meta.len() as usize)).size(11.5).color(theme::MUTED)),
                _ => ui.label(RichText::new("file not found").size(11.5).color(theme::RED)),
            };
        }
    }
    if is_date_param(p) {
        crate::ui_date::date_field(ui, v, egui::Id::unique(("date", salt, &p.full_name)), p.full_name.starts_with("range["), true);
    }
    if !p.options.is_empty() {
        let multi = p.name == "sort" || p.full_name.starts_with("filter[");
        ui.menu_button(RichText::new("+").family(theme::bold()).size(15.0).color(theme::CYAN_DARK), |ui| {
            ui.set_min_width(200.0);
            if multi {
                ui.label(RichText::new("Click to append (comma separated)").size(11.0).color(theme::MUTED));
            }
            ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                for o in &p.options {
                    if ui.button(o).clicked() {
                        if multi && !v.trim().is_empty() {
                            v.push(',');
                            v.push_str(o);
                        } else {
                            *v = o.clone();
                        }
                        ui.close();
                    }
                    if p.name == "sort" && ui.button(format!("-{o}")).on_hover_text("descending").clicked() {
                        if !v.trim().is_empty() {
                            v.push(',');
                        }
                        v.push_str(&format!("-{o}"));
                        ui.close();
                    }
                }
            });
        })
        .response
        .on_hover_text(format!("{} allowed values", p.options.len()));
    }
}

/// Datetime fields, plus filter/range on `*_at` / date fields (documented as strings).
pub fn is_date_param(p: &Param) -> bool {
    p.kind == ParamKind::DateTime || is_date_key(&p.name)
}

pub fn hint(p: &Param) -> String {
    if p.full_name.starts_with("range[") {
        return if is_date_param(p) { "min,max  (dates or {{now}})".into() } else { "min,max".into() };
    }
    if is_date_param(p) {
        return "2026-10-15T09:00:00Z or {{now}}".into();
    }
    if p.full_name.starts_with("filter[") {
        return if p.name == "id" || p.name.ends_with("_id") {
            "an id, e.g. 21  (several: 21,34)".into()
        } else if p.name.ends_with('?') {
            "true or false".into()
        } else {
            format!("exact {}  (several: a,b)", p.name)
        };
    }
    if p.name == "sort" {
        return "e.g. -created_at,id".into();
    }
    match p.kind {
        ParamKind::Integer => "integer".into(),
        ParamKind::Float => "number".into(),
        ParamKind::DateTime => "2026-10-15T09:00:00Z".into(),
        ParamKind::IntArray => "1,2,3".into(),
        ParamKind::Array => "a,b  or  [\"a\",\"b\"]".into(),
        ParamKind::NestedArray => "[{ … }]  (JSON)".into(),
        ParamKind::Hash => "{ … }  (JSON)".into(),
        ParamKind::File => "~/Pictures/me.jpg".into(),
        ParamKind::Enum => format!("one of {} values", p.options.len()),
        _ => "string".into(),
    }
}

/// Snippets for the scripts, like Postman's.
const PRE_SNIPPETS: [(&str, &str); 4] = [
    ("Set an environment variable", "pm.environment.set(\"name\", \"value\");\n"),
    ("Add a header", "pm.request.headers.upsert({ key: \"X-Request-Id\", value: pm.variables.replaceIn(\"{{$guid}}\") });\n"),
    ("Timestamp variable", "pm.variables.set(\"now\", new Date().toISOString());\n"),
    ("Skip when a variable is missing", "if (!pm.environment.get(\"token\")) {\n  pm.execution.skipRequest();\n}\n"),
];
const POST_SNIPPETS: [(&str, &str); 6] = [
    ("Status code is 200", "pm.test(\"Status code is 200\", () => {\n  pm.response.to.have.status(200);\n});\n"),
    ("Response time below 500 ms", "pm.test(\"Response time is below 500 ms\", () => {\n  pm.expect(pm.response.responseTime).to.be.below(500);\n});\n"),
    ("Body has a field", "pm.test(\"Body has an id\", () => {\n  pm.expect(pm.response.json()).to.have.property(\"id\");\n});\n"),
    ("Save a value from the response", "const body = pm.response.json();\npm.environment.set(\"id\", body.id);\n"),
    ("Save a token", "pm.environment.set(\"token\", pm.response.json().access_token);\n"),
    ("Log the response", "console.log(pm.response.code, pm.response.json());\n"),
];

fn scripts_tab(ui: &mut Ui, tab: &mut ReqState) {
    for (title, help, text, snippets, salt) in [
        (
            "Pre-request script",
            "JavaScript run just before sending: pm.environment / pm.variables / pm.globals (get, set), pm.request (url, method, headers, body.raw), console.log.",
            &mut tab.spec.pre_script,
            &PRE_SNIPPETS[..],
            "pre",
        ),
        (
            "Post-response script",
            "Run on the response: pm.response (code, json(), text(), headers, responseTime), pm.test(name, fn) with pm.expect(…) — results in the response's Tests tab, logs in its Console.",
            &mut tab.spec.post_script,
            &POST_SNIPPETS[..],
            "post",
        ),
    ] {
        ui.horizontal(|ui| {
            section(ui, title, help);
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Snippets:").size(12.0).color(theme::MUTED));
            for (name, code) in snippets {
                if ui.small_button(*name).clicked() {
                    if !text.is_empty() && !text.ends_with('\n') {
                        text.push('\n');
                    }
                    text.push_str(code);
                }
            }
        });
        ui.add(
            TextEdit::multiline(text)
                .id(egui::Id::unique(("script", salt, tab.id)))
                .code_editor()
                .desired_rows(8)
                .desired_width(f32::INFINITY)
                .hint_text(hint_rt("// JavaScript (QuickJS) — no network or file access")),
        );
        ui.add_space(10.0);
    }
}

pub fn headers_tab(ui: &mut Ui, tab: &mut ReqState, token: Option<&str>) {
    section(ui, "Automatic headers", "Added by the client on every request.");
    let auth = match (&tab.spec.auth, token) {
        (crate::auth::Auth::None, _) => "(none: this request's auth is “No auth”)".to_owned(),
        (crate::auth::Auth::Inherit, Some(t)) => format!("from the folder / workspace auth (last token {t})"),
        (crate::auth::Auth::Inherit, None) => "from the folder / workspace auth, when it has one".to_owned(),
        (a, _) => format!("from this request's {} (Auth tab)", a.label()),
    };
    let mut auto = vec![("Authorization", auth), ("Accept", "application/json, */*;q=0.8".into())];
    if model::has_body(&tab.spec.verb) {
        auto.push(("Content-Type", "application/json  (multipart/form-data when a file is attached)".into()));
    }
    egui::Grid::new(("auto", tab.id)).num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
        for (k, v) in auto {
            ui.label(RichText::new(k).monospace().color(theme::NAVY));
            ui.label(RichText::new(v).monospace().color(theme::MUTED));
            ui.end_row();
        }
    });
    ui.add_space(10.0);
    section(ui, "Custom headers", "Override or add headers.");
    kv_editor(ui, &mut tab.spec.headers, ("headers", tab.id));
}

pub fn docs_tab(ui: &mut Ui, res: &Resource, m: &Method, tab: &mut ReqState) {
    ui.label(RichText::new(&res.name).family(theme::bold()).size(20.0));
    if !res.description.is_empty() {
        ui.label(RichText::new(&res.description).color(theme::MUTED));
    }
    ui.add_space(8.0);
    ui.label(RichText::new(format!("Method: {}", m.name)).family(theme::bold()).size(16.0));
    if !m.description.is_empty() {
        ui.add(Label::new(&m.description).wrap());
    }
    ui.add_space(8.0);
    section(ui, "Routes", "Every URL served by this method. Click one to use it.");
    let current = tab.spec.doc.as_ref().map(|d| d.route);
    for (i, r) in m.routes.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(&r.verb).family(theme::bold()).color(theme::verb_color(&r.verb)));
            let resp = ui.selectable_label(current == Some(i), RichText::new(&r.path).monospace());
            if r.deprecated {
                pill(ui, "DEPRECATED", theme::WHITE, theme::REED);
            }
            if resp.clicked() {
                if let Some(d) = tab.spec.doc.as_mut() {
                    d.route = i;
                }
                tab.spec.verb = r.verb.clone();
                tab.spec.path = r.path.clone();
            }
        });
    }
    ui.add_space(8.0);
    section(ui, "Parameters reference", "As the API documentation describes them.");
    if m.params.is_empty() {
        ui.label(RichText::new("No parameter.").color(theme::MUTED));
    }
    egui::Grid::new(("ref", tab.id)).num_columns(4).striped(true).spacing([14.0, 6.0]).show(ui, |ui| {
        for h in ["Name", "", "Validation", "Description"] {
            ui.label(RichText::new(h).family(theme::bold()));
        }
        ui.end_row();
        fn rows(ui: &mut Ui, params: &[Param]) {
            for p in params {
                ui.label(RichText::new(&p.full_name).monospace());
                if p.required {
                    pill(ui, "REQUIRED", theme::WHITE, theme::RED);
                } else {
                    pill(ui, "optional", theme::MUTED, theme::CLOUDS);
                }
                ui.label(RichText::new(truncate(&p.validator, 80)).size(12.0).color(theme::MUTED)).on_hover_text(&p.validator);
                ui.add(Label::new(RichText::new(truncate(&p.description, 140)).size(12.5)).wrap()).on_hover_text(&p.description);
                ui.end_row();
                rows(ui, &p.children);
            }
        }
        rows(ui, &m.params);
    });
}

pub fn examples_tab(ui: &mut Ui, m: &Method, doc: Option<&crate::model::DocRef>, actions: &mut Vec<Action>) {
    let learned = doc.and_then(|d| Some((d.clone(), crate::learned::get(ui.ctx())?.lock().get(d)?.clone())));
    if let Some((d, l)) = &learned {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Recorded from a real call").family(theme::bold()).color(theme::CYAN_DARK));
            ui.label(RichText::new(format!("{} · {} {}", crate::learned::when(l), l.verb, l.url)).monospace().size(12.0).color(theme::MUTED));
            if ui.small_button("Forget").on_hover_text("Delete this sample (the next successful GET records a new one)").clicked() {
                actions.push(Action::ForgetExample(d.clone()));
            }
        });
        ui.label(
            RichText::new(format!(
                "The documentation has none, so the first successful GET was kept (lists cut to 2 items{}). The AI assistant reads it to know the returned fields. Stored on this computer only.",
                l.total.map(|t| format!("; {t} items in total")).unwrap_or_default()
            ))
            .size(12.0)
            .color(theme::MUTED),
        );
        code_block(ui, json_view::highlight(&l.response).into());
        if !m.examples.is_empty() {
            ui.separator();
        }
    }
    if m.examples.is_empty() {
        if learned.is_none() {
            ui.label(
                RichText::new("The documentation has no example for this method. Send a GET: the first successful response is kept here as an example.")
                    .color(theme::MUTED),
            );
        }
        return;
    }
    for (i, e) in m.examples.iter().enumerate() {
        let query = if e.query.is_empty() { String::new() } else { format!("?{}", e.query) };
        ui.horizontal(|ui| {
            ui.label(RichText::new(&e.verb).family(theme::bold()).color(theme::verb_color(&e.verb)));
            ui.label(RichText::new(format!("{}{}", e.path, query)).monospace());
            if let Ok(code) = e.code.parse::<u16>() {
                pill(ui, &code.to_string(), theme::WHITE, theme::status_color(code));
            }
        });
        if let Some(req) = &e.request {
            ui.label(RichText::new("Request").family(theme::bold()));
            code_block(ui, json_view::highlight(&cap(req)).into());
        }
        if let Some(resp) = &e.response {
            ui.label(RichText::new("Response").family(theme::bold()));
            code_block(ui, json_view::highlight(&cap(resp)).into());
        }
        if i + 1 < m.examples.len() {
            ui.separator();
        }
    }
}

// ==================================================================== response

pub fn response_ui(ui: &mut Ui, tab: &mut ReqState, tab_id: TabId, actions: &mut Vec<Action>) {
    match tab.spec.protocol {
        model::Protocol::WebSocket => return crate::ui_live::ws_log(ui, tab),
        model::Protocol::Grpc => return crate::ui_live::grpc_result(ui, tab),
        model::Protocol::Http => {}
    }
    ui.horizontal(|ui| {
        ui.label(RichText::new("Response").family(theme::bold()).size(17.0).color(theme::WHITE));
        ui.add_space(8.0);
        if let Some(r) = &tab.response {
            pill(ui, &format!("{} {}", r.status, r.status_text), theme::WHITE, theme::status_color(r.status));
            if let Some(src) = r.header("x-mock-source") {
                pill(ui, "MOCK", theme::WHITE, theme::ORANGE);
                ui.label(RichText::new(src).size(12.0).color(theme::SILVER)).on_hover_text("Answered by the mock server, from this");
            }
            stat(ui, "Time", &format!("{} ms", r.elapsed.as_millis()));
            stat(ui, "Size", &human_size(r.size));
            if let Some(rem) = r.header("x-secondly-ratelimit-remaining") {
                stat(ui, "Rate limit", &format!("{rem}/s left"));
            }
            if let Some(rem) = r.header("x-hourly-ratelimit-remaining") {
                stat(ui, "Hourly", &format!("{rem} left"));
            }
            if !tab.tests.is_empty() {
                let failed = tab.tests.iter().filter(|o| !o.passed).count();
                let (text, color) =
                    if failed == 0 { (format!("✓ {}", crate::assert::summary(&tab.tests)), theme::GREEN) } else { (format!("✗ {failed} failed"), theme::RED) };
                if ui
                    .add(Button::new(RichText::new(text).family(theme::bold()).color(theme::WHITE)).fill(color).small())
                    .on_hover_text("Assertions (Tests tab)")
                    .clicked()
                {
                    tab.resp_tab = RespTab::Tests;
                }
            }
        }
    });

    if tab.pending.is_some() {
        ui.add_space(20.0);
        ui.vertical_centered(|ui| {
            ui.add(egui::Spinner::new().size(28.0).color(theme::CYAN));
            let secs = tab.sent_at.map(|t| t.elapsed().as_secs_f32()).unwrap_or(0.0);
            let url = tab.inflight.as_ref().map(|(_, u)| u.clone()).unwrap_or_default();
            let origin = crate::model::split_origin(&url).0.to_owned();
            let host = if crate::net::mock_target().is_some() {
                "the mock server".to_owned()
            } else if origin.is_empty() {
                "the server".to_owned()
            } else {
                origin
            };
            ui.label(RichText::new(format!("Waiting for {host}… {secs:.1}s")).color(theme::SILVER));
            if ui.button("Cancel").on_hover_text("Esc").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                actions.push(Action::CancelSend(tab_id));
            }
        });
        return;
    }
    if let Some(e) = &tab.error {
        Frame::new().fill(theme::RED.gamma_multiply(0.18)).stroke(Stroke::new(1.0_f32, theme::RED)).corner_radius(4).inner_margin(Margin::same(10)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(e).color(theme::WHITE));
        });
        return;
    }
    let Some(r) = &tab.response else {
        ui.add_space(30.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("Fill the fields and hit Send to get a response.").size(16.0).color(theme::SILVER));
            ui.label(RichText::new("Ctrl+Enter sends · Ctrl+T new tab · Ctrl+W close tab").size(12.0).color(theme::CONCRETE));
        });
        return;
    };

    for note in &r.notes {
        ui.label(RichText::new(note).size(12.5).color(theme::CYAN));
    }
    if let Some(rep) = &r.script {
        for e in &rep.errors {
            ui.label(RichText::new(e).size(12.5).color(theme::RED));
        }
    }
    let logs = r.script.as_ref().map(|s| s.logs.len()).unwrap_or(0);
    if let Some(h) = status_hint(r.status) {
        ui.label(RichText::new(h).size(12.5).color(theme::YELLOW));
    }

    // Pagination (X-Page / X-Per-Page / X-Total headers)
    if let (Some(page), Some(per), Some(total)) = (r.header_u64("x-page"), r.header_u64("x-per-page"), r.header_u64("x-total")) {
        let last = total.div_ceil(per.max(1)).max(1);
        ui.horizontal(|ui| {
            if ui.add_enabled(page > 1, Button::new("◀ Prev")).clicked() {
                actions.push(Action::Paginate(tab_id, page - 1));
            }
            ui.label(RichText::new(format!("Page {page} / {last}")).family(theme::bold()).color(theme::WHITE));
            if ui.add_enabled(page < last, Button::new("Next ▶")).clicked() {
                actions.push(Action::Paginate(tab_id, page + 1));
            }
            ui.label(RichText::new(format!("{total} items · {per} per page")).color(theme::SILVER));
        });
    }

    if !r.body.is_empty() && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::F)) {
        tab.resp_tab = RespTab::Body;
        tab.find.open = true;
        tab.find.focus = true;
    }
    ui.horizontal_wrapped(|ui| {
        dark_subtab(ui, &mut tab.resp_tab, RespTab::Body, "Body");
        dark_subtab(ui, &mut tab.resp_tab, RespTab::Headers, &format!("Headers ({})", r.headers.len()));
        if !tab.tests.is_empty() {
            dark_subtab(ui, &mut tab.resp_tab, RespTab::Tests, &format!("Tests ({})", crate::assert::summary(&tab.tests)));
        } else if tab.resp_tab == RespTab::Tests {
            tab.resp_tab = RespTab::Body;
        }
        if logs > 0 {
            dark_subtab(ui, &mut tab.resp_tab, RespTab::Console, &format!("Console ({logs})"));
        } else if tab.resp_tab == RespTab::Console {
            tab.resp_tab = RespTab::Body;
        }
        // Too narrow for the buttons on the right: they go on the next line instead of over the tabs.
        if left_on_line(ui) < 400.0 {
            ui.end_row();
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Copy").clicked() {
                let text = match tab.resp_tab {
                    RespTab::Body if tab.pretty && r.is_json => pretty_of(&mut tab.pretty_body, &r.body).to_owned(),
                    RespTab::Body => r.body.clone(),
                    RespTab::Headers => r.headers.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\n"),
                    RespTab::Tests => tab
                        .tests
                        .iter()
                        .map(|o| format!("{} {}{}", if o.passed { "✓" } else { "✗" }, o.text, if o.passed { String::new() } else { format!(" — {}", o.detail) }))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    RespTab::Console => r.script.as_ref().map(|s| s.logs.iter().map(|(_, l)| l.as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default(),
                };
                ui.ctx().copy_text(text);
            }
            if tab.resp_tab == RespTab::Body && r.is_json {
                ui.selectable_value(&mut tab.pretty, false, "Raw");
                ui.selectable_value(&mut tab.pretty, true, "Pretty");
            }
            if ui.button("Save as mock").on_hover_text("The mock server will answer this response for this request (Mock tab; save the request)").clicked() {
                tab.spec.mock = Some(crate::mock::Mock::from_response(r.status, &r.body));
                tab.sub = ReqTab::Mock;
            }
            let tip = if tab.spec.visualizer.trim().is_empty() {
                "Open the response as a table in your browser (write your own view in the Visualize tab)"
            } else {
                "Open the response with this request's visualization"
            };
            if ui.button("Visualize ↗").on_hover_text(tip).clicked() {
                actions.push(Action::Visualize(tab_id));
            }
            if tab.resp_tab == RespTab::Body && !tab.find.open && ui.button("Find").on_hover_text("Search the body (Ctrl+F)").clicked() {
                tab.find.open = true;
                tab.find.focus = true;
            }
        });
    });

    if tab.resp_tab == RespTab::Tests {
        ScrollArea::vertical().id_salt(("resp_t", tab.id)).auto_shrink([false, false]).show(ui, |ui| outcomes_ui(ui, &tab.tests));
        return;
    }
    if tab.resp_tab == RespTab::Console {
        ScrollArea::vertical().id_salt(("resp_c", tab.id)).auto_shrink([false, false]).show(ui, |ui| {
            for (level, line) in r.script.iter().flat_map(|s| &s.logs) {
                let color = match level {
                    crate::script::LogLevel::Error => theme::RED,
                    crate::script::LogLevel::Warn => theme::ORANGE,
                    crate::script::LogLevel::Info => theme::CYAN,
                    crate::script::LogLevel::Log => theme::CLOUDS,
                };
                ui.add(egui::Label::new(RichText::new(line).monospace().size(12.5).color(color)).wrap().selectable(true));
            }
        });
        return;
    }
    if tab.resp_tab == RespTab::Headers {
        ScrollArea::both().id_salt(("resp_h", tab.id)).auto_shrink([false, false]).show(ui, |ui| {
            egui::Grid::new(("rh", tab.id)).num_columns(2).striped(true).spacing([16.0, 4.0]).show(ui, |ui| {
                for (k, v) in &r.headers {
                    ui.label(RichText::new(k).monospace().color(theme::CYAN));
                    ui.add(Label::new(RichText::new(v).monospace().color(theme::CLOUDS)).selectable(true));
                    ui.end_row();
                }
            });
        });
        return;
    }
    if r.body.is_empty() {
        ui.label(RichText::new("(empty body)").color(theme::CONCRETE));
        return;
    }
    // Line index and search matches are computed once per response / search, not every frame.
    let json = tab.pretty && r.is_json;
    let text: &str = if json { pretty_of(&mut tab.pretty_body, &r.body) } else { &r.body };
    let key = (text.as_ptr() as usize, text.len());
    if tab.view.as_ref().is_none_or(|(k, _)| *k != key) {
        tab.view = Some((key, line_starts(text)));
        tab.find_cache = None;
    }
    let needle = if tab.find.open { tab.find.text.clone() } else { String::new() };
    if tab.find_cache.as_ref().is_none_or(|(k, n, _)| *k != key || *n != needle) {
        tab.find_cache = Some((key, needle.clone(), json_view::find_all(text, &needle)));
    }
    let matches = &tab.find_cache.as_ref().expect("set above").2;
    if tab.find.open {
        find_bar(ui, &mut tab.find, matches.len(), tab.id);
    }
    let scroll_to = if std::mem::take(&mut tab.find.scroll) { matches.get(tab.find.current).copied() } else { None };
    let lines = &tab.view.as_ref().expect("set above").1;
    code_view(ui, ("resp", tab.id), CodeView { text, lines, json, marks: matches, mark_len: needle.len(), current: tab.find.current, scroll_to });
}

/// Search field, match counter, previous / next and close. Enter = next, Shift+Enter = previous, Esc = close.
fn find_bar(ui: &mut Ui, find: &mut Find, count: usize, salt: u64) {
    let id = egui::Id::unique(("find", salt));
    fn step(find: &mut Find, count: usize, forward: bool) {
        if count > 0 {
            find.current = if forward { (find.current + 1) % count } else { (find.current + count - 1) % count };
            find.scroll = true;
        }
    }
    ui.horizontal(|ui| {
        let edit = ui.add(TextEdit::singleline(&mut find.text).id(id).desired_width(260.0).hint_text(hint_rt("Find in the body…  (Enter: next)")));
        if find.focus {
            edit.request_focus();
            find.focus = false;
        }
        if edit.changed() {
            // A new search starts at the first match.
            find.current = 0;
            find.scroll = true;
        }
        let (enter, shift, esc) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.modifiers.shift, i.key_pressed(egui::Key::Escape)));
        if edit.lost_focus() && enter {
            step(find, count, !shift);
            edit.request_focus();
        }
        if find.text.is_empty() {
            ui.label(RichText::new("type to search").size(12.0).color(theme::CONCRETE));
        } else if count == 0 {
            ui.label(RichText::new("no match").size(12.0).color(theme::RED));
        } else {
            find.current = find.current.min(count - 1);
            ui.label(RichText::new(format!("{} / {count}", find.current + 1)).size(12.0).color(theme::WHITE));
        }
        if ui.add_enabled(count > 0, Button::new("▲").small()).on_hover_text("Previous (Shift+Enter)").clicked() {
            step(find, count, false);
        }
        if ui.add_enabled(count > 0, Button::new("▼").small()).on_hover_text("Next (Enter)").clicked() {
            step(find, count, true);
        }
        if ui.small_button("×").on_hover_text("Close (Esc)").clicked() || (esc && (edit.has_focus() || edit.lost_focus())) {
            find.open = false;
        }
    });
}

pub fn status_hint(status: u16) -> Option<&'static str> {
    Some(match status {
        401 => "401 — not authenticated: check the Auth tab, the workspace auth (Settings) or the environment's token.",
        403 => "403 — your application isn't allowed to do this. App tokens (client_credentials) only have the public scope and no staff rights.",
        404 => "404 — not found. Check the ids / slugs in the path variables.",
        422 => "422 — validation failed. Look at the response body for the field errors.",
        429 => "429 — rate limited. The default limits are 2 requests/second and 1200 requests/hour.",
        500..=599 => "Server error on the API side. Try again later.",
        _ => return None,
    })
}

/// The pretty-printed body, made once per response (`cache` is reset with the response).
fn pretty_of<'a>(cache: &'a mut Option<String>, body: &str) -> &'a str {
    cache.get_or_insert_with(|| match serde_json::from_str::<serde_json::Value>(body) {
        Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|_| body.to_owned()),
        Err(_) => body.to_owned(),
    })
}
