//! Application shell: title bar, settings, sidebar (endpoints / workspace / history),
//! tabs (requests and flows), dialogs, import / export.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::Color32;
use parking_lot::Mutex;

use crate::docs::ApiDoc;
use crate::favorites::Favorites;
use crate::flow::{self, Flow};
use crate::model::{DocRef, RequestSpec};
use crate::net::{self, Credentials, SharedAuth};
use crate::storage::{self, Config, HistoryEntry, SavedTab, Session};
use crate::subst::{self, Substitutions};
use crate::ui_ai;
use crate::ui_flow::{self, FlowState};
use crate::ui_request::ReqState;
use crate::widgets::*;
use crate::workspace::{self, Folder, Node, SavedRequest};
use crate::{install, theme, workspace_fs};

mod actions;
mod chrome;
mod datamap_ui;
mod dialogs;
mod envs;
mod live;
mod profiles_ui;
mod services;
mod settings;
mod sidebar;

use sidebar::*;

/// A tab, by its id: actions and dialogs keep naming the right one when tabs are closed or moved
/// in between (a position could then point at another tab).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TabId(pub u64);

pub enum Action {
    Open(DocRef),
    FolderSettings(u64),
    /// Import an API definition (OpenAPI / Swagger / apipie) from a file, or from a URL.
    ImportApiFile,
    ImportApiUrl(String),
    RemoveApi(String),
    ToggleApi(String, bool),
    /// WebSocket / gRPC tabs.
    WsConnect(TabId),
    WsDisconnect(TabId),
    WsSend(TabId, String, bool),
    GrpcLoad(TabId),
    GrpcCall(TabId),
    GrpcAddProtos(TabId),
    /// The active API profile (None: none), the profiles' editor.
    SelectProfile(Option<String>),
    EditProfiles,
    /// A new tab of this kind (WebSocket, gRPC).
    NewLive(crate::model::Protocol),
    /// GraphQL: read the schema of the tab's URL (introspection).
    GraphQlSchema(TabId),
    /// Have the AI explain this endpoint (“Explained by AI”).
    Explain(DocRef),
    NewTab,
    NewRequestIn(u64),
    NewFlow(Option<u64>),
    NewFolder(u64),
    OpenNode(u64),
    Rename(u64),
    Duplicate(u64),
    MoveTo(u64, u64),
    Delete(u64),
    ExportNode(u64),
    CopyNode(u64),
    ImportFile(u64),
    ImportPaste(u64),
    Close(TabId),
    ForceClose(TabId),
    Select(TabId),
    Send(TabId),
    /// Stop waiting for a request tab's answer (it's ignored when it comes).
    CancelSend(TabId),
    /// Send every saved GET and map the structure of the data (datamap.rs).
    Understand,
    ShowDataMap,
    /// Make this environment the active one (None: none).
    SelectEnv(Option<String>),
    EditEnvs,
    Paginate(TabId, u64),
    SaveTab(TabId),
    SaveTabAs(TabId),
    ExportTab(TabId),
    CopyTab(TabId),
    RequestToFlow(TabId),
    /// Open the tab's response with its visualization script in the browser.
    Visualize(TabId),
    /// Same with a flow's last run.
    VisualizeFlow(TabId),
    ExportFlowResults(TabId),
    LoadFlowResults(TabId),
    RunFlow(TabId),
    /// Run the flow and stop once this block (uid) is done.
    RunFlowTo(TabId, u64),
    /// Goes on with the stopped run of this flow tab (responses already received aren't asked again).
    ResumeFlow(TabId),
    /// Forgets the stopped run of this flow tab.
    DiscardResume(TabId),
    /// Test runner on a folder (the workspace root too), a request or a flow.
    RunTests(u64),
    /// Documentation of a folder (the workspace root too).
    GenerateDocs(u64),
    /// Documentation of the imported APIs: one resource (its key), or all of them.
    GenerateApiDocs(Option<String>),
    StopFlow(TabId),
    /// Get an OAuth 2.0 token now for this auth (Settings, a request's Auth tab).
    FetchToken(crate::auth::Auth),
    Restore(usize),
    ClearHistory,
    ShowCode,
    OpenSettings,
    Uninstall,
    /// Close several tabs; the ones with unsaved changes stay open.
    CloseMany(Vec<TabId>),
    ToggleFavorite(Favorite),
    /// Previous / next tab in the visit history (like a browser).
    NavBack,
    NavForward,
    /// Star into a folder (or move there when already starred).
    AddFavorite(Favorite, Option<u64>),
    NewFavFolder(Option<u64>),
    RenameFavFolder(u64),
    DeleteFavFolder(u64),
    MoveFavFolder(u64, Option<u64>),
    /// Star of the active tab (its saved item, or its documented endpoint).
    ToggleFavoriteTab(TabId),
    NewSubst(Option<u64>),
    EditSubst(u64),
    ToggleSubst(u64),
    DuplicateSubst(u64),
    /// A rule, or a folder (its content moves up).
    DeleteSubst(u64),
    CopySubst(u64),
    ExportSubst(u64),
    ImportSubstFile(Option<u64>),
    ImportSubstPaste(Option<u64>),
    NewSubstFolder(Option<u64>),
    RenameSubstFolder(u64),
    MoveSubst(u64, Option<u64>),
    /// Remove the example response recorded for an endpoint.
    ForgetExample(DocRef),
    /// Show / hide the AI chat.
    AiToggle,
    AiNewChat,
    AiStop,
    AiSend(String),
    /// The user's answer to "send this request?".
    AiConfirm(bool),
    AiLoadModels,
}

