//! Understand (every saved GET, mapped in the background) and the Data map window: the graph of the
//! entities and their links found in GET responses, and the fields of the one selected.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::Duration;

use std::collections::BTreeMap;

use egui::{pos2, vec2, Align2, Color32, CornerRadius, FontId, Pos2, Rect, RichText, ScrollArea, Sense, Shape, Stroke, Ui};

use crate::datamap::DataMap;
use crate::theme;

use super::*;

pub(super) enum UnderstandEvent {
    Progress(usize, usize, String),
    /// The map so far: shown as it grows.
    Live(DataMap),
    Done(DataMap),
}

/// A run of Understand.
pub(super) struct UnderstandRun {
    rx: Receiver<UnderstandEvent>,
    stop: Arc<AtomicBool>,
    done: usize,
    total: usize,
    current: String,
}

/// The Data map tab: the map, where its entities are drawn, the one selected, the visible area.
#[derive(Default)]
pub(super) struct MapView {
    map: Option<DataMap>,
    selected: Option<String>,
    /// The tree's search.
    filter: String,
    loaded: bool,
    /// Layers instead of the tree, and the part of the Layers view shown (pan / zoom).
    layers: bool,
    scene: Option<egui::Rect>,
    /// An endpoint clicked in the details: opened in a tab after the frame.
    open: Option<crate::model::DocRef>,
}

impl MapView {
    /// Forgets the map shown (another profile's is loaded next).
    pub(super) fn clear(&mut self) {
        self.map = None;
        self.selected = None;
        self.loaded = false;
    }

    /// Shows `map` (a live one replaces the last; the selection, the search, what's open stay).
    fn set(&mut self, map: DataMap) {
        self.selected = self.selected.take().filter(|s| map.entities.contains_key(s));
        self.map = Some(map);
        self.loaded = true;
    }
}

impl App {
    /// Sends every saved GET request in the background and maps what comes back.
    pub(super) fn start_understand(&mut self, ctx: &egui::Context) {
        if self.understand.is_some() {
            return;
        }
        // The active profile's: its saved GETs, the GETs of its API definitions.
        let profile = self.profiles.active.clone().unwrap_or_default();
        let requests = crate::datamap::saved_gets(&self.workspace, &profile);
        let doc = match self.profiles.active() {
            Some(p) if !p.apis.is_empty() => Arc::new(crate::apis::load_doc_of(Some(&p.apis))),
            _ => self.doc.clone(),
        };
        let sender = crate::testrun::Sender {
            agent: self.agent.clone(),
            auth: self.auth.clone(),
            creds: self.credentials(),
            doc,
            rules: self.subs_rules.clone(),
            cache: self.subs_cache.clone(),
        };
        let learned = self.learned.clone();
        let delay = Duration::from_millis(self.config.flow_delay_ms);
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let (stop2, ctx) = (stop.clone(), ctx.clone());
        let total = requests.len();
        std::thread::spawn(move || {
            // The map so far goes to the window as it grows, and to disk now and then (closing the app
            // in the middle keeps what was found).
            let mut saved_at = std::time::Instant::now();
            let map = crate::datamap::understand(&sender, &requests, &learned, true, delay, &stop2, &profile, |i, n, what, live| {
                let _ = tx.send(UnderstandEvent::Progress(i, n, what.to_owned()));
                if let Some(map) = live {
                    if saved_at.elapsed() >= Duration::from_secs(20) {
                        crate::datamap::save(&map);
                        saved_at = std::time::Instant::now();
                    }
                    let _ = tx.send(UnderstandEvent::Live(map));
                }
                ctx.request_repaint();
            });
            crate::datamap::save(&map);
            let _ = tx.send(UnderstandEvent::Done(map));
            ctx.request_repaint();
        });
        self.understand = Some(UnderstandRun { rx, stop, done: 0, total, current: String::new() });
        self.show_data_map();
    }

    /// Understand running in the background: (done, total), for the title bar.
    pub(super) fn understand_status(&self) -> Option<(usize, usize)> {
        self.understand.as_ref().map(|r| (r.done.min(r.total), r.total))
    }

    pub(super) fn stop_understand(&self) {
        if let Some(r) = &self.understand {
            r.stop.store(true, Ordering::Relaxed);
        }
    }

    pub(super) fn understand_poll(&mut self) {
        let Some(run) = &mut self.understand else { return };
        let mut done = None;
        let mut live = None;
        while let Ok(ev) = run.rx.try_recv() {
            match ev {
                UnderstandEvent::Progress(i, n, what) => {
                    (run.done, run.total, run.current) = (i, n, what);
                }
                UnderstandEvent::Live(map) => live = Some(map),
                UnderstandEvent::Done(map) => done = Some(map),
            }
        }
        if let Some(map) = live {
            self.use_map(map);
        }
        if let Some(map) = done {
            let read = map.sources.iter().filter(|s| s.problem.is_none()).count();
            self.toast(format!("Data map: {} entities, {} links, from {read} GET responses", map.entities.len(), map.links.len()), theme::GREEN);
            self.understand = None;
            self.use_map(map);
        }
    }

