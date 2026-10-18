//! The sidebar: endpoints, favorites, workspace tree, history, substitutions.

use std::sync::mpsc;
use std::sync::Arc;

use egui::{vec2, Align, Align2, Button, CollapsingHeader, Color32, CornerRadius, FontId, Frame, Layout, Margin, RichText, ScrollArea, Sense, TextEdit, TextStyle, Ui};

use crate::docs::ApiDoc;
use crate::favorites::{self, Favorites};
use crate::model::{self, DocRef};
use crate::net;
use crate::storage;
use crate::subst;
use crate::theme;
use crate::ui_flow::BlockDrop;
use crate::widgets::*;
use crate::workspace::{Folder, Node};

use super::*;

impl App {
    pub(super) fn sidebar(&mut self, root: &mut Ui, actions: &mut Vec<Action>) {
        egui::Panel::left("side")
            .resizable(true)
            .default_size(360.0)
            .size_range(260.0..=640.0)
            .frame(Frame::new().fill(theme::NAVY_DEEP).inner_margin(Margin::same(10)))
            .show(root, |ui| {
                ui.horizontal_wrapped(|ui| {
                    segment(ui, &mut self.side, SideTab::Endpoints, "Endpoints");
                    segment(ui, &mut self.side, SideTab::Favorites, "★ Favorites");
                    segment(ui, &mut self.side, SideTab::Workspace, "Workspace");
                    segment(ui, &mut self.side, SideTab::History, "History");
                    segment(ui, &mut self.side, SideTab::Substitutions, "Substitutions");
                });
                ui.add_space(6.0);
                match self.side {
                    SideTab::Endpoints => self.endpoints(ui, actions),
                    SideTab::Favorites => self.favorites_panel(ui, actions),
                    SideTab::Workspace => self.workspace_panel(ui, actions),
                    SideTab::History => self.history_list(ui, actions),
                    SideTab::Substitutions => self.subst_panel(ui, actions),
                }
            });
    }