pub use crate::favorites::Favorite;

// A handful of tabs: boxing the big variant would save nothing worth the indirection.
#[allow(clippy::large_enum_variant)]
pub enum TabBody {
    Request(ReqState),
    Flow(FlowState),
    /// The Data map (one tab at most; what it shows is `App::map_view`).
    DataMap,
}

pub struct Tab {
    pub id: u64,
    pub title: String,
    /// Workspace node this tab was opened from / saved to.
    pub saved: Option<u64>,
    pub body: TabBody,
}

impl Tab {
    fn badge(&self) -> (&str, Color32) {
        match &self.body {
            TabBody::Request(r) => match r.spec.protocol {
                crate::model::Protocol::Http => (theme::short_verb(&r.spec.verb), theme::verb_color(&r.spec.verb)),
                crate::model::Protocol::WebSocket => ("WS", theme::CYAN),
                crate::model::Protocol::Grpc => ("gRPC", theme::GREEN),
            },
            TabBody::Flow(_) => ("FLOW", theme::CYAN),
            TabBody::DataMap => ("MAP", theme::GREEN),
        }
    }

    fn busy(&self) -> bool {
        match &self.body {
            TabBody::Request(r) => r.pending.is_some() || r.live.busy(),
            TabBody::Flow(f) => f.running(),
            TabBody::DataMap => false,
        }
    }

