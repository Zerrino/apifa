//! What the UI asked for (`Action`), applied after the frame; requests sent and their answers.

use std::sync::mpsc::{self, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::ai;
use crate::flow::{self, Block, BlockKind, Flow, RunSettings};
use crate::model::{self, Kv, RequestSpec};
use crate::storage::{self, HistoryEntry};
use crate::subst;
use crate::theme;
use crate::ui_flow;
use crate::widgets::*;
use crate::workspace::{self, Node, SavedRequest};

use super::*;

impl App {
    pub(super) fn apply(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::Explain(d) => self.explain(d, ctx),
            Action::Open(r) => {
                if let Some(i) = self.tabs.iter().position(|t| matches!(&t.body, TabBody::Request(s) if s.spec.doc.as_ref() == Some(&r) && t.saved.is_none())) {
                    self.active = i;
                    return;
                }
                let Some((res, method)) = self.doc.lookup(&r) else { return };
                let Some(route) = method.routes.get(r.route) else { return };
                // From an imported API: bound to that API's profile.
                let profile = self.profiles.of_api(&res.api).map(|p| p.id.clone()).unwrap_or_default();
                let spec = RequestSpec { doc: Some(r.clone()), verb: route.verb.clone(), path: route.path.clone(), profile, ..Default::default() };
                let title = format!("{} › {}", res.name, method.name);
                self.push_request_tab(title, spec, None);
            }
            Action::NewTab => self.push_request_tab("New request".into(), RequestSpec::default(), None),
            Action::NewRequestIn(folder) => {
                let node = Node::Request(SavedRequest { uid: workspace::new_uid(), name: "New request".into(), spec: RequestSpec::default() });
                let uid = node.uid();
                if let Some(f) = self.workspace.folder_mut(folder) {
                    f.items.push(node);
                }
                self.save_workspace();
                self.apply_open_node(uid);
            }
            Action::NewFlow(folder) => {
                let mut flow = Flow::new("New flow");
                flow.blocks.push(Block::new_kind(BlockKind::from_id("request")));
                match folder {
                    Some(folder) => {
                        let uid = flow.uid;
                        if let Some(f) = self.workspace.folder_mut(folder) {
                            f.items.push(Node::Flow(flow));
                        }
                        self.save_workspace();
                        self.apply_open_node(uid);
                    }
                    None => self.push_flow_tab(flow, None),
                }
            }
            Action::NewFolder(parent) => self.dialog = Some(Dialog::NewFolder { parent, text: "New folder".into() }),
            Action::OpenNode(uid) => self.apply_open_node(uid),
            Action::Rename(uid) => {
                if let Some(n) = self.workspace.find(uid) {
                    self.dialog = Some(Dialog::Rename { uid, text: n.name().to_owned() });
                }
            }
            Action::Duplicate(uid) => {
                let Some(mut copy) = self.workspace.find(uid).cloned() else { return };
                copy.refresh_uids();
                copy.set_name(format!("{} (copy)", copy.name()));
                let parent = self.workspace.parent_of(uid).unwrap_or(self.workspace.uid);
                if let Some(f) = self.workspace.folder_mut(parent) {
                    let pos = f.items.iter().position(|n| n.uid() == uid).map(|p| p + 1).unwrap_or(f.items.len());
                    f.items.insert(pos, copy);
                }
                self.save_workspace();
            }
            Action::MoveTo(uid, folder) => {
                let into_itself = matches!(self.workspace.find(uid), Some(Node::Folder(f)) if f.uid == folder || f.find(folder).is_some());
                if into_itself {
                    self.toast("A folder can't be moved inside itself", theme::ORANGE);
                    return;
                }
                if let Some(node) = self.workspace.remove(uid) {
                    match self.workspace.folder_mut(folder) {
                        Some(f) => f.items.push(node),
                        None => self.workspace.items.push(node),
                    }
                    self.save_workspace();
                }
            }
            Action::Delete(uid) => {
                if let Some(n) = self.workspace.find(uid) {
                    self.dialog = Some(Dialog::ConfirmDelete { uid, label: format!("{} “{}”", n.kind_label(), n.name()) });
                }
            }
            Action::ExportNode(uid) => {
                let node = if uid == self.workspace.uid { Some(Node::Folder(self.workspace.clone())) } else { self.workspace.find(uid).cloned() };
                if let Some(node) = node {
                    self.export_to_file(&node);
                }
            }
            Action::CopyNode(uid) => {
                if let Some(node) = self.workspace.find(uid) {
                    ctx.copy_text(workspace::export(node));
                    self.toast("Copied as JSON — paste it with Import › Paste JSON", theme::GREEN);
                }
            }
            Action::ImportFile(folder) => {
                let tx = self.file_tx.clone();
                std::thread::spawn(move || {
                    if let Some(path) = rfd::FileDialog::new().add_filter("JSON", &["json"]).pick_file() {
                        let _ = tx.send(match std::fs::read_to_string(&path) {
                            Ok(text) => FileEvent::Imported { folder, text },
                            Err(e) => FileEvent::Failed(format!("Can't read {}: {e}", path.display())),
                        });
                    }
                });
            }
            Action::GraphQlSchema(id) => {
                let Some(i) = self.pos(id) else { return };
                let sender = crate::testrun::Sender {
                    agent: self.agent.clone(),
                    auth: self.auth.clone(),
                    creds: self.credentials(),
                    doc: self.doc.clone(),
                    rules: self.subs_rules.clone(),
                    cache: self.subs_cache.clone(),
                };
                let Some(Tab { body: TabBody::Request(tab), .. }) = self.tabs.get_mut(i) else { return };
                let mut spec = tab.spec.clone();
                spec.verb = "POST".into();
                spec.body_mode = crate::model::BodyMode::GraphQl;
                spec.raw_body = crate::graphql::INTROSPECTION_QUERY.into();
                spec.graphql_variables.clear();
                spec.pre_script.clear();
                spec.post_script.clear();
                let (tx, rx) = mpsc::channel();
                tab.schema_rx = Some(rx);
                let ctx = ctx.clone();
                std::thread::spawn(move || {
                    let r = sender.send(&spec).and_then(|r| {
                        if r.status >= 400 {
                            return Err(format!("HTTP {} {}: {}", r.status, r.status_text, r.body.chars().take(200).collect::<String>()));
                        }
                        crate::graphql::parse_introspection(&r.body)
                    });
                    let _ = tx.send(r);
                    ctx.request_repaint();
                });
            }
            Action::FolderSettings(uid) => {
                let (auth, profile) = self.workspace.folder_mut(uid).map(|f| (f.auth.clone(), f.profile.clone())).unwrap_or_default();
                self.dialog = Some(Dialog::FolderSettings { uid, auth: Box::new(auth), profile, show: false });
            }
            Action::ImportApiFile => {
                let tx = self.file_tx.clone();
                std::thread::spawn(move || {
                    if let Some(path) = rfd::FileDialog::new().add_filter("OpenAPI / Swagger / apipie", &["json", "yaml", "yml"]).pick_file() {
                        let _ = tx.send(match std::fs::read_to_string(&path) {
                            Ok(text) => FileEvent::Api { text, source: path.display().to_string() },
                            Err(e) => FileEvent::Failed(format!("Can't read {}: {e}", path.display())),
                        });
                    }
                });
            }
            Action::ImportApiUrl(url) => {
                let (tx, agent, ctx) = (self.file_tx.clone(), self.agent.clone(), ctx.clone());
                std::thread::spawn(move || {
                    let got = agent.get(url.trim()).call().map_err(|e| e.to_string()).and_then(|mut r| {
                        let code = r.status().as_u16();
                        let text = r.body_mut().with_config().limit(32 * 1024 * 1024).read_to_string().map_err(|e| e.to_string())?;
                        if code >= 400 {
                            return Err(format!("HTTP {code}"));
                        }
                        Ok(text)
                    });
                    let _ = tx.send(match got {
                        Ok(text) => FileEvent::Api { text, source: url.trim().to_owned() },
                        Err(e) => FileEvent::Failed(format!("Can't fetch {}: {e}", url.trim())),
                    });
                    ctx.request_repaint();
                });
            }
            Action::RemoveApi(name) => match crate::apis::remove(&name) {
                Ok(()) => {
                    self.reload_doc();
                    self.toast(format!("API “{name}” removed"), theme::CYAN);
                }
                Err(e) => self.toast(e, theme::RED),
            },
            Action::ToggleApi(name, on) => {
                if let Err(e) = crate::apis::set_enabled(&name, on) {
                    self.toast(e, theme::RED);
                }
                self.reload_doc();
            }
            Action::ImportPaste(folder) => self.dialog = Some(Dialog::Import { folder, text: String::new(), error: None }),
            Action::Close(id) => {
                let Some(i) = self.pos(id) else { return };
                if i < self.tabs.len() {
                    if self.has_unsaved_changes(i) {
                        self.active = i;
                        self.dialog = Some(Dialog::ConfirmClose { tab: id });
                    } else {
                        self.apply(Action::ForceClose(id), ctx);
                    }
                }
            }
            Action::ForceClose(id) => {
                let Some(i) = self.pos(id) else { return };
                if i < self.tabs.len() {
                    if let TabBody::Flow(f) = &self.tabs[i].body {
                        if let Some(run) = &f.run {
                            run.stop.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    self.tabs.remove(i);
                    if self.active > i || self.active >= self.tabs.len() {
                        self.active = self.active.saturating_sub(1);
                    }
                }
            }
            Action::Select(id) => {
                if let Some(i) = self.pos(id) {
                    self.active = i;
                }
            }
            Action::Send(id) => {
                let Some(i) = self.pos(id) else { return };
                match &self.tabs[i].body {
                    TabBody::Request(r) if r.spec.protocol == crate::model::Protocol::WebSocket => {
                        if r.live.connected() {
                            let (text, bin) = (r.live.draft.clone(), r.live.binary);
                            self.ws_send(id, text, bin);
                        } else {
                            self.ws_connect(id, ctx);
                        }
                    }
                    TabBody::Request(r) if r.spec.protocol == crate::model::Protocol::Grpc => self.grpc_call(id, ctx),
                    _ => self.send(i, ctx),
                }
            }
            Action::WsConnect(id) => self.ws_connect(id, ctx),
            Action::WsDisconnect(id) => self.ws_disconnect(id),
            Action::WsSend(id, text, bin) => self.ws_send(id, text, bin),
            Action::GrpcLoad(id) => self.grpc_load(id, ctx),
            Action::GrpcCall(id) => self.grpc_call(id, ctx),
            Action::GrpcAddProtos(id) => self.grpc_add_protos(id),
            Action::NewLive(p) => {
                let (title, path) = match p {
                    crate::model::Protocol::WebSocket => ("New WebSocket", "wss://echo.websocket.org"),
                    crate::model::Protocol::Grpc => ("New gRPC", "https://grpcb.in:9001"),
                    crate::model::Protocol::Http => ("New request", "/"),
                };
                self.push_request_tab(title.into(), RequestSpec { protocol: p, path: path.into(), ..Default::default() }, None);
            }
            Action::SelectEnv(name) => self.select_env(name),
            Action::SelectProfile(id) => self.select_profile(id),
            Action::EditProfiles => self.show_profiles = true,
            Action::EditEnvs => self.show_envs = true,
            Action::Understand => self.start_understand(ctx),
            Action::ShowDataMap => self.show_data_map(),
            Action::CancelSend(id) => {
                if let Some(TabBody::Request(r)) = self.pos(id).map(|i| &mut self.tabs[i].body) {
                    // The worker thread finishes on its own; its result has nowhere to go.
                    if r.pending.take().is_some() {
                        r.inflight = None;
                        r.error = Some("Cancelled. The request may still have reached the API; its answer is ignored.".into());
                    }
                }
            }
            Action::Paginate(id, page) => {
                let Some(i) = self.pos(id) else { return };
                let doc = self.doc.clone();
                if let Some(TabBody::Request(st)) = self.tabs.get_mut(i).map(|t| &mut t.body) {
                    let documented =
                        st.spec.doc.as_ref().and_then(|d| doc.lookup(d)).is_some_and(|(_, m)| has_param(&m.params, "page[number]")) && !model::has_body(&st.spec.verb);
                    if documented {
                        st.spec.values.insert("page[number]".into(), page.to_string());
                    } else if let Some(kv) = st.spec.query.iter_mut().find(|kv| kv.key == "page[number]") {
                        kv.value = page.to_string();
                        kv.enabled = true;
                    } else {
                        st.spec.query.push(Kv { enabled: true, key: "page[number]".into(), value: page.to_string() });
                    }
                }
                self.send(i, ctx);
            }
            Action::SaveTab(id) => {
                let Some(i) = self.pos(id) else { return };
                let Some(tab) = self.tabs.get(i) else { return };
                // The Data map is saved by Understand itself.
                if matches!(tab.body, TabBody::DataMap) {
                    return;
                }
                match tab.saved.filter(|uid| self.workspace.find(*uid).is_some()) {
                    Some(uid) => {
                        if let (Some(node), Some(slot)) = (tab.to_node(uid), self.workspace.find_mut(uid)) {
                            *slot = node;
                        }
                        self.save_workspace();
                        self.toast(format!("Saved “{}”", self.tabs[i].title), theme::GREEN);
                    }
                    None => self.apply(Action::SaveTabAs(id), ctx),
                }
            }
            Action::SaveTabAs(id) => {
                let Some(i) = self.pos(id) else { return };
                if let Some(tab) = self.tabs.get(i) {
                    let folder = tab.saved.and_then(|u| self.workspace.parent_of(u)).unwrap_or_else(|| self.default_folder());
                    self.dialog = Some(Dialog::SaveAs { tab: id, name: tab.title.clone(), folder, new_folder: String::new() });
                }
            }
            Action::ExportTab(id) => {
                let Some(i) = self.pos(id) else { return };
                if let Some(node) = self.tabs.get(i).and_then(|t| t.to_node(workspace::new_uid())) {
                    self.export_to_file(&node);
                }
            }
            Action::CopyTab(id) => {
                let Some(i) = self.pos(id) else { return };
                if let Some(node) = self.tabs.get(i).and_then(|t| t.to_node(workspace::new_uid())) {
                    ctx.copy_text(workspace::export(&node));
                    self.toast("Copied as JSON — anyone can paste it with Import › Paste JSON", theme::GREEN);
                }
            }
            Action::RequestToFlow(id) => {
                let Some(i) = self.pos(id) else { return };
                let doc = self.doc.clone();
                let Some(tab) = self.tabs.get(i) else { return };
                let TabBody::Request(st) = &tab.body else { return };
                let mut flow = Flow::new(format!("Flow: {}", tab.title));
                flow.blocks.push(ui_flow::block_from_spec(&doc, &st.spec));
                flow.blocks.push(Block::new_kind(BlockKind::Log { message: "Status {{res.status}}".into() }));
                self.push_flow_tab(flow, None);
            }
            Action::Visualize(id) => {
                let Some(i) = self.pos(id) else { return };
                let Some(Tab { title, body: TabBody::Request(st), .. }) = self.tabs.get(i) else { return };
                let Some(r) = &st.response else { return };
                let url = crate::model::prepare(&st.spec, None).url;
                let origin = crate::model::split_origin(&url).0.to_owned();
                match crate::visualize::open(title, &st.spec.visualizer, r, &origin) {
                    Ok(path) => self.toast(format!("Opened in your browser ({})", path.display()), theme::GREEN),
                    Err(e) => self.toast(e, theme::RED),
                }
            }
            Action::VisualizeFlow(id) => {
                let Some(i) = self.pos(id) else { return };
                let Some(Tab { title, body: TabBody::Flow(st), .. }) = self.tabs.get(i) else { return };
                let Some(vars) = &st.last_vars else { return };
                let empty = serde_json::Map::new();
                let outputs = st.flow.outputs_of(vars.as_object().unwrap_or(&empty));
                let ok = st.result.is_some_and(|(ok, _)| ok);
                match crate::visualize::open_flow(title, &st.flow.visualizer, ok, &outputs, vars) {
                    Ok(path) => self.toast(format!("Opened in your browser ({})", path.display()), theme::GREEN),
                    Err(e) => self.toast(e, theme::RED),
                }
            }
            Action::ExportFlowResults(id) => {
                let Some(i) = self.pos(id) else { return };
                let Some(Tab { title, body: TabBody::Flow(st), .. }) = self.tabs.get(i) else { return };
                let Some(run) = ui_flow::current_run(st) else { return };
                let name = format!("{}.results.json", sanitize(title));
                if let Some(path) = rfd::FileDialog::new().add_filter("JSON", &["json"]).set_file_name(&name).save_file() {
                    let text = serde_json::to_string_pretty(&run).unwrap_or_default();
                    match std::fs::write(&path, text) {
                        Ok(()) => self.toast(format!("Results saved to {}", path.display()), theme::GREEN),
                        Err(e) => self.toast(format!("Export failed: {e}"), theme::RED),
                    }
                }
            }
            Action::LoadFlowResults(id) => {
                let Some(i) = self.pos(id) else { return };
                let Some(path) = rfd::FileDialog::new().add_filter("JSON", &["json"]).pick_file() else { return };
                let loaded = std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| crate::runs::parse(&t));
                let Some(Tab { body: TabBody::Flow(st), .. }) = self.tabs.get_mut(i) else { return };
                match loaded {
                    Ok(run) => {
                        st.reset_console();
                        let file = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                        st.log.push(flow::LogLine { kind: flow::LogKind::Info, depth: 0, text: format!("Results loaded from {file} (nothing was sent)"), detail: None });
                        ui_flow::set_results(st, run.ok, Duration::from_millis(run.duration_ms), run.vars, run.at, "loaded from a file");
                        if st.flow.keep_results {
                            ui_flow::save_results(st);
                        }
                    }
                    Err(e) => self.toast(format!("Can't load these results: {e}"), theme::RED),
                }
            }
            Action::DiscardResume(id) => {
                let Some(i) = self.pos(id) else { return };
                if let Some(Tab { body: TabBody::Flow(st), .. }) = self.tabs.get_mut(i) {
                    crate::resume::discard(st.flow.uid);
                    st.resume = None;
                }
            }
            Action::GenerateDocs(uid) => self.dialog = Some(Dialog::Docs { uid, api: None, format: crate::docgen::Format::Html, examples: true, open: true }),
            Action::GenerateApiDocs(res) => {
                self.dialog = Some(Dialog::Docs { uid: 0, api: Some(res.into_iter().collect()), format: crate::docgen::Format::Html, examples: true, open: true })
            }
            Action::RunTests(uid) => {
                if self.tests.as_ref().is_some_and(|t| t.finished.is_none()) {
                    self.toast("Tests are already running", theme::ORANGE);
                    return;
                }
                let (node, title) = if uid == self.workspace.uid {
                    (Node::Folder(self.workspace.clone()), "the workspace".to_owned())
                } else {
                    match self.workspace.find(uid) {
                        Some(n) => (n.clone(), n.name().to_owned()),
                        None => return,
                    }
                };
                let items = crate::testrun::collect(&node, "");
                let sender = crate::testrun::Sender {
                    agent: self.agent.clone(),
                    auth: self.auth.clone(),
                    creds: self.credentials(),
                    doc: self.doc.clone(),
                    rules: self.subs_rules.clone(),
                    cache: self.subs_cache.clone(),
                };
                let settings = self.run_settings();
                let (tx, rx) = mpsc::channel();
                let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let (run_items, run_stop, ctx) = (items.clone(), stop.clone(), ctx.clone());
                std::thread::spawn(move || {
                    crate::testrun::run(&run_items, &sender, &settings, &run_stop, |r| {
                        let _ = tx.send(r);
                        ctx.request_repaint();
                    });
                    ctx.request_repaint();
                });
                self.tests = Some(crate::ui_tests::TestRun { title, items, results: Vec::new(), rx, stop, started: Instant::now(), finished: None });
            }
            Action::RunFlow(id) | Action::ResumeFlow(id) | Action::RunFlowTo(id, _) => {
                let Some(i) = self.pos(id) else { return };
                let resume = matches!(action, Action::ResumeFlow(_));
                let stop_after = match action {
                    Action::RunFlowTo(_, uid) => Some(uid),
                    _ => None,
                };
                let (agent, auth, creds) = (self.agent.clone(), self.auth.clone(), self.credentials());
                let mut settings = self.run_settings();
                // Its requests use its folders' profile, else the active one.
                settings.profile = self.profile_of(self.tabs.get(i).and_then(|t| t.saved));
                if let Some(Tab { body: TabBody::Flow(st), title, .. }) = self.tabs.get_mut(i) {
                    if st.running() {
                        return;
                    }
                    let mut flow = st.flow.clone();
                    flow.name = title.clone();
                    st.reset_console();
                    let mut settings = settings;
                    settings.stop_after = stop_after;
                    // Every response is kept as it arrives, so a stopped run can be resumed.
                    match crate::resume::prepare(&mut flow, resume) {
                        Ok(cp) => settings.checkpoint = Some(Arc::new(cp)),
                        Err(e) if resume => {
                            st.resume = None;
                            self.toast(format!("Can't resume: {e}"), theme::RED);
                            return;
                        }
                        Err(_) => {}
                    }
                    st.resume = None;
                    let ctx = ctx.clone();
                    st.run = Some(flow::start(flow, agent, auth, creds, settings, move || ctx.request_repaint()));
                }
            }
            Action::StopFlow(id) => {
                let Some(i) = self.pos(id) else { return };
                if let Some(Tab { body: TabBody::Flow(st), .. }) = self.tabs.get(i) {
                    if let Some(run) = &st.run {
                        run.stop.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
            Action::FetchToken(a) => {
                let fill = |v: &str| crate::dates::expand(&crate::env::expand(v));
                let crate::auth::Auth::OAuth2(o) = a.or(&self.config.auth).expand(&fill) else { return };
                let (agent, tokens, ctx) = (self.agent.clone(), self.auth.clone(), ctx.clone());
                self.auth.lock().status = crate::auth::Status::Fetching;
                std::thread::spawn(move || {
                    let _ = crate::auth::token(&agent, &tokens, &o, &crate::auth::Refresh::Always);
                    ctx.request_repaint();
                });
            }
            Action::Restore(i) => {
                if let Some(h) = self.history.get(i).cloned() {
                    self.push_request_tab(h.title, h.spec, None);
                }
            }
            Action::ClearHistory => {
                self.history.clear();
                storage::save("history.json", &self.history);
            }
            Action::ShowCode => self.show_code = true,
            Action::OpenSettings => self.show_settings = true,
            Action::Uninstall => self.dialog = Some(Dialog::Uninstall { delete_data: false, error: None }),
            Action::CloseMany(list) => {
                let mut kept = 0;
                for id in list {
                    let Some(i) = self.pos(id) else { continue };
                    if self.has_unsaved_changes(i) {
                        kept += 1;
                    } else {
                        self.apply(Action::ForceClose(id), ctx);
                    }
                }
                if kept > 0 {
                    self.toast(format!("{kept} tab(s) with unsaved changes were kept open"), theme::ORANGE);
                }
            }
            Action::NavBack | Action::NavForward => {
                let back = matches!(action, Action::NavBack);
                let (from, to) = if back { (&mut self.nav.back, &mut self.nav.forward) } else { (&mut self.nav.forward, &mut self.nav.back) };
                // Skip tabs closed since.
                while let Some(id) = from.pop() {
                    if let Some(i) = self.tabs.iter().position(|t| t.id == id) {
                        if let Some(cur) = self.nav.current {
                            to.push(cur);
                        }
                        self.active = i;
                        self.nav.moving = true;
                        break;
                    }
                }
            }
            Action::ToggleFavorite(f) => {
                self.favorites.toggle(f);
                self.save_favorites();
            }
            Action::AddFavorite(f, folder) => {
                self.favorites.add_to(f, folder);
                self.save_favorites();
                self.side = SideTab::Favorites;
            }
            Action::NewFavFolder(parent) => self.dialog = Some(Dialog::FavFolder { parent, rename: None, text: "New folder".into() }),
            Action::RenameFavFolder(uid) => {
                let text = self.favorites.folder_name(uid).unwrap_or_default();
                self.dialog = Some(Dialog::FavFolder { parent: None, rename: Some(uid), text });
            }
            Action::DeleteFavFolder(uid) => {
                self.favorites.delete_folder(uid);
                self.save_favorites();
            }
            Action::MoveFavFolder(uid, to) => {
                if self.favorites.move_folder(uid, to) {
                    self.save_favorites();
                } else {
                    self.toast("A folder can't be moved inside itself", theme::ORANGE);
                }
            }
            Action::ForgetExample(d) => {
                let mut ex = self.learned.lock();
                if ex.forget(&d) {
                    ex.save();
                }
            }
            Action::AiToggle => {
                self.chat.open = !self.chat.open;
                self.chat.focus = self.chat.open;
            }
            Action::AiNewChat => self.chat.clear(),
            Action::AiStop => self.chat.cancel(),
            Action::AiSend(text) => {
                // Kept as it goes to the AI (the logins / names / emails the app has seen as fingerprints),
                // so the conversation stays the same from one request to the next; shown revealed.
                let text = crate::privacy::mask_text(&text);
                // After a stop the last message can already be the user's: two user turns in a row become one.
                match self.chat.history.last_mut().filter(|m| m.role == ai::Role::User) {
                    Some(last) => last.parts.push(ai::Part::Text(text)),
                    None => self.chat.history.push(ai::Message::user(vec![ai::Part::Text(text)])),
                }
                self.ai_call(ctx);
            }
            Action::AiConfirm(ok) => {
                let Some((id, _, input)) = self.chat.confirm.take() else { return };
                if ok {
                    self.ai_send_request(id, &input, ctx);
                } else {
                    self.chat.results.push(ai::Part::ToolResult { id, content: "The user chose not to send this request.".into(), is_error: true });
                }
            }
            Action::AiLoadModels => {
                let (tx, rx) = mpsc::channel();
                self.ai_setup.models_rx = Some(rx);
                self.ai_setup.models_error = None;
                let (cfg, ctx) = (self.config.ai.clone(), ctx.clone());
                std::thread::spawn(move || {
                    let _ = tx.send(ai::list_models(&cfg));
                    ctx.request_repaint();
                });
            }
            Action::NewSubst(folder) => self.subst_edit = Some(SubstEditor::new(subst::Rule::default(), folder, true)),
            Action::EditSubst(uid) => {
                if let Some(r) = self.subs.rule(uid) {
                    self.subst_edit = Some(SubstEditor::new(r.clone(), self.subs.parent_of(uid), false));
                }
            }
            Action::ToggleSubst(uid) => {
                if let Some(mut r) = self.subs.rule(uid).cloned() {
                    r.enabled = !r.enabled;
                    self.subs.upsert(r, None);
                    self.save_subs();
                }
            }
            Action::DuplicateSubst(uid) => {
                if let Some(mut r) = self.subs.rule(uid).cloned() {
                    r.uid = workspace::new_uid();
                    r.name = format!("{} (copy)", r.name);
                    let folder = self.subs.parent_of(uid);
                    self.subs.upsert(r, folder);
                    self.save_subs();
                }
            }
            Action::DeleteSubst(uid) => {
                let parent = self.subs.parent_of(uid);
                if let Some(subst::Entry::Folder { items, .. }) = self.subs.remove(uid) {
                    for e in items {
                        self.subs.insert(e, parent);
                    }
                }
                self.save_subs();
            }
            Action::CopySubst(uid) => {
                if let Some(e) = self.subs.entry(uid) {
                    ctx.copy_text(subst::export(e));
                    self.toast("Copied as JSON — paste it with Substitutions › Import › Paste JSON", theme::GREEN);
                }
            }
            Action::ExportSubst(uid) => {
                let Some(e) = self.subs.entry(uid) else { return };
                let name = match e {
                    subst::Entry::Rule(r) => r.name.clone(),
                    subst::Entry::Folder { name, .. } => name.clone(),
                };
                let text = subst::export(e);
                if let Some(path) = rfd::FileDialog::new().add_filter("JSON", &["json"]).set_file_name(format!("{}.substitutions.json", sanitize(&name))).save_file() {
                    match std::fs::write(&path, text) {
                        Ok(()) => self.toast(format!("Exported to {}", path.display()), theme::GREEN),
                        Err(e) => self.toast(format!("Export failed: {e}"), theme::RED),
                    }
                }
            }
            Action::ImportSubstFile(folder) => {
                if let Some(path) = rfd::FileDialog::new().add_filter("JSON", &["json"]).pick_file() {
                    let result = std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| self.import_subs(folder, &t));
                    match result {
                        Ok(n) => self.toast(format!("Imported {n} item(s)"), theme::GREEN),
                        Err(e) => self.toast(format!("Import failed: {e}"), theme::RED),
                    }
                }
            }
            Action::ImportSubstPaste(folder) => self.dialog = Some(Dialog::SubstImport { folder, text: String::new(), error: None }),
            Action::NewSubstFolder(parent) => self.dialog = Some(Dialog::SubstFolder { parent, rename: None, text: "New folder".into() }),
            Action::RenameSubstFolder(uid) => {
                if let Some(subst::Entry::Folder { name, .. }) = self.subs.entry(uid) {
                    self.dialog = Some(Dialog::SubstFolder { parent: None, rename: Some(uid), text: name.clone() });
                }
            }
            Action::MoveSubst(uid, to) => {
                if self.subs.move_to(uid, to) {
                    self.save_subs();
                } else {
                    self.toast("A folder can't be moved inside itself", theme::ORANGE);
                }
            }
            Action::ToggleFavoriteTab(id) => match self.pos(id).and_then(|i| self.tab_favorite(i)) {
                Some(f) => self.apply(Action::ToggleFavorite(f), ctx),
                None => self.toast("Save it in the workspace first to star it", theme::ORANGE),
            },
        }
    }

    pub(super) fn run_settings(&self) -> RunSettings {
        RunSettings {
            delay: Duration::from_millis(self.config.flow_delay_ms),
            parallel: self.config.parallel,
            parallel_rate: self.config.parallel_rate,
            max_pages: self.config.max_pages,
            doc: self.doc.clone(),
            flows: Arc::new(self.workspace.flows().into_iter().cloned().collect()),
            subs: self.subs_rules.clone(),
            subs_cache: self.subs_cache.clone(),
            checkpoint: None,
            stop_after: None,
            profile: String::new(),
        }
    }

    pub(super) fn send(&mut self, i: usize, ctx: &egui::Context) {
        let doc = self.doc.clone();
        let creds = self.credentials();
        let (agent, auth) = (self.agent.clone(), self.auth.clone());
        // A saved request whose auth is Inherit takes its folders'; its profile, its folders' else the active one.
        let saved = self.tabs.get(i).and_then(|t| t.saved);
        let folder_auth = saved.map(|uid| self.workspace.auth_for(uid)).unwrap_or_default();
        let profile = self.profile_of(saved);
        let Some(Tab { body: TabBody::Request(tab), .. }) = self.tabs.get_mut(i) else { return };
        if tab.pending.is_some() {
            return;
        }
        let method = tab.spec.doc.as_ref().and_then(|d| doc.lookup(d)).map(|(_, m)| m);
        let prepared = model::prepare(&tab.spec, method);
        if !prepared.missing_path.is_empty() {
            tab.error = Some(format!("Fill the path variable(s) first: {}", prepared.missing_path.iter().map(|v| format!(":{v}")).collect::<Vec<_>>().join(", ")));
            tab.response = None;
            return;
        }
        if let Some(e) = &prepared.body_error {
            tab.error = Some(e.clone());
            tab.response = None;
            return;
        }
        let (tx, rx) = mpsc::channel();
        tab.pending = Some(rx);
        tab.inflight = Some((tab.spec.clone(), prepared.url.clone()));
        tab.sent_at = Some(Instant::now());
        tab.error = None;
        let ctx = ctx.clone();
        let mut spec = tab.spec.clone();
        if spec.auth.is_inherit() {
            spec.auth = folder_auth;
        }
        if spec.profile.is_empty() {
            spec.profile = profile;
        }
        let sender = crate::testrun::Sender { agent, auth, creds, doc, rules: self.subs_rules.clone(), cache: self.subs_cache.clone() };
        std::thread::spawn(move || {
            let result = sender.send(&spec);
            // The logins / names it holds: hidden from then on in what the user writes to the AI.
            if let Ok(r) = &result {
                crate::privacy::learn_body(&r.body);
            }
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    pub(super) fn poll(&mut self) {
        let mut history_changed = false;
        let mut learned = Vec::new();
        let mut gets = Vec::new();
        for t in &mut self.tabs {
            let tab = match &mut t.body {
                TabBody::Flow(f) => {
                    ui_flow::poll(f);
                    continue;
                }
                TabBody::DataMap => continue,
                TabBody::Request(r) => r,
            };
            tab.live.poll();
            if let Some(rx) = &tab.schema_rx {
                if let Ok(r) = rx.try_recv() {
                    tab.graphql = Some(r);
                    tab.schema_rx = None;
                }
            }
            let Some(rx) = &tab.pending else { continue };
            let result = match rx.try_recv() {
                Ok(r) => r,
                Err(TryRecvError::Empty) => continue,
                Err(TryRecvError::Disconnected) => Err("the request worker stopped unexpectedly".into()),
            };
            tab.pending = None;
            let (spec, url) = tab.inflight.take().unwrap_or_else(|| (tab.spec.clone(), String::new()));
            let status = result.as_ref().ok().map(|r| r.status);
            match result {
                Ok(r) => {
                    // No example response in the docs: this one becomes it (Examples tab, and the assistant reads it).
                    if let Some((d, m)) = spec.doc.as_ref().and_then(|d| Some((d, self.doc.lookup(d)?.1))) {
                        if crate::learned::record_shared(&self.learned, d, m, &spec.verb, &url, &r) {
                            learned.push(m.name.clone());
                        }
                    }
                    if spec.verb.eq_ignore_ascii_case("GET") && r.status < 400 && r.is_json && crate::net::mock_target().is_none() {
                        gets.push((model::path_of(&url).to_owned(), r.body.clone(), spec.profile.clone()));
                    }
                    tab.tests = crate::assert::check(&spec.assertions, &crate::assert::context(&r), &|v| crate::dates::expand(v));
                    // The post-response script's `pm.test`s with them.
                    if let Some(rep) = &r.script {
                        tab.tests.extend(rep.tests.iter().map(|t| crate::assert::Outcome {
                            text: format!("pm.test “{}”", t.name),
                            passed: t.passed,
                            detail: t.error.clone().unwrap_or_default(),
                        }));
                    }
                    tab.response = Some(r);
                    // The view cache is keyed by the text's address: the new body can reuse the old one's.
                    tab.view = None;
                    tab.find_cache = None;
                    tab.pretty_body = None;
                    tab.error = None;
                }
                Err(e) => {
                    tab.response = None;
                    tab.tests.clear();
                    tab.error = Some(e);
                }
            }
            self.history.push(HistoryEntry { title: t.title.clone(), verb: spec.verb.clone(), url, status, when: storage::now(), spec });
            history_changed = true;
        }
        for name in learned {
            self.toast(format!("The docs had no example response for “{name}”: this one was saved (Examples tab)"), theme::CYAN);
        }
        for (path, body, profile) in gets {
            self.absorb_get(&path, &body, &profile);
        }
        if history_changed {
            let excess = self.history.len().saturating_sub(storage::MAX_HISTORY);
            self.history.drain(..excess);
            storage::save("history.json", &self.history);
        }
        while let Ok(ev) = self.file_rx.try_recv() {
            match ev {
                FileEvent::Imported { folder, text } => {
                    if let Err(e) = self.import_node(folder, &text) {
                        self.toast(e, theme::RED);
                    }
                }
                FileEvent::Api { text, source } => self.import_api(&text, &source),
                FileEvent::Exported(path) => self.toast(format!("Exported to {}", path.display()), theme::GREEN),
                FileEvent::Failed(e) => self.toast(e, theme::RED),
            }
        }
        self.watch_workspace(false);
        if let Ok(st) = self.install_rx.try_recv() {
            self.install = Some(st);
        }
        self.toasts.retain(|t| t.until > Instant::now());
    }
}
