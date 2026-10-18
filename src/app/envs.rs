//! Environments: the picker in the title bar and the editor window.

use egui::{vec2, Align2, Button, RichText, ScrollArea, TextEdit, Ui};

use crate::env::Environment;
use crate::theme;

use super::*;

/// The variables of one environment: on / off, name, value (masked when secret), secret, remove.
fn env_vars_editor(ui: &mut Ui, e: &mut Environment, salt: usize) {
    let mut remove = None;
    let show_id = egui::Id::unique(("env_show", salt));
    let mut show = ui.data(|d| d.get_temp::<bool>(show_id)).unwrap_or(false);
    egui::Grid::new(("env_grid", salt)).num_columns(5).spacing([6.0, 4.0]).show(ui, |ui| {
        for (i, kv) in e.vars.iter_mut().enumerate() {
            ui.checkbox(&mut kv.enabled, "");
            ui.add(TextEdit::singleline(&mut kv.key).hint_text("name").desired_width(140.0).font(egui::TextStyle::Monospace));
            let secret = e.secrets.iter().any(|s| s == kv.key.trim());
            ui.add(TextEdit::singleline(&mut kv.value).hint_text("value").desired_width(240.0).password(secret && !show).font(egui::TextStyle::Monospace));
            let mut on = secret;
            if ui.checkbox(&mut on, "secret").changed() {
                let k = kv.key.trim().to_owned();
                if on {
                    e.secrets.push(k);
                } else {
                    e.secrets.retain(|s| *s != k);
                }
            }
            if ui.small_button("✕").clicked() {
                remove = Some(i);
            }
            ui.end_row();
        }
    });
    if let Some(i) = remove {
        let gone = e.vars.remove(i);
        e.secrets.retain(|s| *s != gone.key.trim());
    }
    ui.horizontal(|ui| {
        if ui.small_button("+ Variable").clicked() {
            e.vars.push(crate::model::Kv { enabled: true, key: String::new(), value: String::new() });
        }
        if !e.secrets.is_empty() && ui.small_button(if show { "Hide secrets" } else { "Show secrets" }).clicked() {
            show = !show;
        }
    });
    ui.data_mut(|d| d.insert_temp(show_id, show));
}

impl App {
    /// The active environment, and the others to pick from (title bar).
    pub(super) fn env_picker(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let active = self.envs.active.clone();
        let text = match &active {
            Some(name) => format!("Env: {name}"),
            None => "No environment".to_owned(),
        };
        let color = if active.is_some() { theme::CYAN } else { theme::SILVER };
        egui::ComboBox::from_id_salt("env_picker")
            .selected_text(RichText::new(text).size(13.0).color(color))
            .width(150.0)
            .show_ui(ui, |ui| {
                if ui.selectable_label(active.is_none(), "No environment").clicked() {
                    actions.push(Action::SelectEnv(None));
                }
                for e in &self.envs.list {
                    if ui.selectable_label(active.as_deref() == Some(e.name.as_str()), &e.name).clicked() {
                        actions.push(Action::SelectEnv(Some(e.name.clone())));
                    }
                }
                ui.separator();
                if ui.button("Edit environments…").clicked() {
                    actions.push(Action::EditEnvs);
                }
            })
            .response
            .on_hover_text("{{name}} in requests and flows takes the value of the active environment (baseUrl, token, user_id…)");
    }

    /// Makes `name` the active environment: saved, and used by the next requests and runs.
    pub(super) fn select_env(&mut self, name: Option<String>) {
        if let Err(e) = self.envs.select(name.as_deref()) {
            self.toast(e, theme::RED);
            return;
        }
        self.envs_changed();
    }

    fn envs_changed(&mut self) {
        if !self.no_save {
            self.envs.save();
        }
        crate::env::set_current(self.envs.active_vars());
    }