    /// Shows `map` and makes the links of the request forms follow it (`category_id` → GET categories).
    fn use_map(&mut self, map: DataMap) {
        self.doc.set_data_map(Some(&map));
        self.map_view.set(map);
    }

    /// A GET sent from a tab, on an endpoint the data map hasn't read: added to it (and saved). Not
    /// while Understand runs (it builds its own and would replace this).
    pub(super) fn absorb_get(&mut self, path: &str, body: &str, profile: &str) {
        // Only into the map shown (the active profile's), not while Understand builds it.
        let active = self.profiles.active.clone().unwrap_or_default();
        let profile = if profile.is_empty() { active.as_str() } else { profile };
        if self.understand.is_some() || profile != active {
            return;
        }
        let route = self.doc.match_route("GET", path.split('?').next().unwrap_or(path));
        let template = route.as_ref().and_then(|d| Some(format!("GET {}", self.doc.lookup(d)?.1.routes.get(d.route)?.path)));
        let mut map = self.map_view.map.clone().or_else(crate::datamap::load).unwrap_or_default();
        map.profile = profile.to_owned();
        // Already read: nothing new (checked before parsing the body).
        if template.is_some_and(|t| map.sources.iter().any(|s| s.route == t && s.problem.is_none())) {
            return;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else { return };
        if crate::datamap::absorb(&mut map, &self.doc, "a request tab", path, &v) {
            crate::datamap::save(&map);
            let n = map.entities.len();
            self.use_map(map);
            self.toast(format!("Data map: a new endpoint was read ({n} entities now)"), theme::CYAN);
        }
    }

    pub(super) fn map_view_map(&self) -> Option<DataMap> {
        self.map_view.map.clone()
    }

    pub(super) fn map_view_has_map(&self) -> bool {
        self.map_view.map.is_some()
    }

    /// Writes the “Explained by AI” of an endpoint on a thread (from its docs, its GET's example answer,
    /// the data map; all anonymized), kept in `ai_explanations.json`.
    pub(super) fn explain(&mut self, d: crate::model::DocRef, ctx: &egui::Context) {
        let Some(map) = self.map_view.map.clone() else { return };
        let Some((get, example)) = crate::explain::source(&self.doc, &self.learned.lock(), &d) else {
            self.toast("No example answer to explain it from: send its GET once (or run Understand)", theme::ORANGE);
            return;
        };
        let Some(prompt) = crate::explain::prompt(&self.doc, &d, &get, &example, &map) else { return };
        let from = self.doc.lookup(&get).and_then(|(_, m)| m.routes.get(get.route)).map(|r| format!("GET {}", r.path)).unwrap_or_default();
        let key = crate::explain::key(&d);
        {
            let mut ex = self.explanations.lock();
            if !ex.busy.insert(key.clone()) {
                return;
            }
            ex.errors.remove(&key);
        }
        let (cfg, shared, ctx) = (self.config.ai.clone(), self.explanations.clone(), ctx.clone());
        std::thread::spawn(move || {
            let result = crate::explain::write(&cfg, &prompt);
            let mut ex = shared.lock();
            ex.busy.remove(&key);
            match result {
                Ok(text) => {
                    ex.map.insert(key, crate::explain::Explanation { text, model: cfg.model(), at: crate::storage::now(), from });
                    ex.save();
                }
                Err(e) => {
                    ex.errors.insert(key, e);
                }
            }
            ctx.request_repaint();
        });
    }

    /// At start: the saved map, for the links (the tab shows it when opened).
    pub(super) fn init_data_map(&mut self) {
        if let Some(m) = crate::datamap::load() {
            self.use_map(m);
        }
    }

    /// The Data map tab (at most one). The map is the one saved by the last Understand: opening it runs nothing.
    pub(super) fn show_data_map(&mut self) {
        if let Some(i) = self.tabs.iter().position(|t| matches!(t.body, TabBody::DataMap)) {
            self.active = i;
            return;
        }
        let id = self.next_tab_id();
        self.tabs.push(Tab { id, title: "Data map".into(), saved: None, body: TabBody::DataMap });
        self.active = self.tabs.len() - 1;
    }

    /// What the Data map tab shows.
    pub(super) fn datamap_tab(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        if !self.map_view.loaded {
            self.map_view.loaded = true;
            if let Some(m) = crate::datamap::load() {
                self.use_map(m);
            }
        }
        let (mut start, mut export, mut load) = (false, None, false);
        let profile = self.profiles.active().map(|p| p.name.clone());
        ui.vertical(|ui| {
            ui.horizontal_wrapped(|ui| {
                // Each API profile has its own map: the active one's is shown (title bar: API).
                let what = profile.as_deref().map_or("No API profile".to_owned(), |n| format!("API: {n}"));
                pill(ui, &what, theme::WHITE, theme::CYAN_DARK);
                match &self.understand {
                    Some(run) => {
                        ui.add(egui::Spinner::new().color(theme::CYAN));
                        pill(ui, "LIVE", theme::WHITE, theme::GREEN);
                        ui.label(RichText::new(format!("Understand: {} of {} · {}", run.done.min(run.total), run.total, run.current)).color(theme::SILVER))
                            .on_hover_text("Runs in the background: switch tab and go on, the map keeps growing (the title bar shows how far it is)");
                        if ui.small_button("Stop").on_hover_text("Ends it here: the map keeps what was found").clicked() {
                            run.stop.store(true, Ordering::Relaxed);
                        }
                    }
                    None => {
                        let n = crate::datamap::saved_gets(&self.workspace, self.profiles.active.as_deref().unwrap_or_default()).len();
                        if ui
                            .button(RichText::new(if self.map_view.map.is_some() { "⟳ Understand again" } else { "▶ Understand" }).family(theme::bold()))
                            .on_hover_text(format!(
                                "Sends your {n} saved GET request(s) (only GETs) and the examples recorded from real GETs, \
                                     and maps the structure of what comes back. No value is kept."
                            ))
                            .clicked()
                        {
                            start = true;
                        }
                    }
                }
                // Export / load: a map kept aside, shared, or brought from another computer (it holds no value).
                ui.add_enabled_ui(self.map_view.map.is_some(), |ui| {
                    ui.menu_button("Export", |ui| {
                        for f in crate::datamap::Export::ALL {
                            if ui.button(f.label()).clicked() {
                                export = Some(f);
                                ui.close();
                            }
                        }
                    })
                    .response
                    .on_hover_text("Saves the map to a file: JSON (can be loaded again), Markdown or an HTML page. It holds no value of your data.");
                });
                if ui
                    .add_enabled(self.understand.is_none(), egui::Button::new("Load…"))
                    .on_hover_text("Opens a data map exported as JSON: it replaces the current one (export that one first to keep it)")
                    .clicked()
                {
                    load = true;
                }
                if let Some(map) = &self.map_view.map {
                    let read = map.sources.iter().filter(|s| s.problem.is_none()).count();
                    ui.label(
                        RichText::new(format!(
                            "{} entities · {} links · from {read} GET responses · built {}",
                            map.entities.len(),
                            map.links.len(),
                            crate::runs::when(map.built_at)
                        ))
                        .size(12.0)
                        .color(theme::CONCRETE),
                    );
                }
            });
            ui.label(
                RichText::new(
                    "Each entity's fields as a tree; → opens the entity a field links to (cyan: its ids are that entity's, confirmed). Only GET responses, no value.",
                )
                .size(11.5)
                .color(theme::CONCRETE),
            );
            ui.horizontal(|ui| {
                dark_subtab(ui, &mut self.map_view.layers, false, "Tree");
                dark_subtab(ui, &mut self.map_view.layers, true, "Layers");
                let tip = if self.map_view.layers {
                    "Only the entities a GET returned. Layer 0: what nothing else points to; each one sits above what it uses \
                         (what everything depends on ends at the bottom). Each link starts at its field. Dashed: an *_id, or a link going up (a cycle). \
                         Click an entity: its links light up. Scroll to zoom, drag to move."
                } else {
                    "Each entity's fields; → opens the entity a field links to."
                };
                ui.label(RichText::new(tip).size(11.5).color(theme::CONCRETE));
            });
            ui.separator();
            if self.map_view.map.is_none() {
                ui.add_space(30.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("No map yet: Understand sends your saved GET requests and maps the structure of the answers.").color(theme::SILVER));
                });
                return;
            }
            let side_w = 330.0;
            ui.horizontal_top(|ui| {
                let tree_size = vec2((ui.available_width() - side_w - 12.0).max(300.0), ui.available_height().max(300.0));
                // Top-down inside: the row around it is horizontal.
                ui.allocate_ui_with_layout(tree_size, egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_min_size(tree_size);
                    ui.set_max_width(tree_size.x);
                    if self.map_view.layers {
                        self.map_layers(ui);
                    } else {
                        self.map_tree(ui);
                    }
                });
                ui.separator();
                ui.vertical(|ui| {
                    ui.set_width(side_w);
                    ui.set_max_width(side_w);
                    self.map_details(ui);
                });
            });
        });
        if start {
            self.start_understand(ctx);
        }
        if let Some(format) = export {
            self.export_data_map(format);
        }
        if load {
            self.load_data_map();
        }
        if let Some(d) = self.map_view.open.take() {
            self.apply(Action::Open(d), ctx);
        }
    }

    fn export_data_map(&mut self, format: crate::datamap::Export) {
        let Some(map) = &self.map_view.map else { return };
        let day = chrono::DateTime::from_timestamp(map.built_at as i64, 0).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default();
        let name = format!("data-map-{day}.{}", format.ext());
        let Some(path) = rfd::FileDialog::new().add_filter(format.label(), &[format.ext()]).set_file_name(&name).save_file() else { return };
        match std::fs::write(&path, crate::datamap::render(map, format)) {
            Ok(()) => self.toast(format!("Data map saved to {}", path.display()), theme::GREEN),
            Err(e) => self.toast(format!("Export failed: {e}"), theme::RED),
        }
    }

    fn load_data_map(&mut self) {
        let Some(path) = rfd::FileDialog::new().add_filter("Data map (JSON)", &["json"]).pick_file() else { return };
        match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| crate::datamap::parse(&t)) {
            Ok(map) => {
                crate::datamap::replace(&map);
                let n = map.entities.len();
                self.use_map(map);
                self.toast(format!("Data map loaded ({n} entities): it is the current one now"), theme::GREEN);
            }
            Err(e) => self.toast(format!("Can't load this file: {e}"), theme::RED),
        }
    }

    /// Each entity as a tree: its fields, a linked entity opening on its fields (`→ users`).
    fn map_tree(&mut self, ui: &mut Ui) {
        let view = &mut self.map_view;
        let Some(map) = &view.map else { return };
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut view.filter).hint_text("Find an entity or a field (e.g. email, user_id)…").desired_width(ui.available_width()));
        });
        let needle = view.filter.trim().to_lowercase();
        let mut select = None;
        ScrollArea::vertical().id_salt("map_tree").auto_shrink([false, false]).show(ui, |ui| {
            let (own, inside): (Vec<_>, Vec<_>) = map.entities.values().partition(|e| !e.endpoints.is_empty());
            for (title, list) in [("Returned by a GET", own), ("Only seen inside other entities", inside)] {
                let list: Vec<_> = list.into_iter().filter(|e| needle.is_empty() || mentions(map, e, &needle)).collect();
                if list.is_empty() {
                    continue;
                }
                ui.add_space(4.0);
                ui.label(RichText::new(format!("{title} ({})", list.len())).family(theme::bold()).size(12.5).color(theme::CONCRETE));
                for e in list {
                    let Some(shape) = crate::datamap::shape_of(map, &e.name) else { continue };
                    let is_sel = view.selected.as_deref() == Some(e.name.as_str());
                    let head = RichText::new(&e.name).family(theme::bold()).size(14.5).color(if is_sel { theme::YELLOW } else { theme::CYAN });
                    // While searching, open on what matches.
                    let open = (!needle.is_empty()).then_some(true);
                    let resp = egui::CollapsingHeader::new(head).id_salt(("map_entity", &e.name)).open(open).show(ui, |ui| {
                        fields_ui(ui, map, &e.name, shape, "", &mut vec![e.name.clone()], &needle);
                    });
                    if resp.header_response.clicked() {
                        select = Some(e.name.clone());
                    }
                }
            }
        });
        if let Some(s) = select {
            view.selected = Some(s);
        }
    }

    /// The selected entity: where it comes from, what links to it.
    fn map_details(&mut self, ui: &mut Ui) {
        let view = &mut self.map_view;
        let Some(map) = &view.map else { return };
        let Some(e) = view.selected.as_deref().and_then(|s| map.entities.get(s)) else {
            ui.label(RichText::new("Click an entity of the tree to see where it comes from and what links to it.").color(theme::SILVER));
            ui.add_space(8.0);
            let problems: Vec<_> = map.sources.iter().filter(|s| s.problem.is_some()).collect();
            if !problems.is_empty() {
                ui.label(RichText::new(format!("{} GET(s) gave nothing to read:", problems.len())).color(theme::ORANGE));
                ScrollArea::vertical().id_salt("map_problems").show(ui, |ui| {
                    for p in &problems {
                        ui.add(
                            egui::Label::new(RichText::new(format!("{} — {}", p.route, p.problem.as_deref().unwrap_or_default())).size(12.0).color(theme::SILVER)).wrap(),
                        );
                    }
                });
            }
            return;
        };
        let mut select = None;
        ScrollArea::vertical().id_salt("map_details").auto_shrink([false, false]).show(ui, |ui| {
            ui.label(RichText::new(&e.name).family(theme::bold()).size(17.0).color(theme::WHITE));
            if !e.documented {
                ui.label(RichText::new("Not a resource of the docs: named after the field it was found in").size(12.0).color(theme::ORANGE));
            }
            if e.endpoints.is_empty() {
                ui.label(RichText::new("No GET returns it on its own: it was seen inside others.").size(12.0).color(theme::SILVER));
            }
            for ep in &e.endpoints {
                // A GET it was read from: opens in a tab.
                match ep.strip_prefix("GET ").and_then(|p| self.doc.match_route("GET", p)) {
                    Some(d) => {
                        let resp = ui
                            .add(egui::Label::new(RichText::new(ep).monospace().size(12.0).color(theme::CYAN).underline()).sense(Sense::click()))
                            .on_hover_text("Open this GET in a tab");
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if resp.clicked() {
                            view.open = Some(d);
                        }
                    }
                    None => {
                        ui.label(RichText::new(ep).monospace().size(12.0).color(theme::CYAN));
                    }
                }
            }
            if !e.aliases.is_empty() {
                ui.label(RichText::new(format!("Also found as: {}", e.aliases.iter().cloned().collect::<Vec<_>>().join(", "))).size(12.0).color(theme::SILVER));
            }
            ui.add_space(8.0);
            ui.label(RichText::new("Linked from").family(theme::bold()).color(theme::CLOUDS));
            let from: Vec<_> = map.links.iter().filter(|l| l.to == e.name).collect();
            if from.is_empty() {
                ui.label(RichText::new("nothing").size(12.0).color(theme::CONCRETE));
            }
            for l in from {
                let confirmed = if l.ids == 0 { String::new() } else { format!("  ({} of {} ids confirmed)", l.confirmed, l.ids) };
                let text = format!("{} › {}  {}{confirmed}", l.from, l.field, l.kind.label());
                if ui
                    .add(egui::Label::new(RichText::new(text).size(12.0).color(theme::SILVER)).sense(Sense::click()))
                    .on_hover_text(format!("Go to {}", l.from))
                    .clicked()
                {
                    select = Some(l.from.clone());
                }
            }
            if !e.full.fields.is_empty() && !e.embedded.fields.is_empty() {
                let missing: Vec<&String> = e.full.fields.keys().filter(|k| !e.embedded.fields.contains_key(*k)).collect();
                ui.add_space(8.0);
                ui.label(RichText::new("Embedded in another entity").family(theme::bold()).color(theme::CLOUDS));
                ui.add(
                    egui::Label::new(
                        RichText::new(format!(
                            "{} of its {} fields{}",
                            e.embedded.fields.keys().filter(|k| e.full.fields.contains_key(*k)).count(),
                            e.full.fields.len(),
                            if missing.is_empty() { String::new() } else { format!("; not: {}", missing.iter().map(|m| m.as_str()).collect::<Vec<_>>().join(", ")) }
                        ))
                        .size(12.0)
                        .color(theme::SILVER),
                    )
                    .wrap(),
                );
            }
        });
        if let Some(s) = select {
            view.selected = Some(s);
        }
    }
}