    /// The tab's content as a workspace node (the Data map isn't one).
    fn to_node(&self, uid: u64) -> Option<Node> {
        Some(match &self.body {
            TabBody::Request(r) => Node::Request(SavedRequest { uid, name: self.title.clone(), spec: r.spec.clone() }),
            TabBody::Flow(f) => {
                let mut flow = f.flow.clone();
                flow.uid = uid;
                flow.name = self.title.clone();
                Node::Flow(flow)
            }
            TabBody::DataMap => return None,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SideTab {
    Endpoints,
    Favorites,
    Workspace,
    History,
    Substitutions,
}

/// The substitution being edited in its window.
struct SubstEditor {
    rule: subst::Rule,
    folder: Option<u64>,
    is_new: bool,
    /// `rule.fields` as typed (comma separated).
    fields: String,
    test_value: String,
    test: Option<Result<String, String>>,
    test_rx: Option<Receiver<Result<String, String>>>,
}

impl SubstEditor {
    fn new(rule: subst::Rule, folder: Option<u64>, is_new: bool) -> Self {
        let fields = rule.fields.join(", ");
        Self { rule, folder, is_new, fields, test_value: String::new(), test: None, test_rx: None }
    }
}

enum Dialog {
    Rename {
        uid: u64,
        text: String,
    },
    NewFolder {
        parent: u64,
        text: String,
    },
    SaveAs {
        tab: TabId,
        name: String,
        folder: u64,
        new_folder: String,
    },
    Import {
        folder: u64,
        text: String,
        error: Option<String>,
    },
    ConfirmDelete {
        uid: u64,
        label: String,
    },
    ConfirmClose {
        tab: TabId,
    },
    /// A folder's settings: its auth (what its requests inherit).
    FolderSettings {
        uid: u64,
        auth: Box<crate::auth::Auth>,
        profile: String,
        show: bool,
    },
    Uninstall {
        delete_data: bool,
        error: Option<String>,
    },
    /// Create (`rename: None`) or rename a favorites folder.
    FavFolder {
        parent: Option<u64>,
        rename: Option<u64>,
        text: String,
    },
    /// Same for a substitutions folder.
    SubstFolder {
        parent: Option<u64>,
        rename: Option<u64>,
        text: String,
    },
    SubstImport {
        folder: Option<u64>,
        text: String,
        error: Option<String>,
    },
    /// `api`: the imported API docs (resource keys, empty = all) instead of the workspace folder `uid`.
    Docs {
        uid: u64,
        api: Option<Vec<String>>,
        format: crate::docgen::Format,
        examples: bool,
        open: bool,
    },
}

enum FileEvent {
    Imported {
        folder: u64,
        text: String,
    },
    /// An API definition read from a file or a URL.
    Api {
        text: String,
        source: String,
    },
    Exported(PathBuf),
    Failed(String),
}

struct Toast {
    text: String,
    color: Color32,
    until: Instant,
}

pub struct App {
    doc: Arc<ApiDoc>,
    doc_error: Option<String>,
    agent: ureq::Agent,
    auth: SharedAuth,
    config: Config,
    show_secret: bool,
    show_settings: bool,
    tabs: Vec<Tab>,
    active: usize,
    next_id: u64,
    side: SideTab,
    search: String,
    history: Vec<HistoryEntry>,
    workspace: Folder,
    dialog: Option<Dialog>,
    show_code: bool,
    /// The test runner window.
    tests: Option<crate::ui_tests::TestRun>,
    toasts: Vec<Toast>,
    file_tx: Sender<FileEvent>,
    file_rx: Receiver<FileEvent>,
    split: f32,
    last_save: Instant,
    install: Option<install::Status>,
    install_rx: Receiver<install::Status>,
    favorites: Favorites,
    subs: Substitutions,
    /// Enabled-or-not rules flattened, shared with workers and widgets.
    subs_rules: Arc<Vec<subst::Rule>>,
    /// Lookups already done this session.
    subs_cache: Arc<subst::Cache>,
    subst_edit: Option<SubstEditor>,
    /// Example responses recorded from real calls (endpoints the docs give none for).
    learned: crate::learned::Shared,
    /// “Explained by AI” of the endpoints (`explain.rs`).
    explanations: crate::explain::Shared,
    chat: ui_ai::Chat,
    mcp: McpState,
    mock: MockState,
    ai_setup: AiSetup,
    disk: WorkspaceDisk,
    /// Hash of the session last written.
    session_hash: u64,
    /// The keychain refused the secrets once already (said once).
    keychain_warned: bool,
    /// Environments (`{{baseUrl}}`, `{{token}}`…), the editor window and the one selected in it.
    envs: crate::env::Environments,
    show_envs: bool,
    /// API profiles (base URL, auth, definitions, data map per API), their editor and its selection.
    profiles: crate::profiles::Profiles,
    show_profiles: bool,
    profile_sel: usize,
    profile_show_secrets: bool,
    /// Understand running, and the Data map window.
    understand: Option<datamap_ui::UnderstandRun>,
    map_view: datamap_ui::MapView,
    env_sel: usize,
    /// Set after an uninstall that deleted the data: nothing may be written again.
    no_save: bool,
    nav: Nav,
}

/// The MCP server (Settings), the tool calls it hands over and its settings' state.
struct McpState {
    /// The server when it's on.
    server: Option<crate::mcp::Server>,
    tx: Sender<crate::mcp::Call>,
    rx: Receiver<crate::mcp::Call>,
    /// send_request calls from agents, waiting for the user's OK (first one shown).
    confirm: std::collections::VecDeque<crate::mcp::Call>,
    error: Option<String>,
    show_token: bool,
    /// The port being typed in the settings (applied with its button).
    port_edit: Option<u16>,
    /// The client picked in the settings' setup list.
    client: usize,
}

/// The mock server (Settings) and what it answers from.
struct MockState {
    server: Option<crate::mock::Server>,
    mocks: Arc<crate::mock::Mocks>,
    error: Option<String>,
    port_edit: Option<u16>,
}

/// Settings › AI assistant.
#[derive(Default)]
struct AiSetup {
    /// Models offered by the provider (Load models).
    models: Vec<String>,
    models_rx: Option<Receiver<Result<Vec<String>, String>>>,
    models_error: Option<String>,
    show_key: bool,
}

/// The workspace files as the app last saw them.
struct WorkspaceDisk {
    index: workspace_fs::Index,
    signature: u64,
    /// Signatures from the watcher thread (`workspace_fs::watch`).
    watch: mpsc::Receiver<(u64, Instant)>,
    /// When the app last wrote the workspace: older signatures from the watcher are stale.
    saved_at: Instant,
}

/// Visited tabs (ids) for the ← / → buttons.
#[derive(Default)]
struct Nav {
    back: Vec<u64>,
    forward: Vec<u64>,
    current: Option<u64>,
    /// The active tab changed through ← / →: don't record it as a new visit.
    moving: bool,
}

#[cfg(target_os = "macos")]
const FULLSCREEN_TIPS: (&str, &str) = ("Full screen (⌃⌘F)", "Exit full screen (⌃⌘F)");

#[cfg(not(target_os = "macos"))]
const FULLSCREEN_TIPS: (&str, &str) = ("Full screen (F11)", "Exit full screen (F11)");
impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::install(&cc.egui_ctx);
        let (doc, doc_error) = match ApiDoc::load() {
            Ok(d) => (d, None),
            Err(e) => (ApiDoc::empty(), Some(e)),
        };
        let (file_tx, file_rx) = mpsc::channel();
        let (mcp_tx, mcp_rx) = mpsc::channel();
        let (install_tx, install_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = install_tx.send(install::status());
        });
        let doc = Arc::new(doc);
        let config: Config = crate::secrets::load_config();
        let session: Session = storage::load("session.json");
        let mut toasts = Vec::new();
        let (workspace, ws_index, needs_save) = load_workspace(&mut toasts);
        let repaint = cc.egui_ctx.clone();
        let history = storage::load("history.json");
        let favorites = storage::load("favorites.json");
        let subs = Substitutions::load(&doc);
        let envs = crate::env::Environments::load();
        let profiles = crate::profiles::Profiles::load();
        crate::profiles::set_current(&profiles);
        crate::env::set_current(envs.active_vars());
        for p in storage::take_problems() {
            toasts.push(Toast { text: p, color: theme::RED, until: Instant::now() + Duration::from_secs(15) });
        }
        let mut app = Self {
            doc: doc.clone(),
            doc_error,
            agent: net::agent(),
            auth: Arc::default(),
            show_settings: false,
            config,
            show_secret: false,
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            side: SideTab::Endpoints,
            search: String::new(),
            history,
            workspace,
            dialog: None,
            show_code: false,
            tests: None,
            toasts,
            file_tx,
            file_rx,
            split: session.split,
            last_save: Instant::now(),
            install: None,
            install_rx,
            no_save: false,
            nav: Nav::default(),
            favorites,
            subs_rules: Arc::default(),
            subs,
            subs_cache: Arc::default(),
            subst_edit: None,
            learned: Arc::new(Mutex::new(crate::learned::Examples::load())),
            explanations: Arc::new(Mutex::new(crate::explain::Explanations::load())),
            chat: ui_ai::Chat::default(),
            mcp: McpState { server: None, tx: mcp_tx, rx: mcp_rx, confirm: Default::default(), error: None, show_token: false, port_edit: None, client: 0 },
            mock: MockState { server: None, mocks: Arc::new(crate::mock::Mocks::new(doc.clone())), error: None, port_edit: None },
            ai_setup: AiSetup::default(),
            session_hash: Default::default(),
            keychain_warned: false,
            envs,
            show_envs: false,
            profiles,
            show_profiles: false,
            profile_sel: 0,
            profile_show_secrets: false,
            understand: None,
            map_view: Default::default(),
            env_sel: 0,
            disk: WorkspaceDisk { index: ws_index, signature: 0, watch: workspace_fs::watch(move || repaint.request_repaint()), saved_at: Instant::now() },
        };
        if needs_save {
            app.save_workspace();
        }
        app.subs_rules = Arc::new(app.subs.rules());
        app.disk.signature = workspace_fs::signature();
        workspace_fs::write_docs();
        for mut t in session.tabs {
            // An unchanged tab shows its file as it is now, not the copy from when the app was closed.
            if t.unsaved == Some(false) {
                match t.saved.and_then(|uid| app.workspace.find(uid)) {
                    Some(Node::Request(r)) if t.spec.is_some() => {
                        t.title = r.name.clone();
                        t.spec = Some(r.spec.clone());
                    }
                    Some(Node::Flow(f)) if t.flow.is_some() => t.flow = Some(f.clone()),
                    _ => {}
                }
            }
            match (t.flow, t.spec) {
                _ if t.data_map => app.show_data_map(),
                (Some(flow), _) => app.push_flow_tab(flow, t.saved),
                (None, Some(spec)) => app.push_request_tab(t.title, spec, t.saved),
                _ => {}
            }
        }
        app.active = session.active.min(app.tabs.len().saturating_sub(1));
        app.init_data_map();
        app.apply_workspace_settings();
        app
    }

    /// Where the tab is now (None: closed since).
    fn pos(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id.0)
    }