    pub(super) fn env_window(&mut self, ctx: &egui::Context) {
        if !self.show_envs {
            return;
        }
        let mut open = true;
        let before = serde_json::to_string(&self.envs).unwrap_or_default();
        egui::Window::new(RichText::new("Environments").family(theme::bold()))
            .open(&mut open)
            .collapsible(false)
            .default_width(640.0)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(
                    RichText::new(
                        "Values used as {{name}} in every request (path, fields, query, headers, body) and as starting variables of flows \
                         (a flow's inputs replace them). One is active at a time: pick it in the title bar.",
                    )
                    .color(theme::SILVER),
                );
                ui.add_space(6.0);
                ui.horizontal_top(|ui| {
                    // The list.
                    ui.vertical(|ui| {
                        ui.set_width(170.0);
                        ScrollArea::vertical().id_salt("env_list").max_height(320.0).show(ui, |ui| {
                            for (i, e) in self.envs.list.iter().enumerate() {
                                let active = self.envs.active.as_deref() == Some(e.name.as_str());
                                let label = if active { format!("● {}", e.name) } else { e.name.clone() };
                                if ui.selectable_label(self.env_sel == i, label).clicked() {
                                    self.env_sel = i;
                                }
                            }
                        });
                        if ui.button("+ New environment").clicked() {
                            let mut n = self.envs.list.len() + 1;
                            while self.envs.list.iter().any(|e| e.name == format!("Environment {n}")) {
                                n += 1;
                            }
                            self.envs.list.push(Environment {
                                name: format!("Environment {n}"),
                                vars: vec![crate::model::Kv { enabled: true, key: "baseUrl".into(), value: String::new() }],
                                secrets: Vec::new(),
                            });
                            self.env_sel = self.envs.list.len() - 1;
                        }
                        if let Some(e) = self.envs.list.get(self.env_sel).cloned() {
                            if ui.button("Duplicate").on_hover_text("A copy of the selected one (e.g. prod from staging)").clicked() {
                                self.envs.list.push(Environment { name: format!("{} copy", e.name), ..e });
                                self.env_sel = self.envs.list.len() - 1;
                            }
                        }
                    });
                    ui.separator();
                    // The selected one.
                    ui.vertical(|ui| {
                        let Some(e) = self.envs.list.get_mut(self.env_sel) else {
                            ui.label(RichText::new("No environment yet: + New environment.").color(theme::SILVER));
                            return;
                        };
                        let old_name = e.name.clone();
                        ui.horizontal(|ui| {
                            ui.label("Name");
                            ui.add(TextEdit::singleline(&mut e.name).desired_width(220.0));
                        });
                        let renamed = e.name != old_name;
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(
                                "baseUrl is where relative paths go (/users → {{baseUrl}}/users). Mark secrets: they are masked here and never given to an AI.",
                            )
                            .size(12.0)
                            .color(theme::SILVER),
                        );
                        ScrollArea::vertical().id_salt("env_vars").max_height(300.0).show(ui, |ui| env_vars_editor(ui, e, self.env_sel));
                        let mut delete = false;
                        ui.horizontal(|ui| {
                            let is_active = self.envs.active.as_deref() == Some(old_name.as_str());
                            if !is_active && ui.button("Make it active").clicked() {
                                self.envs.active = Some(self.envs.list[self.env_sel].name.clone());
                            }
                            if ui.add(Button::new(RichText::new("Delete").color(theme::RED))).clicked() {
                                delete = true;
                            }
                        });
                        // The active one follows its rename; a deleted one is no longer active.
                        if renamed && self.envs.active.as_deref() == Some(old_name.as_str()) {
                            self.envs.active = Some(self.envs.list[self.env_sel].name.clone());
                        }
                        if delete {
                            let gone = self.envs.list.remove(self.env_sel);
                            if self.envs.active.as_deref() == Some(gone.name.as_str()) {
                                self.envs.active = None;
                            }
                            self.env_sel = self.env_sel.saturating_sub(1);
                        }
                    });
                });
            });
        self.show_envs = open;
        if serde_json::to_string(&self.envs).unwrap_or_default() != before {
            self.envs_changed();
        }
    }
}