    pub(super) fn subst_panel(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let mut fold: Option<bool> = None;
        ui.horizontal_wrapped(|ui| {
            if ui.button("+ Substitution").clicked() {
                actions.push(Action::NewSubst(None));
            }
            if ui.button("+ Folder").clicked() {
                actions.push(Action::NewSubstFolder(None));
            }
            ui.menu_button("Import", |ui| {
                if ui.button("From file…").clicked() {
                    actions.push(Action::ImportSubstFile(None));
                    ui.close();
                }
                if ui.button("Paste JSON…").clicked() {
                    actions.push(Action::ImportSubstPaste(None));
                    ui.close();
                }
            });
            fold = fold_row(ui);
        });
        ui.label(
            RichText::new("Type a login where a user_id is expected (or a name, a slug…): the id is looked up just before sending. Right-click to edit, move, share.")
                .size(11.5)
                .color(theme::CONCRETE),
        );
        ui.add_space(4.0);
        let paths = self.subs.folder_paths();
        ScrollArea::vertical().id_salt("substitutions").auto_shrink([false, false]).show(ui, |ui| {
            if self.subs.items.is_empty() {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| ui.label(RichText::new("No substitution yet.").color(theme::CONCRETE)));
            }
            subst_tree(ui, &self.subs.items, &paths, fold, actions);
            let rest = ui.allocate_response(vec2(ui.available_width(), ui.available_height().max(40.0)), Sense::click());
            rest.context_menu(|ui| {
                if ui.button("New substitution").clicked() {
                    actions.push(Action::NewSubst(None));
                    ui.close();
                }
                if ui.button("New folder").clicked() {
                    actions.push(Action::NewSubstFolder(None));
                    ui.close();
                }
            });
        });
    }

    pub(super) fn save_subs(&mut self) {
        storage::save("substitutions.json", &self.subs);
        self.subs_rules = Arc::new(self.subs.rules());
        self.subs_cache.clear();
    }

    pub(super) fn import_subs(&mut self, folder: Option<u64>, text: &str) -> Result<usize, String> {
        let entries = subst::import(text)?;
        let n = entries.len();
        for e in entries {
            self.subs.insert(e, folder);
        }
        self.save_subs();
        self.side = SideTab::Substitutions;
        Ok(n)
    }

    /// The editor window of a substitution (with a live test).
    pub(super) fn subst_window(&mut self, ctx: &egui::Context) {
        let creds = self.credentials();
        let Some(ed) = &mut self.subst_edit else { return };
        if let Some(rx) = &ed.test_rx {
            if let Ok(r) = rx.try_recv() {
                ed.test = Some(r);
                ed.test_rx = None;
            }
        }
        let mut open = true;
        let (mut save, mut cancel, mut test) = (false, false, false);
        egui::Window::new(if ed.is_new { "New substitution" } else { "Substitution" }).open(&mut open).collapsible(false).resizable(true).default_width(620.0).show(
            ctx,
            |ui| {
                let field = |ui: &mut Ui, label: &str, help: &str| {
                    ui.add_space(4.0);
                    ui.label(RichText::new(label).family(theme::bold()));
                    if !help.is_empty() {
                        ui.label(RichText::new(help).size(11.5).color(theme::SILVER));
                    }
                };
                field(ui, "Name", "");
                ui.add(TextEdit::singleline(&mut ed.rule.name).desired_width(f32::INFINITY));
                field(ui, "Description", "");
                ui.add(TextEdit::singleline(&mut ed.rule.description).desired_width(f32::INFINITY).hint_text("Type a login where a user_id is expected"));
                field(ui, "Fields it watches", "Comma separated. user_id also matches :user_id, filter[user_id], user[user_id]…");
                ui.add(TextEdit::singleline(&mut ed.fields).font(TextStyle::Monospace).desired_width(f32::INFINITY).hint_text("user_id, users_id"));
                ui.horizontal(|ui| {
                    ui.label("Replace");
                    egui::ComboBox::from_id_salt("subst_when").selected_text(ed.rule.when.label()).show_ui(ui, |ui| {
                        ui.selectable_value(&mut ed.rule.when, subst::When::NotNumber, subst::When::NotNumber.label());
                        ui.selectable_value(&mut ed.rule.when, subst::When::Always, subst::When::Always.label());
                    });
                    ui.checkbox(&mut ed.rule.enabled, "Enabled");
                });
                field(ui, "Look the value up with", "A GET. {{value}} is what was typed in the field.");
                ui.horizontal(|ui| {
                    ui.label(RichText::new("GET").family(theme::bold()).color(theme::GREEN));
                    ui.add(TextEdit::singleline(&mut ed.rule.path).font(TextStyle::Monospace).desired_width(f32::INFINITY).hint_text("/v1/users"));
                });
                kv_editor(ui, &mut ed.rule.query, "subst_query");
                field(ui, "Take", "Where the value is in the response: body[0].id, body.id…");
                ui.add(TextEdit::singleline(&mut ed.rule.extract).font(TextStyle::Monospace).desired_width(f32::INFINITY).hint_text("body[0].id"));
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Test with").family(theme::bold()));
                    ui.add(TextEdit::singleline(&mut ed.test_value).font(TextStyle::Monospace).desired_width(180.0).hint_text("jdoe"));
                    if ed.test_rx.is_some() {
                        ui.add(egui::Spinner::new().color(theme::CYAN));
                    } else if ui.add_enabled(!ed.test_value.trim().is_empty(), Button::new("Test")).clicked() {
                        test = true;
                    }
                    match &ed.test {
                        Some(Ok(v)) => {
                            ui.label(RichText::new(format!("→ {v}")).monospace().color(theme::GREEN));
                        }
                        Some(Err(e)) => {
                            ui.label(RichText::new(e).color(theme::RED));
                        }
                        None => {}
                    }
                });
                ui.label(
                    RichText::new(format!("Request: {}", ed.rule.lookup(if ed.test_value.trim().is_empty() { "{{value}}" } else { ed.test_value.trim() }).display_url))
                        .monospace()
                        .size(11.5)
                        .color(theme::SILVER),
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.add(Button::new(RichText::new("Save").family(theme::bold()).color(theme::WHITE)).fill(theme::CYAN)).clicked() {
                        save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            },
        );
        ed.rule.fields = ed.fields.split(',').map(|f| f.trim().to_owned()).filter(|f| !f.is_empty()).collect();
        if test {
            let (tx, rx) = mpsc::channel();
            ed.test_rx = Some(rx);
            ed.test = None;
            let (rule, value) = (ed.rule.clone(), ed.test_value.trim().to_owned());
            let (agent, auth, creds, ctx) = (self.agent.clone(), self.auth.clone(), creds, ctx.clone());
            std::thread::spawn(move || {
                let r = net::execute(&agent, &auth, &creds, &rule.lookup(&value)).and_then(|resp| rule.extract_from(&value, &resp));
                let _ = tx.send(r);
                ctx.request_repaint();
            });
        }
        if save {
            let (rule, folder) = (ed.rule.clone(), ed.folder);
            self.subs.upsert(rule, folder);
            self.save_subs();
            self.subst_edit = None;
        } else if cancel || !open {
            self.subst_edit = None;
        }
    }

    /// The imported API definitions: on / off, remove, import another (file or URL).
    fn apis_section(&mut self, ui: &mut Ui, empty: bool, actions: &mut Vec<Action>) {
        let apis = crate::apis::list();
        let head = RichText::new(format!("APIs ({})", apis.len())).family(theme::bold()).size(12.5).color(theme::CLOUDS);
        egui::CollapsingHeader::new(head).id_salt("apis_section").default_open(empty || apis.is_empty()).show(ui, |ui| {
            if apis.is_empty() {
                ui.label(
                    RichText::new("Import an API definition — OpenAPI 3, Swagger 2 (JSON / YAML) or apipie — to get its endpoints, their forms and docs here.")
                        .size(12.0)
                        .color(theme::SILVER),
                );
            }
            for a in &apis {
                ui.horizontal(|ui| {
                    let mut on = a.enabled;
                    if ui.checkbox(&mut on, "").on_hover_text("Shown in the endpoints").changed() {
                        actions.push(Action::ToggleApi(a.name.clone(), on));
                    }
                    let tip = format!("{} · {} endpoints · {}\n{}", a.format, a.endpoints, a.version, a.base_urls.join("\n"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Remove").clicked() {
                            actions.push(Action::RemoveApi(a.name.clone()));
                        }
                        if a.source.starts_with("http") && ui.small_button("Reload").on_hover_text(&a.source).clicked() {
                            actions.push(Action::ImportApiUrl(a.source.clone()));
                        }
                        // What's left: the name, cut when too long (the row never widens the sidebar).
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.add(egui::Label::new(RichText::new(&a.name).color(theme::CLOUDS)).truncate()).on_hover_text(tip);
                        });
                    });
                });
            }
            if ui.small_button("Import file…").clicked() {
                actions.push(Action::ImportApiFile);
            }
            // Drawn from the right: the field takes what's left, so the row never widens the sidebar.
            let url_id = egui::Id::unique("api_url");
            let mut url: String = ui.data(|d| d.get_temp(url_id)).unwrap_or_default();
            let mut go = false;
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    go |= ui.add_enabled(url.trim().starts_with("http"), Button::new("Fetch")).clicked();
                    let edit = ui.add(TextEdit::singleline(&mut url).hint_text("or a URL: https://…/openapi.json").desired_width(ui.available_width()));
                    go |= edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                });
            });
            if go && url.trim().starts_with("http") {
                actions.push(Action::ImportApiUrl(url.trim().to_owned()));
                url.clear();
            }
            ui.data_mut(|d| d.insert_temp(url_id, url));
        });
    }

    pub(super) fn endpoints(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        // Set by the −/+ buttons for this frame: collapse / expand every folder.
        let mut fold: Option<bool> = None;
        let doc = self.doc.clone();
        let searched =
            ui.add(TextEdit::singleline(&mut self.search).hint_text(format!("Search {} endpoints…", doc.endpoint_count)).desired_width(f32::INFINITY)).changed();
        self.apis_section(ui, doc.endpoint_count == 0, actions);
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} resources · {} endpoints", doc.resources.len(), doc.endpoint_count)).size(11.5).color(theme::CONCRETE));
        });
        ui.horizontal_wrapped(|ui| {
            if ui.add(Button::new(RichText::new("+ New request").size(12.0)).corner_radius(3)).clicked() {
                actions.push(Action::NewTab);
            }
            if ui
                .add(Button::new(RichText::new("Generate docs…").size(12.0)).corner_radius(3))
                .on_hover_text("Documentation of the imported APIs (HTML, Markdown or OpenAPI). Right-click a resource for only that one.")
                .clicked()
            {
                actions.push(Action::GenerateApiDocs(None));
            }
            fold = fold_row(ui);
        });
        if let Some(e) = &self.doc_error {
            ui.colored_label(theme::RED, e);
        }
        ui.add_space(4.0);
        let needle = self.search.trim().to_lowercase();
        let terms: Vec<&str> = needle.split_whitespace().collect();
        let active_ref = match self.tabs.get(self.active).map(|t| &t.body) {
            Some(TabBody::Request(r)) => r.spec.doc.clone(),
            _ => None,
        };
        let fav_paths = self.favorites.folder_paths();
        let mut list = ScrollArea::vertical().id_salt("endpoints").auto_shrink([false, false]);
        if searched {
            // New results: start from the top of the list.
            list = list.vertical_scroll_offset(0.0);
        }
        list.show(ui, |ui| {
            let mut shown = 0;
            // Best match first: closest category, then its shortest routes — grouped by API when there
            // are several (each its own profile).
            let hits = doc.search(&needle);
            let mut apis: Vec<&str> = Vec::new();
            for h in &hits {
                let a = doc.resources[h.resource].api.as_str();
                if !apis.contains(&a) {
                    apis.push(a);
                }
            }
            let grouped = apis.len() > 1;
            for api in apis {
                if grouped {
                    ui.add_space(6.0);
                    let profile = self.profiles.of_api(api).map(|p| format!("  · profile {}", p.name)).unwrap_or_default();
                    ui.label(RichText::new(format!("{}{profile}", if api.is_empty() { "Other" } else { api })).family(theme::bold()).size(12.5).color(theme::CYAN));
                }
                for hit in hits.iter().filter(|h| doc.resources[h.resource].api == api).cloned() {
                    let res = &doc.resources[hit.resource];
                    let rows = hit.routes;
                    shown += rows.len();
                    let title = RichText::new(format!("{}  ", res.name)).family(theme::bold()).size(14.0).color(theme::WHITE);
                    let mut header = CollapsingHeader::new(title).id_salt(("res", &res.key));
                    if let Some(open) = fold {
                        header = header.open(Some(open));
                    } else if !terms.is_empty() {
                        header = header.open(Some(true));
                    }
                    let resp = header.show(ui, |ui| {
                        for (mi, ri) in rows {
                            let m = &res.methods[mi];
                            let r = &m.routes[ri];
                            let this = DocRef { resource: res.key.clone(), method: m.name.clone(), route: ri };
                            let selected = active_ref.as_ref() == Some(&this);
                            let row = endpoint_row(ui, &r.verb, &r.path, selected, r.deprecated);
                            let row = row.on_hover_ui(|ui| {
                                ui.label(RichText::new(format!("{} {}", r.verb, r.path)).monospace());
                                ui.label(RichText::new(format!("{} › {}", res.name, m.name)).strong());
                                if !m.description.is_empty() {
                                    ui.label(truncate(&m.description, 300));
                                }
                            });
                            let fav = Favorite::Endpoint(this.clone());
                            let starred = self.favorites.contains(&fav);
                            if starred {
                                paint_star(ui, row.rect);
                            }
                            row.context_menu(|ui| {
                                if ui.button("Open").clicked() {
                                    actions.push(Action::Open(this.clone()));
                                    ui.close();
                                }
                                fav_menu(ui, &fav, starred, &fav_paths, actions);
                            });
                            row.dnd_set_drag_payload(BlockDrop::Endpoint(this.clone()));
                            if row.clicked() {
                                actions.push(Action::Open(this));
                            }
                        }
                    });
                    resp.header_response.on_hover_text(&res.description).context_menu(|ui| {
                        if ui.button("Generate documentation…").clicked() {
                            actions.push(Action::GenerateApiDocs(Some(res.key.clone())));
                            ui.close();
                        }
                    });
                }
            }
            if shown == 0 {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| ui.label(RichText::new("No endpoint matches.").color(theme::CONCRETE)));
            }
        });
    }

    pub(super) fn favorites_panel(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        // Set by the −/+ buttons for this frame: collapse / expand every folder.
        let mut fold: Option<bool> = None;
        ui.horizontal_wrapped(|ui| {
            if ui.button("+ Folder").on_hover_text("New favorites folder").clicked() {
                actions.push(Action::NewFavFolder(None));
            }
            ui.label(RichText::new("Right-click to move, rename or remove").size(11.5).color(theme::CONCRETE));
            fold = fold_row(ui);
        });
        ui.label(RichText::new("Star endpoints, requests and flows with a right-click, or with ☆ at the top of a tab.").size(11.5).color(theme::CONCRETE));
        ui.add_space(4.0);
        let ctx = FavCtx {
            doc: &self.doc,
            workspace: &self.workspace,
            paths: self.favorites.folder_paths(),
            open_doc: match self.tabs.get(self.active).map(|t| &t.body) {
                Some(TabBody::Request(st)) => st.spec.doc.clone(),
                _ => None,
            },
            open_uid: self.tabs.get(self.active).and_then(|t| t.saved),
        };
        ScrollArea::vertical().id_salt("favorites").auto_shrink([false, false]).show(ui, |ui| {
            if fav_tree(ui, &self.favorites.items, &ctx, fold, actions) == 0 && self.favorites.items.is_empty() {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| ui.label(RichText::new("No favorites yet.").color(theme::CONCRETE)));
            }
            let rest = ui.allocate_response(vec2(ui.available_width(), ui.available_height().max(40.0)), Sense::click());
            rest.context_menu(|ui| {
                if ui.button("New folder").clicked() {
                    actions.push(Action::NewFavFolder(None));
                    ui.close();
                }
            });
        });
    }

    pub(super) fn workspace_panel(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        // Set by the −/+ buttons for this frame: collapse / expand every folder.
        let mut fold: Option<bool> = None;
        let root = self.workspace.uid;
        let target = self.default_folder();
        ui.horizontal_wrapped(|ui| {
            if ui.button("+ Request").on_hover_text("New saved request").clicked() {
                actions.push(Action::NewRequestIn(target));
            }
            if ui.button("+ Flow").on_hover_text("New flow (chain API calls with blocks)").clicked() {
                actions.push(Action::NewFlow(Some(target)));
            }
            if ui.button("+ Folder").clicked() {
                actions.push(Action::NewFolder(root));
            }
            ui.menu_button("Import", |ui| {
                if ui.button("From file…").clicked() {
                    actions.push(Action::ImportFile(root));
                    ui.close();
                }
                if ui.button("Paste JSON…").clicked() {
                    actions.push(Action::ImportPaste(root));
                    ui.close();
                }
            });
            fold = fold_row(ui);
        });
        ui.horizontal_wrapped(|ui| {
            if ui
                .button("Data map")
                .on_hover_text("The structure of the data found in GET responses: entities, fields, links (Understand builds it, from the tab)")
                .clicked()
            {
                actions.push(Action::ShowDataMap);
            }
        });
        let (nr, nf) = self.workspace.count();
        ui.label(RichText::new(format!("{nr} requests · {nf} flows — right-click an item for more")).size(11.5).color(theme::CONCRETE));
        ui.add_space(4.0);
        let folders = self.workspace.folder_paths();
        let open: Option<u64> = self.tabs.get(self.active).and_then(|t| t.saved);
        ScrollArea::vertical().id_salt("workspace").auto_shrink([false, false]).show(ui, |ui| {
            tree(ui, &self.workspace, open, &folders, &self.favorites, fold, actions);
            if self.workspace.items.is_empty() {
                ui.label(RichText::new("Empty. Create a request or a flow, or import a shared file.").color(theme::CONCRETE));
            }
            // Right-click on the empty area
            let rest = ui.allocate_response(vec2(ui.available_width(), ui.available_height().max(40.0)), Sense::click());
            rest.context_menu(|ui| folder_menu(ui, root, actions));
        });
    }

    pub(super) fn history_list(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} requests, most recent first", self.history.len())).size(11.5).color(theme::CONCRETE));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if !self.history.is_empty() && ui.small_button("Clear").clicked() {
                    actions.push(Action::ClearHistory);
                }
            });
        });
        ScrollArea::vertical().id_salt("history").auto_shrink([false, false]).show(ui, |ui| {
            if self.history.is_empty() {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| ui.label(RichText::new("Sent requests show up here.").color(theme::CONCRETE)));
            }
            for (i, h) in self.history.iter().enumerate().rev() {
                let w = ui.available_width();
                let (rect, resp) = ui.allocate_exact_size(vec2(w, 40.0), Sense::click());
                if ui.is_rect_visible(rect) {
                    let p = ui.painter_at(rect);
                    if resp.hovered() {
                        p.rect_filled(rect, CornerRadius::same(3), Color32::from_white_alpha(10));
                    }
                    let top = rect.left_top() + vec2(8.0, 11.0);
                    p.text(top, Align2::LEFT_CENTER, theme::short_verb(&h.verb), FontId::new(11.0, theme::bold()), theme::verb_color(&h.verb));
                    p.text(top + vec2(50.0, 0.0), Align2::LEFT_CENTER, &h.title, FontId::proportional(13.0), theme::WHITE);
                    let path = model::path_of(&h.url);
                    p.text(top + vec2(50.0, 17.0), Align2::LEFT_CENTER, path, FontId::monospace(11.0), theme::SILVER);
                    let right = rect.right_top() + vec2(-8.0, 11.0);
                    let (st, col) = match h.status {
                        Some(s) => (s.to_string(), theme::status_color(s)),
                        None => ("ERR".into(), theme::RED),
                    };
                    p.text(right, Align2::RIGHT_CENTER, st, FontId::new(11.5, theme::bold()), col);
                    p.text(right + vec2(0.0, 17.0), Align2::RIGHT_CENTER, storage::ago(h.when), FontId::proportional(10.5), theme::CONCRETE);
                }
                if resp.on_hover_text(&h.url).clicked() {
                    actions.push(Action::Restore(i));
                }
            }
        });
    }
}

