//! The settings window.

use egui::{vec2, Align2, Button, RichText, ScrollArea, TextEdit, TextStyle};

use crate::storage;
use crate::theme;

use super::*;

impl App {
    pub(super) fn settings_window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if !self.show_settings {
            return;
        }
        let mut open = true;
        let mut changed = false;
        egui::Window::new(RichText::new("Settings").family(theme::bold()).size(18.0))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .min_width(580.0)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_width(580.0);
                let max_h = ctx.content_rect().height() - 140.0;
                ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
                ui.set_width(560.0);
                ui.label(RichText::new("WORKSPACE").family(theme::bold()).size(12.5).color(theme::CYAN));
                ui.horizontal(|ui| {
                    fixed(ui, 130.0, |ui| {
                        ui.label(RichText::new("Base URL").family(theme::bold()));
                    });
                    changed |= ui
                        .add_sized([400.0, 26.0], TextEdit::singleline(&mut self.config.base_url).font(TextStyle::Monospace).hint_text("https://api.example.com"))
                        .on_hover_text("Where relative paths (/users/:id) go. An environment's baseUrl wins over it; a full URL in a request ignores both.")
                        .changed();
                });
                ui.add_space(6.0);
                ui.label(RichText::new("AUTHENTICATION").family(theme::bold()).size(12.5).color(theme::CYAN));
                ui.label(
                    RichText::new("Used by every request and folder whose auth is “Inherit”. Its secrets are kept in the system keychain.")
                        .size(12.5)
                        .color(theme::SILVER),
                );
                ui.add_space(4.0);
                let mut get_token = false;
                changed |= crate::ui_auth::auth_editor(ui, &mut self.config.auth, None, &mut self.show_secret, &mut get_token, "workspace_auth");
                if get_token {
                    actions.push(Action::FetchToken(self.config.auth.clone()));
                }
                ui.horizontal(|ui| {
                    let tokens = self.auth.lock();
                    match &tokens.status {
                        crate::auth::Status::Fetching => {
                            ui.add(egui::Spinner::new().color(theme::CYAN));
                            ui.label(RichText::new("Getting a token…").color(theme::SILVER));
                        }
                        crate::auth::Status::Failed(e) => {
                            ui.add(egui::Label::new(RichText::new(e).color(theme::RED)).wrap());
                        }
                        crate::auth::Status::Ready => {
                            ui.label(RichText::new("✓ Token received").color(theme::GREEN));
                        }
                        crate::auth::Status::Idle => {}
                    }
                });
                if ui.small_button("Forget the tokens").on_hover_text("The next request asks for new ones").clicked() {
                    self.auth.lock().forget();
                }
                ui.separator();
                ui.label(RichText::new("FLOWS").family(theme::bold()).size(12.5).color(theme::CYAN));
                egui::Grid::new("flowcfg").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                    ui.label("Delay between requests");
                    changed |= ui
                        .add(egui::DragValue::new(&mut self.config.flow_delay_ms).range(0..=10_000).speed(10).suffix(" ms"))
                        .on_hover_text("Spacing between the requests of a flow (rate limits)")
                        .changed();
                    ui.end_row();
                    ui.label("Parallel loops: items at once");
                    changed |= ui
                        .add(egui::DragValue::new(&mut self.config.parallel).range(1..=32))
                        .on_hover_text("“For each” blocks set to Parallel run this many items at the same time")
                        .changed();
                    ui.end_row();
                    ui.label("Parallel loops: requests / second");
                    ui.horizontal(|ui| {
                        changed |= ui
                            .add(egui::DragValue::new(&mut self.config.parallel_rate).range(0.0..=50.0).speed(0.1).max_decimals(1).custom_formatter(|v, _| if v == 0.0 { "auto".into() } else { format!("{v:.1}/s") }))
                            .on_hover_text("auto = the limit the API announces for your application (x-secondly-ratelimit-limit, e.g. 2/s or 8/s). Requests refused with 429 are sent again after a pause.")
                            .changed();
                    });
                    ui.end_row();
                    ui.label("Max pages for “Fetch all pages”");
                    changed |= ui.add(egui::DragValue::new(&mut self.config.max_pages).range(1..=1000)).changed();
                    ui.end_row();
                });
                ui.separator();
                changed |= self.ai_settings(ui, actions);
                ui.separator();
                changed |= self.mcp_settings(ui);
                ui.separator();
                changed |= self.mock_settings(ui);
                ui.separator();
                ui.label(RichText::new("DATA").family(theme::bold()).size(12.5).color(theme::CYAN));
                let (nr, nf) = self.workspace.count();
                ui.label(RichText::new(format!("Workspace: {nr} requests, {nf} flows — stored in {}", storage::dir().display())).size(12.5).color(theme::SILVER));
                ui.horizontal(|ui| {
                    if ui.button("Export whole workspace…").on_hover_text("Backup or share everything in one file").clicked() {
                        actions.push(Action::ExportNode(self.workspace.uid));
                    }
                    if ui.button("Import file…").clicked() {
                        actions.push(Action::ImportFile(self.workspace.uid));
                    }
                    if ui.button("Paste JSON…").clicked() {
                        actions.push(Action::ImportPaste(self.workspace.uid));
                    }
                });
                ui.separator();
                ui.label(RichText::new("APPLICATION").family(theme::bold()).size(12.5).color(theme::CYAN));
                match &self.install {
                    None => {
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new().color(theme::CYAN));
                            ui.label(RichText::new("Checking installation…").color(theme::SILVER));
                        });
                    }
                    Some(st) => {
                        ui.label(RichText::new(st.describe()).size(12.5).color(theme::SILVER));
                        let btn = Button::new(RichText::new("Uninstall…").color(theme::WHITE)).fill(theme::RED);
                        if ui.add_enabled(st.installed(), btn).on_hover_text("Remove the app from your applications").clicked() {
                            actions.push(Action::Uninstall);
                        }
                    }
                }
                });
            });
        if changed {
            self.save_config();
            self.apply_workspace_settings();
        }
        self.show_settings = open;
    }
}