    fn next_tab_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn push_request_tab(&mut self, title: String, spec: RequestSpec, saved: Option<u64>) {
        let id = self.next_tab_id();
        self.tabs.push(Tab { id, title, saved, body: TabBody::Request(ReqState::new(id, spec)) });
        self.active = self.tabs.len() - 1;
    }

    fn push_flow_tab(&mut self, flow: Flow, saved: Option<u64>) {
        let id = self.next_tab_id();
        let title = flow.name.clone();
        let mut st = FlowState::new(id, flow);
        // A new flow opens in edit mode; a saved one opens on its inputs.
        st.editing = saved.is_none() || st.flow.blocks.is_empty();
        // Kept results come back: Visualize and the outputs work without running again.
        if st.flow.keep_results {
            if let Some(run) = crate::runs::load(st.flow.uid) {
                let when = crate::runs::when(run.at);
                st.log.push(flow::LogLine { kind: flow::LogKind::Info, depth: 0, text: format!("Results of {when} (kept): run again to refresh them"), detail: None });
                ui_flow::set_results(&mut st, run.ok, Duration::from_millis(run.duration_ms), run.vars, run.at, "kept from the last run");
            }
        }
        self.tabs.push(Tab { id, title, saved, body: TabBody::Flow(st) });
        self.active = self.tabs.len() - 1;
    }

