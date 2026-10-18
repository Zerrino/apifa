//! API profiles: the picker in the title bar and the editor window.

use egui::{vec2, Align2, Button, RichText, ScrollArea, TextEdit, Ui};

use crate::profiles::Profile;
use crate::theme;

use super::*;

impl App {
    /// The active profile, and the others to pick from (title bar).
    pub(super) fn profile_picker(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let active = self.profiles.active().map(|p| p.name.clone());
        let color = if active.is_some() { theme::CYAN } else { theme::SILVER };
        egui::ComboBox::from_id_salt("profile_picker")
            .selected_text(RichText::new(active.as_deref().map_or("No API profile".to_owned(), |n| format!("API: {n}"))).size(13.0).color(color))
            .width(160.0)
            .show_ui(ui, |ui| {
                if ui.selectable_label(active.is_none(), "No API profile").clicked() {
                    actions.push(Action::SelectProfile(None));
                }
                for p in &self.profiles.list {
                    if ui.selectable_label(self.profiles.active.as_deref() == Some(p.id.as_str()), &p.name).clicked() {
                        actions.push(Action::SelectProfile(Some(p.id.clone())));
                    }
                }
                ui.separator();
                if ui.button("Edit API profiles…").clicked() {
                    actions.push(Action::EditProfiles);
                }
            })
            .response
            .on_hover_text("The API you work on: its base URL, its authentication (its own tokens), its endpoints and its data map. Requests of a folder bound to a profile always use it.");
    }

    /// Makes `id` the active profile: saved, used by the next requests, the data map follows.
    pub(super) fn select_profile(&mut self, id: Option<String>) {
        if let Err(e) = self.profiles.select(id.as_deref()) {
            self.toast(e, theme::RED);
            return;
        }
        self.profiles_changed();
        // The data map of that profile (the tab loads it when shown).
        self.map_view.clear();
        self.doc.set_data_map(crate::datamap::load().as_ref());
    }

    pub(super) fn profiles_changed(&mut self) {
        if !self.no_save {
            self.profiles.save();
        }
        crate::profiles::set_current(&self.profiles);
    }

    /// The profile a saved node (request, flow) uses: its folders', else the active one (empty: none).
    pub(super) fn profile_of(&self, uid: Option<u64>) -> String {
        uid.map(|u| self.workspace.profile_for(u)).filter(|p| !p.is_empty()).unwrap_or_else(|| self.profiles.active.clone().unwrap_or_default())
    }

    pub(super) fn profiles_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if !self.show_profiles {
            return;
        }
        let mut open = true;
        let before = serde_json::to_string(&self.profiles).unwrap_or_default();
        let apis = crate::apis::list();
        egui::Window::new(RichText::new("API profiles").family(theme::bold()))
            .open(&mut open)
            .collapsible(false)
            .default_width(760.0)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "One profile per API you work with: its base URL, its own authentication (its own tokens), the API definitions that describe it \
                         and its own data map. Bind a folder to a profile (Folder settings…) and its requests always use it; the others use the active one.",
                    )
                    .color(theme::SILVER),
                );
                ui.add_space(6.0);
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(180.0);
                        ScrollArea::vertical().id_salt("profile_list").max_height(380.0).show(ui, |ui| {
                            for (i, p) in self.profiles.list.iter().enumerate() {
                                let active = self.profiles.active.as_deref() == Some(p.id.as_str());
                                let label = if active { format!("● {}", p.name) } else { p.name.clone() };
                                if ui.selectable_label(self.profile_sel == i, label).clicked() {
                                    self.profile_sel = i;
                                }
                            }
                        });
                        if ui.button("+ New profile").clicked() {
                            let n = self.profiles.list.len() + 1;
                            self.profiles.add(Profile { name: format!("API {n}"), ..Default::default() });
                            self.profile_sel = self.profiles.list.len() - 1;
                        }
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        let sel = self.profile_sel;
                        let show = &mut self.profile_show_secrets;
                        let Some(p) = self.profiles.list.get_mut(sel) else {
                            ui.label(RichText::new("No profile yet: + New profile, or import an API definition (one is made for it).").color(theme::SILVER));
                            return;
                        };
                        egui::Grid::new("profile_head").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                            ui.label("Name");
                            ui.add(TextEdit::singleline(&mut p.name).desired_width(320.0));
                            ui.end_row();
                            ui.label("Id");
                            ui.label(RichText::new(&p.id).monospace().color(theme::SILVER)).on_hover_text("What folders point to, and the data map's file name");
                            ui.end_row();
                            ui.label("Base URL");
                            ui.add(
                                TextEdit::singleline(&mut p.base_url)
                                    .font(egui::TextStyle::Monospace)
                                    .hint_text("https://api.example.com  ({{vars}} allowed)")
                                    .desired_width(420.0),
                            )
                            .on_hover_text("Where its relative paths go. Empty: the environment's baseUrl, else Settings › Base URL.");
                            ui.end_row();
                            ui.label("Notes");
                            ui.add(TextEdit::singleline(&mut p.description).desired_width(420.0));
                            ui.end_row();
                        });
                        ui.add_space(6.0);
                        ui.label(RichText::new("API definitions").family(theme::bold()));
                        if apis.is_empty() {
                            ui.label(RichText::new("None imported yet (sidebar › Endpoints › APIs).").size(12.0).color(theme::SILVER));
                        }
                        ui.horizontal_wrapped(|ui| {
                            for a in &apis {
                                let mut on = p.apis.contains(&a.name);
                                if ui.checkbox(&mut on, &a.name).on_hover_text(format!("{} endpoints", a.endpoints)).changed() {
                                    if on {
                                        p.apis.push(a.name.clone());
                                    } else {
                                        p.apis.retain(|x| *x != a.name);
                                    }
                                }
                            }
                        });
                        ui.add_space(6.0);
                        ui.label(RichText::new("Authentication").family(theme::bold()));
                        let mut get = false;
                        ScrollArea::vertical().id_salt("profile_auth").max_height(300.0).show(ui, |ui| {
                            crate::ui_auth::auth_editor(ui, &mut p.auth, Some("the workspace's authentication (Settings)"), show, &mut get, ("profile_auth", sel));
                        });
                        if get {
                            actions.push(Action::FetchToken(p.auth.clone()));
                        }
                        ui.add_space(6.0);
                        let id = p.id.clone();
                        let mut delete = false;
                        ui.horizontal(|ui| {
                            if self.profiles.active.as_deref() != Some(id.as_str()) && ui.button("Make it active").clicked() {
                                actions.push(Action::SelectProfile(Some(id.clone())));
                            }
                            if ui.add(Button::new(RichText::new("Delete").color(theme::RED))).on_hover_text("Its folders go back to the active profile").clicked() {
                                delete = true;
                            }
                        });
                        if delete {
                            self.profiles.list.remove(sel);
                            if self.profiles.active.as_deref() == Some(id.as_str()) {
                                self.profiles.active = None;
                            }
                            self.profile_sel = sel.saturating_sub(1);
                        }
                    });
                });
            });
        self.show_profiles = open;
        if serde_json::to_string(&self.profiles).unwrap_or_default() != before {
            self.profiles_changed();
        }
    }
}