/// The entity, or one of its fields (one level, and the fields of nested objects), matches the search.
/// A field of `s` (or of its nested objects) has `needle` in its name.
fn shape_has(s: &crate::datamap::Shape, needle: &str, depth: usize) -> bool {
    s.fields.iter().any(|(k, f)| {
        k.to_lowercase().contains(needle) || (depth < 3 && (shape_has(f, needle, depth + 1) || f.array.as_ref().is_some_and(|a| shape_has(&a.items, needle, depth + 1))))
    })
}

fn mentions(map: &DataMap, e: &crate::datamap::Entity, needle: &str) -> bool {
    e.name.to_lowercase().contains(needle)
        || e.aliases.iter().any(|a| a.contains(needle))
        || crate::datamap::shape_of(map, &e.name).is_some_and(|s| shape_has(s, needle, 0))
}

/// One level of the tree. A row: `name  type  in 80%  → target (link)`; what opens shows under it.
fn fields_ui(ui: &mut Ui, map: &DataMap, entity: &str, shape: &crate::datamap::Shape, prefix: &str, path: &mut Vec<String>, needle: &str) {
    use crate::datamap::Open;
    for it in crate::datamap::level(map, entity, shape, prefix) {
        let mut job = egui::text::LayoutJob::default();
        let found = !needle.is_empty() && it.name.to_lowercase().contains(needle);
        let fmt = |color: Color32, mono: bool| egui::TextFormat {
            font_id: if mono { FontId::monospace(12.5) } else { FontId::proportional(12.5) },
            color,
            background: if found && mono { theme::NAVY } else { Color32::TRANSPARENT },
            ..Default::default()
        };
        job.append(&it.name, 0.0, fmt(if found { theme::YELLOW } else { theme::WHITE }, true));
        job.append(&it.desc, 10.0, fmt(theme::SILVER, false));
        if it.presence < 100 {
            job.append(&format!("in {}%", it.presence), 8.0, fmt(theme::ORANGE, false));
        }
        if let Some(l) = it.link {
            job.append(&crate::datamap::link_text(l), 10.0, fmt(if l.confirmed > 0 { theme::CYAN } else { theme::CLOUDS }, false));
        }
        let id = (entity.to_owned(), prefix.to_owned(), it.name.clone(), path.len());
        match it.open {
            Open::Nothing => {
                ui.horizontal(|ui| {
                    // Lined up with the rows that open (their arrow).
                    ui.add_space(ui.spacing().indent);
                    ui.label(job);
                });
            }
            Open::Fields(s, p) => {
                // Searching: open the branches with a match, close the others.
                let open = (!needle.is_empty()).then(|| shape_has(s, needle, 0));
                egui::CollapsingHeader::new(job).id_salt(id).open(open).show(ui, |ui| fields_ui(ui, map, entity, s, &p, path, needle));
            }
            Open::Entity(to) => {
                egui::CollapsingHeader::new(job).id_salt(id).show(ui, |ui| {
                    if path.contains(&to) {
                        ui.label(RichText::new(format!("↻ {to}: already open above")).size(12.0).color(theme::CONCRETE));
                        return;
                    }
                    // As it is embedded here (fewer fields than on its own endpoint, often).
                    let embedded = map.entities.get(&to).map(|e| &e.embedded).filter(|s| s.objects > 0);
                    match embedded.or_else(|| crate::datamap::shape_of(map, &to)).filter(|s| !s.fields.is_empty()) {
                        Some(s) => {
                            path.push(to.clone());
                            fields_ui(ui, map, &to, s, "", path, "");
                            path.pop();
                        }
                        None => {
                            ui.label(RichText::new(format!("No field of {to} seen (no GET returned it)")).size(12.0).color(theme::CONCRETE));
                        }
                    }
                });
            }
        }
    }
}

