//! Dialogs: confirmations, rename, new folder, save as.

use egui::{Button, Id, RichText, ScrollArea, TextEdit, ViewportCommand};

use crate::storage;
use crate::workspace::{self, Folder, Node};
use crate::{install, theme};

use super::*;

impl App {
    pub(super) fn dialogs(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let Some(mut dialog) = self.dialog.take() else { return };
        let mut keep = true;
        let modal = egui::Modal::new(Id::unique("dialog")).show(ctx, |ui| {
            ui.set_width(460.0);
            match &mut dialog {
                Dialog::Rename { uid, text } => {
                    ui.label(RichText::new("Rename").family(theme::bold()).size(17.0));
                    ui.add(TextEdit::singleline(text).desired_width(f32::INFINITY)).request_focus();
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let clicked = buttons(ui, "Rename");
                    let ok = enter || clicked.is_some_and(|(ok, _)| ok);
                    if ok && !text.trim().is_empty() {
                        self.rename(*uid, text.trim());
                    }
                    if ok || clicked.is_some() {
                        keep = false;
                    }
                }
                Dialog::FolderSettings { uid, auth, profile, show } => {
                    let name = self.workspace.folder_mut(*uid).map(|f| f.name.clone()).unwrap_or_default();
                    ui.label(RichText::new(format!("Folder “{name}”")).family(theme::bold()).size(17.0));
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("API profile").family(theme::bold()));
                        let shown = self.profiles.get(profile).map(|p| p.name.clone()).unwrap_or_else(|| "— its parent's, else the active one —".into());
                        egui::ComboBox::from_id_salt(("folder_profile", *uid)).selected_text(shown).width(260.0).show_ui(ui, |ui| {
                            if ui.selectable_label(profile.is_empty(), "— its parent's, else the active one —").clicked() {
                                profile.clear();
                            }
                            for p in &self.profiles.list {
                                if ui.selectable_label(*profile == p.id, &p.name).clicked() {
                                    *profile = p.id.clone();
                                }
                            }
                        });
                    });
                    ui.label(RichText::new("Its requests and flows use that profile's base URL and authentication, whatever profile is active.").size(12.0).color(theme::SILVER));
                    ui.add_space(6.0);
                    ui.label(RichText::new("Authentication").family(theme::bold()));
                    ui.label(RichText::new("Its requests and sub-folders whose auth is “Inherit” use this one (Inherit here: the profile's).").color(theme::SILVER));
                    ui.add_space(6.0);
                    let mut get = false;
                    crate::ui_auth::auth_editor(ui, auth, Some("the parent folder's, else the workspace's (Settings)"), show, &mut get, ("folder_auth", *uid));
                    if get {
                        actions.push(Action::FetchToken((**auth).clone()));
                    }
                    if let Some((ok, _)) = buttons(ui, "Save") {
                        if ok {
                            if let Some(f) = self.workspace.folder_mut(*uid) {
                                f.auth = (**auth).clone();
                                f.profile = profile.clone();
                            }
                            self.save_workspace();
                        }
                        keep = false;
                    }
                }
                Dialog::Docs { uid, api, format, examples, open } => {
                    let name = match api {
                        Some(keys) if keys.is_empty() => "the imported APIs".to_owned(),
                        Some(keys) => self.doc.resources.iter().filter(|r| keys.contains(&r.key)).map(|r| r.name.clone()).collect::<Vec<_>>().join(", "),
                        None if *uid == self.workspace.uid => "the workspace".to_owned(),
                        None => self.workspace.find(*uid).map(|n| n.name().to_owned()).unwrap_or_default(),
                    };
                    ui.label(RichText::new(format!("Documentation of {name}")).family(theme::bold()).size(17.0));
                    let what = if api.is_some() {
                        "Every documented endpoint, by resource: its description, all its parameters, a cURL command, an example response."
                    } else {
                        "Every saved request and flow in it: your notes (Docs tab), the API docs of each endpoint, parameters with their saved values, a cURL command, assertions, an example response."
                    };
                    ui.label(RichText::new(what).color(theme::MUTED));
                    ui.add_space(6.0);
                    for f in crate::docgen::Format::ALL {
                        let tip = match f {
                            crate::docgen::Format::Html => "One page, works offline, with a search box",
                            crate::docgen::Format::Markdown => "For a README, a wiki, a Git repository",
                            crate::docgen::Format::OpenApi => "For Swagger UI, Redoc, code generators, other API clients",
                        };
                        ui.radio_value(format, f, f.label()).on_hover_text(tip);
                    }
                    ui.add_space(4.0);
                    ui.checkbox(examples, "Include example responses")
                        .on_hover_text("From your mocks, the API docs, or responses recorded from real calls: they can hold real people's data, check before sharing");
                    if *format == crate::docgen::Format::Html {
                        ui.checkbox(open, "Open it in the browser");
                    }
                    if let Some((ok, _)) = buttons(ui, "Save…") {
                        if ok {
                            let generated = match api {
                                Some(keys) => {
                                    let title = if keys.is_empty() { "API".to_owned() } else { format!("API — {name}") };
                                    let (text, n) = crate::docgen::generate_api(&self.doc, keys, &crate::docgen::Options { title, examples: *examples, data_map: crate::datamap::load() }, *format);
                                    Some((text, format!("{n} endpoint(s)")))
                                }
                                None => {
                                    let node = if *uid == self.workspace.uid { Some(Node::Folder(self.workspace.clone())) } else { self.workspace.find(*uid).cloned() };
                                    node.map(|node| {
                                        let items = crate::testrun::collect(&node, "");
                                        let title = format!("apifa — {}", if *uid == self.workspace.uid { "workspace".to_owned() } else { name.clone() });
                                        (crate::docgen::generate(&items, &self.doc, &crate::docgen::Options { title, examples: *examples, data_map: crate::datamap::load() }, *format), format!("{} request(s) / flow(s)", items.len()))
                                    })
                                }
                            };
                            if let Some((text, count)) = generated {
                                let file = format!("{}.{}", sanitize(&name), format.extension());
                                if let Some(path) = rfd::FileDialog::new().set_file_name(&file).save_file() {
                                    match std::fs::write(&path, text) {
                                        Ok(()) => {
                                            self.toast(format!("{count} documented in {}", path.display()), theme::GREEN);
                                            if *open && *format == crate::docgen::Format::Html {
                                                if let Err(e) = crate::visualize::open_in_browser(&path) {
                                                    self.toast(e, theme::RED);
                                                }
                                            }
                                        }
                                        Err(e) => self.toast(format!("Can't write {}: {e}", path.display()), theme::RED),
                                    }
                                }
                            }
                        }
                        keep = false;
                    }
                }
                Dialog::NewFolder { parent, text } => {
                    ui.label(RichText::new("New folder").family(theme::bold()).size(17.0));
                    ui.add(TextEdit::singleline(text).desired_width(f32::INFINITY)).request_focus();
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let clicked = buttons(ui, "Create");
                    let ok = enter || clicked.is_some_and(|(ok, _)| ok);
                    if ok {
                        self.create_folder(*parent, text.trim());
                    }
                    if ok || clicked.is_some() {
                        keep = false;
                    }
                }
                Dialog::SaveAs { tab, name, folder, new_folder } => {
                    ui.label(RichText::new("Save in workspace").family(theme::bold()).size(17.0));
                    ui.label("Name");
                    ui.add(TextEdit::singleline(name).desired_width(f32::INFINITY));
                    ui.label("Folder");
                    let paths = self.workspace.folder_paths();
                    let current = paths.iter().find(|(u, _)| u == folder).map(|(_, p)| p.clone()).unwrap_or_default();
                    egui::ComboBox::from_id_salt("save_folder").width(440.0).selected_text(current).show_ui(ui, |ui| {
                        for (u, p) in &paths {
                            ui.selectable_value(folder, *u, p);
                        }
                    });
                    ui.add(TextEdit::singleline(new_folder).desired_width(f32::INFINITY).hint_text("…or create a new sub-folder (optional)"));
                    if let Some((ok, _)) = buttons(ui, "Save") {
                        keep = false;
                        if ok {
                            if let Some(i) = self.pos(*tab) {
                                self.save_tab_into(i, name.trim(), *folder, new_folder.trim());
                            }
                        }
                    }
                }
                Dialog::Import { folder, text, error } => {
                    ui.label(RichText::new("Import").family(theme::bold()).size(17.0));
                    ui.label(
                        RichText::new("Paste a request, a flow or a folder shared by someone (or a Postman collection).")
                            .size(12.5)
                            .color(theme::SILVER),
                    );
                    ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        ui.add(TextEdit::multiline(text).code_editor().desired_rows(12).desired_width(f32::INFINITY).hint_text("{ \"format\": \"apifa\", … }"));
                    });
                    if let Some(e) = error {
                        ui.colored_label(theme::RED, e.as_str());
                    }
                    match buttons(ui, "Import") {
                        Some((true, _)) => match self.import_node(*folder, text) {
                            Ok(()) => keep = false,
                            Err(e) => *error = Some(e),
                        },
                        Some(_) => keep = false,
                        None => {}
                    }
                }
                Dialog::ConfirmDelete { uid, label } => {
                    ui.label(RichText::new("Delete?").family(theme::bold()).size(17.0));
                    ui.label(format!("The {label} will be removed from your workspace. Export it first if you want to keep a copy."));
                    if let Some((ok, _)) = buttons(ui, "Delete") {
                        keep = false;
                        if ok {
                            let uid = *uid;
                            self.workspace.remove(uid);
                            // A folder takes its requests and flows along: their tabs aren't saved anywhere any more.
                            for t in &mut self.tabs {
                                t.saved = t.saved.filter(|u| self.workspace.find(*u).is_some());
                            }
                            self.save_workspace();
                        }
                    }
                }
                Dialog::FavFolder { parent, rename, text } => {
                    let title = if rename.is_some() { "Rename folder" } else { "New favorites folder" };
                    ui.label(RichText::new(title).family(theme::bold()).size(17.0));
                    ui.add(TextEdit::singleline(text).desired_width(f32::INFINITY)).request_focus();
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let clicked = buttons(ui, if rename.is_some() { "Rename" } else { "Create" });
                    let ok = enter || clicked.is_some_and(|(ok, _)| ok);
                    if ok {
                        match rename {
                            Some(uid) => self.favorites.rename_folder(*uid, text),
                            None => self.favorites.add_folder(*parent, text),
                        }
                        self.save_favorites();
                    }
                    if ok || clicked.is_some() {
                        keep = false;
                    }
                }
                Dialog::SubstFolder { parent, rename, text } => {
                    let title = if rename.is_some() { "Rename folder" } else { "New substitutions folder" };
                    ui.label(RichText::new(title).family(theme::bold()).size(17.0));
                    ui.add(TextEdit::singleline(text).desired_width(f32::INFINITY)).request_focus();
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let clicked = buttons(ui, if rename.is_some() { "Rename" } else { "Create" });
                    let ok = enter || clicked.is_some_and(|(ok, _)| ok);
                    if ok {
                        match rename {
                            Some(uid) => self.subs.rename_folder(*uid, text),
                            None => self.subs.add_folder(*parent, text),
                        }
                        self.save_subs();
                    }
                    if ok || clicked.is_some() {
                        keep = false;
                    }
                }
                Dialog::SubstImport { folder, text, error } => {
                    ui.label(RichText::new("Import substitutions").family(theme::bold()).size(17.0));
                    ui.label(RichText::new("Paste a substitution or a folder shared by someone.").size(12.5).color(theme::SILVER));
                    ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                        ui.add(TextEdit::multiline(text).code_editor().desired_rows(12).desired_width(f32::INFINITY).hint_text("{ \"format\": \"apifa-substitutions\", … }"));
                    });
                    if let Some(e) = error {
                        ui.colored_label(theme::RED, e.as_str());
                    }
                    match buttons(ui, "Import") {
                        Some((true, _)) => match self.import_subs(*folder, text) {
                            Ok(_) => keep = false,
                            Err(e) => *error = Some(e),
                        },
                        Some(_) => keep = false,
                        None => {}
                    }
                }
                Dialog::Uninstall { delete_data, error } => {
                    ui.label(RichText::new("Uninstall apifa?").family(theme::bold()).size(17.0));
                    ui.label("The app will be removed from your applications and will close.");
                    ui.checkbox(delete_data, "Also delete my data (UID / SECRET, workspace, history)");
                    if !*delete_data {
                        ui.label(RichText::new(format!("Your data stays in {}.", storage::dir().display())).size(12.0).color(theme::SILVER));
                    }
                    if let Some(e) = error {
                        ui.colored_label(theme::RED, e.as_str());
                    }
                    match buttons(ui, "Uninstall") {
                        Some((true, _)) => match install::uninstall(*delete_data) {
                            Ok(_) => {
                                self.no_save = *delete_data;
                                keep = false;
                                ctx.send_viewport_cmd(ViewportCommand::Close);
                            }
                            Err(e) => *error = Some(e),
                        },
                        Some(_) => keep = false,
                        None => {}
                    }
                }
                Dialog::ConfirmClose { tab } => {
                    let title = self.pos(*tab).map(|i| self.tabs[i].title.clone()).unwrap_or_default();
                    ui.label(RichText::new("Unsaved changes").family(theme::bold()).size(17.0));
                    ui.label(format!("“{title}” has changes that aren't saved in your workspace."));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.add(Button::new(RichText::new("Save").color(theme::WHITE)).fill(theme::CYAN)).clicked() {
                            // Without its file the save asks where (a dialog): the tab must stay until then.
                            let linked = self.pos(*tab).and_then(|i| self.tabs[i].saved).is_some_and(|u| self.workspace.find(u).is_some());
                            actions.push(Action::SaveTab(*tab));
                            if linked {
                                actions.push(Action::ForceClose(*tab));
                            }
                            keep = false;
                        }
                        if ui.button("Discard").clicked() {
                            actions.push(Action::ForceClose(*tab));
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
            }
        });
        if modal.should_close() {
            keep = false;
        }
        if keep && self.dialog.is_none() {
            self.dialog = Some(dialog);
        }
    }

    pub(super) fn rename(&mut self, uid: u64, name: &str) {
        if let Some(n) = self.workspace.find_mut(uid) {
            n.set_name(name.to_owned());
        }
        for t in &mut self.tabs {
            if t.saved == Some(uid) {
                t.title = name.to_owned();
                if let TabBody::Flow(f) = &mut t.body {
                    f.flow.name = name.to_owned();
                }
            }
        }
        self.save_workspace();
    }

    pub(super) fn create_folder(&mut self, parent: u64, name: &str) -> Option<u64> {
        let name = if name.is_empty() { "New folder" } else { name };
        let folder = Folder::new(name);
        let uid = folder.uid;
        let f = self.workspace.folder_mut(parent)?;
        f.items.push(Node::Folder(folder));
        self.save_workspace();
        self.side = SideTab::Workspace;
        Some(uid)
    }

    pub(super) fn save_tab_into(&mut self, i: usize, name: &str, folder: u64, new_folder: &str) {
        let folder = if new_folder.is_empty() { Some(folder) } else { self.create_folder(folder, new_folder) };
        let Some(folder) = folder else { return };
        let Some(tab) = self.tabs.get_mut(i) else { return };
        if !name.is_empty() {
            tab.title = name.to_owned();
            if let TabBody::Flow(f) = &mut tab.body {
                f.flow.name = name.to_owned();
            }
        }
        // Results and resume files are named after the flow's uid: a flow saved for the first time keeps
        // its own (what it already ran stays linked); a copy of a saved one gets a new uid and files of its own.
        let uid = match &tab.body {
            TabBody::Flow(f) if tab.saved.is_none() && f.flow.uid != 0 && self.workspace.find(f.flow.uid).is_none() => f.flow.uid,
            _ => workspace::new_uid(),
        };
        if let TabBody::Flow(f) = &mut tab.body {
            f.flow.uid = uid;
        }
        let Some(node) = tab.to_node(uid) else { return };
        let title = tab.title.clone();
        // The folder can be gone (reloaded from disk while the dialog was open): the workspace root then.
        let target = if self.workspace.folder_mut(folder).is_some() { folder } else { self.workspace.uid };
        let Some(f) = self.workspace.folder_mut(target) else { return };
        f.items.push(node);
        if let Some(tab) = self.tabs.get_mut(i) {
            tab.saved = Some(uid);
        }
        self.save_workspace();
        self.side = SideTab::Workspace;
        self.toast(format!("Saved “{title}”"), theme::GREEN);
    }
}