/// Substitution folders and rules (sidebar).
pub(super) fn subst_tree(ui: &mut Ui, items: &[subst::Entry], paths: &[(Option<u64>, String)], fold: Option<bool>, actions: &mut Vec<Action>) {
    let move_menu = |ui: &mut Ui, uid: u64, actions: &mut Vec<Action>| {
        if paths.len() > 1 {
            ui.menu_button("Move to", |ui| {
                for (to, path) in paths {
                    if *to != Some(uid) && ui.button(path).clicked() {
                        actions.push(Action::MoveSubst(uid, *to));
                        ui.close();
                    }
                }
            });
        }
    };
    for e in items {
        match e {
            subst::Entry::Folder { uid, name, items } => {
                let uid = *uid;
                let title = RichText::new(format!("{name}   {}", items.len())).family(theme::bold()).size(14.0).color(theme::WHITE);
                let resp = CollapsingHeader::new(title).id_salt(("substfolder", uid)).default_open(true).open(fold).show(ui, |ui| {
                    if items.is_empty() {
                        ui.label(RichText::new("empty — right-click the folder").size(11.5).color(theme::CONCRETE));
                    }
                    subst_tree(ui, items, paths, fold, actions);
                });
                resp.header_response.context_menu(|ui| {
                    if ui.button("New substitution here").clicked() {
                        actions.push(Action::NewSubst(Some(uid)));
                        ui.close();
                    }
                    if ui.button("New sub-folder").clicked() {
                        actions.push(Action::NewSubstFolder(Some(uid)));
                        ui.close();
                    }
                    if ui.button("Rename…").clicked() {
                        actions.push(Action::RenameSubstFolder(uid));
                        ui.close();
                    }
                    move_menu(ui, uid, actions);
                    ui.separator();
                    if ui.button("Copy as JSON (share)").clicked() {
                        actions.push(Action::CopySubst(uid));
                        ui.close();
                    }
                    if ui.button("Export to file…").clicked() {
                        actions.push(Action::ExportSubst(uid));
                        ui.close();
                    }
                    if ui.button("Import here…").clicked() {
                        actions.push(Action::ImportSubstPaste(Some(uid)));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(RichText::new("Delete folder (keeps its substitutions)").color(theme::RED)).clicked() {
                        actions.push(Action::DeleteSubst(uid));
                        ui.close();
                    }
                });
            }
            subst::Entry::Rule(r) => {
                let uid = r.uid;
                let name = if r.enabled { r.name.clone() } else { format!("{}  (off)", r.name) };
                let row = named_row(ui, "SUB", &name, false).on_hover_ui(|ui| {
                    if !r.description.is_empty() {
                        ui.label(&r.description);
                    }
                    ui.label(RichText::new(r.summary()).monospace().size(12.0));
                    ui.label(RichText::new(format!("Replaces {}", r.when.label())).size(11.5));
                });
                if row.clicked() {
                    actions.push(Action::EditSubst(uid));
                }
                row.context_menu(|ui| {
                    if ui.button("Edit…").clicked() {
                        actions.push(Action::EditSubst(uid));
                        ui.close();
                    }
                    if ui.button(if r.enabled { "Disable" } else { "Enable" }).clicked() {
                        actions.push(Action::ToggleSubst(uid));
                        ui.close();
                    }
                    if ui.button("Duplicate").clicked() {
                        actions.push(Action::DuplicateSubst(uid));
                        ui.close();
                    }
                    move_menu(ui, uid, actions);
                    ui.separator();
                    if ui.button("Copy as JSON (share)").clicked() {
                        actions.push(Action::CopySubst(uid));
                        ui.close();
                    }
                    if ui.button("Export to file…").clicked() {
                        actions.push(Action::ExportSubst(uid));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(RichText::new("Delete").color(theme::RED)).clicked() {
                        actions.push(Action::DeleteSubst(uid));
                        ui.close();
                    }
                });
            }
        }
    }
}

/// OK / Cancel row; returns `Some((ok, cancel))` when one was clicked.
pub(super) fn buttons(ui: &mut Ui, ok_label: &str) -> Option<(bool, bool)> {
    ui.add_space(8.0);
    let mut out = None;
    ui.horizontal(|ui| {
        if ui.add(Button::new(RichText::new(ok_label).family(theme::bold()).color(theme::WHITE)).fill(theme::CYAN)).clicked() {
            out = Some((true, false));
        }
        if ui.button("Cancel").clicked() {
            out = Some((false, true));
        }
    });
    out
}

pub(super) fn folder_menu(ui: &mut Ui, uid: u64, actions: &mut Vec<Action>) {
    for (label, action) in [
        ("New request here", Action::NewRequestIn(uid)),
        ("New flow here", Action::NewFlow(Some(uid))),
        ("New sub-folder", Action::NewFolder(uid)),
        ("Import file here…", Action::ImportFile(uid)),
        ("Paste JSON here…", Action::ImportPaste(uid)),
        ("▶ Run tests", Action::RunTests(uid)),
        ("Generate documentation…", Action::GenerateDocs(uid)),
        ("Folder settings (auth)…", Action::FolderSettings(uid)),
    ] {
        if ui.button(label).clicked() {
            actions.push(action);
            ui.close();
        }
    }
}

pub(super) fn node_menu(ui: &mut Ui, node: &Node, folders: &[(u64, String)], favorites: &Favorites, actions: &mut Vec<Action>) {
    let uid = node.uid();
    if let Node::Folder(_) = node {
        folder_menu(ui, uid, actions);
        ui.separator();
    } else {
        if ui.button("Open").clicked() {
            actions.push(Action::OpenNode(uid));
            ui.close();
        }
        if ui.button("▶ Run test").on_hover_text("Send it (or run the flow) and check its assertions").clicked() {
            actions.push(Action::RunTests(uid));
            ui.close();
        }
        let fav = Favorite::Item { uid };
        fav_menu(ui, &fav, favorites.contains(&fav), &favorites.folder_paths(), actions);
    }
    if ui.button("Rename…").clicked() {
        actions.push(Action::Rename(uid));
        ui.close();
    }
    if ui.button("Duplicate").clicked() {
        actions.push(Action::Duplicate(uid));
        ui.close();
    }
    ui.menu_button("Move to", |ui| {
        for (f, path) in folders {
            if *f != uid && ui.button(path).clicked() {
                actions.push(Action::MoveTo(uid, *f));
                ui.close();
            }
        }
    });
    ui.separator();
    if ui.button("Export to file… (share)").clicked() {
        actions.push(Action::ExportNode(uid));
        ui.close();
    }
    if ui.button("Copy as JSON (share)").clicked() {
        actions.push(Action::CopyNode(uid));
        ui.close();
    }
    ui.separator();
    if ui.button(RichText::new("Delete…").color(theme::RED)).clicked() {
        actions.push(Action::Delete(uid));
        ui.close();
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn tree(ui: &mut Ui, folder: &Folder, open: Option<u64>, folders: &[(u64, String)], favorites: &Favorites, fold: Option<bool>, actions: &mut Vec<Action>) {
    for node in &folder.items {
        match node {
            Node::Folder(f) => {
                let (nr, nf) = f.count();
                let title = RichText::new(format!("{}   {}", f.name, nr + nf)).family(theme::bold()).size(14.0).color(theme::WHITE);
                let resp = CollapsingHeader::new(title).id_salt(("folder", f.uid)).default_open(true).open(fold).show(ui, |ui| {
                    if f.items.is_empty() {
                        ui.label(RichText::new("empty — right-click the folder").size(11.5).color(theme::CONCRETE));
                    }
                    tree(ui, f, open, folders, favorites, fold, actions);
                });
                resp.header_response.context_menu(|ui| node_menu(ui, node, folders, favorites, actions));
            }
            Node::Request(r) => {
                let row =
                    named_row(ui, &r.spec.verb, &r.name, open == Some(r.uid)).on_hover_text(format!("{} {}  ·  drag it onto a flow to add it", r.spec.verb, r.spec.path));
                row.dnd_set_drag_payload(BlockDrop::Request(Box::new(r.spec.clone())));
                if row.clicked() {
                    actions.push(Action::OpenNode(r.uid));
                }
                if favorites.contains(&Favorite::Item { uid: node.uid() }) {
                    paint_star(ui, row.rect);
                }
                row.context_menu(|ui| node_menu(ui, node, folders, favorites, actions));
            }
            Node::Flow(fl) => {
                let tip = if fl.description.is_empty() { format!("{} blocks", fl.blocks.len()) } else { fl.description.clone() };
                let row = named_row(ui, "FLOW", &fl.name, open == Some(fl.uid)).on_hover_text(format!("{tip}  ·  drag it onto a flow to run it from there"));
                row.dnd_set_drag_payload(BlockDrop::Flow { uid: fl.uid, name: fl.name.clone() });
                if row.clicked() {
                    actions.push(Action::OpenNode(fl.uid));
                }
                if favorites.contains(&Favorite::Item { uid: node.uid() }) {
                    paint_star(ui, row.rect);
                }
                row.context_menu(|ui| node_menu(ui, node, folders, favorites, actions));
            }
        }
    }
}

/// Small yellow star at the right end of a list row.
pub(super) fn paint_star(ui: &Ui, rect: egui::Rect) {
    ui.painter().text(rect.right_center() - vec2(8.0, 0.0), Align2::RIGHT_CENTER, "★", FontId::proportional(13.0), theme::YELLOW);
}

/// "Add to favorites" / "Remove" entries, with a folder choice when folders exist.
pub(super) fn fav_menu(ui: &mut Ui, fav: &Favorite, starred: bool, paths: &[(Option<u64>, String)], actions: &mut Vec<Action>) {
    if starred {
        if ui.button("Remove from favorites").clicked() {
            actions.push(Action::ToggleFavorite(fav.clone()));
            ui.close();
        }
    } else if paths.len() <= 1 {
        if ui.button("★ Add to favorites").clicked() {
            actions.push(Action::AddFavorite(fav.clone(), None));
            ui.close();
        }
    } else {
        ui.menu_button("★ Add to favorites", |ui| {
            for (uid, path) in paths {
                if ui.button(path).clicked() {
                    actions.push(Action::AddFavorite(fav.clone(), *uid));
                    ui.close();
                }
            }
        });
    }
}

pub(super) struct FavCtx<'a> {
    doc: &'a ApiDoc,
    workspace: &'a Folder,
    paths: Vec<(Option<u64>, String)>,
    open_doc: Option<DocRef>,
    open_uid: Option<u64>,
}

/// Draws favorites and folders; returns how many favorites were shown.
pub(super) fn fav_tree(ui: &mut Ui, entries: &[favorites::Entry], c: &FavCtx, fold: Option<bool>, actions: &mut Vec<Action>) -> usize {
    use favorites::Entry;
    let mut shown = 0;
    for e in entries {
        if let Entry::Folder { uid, name, items } = e {
            let title = RichText::new(format!("{name}   {}", items.len())).family(theme::bold()).size(14.0).color(theme::WHITE);
            let resp = CollapsingHeader::new(title).id_salt(("favfolder", *uid)).default_open(true).open(fold).show(ui, |ui| {
                if items.is_empty() {
                    ui.label(RichText::new("empty — right-click a favorite › Move to").size(11.5).color(theme::CONCRETE));
                }
                shown += fav_tree(ui, items, c, fold, actions);
            });
            let uid = *uid;
            resp.header_response.context_menu(|ui| {
                if ui.button("New sub-folder").clicked() {
                    actions.push(Action::NewFavFolder(Some(uid)));
                    ui.close();
                }
                if ui.button("Rename…").clicked() {
                    actions.push(Action::RenameFavFolder(uid));
                    ui.close();
                }
                ui.menu_button("Move to", |ui| {
                    for (to, path) in &c.paths {
                        if *to != Some(uid) && ui.button(path).clicked() {
                            actions.push(Action::MoveFavFolder(uid, *to));
                            ui.close();
                        }
                    }
                });
                ui.separator();
                if ui.button(RichText::new("Delete folder (keeps its favorites)").color(theme::RED)).clicked() {
                    actions.push(Action::DeleteFavFolder(uid));
                    ui.close();
                }
            });
            continue;
        }
        let Some(fav) = e.favorite() else { continue };
        let (row, open_action) = match &fav {
            Favorite::Endpoint(r) => {
                let Some((res, m)) = c.doc.lookup(r) else { continue };
                let Some(route) = m.routes.get(r.route) else { continue };
                let selected = c.open_doc.as_ref() == Some(r);
                let row = named_row(ui, &route.verb, &format!("{} › {}", res.name, m.name), selected).on_hover_text(format!("{} {}", route.verb, route.path));
                row.dnd_set_drag_payload(BlockDrop::Endpoint(r.clone()));
                (row, Action::Open(r.clone()))
            }
            Favorite::Item { uid } => {
                let Some(node) = c.workspace.find(*uid) else { continue };
                let badge = match node {
                    Node::Request(r) => r.spec.verb.clone(),
                    Node::Flow(_) => "FLOW".into(),
                    Node::Folder(_) => continue,
                };
                let row = named_row(ui, &badge, node.name(), c.open_uid == Some(*uid)).on_hover_text("Saved in your workspace");
                if let Node::Request(r) = node {
                    row.dnd_set_drag_payload(BlockDrop::Request(Box::new(r.spec.clone())));
                }
                (row, Action::OpenNode(*uid))
            }
        };
        shown += 1;
        row.context_menu(|ui| {
            if ui.button("Open").clicked() {
                actions.push(open_action.clone_open());
                ui.close();
            }
            if c.paths.len() > 1 {
                ui.menu_button("Move to", |ui| {
                    for (to, path) in &c.paths {
                        if ui.button(path).clicked() {
                            actions.push(Action::AddFavorite(fav.clone(), *to));
                            ui.close();
                        }
                    }
                });
            }
            if ui.button("Remove from favorites").clicked() {
                actions.push(Action::ToggleFavorite(fav.clone()));
                ui.close();
            }
        });
        if row.clicked() {
            actions.push(open_action);
        }
    }
    shown
}