/// Size of a node of the Layers view.
const NODE_W: f32 = 230.0;
const HEAD_H: f32 = 30.0;
const ROW_H: f32 = 19.0;
/// Link rows shown in a node (the next ones counted).
const MAX_ROWS: usize = 14;

/// Where each node of the Layers view is, and its link rows (field, target, in the view).
struct Placed {
    rect: Rect,
    rows: Vec<(String, String, bool, crate::datamap::LinkKind)>,
    more: usize,
    gets: usize,
}

/// Nodes per row: a wide layer goes on several rows.
const PER_ROW: usize = 6;

/// Where each node goes, and the bands' titles (`layer 0`…, then the entities linked to nothing) with their top.
fn place(map: &DataMap, layered: &crate::datamap::Layered) -> (BTreeMap<String, Placed>, Vec<(String, f32)>) {
    let shown: std::collections::HashSet<&str> = layered.layers.iter().flatten().map(String::as_str).collect();
    let linked: std::collections::HashSet<&str> = layered.edges.iter().flat_map(|e| [e.from.as_str(), e.to.as_str()]).collect();
    // The layers of what links to something, then one band for what links to nothing (no other GET's entity).
    let mut bands: Vec<(String, Vec<&String>)> = layered
        .layers
        .iter()
        .enumerate()
        .map(|(i, l)| (format!("layer {i}"), l.iter().filter(|n| linked.contains(n.as_str())).collect::<Vec<_>>()))
        .filter(|(_, l)| !l.is_empty())
        .collect();
    let alone: Vec<&String> = layered.layers.iter().flatten().filter(|n| !linked.contains(n.as_str())).collect();
    if !alone.is_empty() {
        bands.push(("no link with another GET's entity".to_owned(), alone));
    }
    let row_w = PER_ROW as f32 * (NODE_W + 50.0);
    let mut out = BTreeMap::new();
    let mut titles = Vec::new();
    let mut y = 0.0;
    for (title, nodes) in bands {
        titles.push((title, y));
        y += 34.0;
        for row in nodes.chunks(PER_ROW) {
            let mut tallest: f32 = 0.0;
            let x0 = (row_w - row.len() as f32 * (NODE_W + 50.0)) / 2.0;
            for (i, name) in row.iter().enumerate() {
                let mut rows: Vec<_> = map
                    .links
                    .iter()
                    .filter(|l| &l.from == *name && &l.to != *name)
                    .map(|l| (l.field.clone(), l.to.clone(), shown.contains(l.to.as_str()), l.kind.clone()))
                    .collect();
                // What leads somewhere in the view first.
                rows.sort_by(|a, b| (!a.2, &a.0).cmp(&(!b.2, &b.0)));
                let more = rows.len().saturating_sub(MAX_ROWS);
                rows.truncate(MAX_ROWS);
                let h = HEAD_H + rows.len() as f32 * ROW_H + if more > 0 { ROW_H } else { 0.0 } + 6.0;
                tallest = tallest.max(h);
                let gets = map.entities.get(*name).map_or(0, |e| e.endpoints.len());
                out.insert((*name).clone(), Placed { rect: Rect::from_min_size(pos2(x0 + i as f32 * (NODE_W + 50.0), y), vec2(NODE_W, h)), rows, more, gets });
            }
            y += tallest + 60.0;
        }
        y += 40.0;
    }
    (out, titles)
}