    fn save_session(&mut self) {
        let tabs = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let unsaved = Some(t.saved.is_none() || self.has_unsaved_changes(i));
                match &t.body {
                    TabBody::Request(r) => SavedTab { title: t.title.clone(), saved: t.saved, spec: Some(r.spec.clone()), flow: None, unsaved, data_map: false },
                    TabBody::Flow(f) => SavedTab { title: t.title.clone(), saved: t.saved, spec: None, flow: Some(f.flow.clone()), unsaved, data_map: false },
                    TabBody::DataMap => SavedTab { title: t.title.clone(), saved: None, spec: None, flow: None, unsaved: None, data_map: true },
                }
            })
            .collect();
        // Every 10 s: written only when something changed.
        storage::save_if_changed("session.json", &Session { tabs, active: self.active, split: self.split }, &mut self.session_hash);
    }

    fn save_workspace(&mut self) {
        if self.no_save {
            return;
        }
        if let Err(e) = workspace_fs::save(&self.workspace, &mut self.disk.index) {
            self.toast(e, theme::RED);
        }
        self.disk.signature = workspace_fs::signature();
        self.disk.saved_at = Instant::now();
    }

    /// Reads outside changes now, before something that may save the workspace.
    fn force_workspace_check(&mut self) {
        self.watch_workspace(true);
    }

    /// Picks up files changed outside the app (editor, AI agent, git…): what the watcher thread saw,
    /// or (`now`) a signature taken right away.
    fn watch_workspace(&mut self, now: bool) {
        if self.no_save {
            return;
        }
        let seen = self.disk.watch.try_iter().filter(|(_, at)| *at > self.disk.saved_at).last().map(|(sig, _)| sig);
        let sig = match (now, seen) {
            (true, _) => workspace_fs::signature(),
            (false, Some(sig)) => sig,
            (false, None) => return,
        };
        if sig == self.disk.signature {
            return;
        }
        self.disk.signature = sig;
        let Some(loaded) = workspace_fs::load() else { return };
        // Open tabs follow the files, unless they hold unsaved edits.
        let unchanged: Vec<bool> = (0..self.tabs.len()).map(|i| !self.has_unsaved_changes(i)).collect();
        self.workspace = loaded.root;
        self.disk.index = loaded.index;
        for (tab, unchanged) in self.tabs.iter_mut().zip(unchanged) {
            let Some(uid) = tab.saved else { continue };
            match (self.workspace.find(uid), &mut tab.body) {
                (Some(Node::Request(r)), TabBody::Request(st)) if unchanged => {
                    tab.title = r.name.clone();
                    st.spec = r.spec.clone();
                }
                (Some(Node::Flow(f)), TabBody::Flow(st)) if unchanged => {
                    tab.title = f.name.clone();
                    st.flow = f.clone();
                }
                (None, _) => tab.saved = None,
                _ => {}
            }
        }
        for p in loaded.problems.iter().take(3) {
            self.toast(format!("Skipped unreadable file — {p}"), theme::RED);
        }
        self.toast("Workspace reloaded (files changed on disk)", theme::CYAN);
        if loaded.needs_save {
            self.save_workspace();
        }
    }

    /// What the star of a tab refers to.
    fn save_config(&mut self) {
        if let Err(e) = crate::secrets::save_config(&self.config) {
            // Once: settings are saved on every change.
            if !self.keychain_warned {
                self.keychain_warned = true;
                self.toast(format!("The keychain refused the secrets ({e}): they are kept in config.json"), theme::ORANGE);
            }
        }
    }

    /// Records tab changes for ← / → (a new visit clears the forward history).
    fn track_visits(&mut self) {
        let current = self.tabs.get(self.active).map(|t| t.id);
        if current == self.nav.current {
            return;
        }
        if !self.nav.moving {
            if let Some(prev) = self.nav.current {
                if self.tabs.iter().any(|t| t.id == prev) {
                    self.nav.back.push(prev);
                }
            }
            self.nav.forward.clear();
        }
        self.nav.moving = false;
        self.nav.current = current;
        // The 200 latest visits (truncate would keep the oldest and drop every new one).
        let n = self.nav.back.len();
        if n > 200 {
            self.nav.back.drain(..n - 200);
        }
    }

    fn can_nav(&self, list: &[u64]) -> bool {
        list.iter().any(|id| self.tabs.iter().any(|t| t.id == *id))
    }

    fn save_favorites(&self) {
        storage::save("favorites.json", &self.favorites);
    }

    fn tab_favorite(&self, i: usize) -> Option<Favorite> {
        let t = self.tabs.get(i)?;
        if let Some(uid) = t.saved {
            return Some(Favorite::Item { uid });
        }
        match &t.body {
            TabBody::Request(r) => r.spec.doc.clone().map(Favorite::Endpoint),
            TabBody::Flow(_) | TabBody::DataMap => None,
        }
    }

    fn credentials(&self) -> Credentials {
        Credentials { auth: self.config.auth.clone() }
    }

    /// The workspace's base URL and auth, for everything that sends (also the CLI's).
    fn apply_workspace_settings(&self) {
        crate::model::set_default_base(&self.config.base_url);
        crate::auth::set_default(self.config.auth.clone());
    }

    fn toast(&mut self, text: impl Into<String>, color: Color32) {
        self.toasts.push(Toast { text: text.into(), color, until: Instant::now() + Duration::from_secs(4) });
    }

    /// Default folder to save into: "My requests" if it exists, otherwise the root.
    fn default_folder(&self) -> u64 {
        self.workspace
            .items
            .iter()
            .find_map(|n| match n {
                Node::Folder(f) if f.name == "My requests" => Some(f.uid),
                _ => None,
            })
            .unwrap_or(self.workspace.uid)
    }

    fn has_unsaved_changes(&self, i: usize) -> bool {
        let t = &self.tabs[i];
        match t.saved.and_then(|uid| self.workspace.find(uid)) {
            Some(node) => {
                // The request form adds an empty value per field it shows: that alone isn't a change
                // (else the tab stops following its file as soon as it's looked at).
                let normalized = |n: &Node| {
                    let mut v = serde_json::to_value(n).ok()?;
                    if let Some(spec) = v.get_mut("spec") {
                        for k in ["values", "path_values"] {
                            if let Some(serde_json::Value::Object(m)) = spec.get_mut(k) {
                                m.retain(|_, x| x.as_str() != Some(""));
                            }
                        }
                    }
                    Some(v)
                };
                t.to_node(node.uid()).is_some_and(|mine| normalized(node) != normalized(&mine))
            }
            None => matches!(&t.body, TabBody::Flow(f) if !f.flow.blocks.is_empty()),
        }
    }

    fn export_to_file(&self, node: &Node) {
        let text = workspace::export(node);
        let name = format!("{}.42api.json", sanitize(node.name()));
        let tx = self.file_tx.clone();
        std::thread::spawn(move || {
            let picked = rfd::FileDialog::new().add_filter("apifa", &["json"]).set_file_name(&name).save_file();
            if let Some(path) = picked {
                let _ = tx.send(match std::fs::write(&path, text) {
                    Ok(()) => FileEvent::Exported(path),
                    Err(e) => FileEvent::Failed(format!("Export failed: {e}")),
                });
            }
        });
    }

    /// Imports an API definition and makes the docs from all of them again. The API gets its own
    /// profile (base URL from its servers, the kind of auth it declares), active when none is.
    fn import_api(&mut self, text: &str, source: &str) {
        match crate::apis::import(text, source, None) {
            Ok((entry, parsed)) => {
                let id = crate::profiles::profile_for_import(&mut self.profiles, &entry, parsed.auth);
                if self.profiles.active.is_none() {
                    self.profiles.active = Some(id.clone());
                }
                self.profiles_changed();
                self.map_view.clear();
                self.reload_doc();
                let name = self.profiles.get(&id).map(|p| p.name.clone()).unwrap_or_default();
                self.toast(format!("API “{}” imported: {} endpoints · profile “{name}” (Edit API profiles… to set its auth)", entry.name, entry.endpoints), theme::GREEN);
                self.side = SideTab::Endpoints;
            }
            Err(e) => self.toast(e, theme::RED),
        }
    }

    /// The docs made again from the imported definitions.
    fn reload_doc(&mut self) {
        let doc = ApiDoc::load().unwrap_or_else(|_| ApiDoc::empty());
        let map = self.map_view_map();
        doc.set_data_map(map.as_ref());
        self.doc = Arc::new(doc);
    }

    fn import_node(&mut self, folder: u64, text: &str) -> Result<(), String> {
        let node = workspace::import(text)?;
        let label = format!("Imported {} “{}”", node.kind_label(), node.name());
        let target = self.workspace.folder_mut(folder).map(|f| f.uid).unwrap_or(self.workspace.uid);
        let uid = node.uid();
        let opens = !matches!(node, Node::Folder(_));
        if let Some(f) = self.workspace.folder_mut(target) {
            f.items.push(node);
        }
        self.save_workspace();
        self.side = SideTab::Workspace;
        self.toast(label, theme::GREEN);
        if opens {
            self.apply_open_node(uid);
        }
        Ok(())
    }

    fn apply_open_node(&mut self, uid: u64) {
        if let Some(i) = self.tabs.iter().position(|t| t.saved == Some(uid)) {
            self.active = i;
            return;
        }
        match self.workspace.find(uid).cloned() {
            Some(Node::Request(r)) => self.push_request_tab(r.name, r.spec, Some(uid)),
            Some(Node::Flow(f)) => self.push_flow_tab(f, Some(uid)),
            _ => {}
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = &root.ctx().clone();
        // A dialog (rename, delete, save as…) can save the workspace on this click: files edited outside
        // since the last check are read first, or the save writes the old content back over them.
        if self.dialog.is_some() && ctx.input(|i| i.pointer.any_pressed() || i.key_pressed(egui::Key::Enter)) {
            self.force_workspace_check();
        }
        self.poll();
        // What scripts set in the environment (`pm.environment.set`) is kept in it.
        let changes = crate::env::take_changes();
        if !changes.is_empty() && self.envs.apply_changes(&changes) {
            self.envs.save();
        }
        self.understand_poll();
        self.ai_poll(ctx);
        self.mcp_sync(ctx);
        self.mock_sync(ctx);
        self.mcp_poll(ctx);
        subst::set_rules(ctx, self.subs_rules.clone());
        crate::learned::set(ctx, self.learned.clone());
        crate::explain::set(ctx, self.explanations.clone(), self.config.ai.ready() && self.map_view_has_map());
        let mut actions = Vec::new();
        self.top_bar(root, ctx, &mut actions);
        self.sidebar(root, &mut actions);
        self.ai_panel(root, &mut actions);
        self.central(root, &mut actions);
        self.settings_window(ctx, &mut actions);
        self.code_window(ctx);
        self.env_window(ctx);
        self.profiles_window(ctx, &mut actions);
        if let Some(run) = &mut self.tests {
            if !crate::ui_tests::window(ctx, run, &mut actions) {
                self.tests = None;
            }
        }
        self.subst_window(ctx);
        self.mcp_confirm_window(ctx);
        self.dialogs(ctx, &mut actions);
        self.toasts_ui(ctx);
        self.drag_preview(ctx);
        self.shortcuts(ctx, &mut actions);
        self.resize_edges(ctx);
        // Same before the actions (Save, move, rename…).
        if !actions.is_empty() {
            self.force_workspace_check();
        }
        for a in actions {
            self.apply(a, ctx);
        }
        self.track_visits();
        if !self.no_save && self.last_save.elapsed() > Duration::from_secs(10) {
            self.save_session();
            self.last_save = Instant::now();
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if !self.no_save {
            self.save_session();
        }
    }
}

fn sanitize(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    if s.is_empty() {
        "export".into()
    } else {
        s
    }
}

/// The workspace from its folder; migrates the older single-file `workspace.json`.
fn load_workspace(toasts: &mut Vec<Toast>) -> (Folder, workspace_fs::Index, bool) {
    if let Some(loaded) = workspace_fs::load() {
        for p in loaded.problems.iter().take(3) {
            toasts.push(Toast { text: format!("Skipped unreadable file — {p}"), color: theme::RED, until: Instant::now() + Duration::from_secs(12) });
        }
        return (loaded.root, loaded.index, loaded.needs_save);
    }
    let root = match storage::load_checked::<Folder>("workspace.json") {
        Ok(Some(w)) => {
            let old = storage::dir().join("workspace.json");
            let _ = std::fs::rename(&old, storage::dir().join("workspace.json.migrated"));
            w
        }
        Ok(None) => workspace::default_workspace(),
        Err(e) => {
            toasts.push(Toast { text: e, color: theme::RED, until: Instant::now() + Duration::from_secs(15) });
            workspace::default_workspace()
        }
    };
    (root, workspace_fs::Index::default(), true)
}

impl Action {
    /// Copy of an open action (`Open` / `OpenNode`), used by menus.
    fn clone_open(&self) -> Action {
        match self {
            Action::Open(r) => Action::Open(r.clone()),
            Action::OpenNode(u) => Action::OpenNode(*u),
            _ => Action::NewTab,
        }
    }
}