/// Points of a cubic Bézier curve (to draw it solid or dashed).
fn bezier(p: [Pos2; 4], n: usize) -> Vec<Pos2> {
    (0..=n)
        .map(|i| {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
            pos2(w.iter().zip(&p).map(|(w, p)| w * p.x).sum(), w.iter().zip(&p).map(|(w, p)| w * p.y).sum())
        })
        .collect()
}

impl App {
    /// Layers: the entities a GET returned, what nothing points to on top, each one above what it uses.
    fn map_layers(&mut self, ui: &mut Ui) {
        let view = &mut self.map_view;
        let Some(map) = &view.map else { return };
        layers_view(ui, map, &mut view.selected, &mut view.scene);
    }
}

/// The Layers view of `map` (`selected`: the entity lit up; `scene`: the part shown, pan / zoom).
pub(crate) fn layers_view(ui: &mut Ui, map: &DataMap, selected: &mut Option<String>, scene: &mut Option<Rect>) {
    let layered = crate::datamap::layers(map);
    if layered.layers.is_empty() {
        ui.label(RichText::new("No entity returned by a GET yet.").color(theme::SILVER));
        return;
    }
    let (placed, titles) = place(map, &layered);
    let bounds = placed.values().fold(Rect::NOTHING, |r, p| r.union(p.rect)).expand(40.0);
    // First shown: the whole width, from the top (the layers read downward).
    let start = Rect::from_min_size(bounds.min, vec2(bounds.width(), bounds.width() * 0.62));
    let mut scene_rect = scene.unwrap_or(start);
    let current = selected.clone();
    // The selected entity and what it links to or is linked from.
    let related: std::collections::HashSet<&str> = layered
        .edges
        .iter()
        .filter(|e| current.as_deref().is_some_and(|s| s == e.from || s == e.to))
        .flat_map(|e| [e.from.as_str(), e.to.as_str()])
        .chain(current.as_deref())
        .collect();
    let mut clicked = None;
    egui::Frame::new().fill(theme::CODE_BG).corner_radius(6).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        egui::Scene::new().zoom_range(0.1..=2.5).show(ui, &mut scene_rect, |ui| {
            let painter = ui.painter().clone();
            // The bands: a line and its title.
            for (title, top) in &titles {
                painter.hline(bounds.x_range(), *top, Stroke::new(1.0, Color32::from_white_alpha(25)));
                painter.text(pos2(bounds.left() + 4.0, top + 12.0), Align2::LEFT_CENTER, title, FontId::proportional(13.0), theme::CONCRETE);
            }
            let lit = |a: &str, b: &str| current.as_deref().is_some_and(|s| s == a || s == b);
            // Links: from the row of the field to the top of the entity it leads to.
            for e in &layered.edges {
                let (Some(from), Some(to)) = (placed.get(&e.from), placed.get(&e.to)) else { continue };
                let Some(row) = from.rows.iter().position(|r| r.0 == e.field && r.1 == e.to) else { continue };
                let start = pos2(from.rect.right(), from.rect.top() + HEAD_H + (row as f32 + 0.5) * ROW_H);
                let end = if e.back { to.rect.center_bottom() } else { to.rect.center_top() };
                let bend = ((end.y - start.y).abs() * 0.5).max(40.0);
                let c2 = if e.back { end + vec2(0.0, bend) } else { end - vec2(0.0, bend) };
                let points = bezier([start, start + vec2(bend.min(120.0), 0.0), c2, end], 24);
                let hot = lit(&e.from, &e.to);
                let color = match (hot, e.confirmed, e.back) {
                    (true, ..) => theme::YELLOW,
                    // Something selected: the rest fades.
                    _ if current.is_some() => Color32::from_white_alpha(18),
                    (false, _, true) => Color32::from_white_alpha(50),
                    (false, true, false) => theme::CYAN,
                    (false, false, false) => Color32::from_white_alpha(110),
                };
                let stroke = Stroke::new(if hot { 2.4 } else { 1.3 }, color);
                if e.kind == crate::datamap::LinkKind::Reference || e.back {
                    painter.extend(Shape::dashed_line(&points, stroke, 7.0, 4.0));
                } else {
                    painter.add(Shape::line(points.clone(), stroke));
                }
                let n = points.len();
                let dir = (points[n - 1] - points[n - 2]).normalized();
                let side = vec2(-dir.y, dir.x);
                painter.add(Shape::convex_polygon(vec![end, end - dir * 9.0 + side * 4.5, end - dir * 9.0 - side * 4.5], color, Stroke::NONE));
            }
            // Nodes: the entity, its link fields like the first level of its tree.
            for (name, p) in &placed {
                let is_sel = current.as_deref() == Some(name.as_str());
                let resp = ui.interact(p.rect, ui.scope_id().with(("layer_node", name)), Sense::click());
                painter.rect(
                    p.rect,
                    CornerRadius::same(7),
                    theme::NAVY_DEEP,
                    Stroke::new(if is_sel { 2.4 } else { 1.0 }, if is_sel { theme::YELLOW } else { theme::CYAN }),
                    egui::StrokeKind::Inside,
                );
                let head = Rect::from_min_size(p.rect.min, vec2(NODE_W, HEAD_H));
                painter.rect_filled(head, CornerRadius { nw: 7, ne: 7, sw: 0, se: 0 }, if is_sel { theme::YELLOW } else { theme::CYAN_DARK });
                painter.text(
                    head.left_center() + vec2(10.0, 0.0),
                    Align2::LEFT_CENTER,
                    name,
                    FontId::new(14.0, theme::bold()),
                    if is_sel { theme::NAVY_DEEP } else { theme::WHITE },
                );
                painter.text(
                    head.right_center() - vec2(10.0, 0.0),
                    Align2::RIGHT_CENTER,
                    format!("{} GET", p.gets),
                    FontId::proportional(11.0),
                    if is_sel { theme::NAVY_DEEP } else { theme::CLOUDS },
                );
                for (i, (field, to, shown, kind)) in p.rows.iter().enumerate() {
                    let y = p.rect.top() + HEAD_H + (i as f32 + 0.5) * ROW_H;
                    let branch = if i + 1 == p.rows.len() && p.more == 0 { "└ " } else { "├ " };
                    let arrow = if *kind == crate::datamap::LinkKind::Reference { "⇢" } else { "→" };
                    let color = if *shown { theme::CLOUDS } else { theme::CONCRETE };
                    // The target on the right; the field cut to what's left of the row (… at its end).
                    let target_color = if *shown { theme::CYAN } else { theme::CONCRETE };
                    let target = painter.text(pos2(p.rect.right() - 8.0, y), Align2::RIGHT_CENTER, format!("{arrow} {to}"), FontId::proportional(11.0), target_color);
                    let fits = ((target.left() - p.rect.left() - 20.0).max(30.0) / 7.0) as usize;
                    let full = format!("{branch}{field}");
                    let text = if full.chars().count() > fits { format!("{}…", full.chars().take(fits.saturating_sub(1)).collect::<String>()) } else { full };
                    painter.text(pos2(p.rect.left() + 8.0, y), Align2::LEFT_CENTER, text, FontId::monospace(11.5), color);
                }
                if p.more > 0 {
                    let y = p.rect.top() + HEAD_H + (p.rows.len() as f32 + 0.5) * ROW_H;
                    painter.text(pos2(p.rect.left() + 8.0, y), Align2::LEFT_CENTER, format!("└ … {} more links", p.more), FontId::proportional(11.0), theme::CONCRETE);
                }
                // Not linked to the selected one: faded.
                if current.is_some() && !related.contains(name.as_str()) {
                    painter.rect_filled(p.rect, CornerRadius::same(7), theme::CODE_BG.gamma_multiply(0.75));
                }
                let tip = format!("{name}\n{}", map.entities.get(name).map(|e| e.endpoints.iter().cloned().collect::<Vec<_>>().join("\n")).unwrap_or_default());
                if resp.on_hover_text(tip).clicked() {
                    clicked = Some(name.clone());
                }
            }
        });
    });
    *scene = Some(scene_rect);
    if let Some(c) = clicked {
        *selected = if selected.as_deref() == Some(c.as_str()) { None } else { Some(c) };
    }
}
