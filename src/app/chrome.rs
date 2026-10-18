//! Title bar, tab strip, the central request / flow area, keyboard shortcuts, toasts.

use std::time::Duration;

use egui::{vec2, Align, Align2, Button, Color32, CornerRadius, CursorIcon, FontId, Frame, Id, Layout, Margin, RichText, ScrollArea, Sense, Stroke, Ui, ViewportCommand};

use crate::model;
use crate::net;
use crate::theme;
use crate::ui_flow::{self, BlockDrop};
use crate::ui_request;
use crate::widgets::*;

use super::*;

impl App {
    pub(super) fn top_bar(&mut self, root: &mut Ui, ctx: &egui::Context, actions: &mut Vec<Action>) {
        egui::Panel::top("top").exact_size(52.0).frame(Frame::new().fill(theme::CODE_BG).inner_margin(Margin { left: 16, right: 6, top: 6, bottom: 6 })).show(
            root,
            |ui| {
                // The whole bar moves the window (registered first: widgets on top keep their clicks).
                let bar = ui.interact(ui.max_rect(), Id::unique("titlebar"), Sense::click_and_drag());
                // Real full screen (whole display, no menu bar / dock), not just a maximized window.
                let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                // F11, or ⌃⌘F on macOS (where F11 shows the desktop by default).
                let shortcut = ctx.input(|i| i.key_pressed(egui::Key::F11) || (i.modifiers.ctrl && i.modifiers.mac_cmd && i.key_pressed(egui::Key::F)));
                if bar.double_clicked() || shortcut {
                    ctx.send_viewport_cmd(ViewportCommand::Fullscreen(!fullscreen));
                } else if bar.drag_started_by(egui::PointerButton::Primary) {
                    ctx.send_viewport_cmd(ViewportCommand::StartDrag);
                }
                ui.horizontal_centered(|ui| {
                    logo(ui, 26.0);
                    ui.label(RichText::new("apifa").family(theme::bold()).size(21.0).color(theme::WHITE));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        let size = vec2(40.0, 30.0);
                        if icon_button(ui, Icon::Close, size, theme::SILVER, theme::RED).on_hover_text("Quit").clicked() {
                            ctx.send_viewport_cmd(ViewportCommand::Close);
                        }
                        let (icon, tip) = if fullscreen { (Icon::Restore, FULLSCREEN_TIPS.1) } else { (Icon::Maximize, FULLSCREEN_TIPS.0) };
                        if icon_button(ui, icon, size, theme::SILVER, theme::NAVY_HOVER).on_hover_text(tip).clicked() {
                            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(!fullscreen));
                        }
                        if icon_button(ui, Icon::Minimize, size, theme::SILVER, theme::NAVY_HOVER).on_hover_text("Minimize").clicked() {
                            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
                        }
                        ui.add_space(14.0);
                        if net::mock_target().is_some() {
                            let badge = Button::new(RichText::new("MOCK").family(theme::bold()).size(13.0).color(theme::WHITE))
                                .fill(theme::ORANGE)
                                .corner_radius(4)
                                .min_size(vec2(52.0, 24.0));
                            if ui.add(badge).on_hover_text("Requests go to the mock server, not to the real API (Settings › Mock server)").clicked() {
                                actions.push(Action::OpenSettings);
                            }
                            ui.add_space(6.0);
                        }
                        // Only once an API key is set (Settings › AI assistant).
                        if self.config.ai.ready() {
                            let on = self.chat.open;
                            let ai_btn = Button::new(RichText::new("AI").family(theme::bold()).size(14.0).color(if on { theme::WHITE } else { theme::CYAN }))
                                .fill(if on { theme::CYAN_DARK } else { theme::NAVY })
                                .stroke(Stroke::new(1.0_f32, theme::CYAN))
                                .corner_radius(4)
                                .min_size(vec2(40.0, 26.0));
                            if ui.add(ai_btn).on_hover_text("AI assistant: ask it to make requests, flows, substitutions").clicked() {
                                actions.push(Action::AiToggle);
                            }
                            ui.add_space(6.0);
                        }
                        let gear = icon_button(ui, Icon::Gear, vec2(34.0, 30.0), theme::CLOUDS, theme::NAVY_HOVER);
                        if gear.on_hover_text("Settings — base URL, authentication, AI, MCP, mock server").clicked() {
                            actions.push(Action::OpenSettings);
                        }
                        ui.add_space(6.0);
                        if self.token_chip(ui).clicked() {
                            // The active profile's auth is set in its editor; the workspace's in Settings.
                            let in_profile = self.profiles.active().is_some_and(|p| !p.auth.is_inherit());
                            actions.push(if in_profile { Action::EditProfiles } else { Action::OpenSettings });
                        }
                        ui.add_space(8.0);
                        self.env_picker(ui, actions);
                        self.profile_picker(ui, actions);
                        // Understand going on in the background: how far, and a way back to the map.
                        if let Some((done, total)) = self.understand_status() {
                            ui.add_space(8.0);
                            if ui.small_button("■").on_hover_text("Stop Understand (the map keeps what was found)").clicked() {
                                self.stop_understand();
                            }
                            let chip = egui::Button::new(RichText::new(format!("Understand {done}/{total}")).size(12.5).color(theme::WHITE))
                                .fill(theme::CYAN_DARK)
                                .corner_radius(4);
                            if ui.add(chip).on_hover_text("Mapping the data in the background: open the live map").clicked() {
                                actions.push(Action::ShowDataMap);
                            }
                            ui.add(egui::Spinner::new().size(14.0).color(theme::CYAN));
                        }
                    });
                });
            },
        );
    }

    /// Compact authentication status of the workspace; clicking it opens the settings.
    pub(super) fn token_chip(&self, ui: &mut Ui) -> egui::Response {
        let tokens = self.auth.lock();
        // The active profile's auth, else the workspace's.
        let auth = self.profiles.active().map(|p| p.auth.or(&self.config.auth).clone()).unwrap_or_else(|| self.config.auth.clone());
        let (text, color) = match (&auth, &tokens.status) {
            (crate::auth::Auth::OAuth2(_), crate::auth::Status::Fetching) => ("Getting a token…".to_owned(), theme::CYAN),
            (crate::auth::Auth::OAuth2(_), crate::auth::Status::Failed(e)) => (format!("Auth error: {}", truncate(e, 40)), theme::RED),
            (crate::auth::Auth::OAuth2(_), _) => match &tokens.last {
                Some((_, Some(at))) => {
                    let left = at.duration_since(std::time::SystemTime::now()).unwrap_or_default().as_secs();
                    ui.ctx().request_repaint_after(Duration::from_secs(30));
                    if left == 0 {
                        ("OAuth 2.0 · token renewed on the next call".to_owned(), theme::ORANGE)
                    } else {
                        (format!("OAuth 2.0 · token {}h{:02}m", left / 3600, (left % 3600) / 60), theme::GREEN)
                    }
                }
                Some((_, None)) => ("OAuth 2.0 · token ready".to_owned(), theme::GREEN),
                None => ("OAuth 2.0 · token on the first call".to_owned(), theme::SILVER),
            },
            (a, _) => (a.label().to_owned(), if matches!(a, crate::auth::Auth::None) { theme::SILVER } else { theme::GREEN }),
        };
        let galley = ui.painter().layout_no_wrap(text, FontId::proportional(12.5), theme::CLOUDS);
        let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 34.0, 26.0), Sense::click());
        let fill = if resp.hovered() { theme::NAVY_HOVER } else { theme::NAVY_DARK };
        ui.painter().rect_filled(rect, CornerRadius::same(13), fill);
        ui.painter().circle_filled(egui::pos2(rect.left() + 14.0, rect.center().y), 4.0, color);
        ui.painter().galley(egui::pos2(rect.left() + 25.0, rect.center().y - galley.size().y / 2.0), galley, theme::CLOUDS);
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        match &tokens.status {
            crate::auth::Status::Failed(e) => resp.on_hover_text(e.clone()),
            _ => resp.on_hover_text("The authentication of the active API profile, else the workspace's (Settings)"),
        }
    }

    /// Without system decorations the window edges must start the resize themselves
    /// (macOS keeps its native, invisible ones).
    pub(super) fn resize_edges(&self, ctx: &egui::Context) {
        use egui::viewport::ResizeDirection as D;
        if cfg!(target_os = "macos") || ctx.input(|i| i.viewport().maximized.unwrap_or(false) || i.viewport().fullscreen.unwrap_or(false)) {
            return;
        }
        let rect = ctx.content_rect();
        let Some(p) = ctx.input(|i| i.pointer.hover_pos()) else { return };
        let m = 6.0;
        let (l, r, t, b) = (p.x <= rect.left() + m, p.x >= rect.right() - m, p.y <= rect.top() + m, p.y >= rect.bottom() - m);
        let (dir, cursor) = match (l, r, t, b) {
            (true, _, true, _) => (D::NorthWest, CursorIcon::ResizeNorthWest),
            (_, true, true, _) => (D::NorthEast, CursorIcon::ResizeNorthEast),
            (true, _, _, true) => (D::SouthWest, CursorIcon::ResizeSouthWest),
            (_, true, _, true) => (D::SouthEast, CursorIcon::ResizeSouthEast),
            (true, ..) => (D::West, CursorIcon::ResizeWest),
            (_, true, ..) => (D::East, CursorIcon::ResizeEast),
            (_, _, true, _) => (D::North, CursorIcon::ResizeNorth),
            (_, _, _, true) => (D::South, CursorIcon::ResizeSouth),
            _ => return,
        };
        ctx.set_cursor_icon(cursor);
        if ctx.input(|i| i.pointer.primary_pressed()) {
            ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
        }
    }

    pub(super) fn central(&mut self, root: &mut Ui, actions: &mut Vec<Action>) {
        egui::CentralPanel::default().frame(Frame::new().fill(theme::NAVY).inner_margin(Margin::same(12))).show(root, |ui| {
            paint_space_background(ui, self.config.animate_background);
            if self.tabs.is_empty() {
                welcome(ui, (self.doc.resources.len(), self.doc.endpoint_count), actions);
                return;
            }
            self.tab_strip(ui, actions);
            if matches!(self.tabs[self.active].body, TabBody::DataMap) {
                let full = ui.available_rect_before_wrap();
                let mut area = child_in(ui, full);
                Frame::new().fill(theme::CODE_BG).corner_radius(CornerRadius { nw: 0, ne: 6, sw: 6, se: 6 }).inner_margin(Margin::same(14)).show(&mut area, |ui| {
                    ui.set_min_size(ui.available_size());
                    let ctx = ui.ctx().clone();
                    self.datamap_tab(ui, &ctx);
                });
                ui.advance_cursor_after_rect(full);
                return;
            }
            let doc = self.doc.clone();
            let idx = self.active;
            let tab_id = TabId(self.tabs[idx].id);
            let token_hint = self.auth.lock().last.as_ref().map(|(t, _)| mask(t));
            let full = ui.available_rect_before_wrap();
            let (top, bottom) = splitter(ui, full, &mut self.split, Id::unique("split"), 220.0, 110.0);
            let fav = self.tab_favorite(idx).map(|f| self.favorites.contains(&f));
            let library = self.workspace.flows();
            let tab = &mut self.tabs[idx];
            let saved = tab.saved.is_some();

            let mut card = child_in(ui, top);
            Frame::new().fill(theme::WHITE).corner_radius(CornerRadius { nw: 0, ne: 6, sw: 6, se: 6 }).inner_margin(Margin::same(16)).show(&mut card, |ui| {
                ui.style_mut().visuals = theme::card_visuals();
                ui.set_min_size(ui.available_size());
                match &mut tab.body {
                    TabBody::Request(st) => {
                        let method = st.spec.doc.as_ref().and_then(|d| doc.lookup(d));
                        ui_request::request_ui(ui, method, &doc, &mut tab.title, st, tab_id, saved, fav, actions, token_hint.as_deref());
                    }
                    TabBody::Flow(st) => {
                        ui_flow::flow_ui(ui, &doc, &library, st, tab_id, saved, fav, actions);
                        tab.title = st.flow.name.clone();
                    }
                    TabBody::DataMap => {}
                }
            });

            let mut lower = child_in(ui, bottom);
            Frame::new().fill(theme::CODE_BG).corner_radius(6).inner_margin(Margin::same(12)).show(&mut lower, |ui| {
                ui.set_min_size(ui.available_size());
                match &mut tab.body {
                    TabBody::Request(st) => ui_request::response_ui(ui, st, tab_id, actions),
                    TabBody::Flow(st) => ui_flow::console_ui(ui, st, tab_id, actions),
                    TabBody::DataMap => {}
                }
            });
            ui.advance_cursor_after_rect(full);
        });
    }

    pub(super) fn tab_strip(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let (can_back, can_forward) = (self.can_nav(&self.nav.back), self.can_nav(&self.nav.forward));
        let tab_ids: Vec<TabId> = self.tabs.iter().map(|t| TabId(t.id)).collect();
        ScrollArea::horizontal().id_salt("tabs").max_height(36.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (icon, enabled, action, tip) in
                    [(Icon::Back, can_back, Action::NavBack, "Back to the previous tab (Alt+←)"), (Icon::Forward, can_forward, Action::NavForward, "Forward (Alt+→)")]
                {
                    let color = if enabled { theme::CLOUDS } else { Color32::from_white_alpha(70) };
                    let resp = ui.add_enabled_ui(enabled, |ui| icon_button(ui, icon, vec2(28.0, 30.0), color, theme::NAVY_HOVER)).inner;
                    if resp.on_hover_text(tip).clicked() {
                        actions.push(action);
                    }
                }
                ui.add_space(6.0);
                for (i, t) in self.tabs.iter().enumerate() {
                    let active = i == self.active;
                    let (badge, badge_col) = t.badge();
                    let text_col = if active { theme::NAVY } else { theme::CLOUDS };
                    let badge_g = ui.painter().layout_no_wrap(badge.to_owned(), FontId::new(11.0, theme::bold()), badge_col);
                    let title_g = ui.painter().layout_no_wrap(truncate(&t.title, 30), FontId::proportional(14.0), text_col);
                    let busy_w = if t.busy() { 14.0 } else { 0.0 };
                    let w = 10.0 + badge_g.size().x + 7.0 + title_g.size().x + busy_w + 30.0;
                    let (rect, resp) = ui.allocate_exact_size(vec2(w, 34.0), Sense::click());
                    let close_rect = egui::Rect::from_center_size(egui::pos2(rect.right() - 15.0, rect.center().y), vec2(20.0, 20.0));
                    // Registered after the tab itself, so it wins the click.
                    let close = ui.interact(close_rect, ui.scope_id().with(("close", t.id)), Sense::click());
                    let p = ui.painter();
                    let fill = if active {
                        theme::WHITE
                    } else if resp.hovered() {
                        theme::NAVY_HOVER
                    } else {
                        theme::NAVY_DARK
                    };
                    p.rect_filled(rect, CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, fill);
                    let mut x = rect.left() + 10.0;
                    p.galley(egui::pos2(x, rect.center().y - badge_g.size().y / 2.0), badge_g.clone(), badge_col);
                    x += badge_g.size().x + 7.0;
                    p.galley(egui::pos2(x, rect.center().y - title_g.size().y / 2.0), title_g.clone(), text_col);
                    x += title_g.size().x;
                    if t.busy() {
                        p.circle_filled(egui::pos2(x + 8.0, rect.center().y), 3.5, theme::CYAN);
                        ui.ctx().request_repaint_after(Duration::from_millis(250));
                    }
                    if close.hovered() {
                        p.rect_filled(close_rect, CornerRadius::same(3), if active { theme::CLOUDS } else { theme::NAVY_DEEP });
                    }
                    let close_col = if close.hovered() { theme::RED } else { theme::CONCRETE };
                    paint_icon(p, close_rect.shrink(3.0), Icon::Close, close_col);
                    if close.clicked() || resp.middle_clicked() {
                        actions.push(Action::Close(TabId(t.id)));
                    } else if resp.clicked() {
                        actions.push(Action::Select(TabId(t.id)));
                    }
                    close.on_hover_text("Close (Ctrl+W)");
                    let tip = match &t.body {
                        TabBody::Request(r) => format!("{} {}", r.spec.verb, r.spec.path),
                        TabBody::Flow(f) => format!("Flow · {} blocks", f.flow.blocks.len()),
                        TabBody::DataMap => "Entities and links found in GET responses".into(),
                    };
                    let n = self.tabs.len();
                    resp.on_hover_text(tip).context_menu(|ui| {
                        let mut item = |ui: &mut Ui, label: &str, enabled: bool, action: Action| {
                            if ui.add_enabled(enabled, Button::new(label)).clicked() {
                                actions.push(action);
                                ui.close();
                            }
                        };
                        let ids = |r: std::ops::Range<usize>| r.filter_map(|j| tab_ids.get(j).copied()).collect::<Vec<_>>();
                        item(ui, "Close", true, Action::Close(TabId(t.id)));
                        item(ui, "Close other tabs", n > 1, Action::CloseMany(tab_ids.iter().copied().filter(|&j| j != TabId(t.id)).collect()));
                        item(ui, "Close tabs to the right", i + 1 < n, Action::CloseMany(ids(i + 1..n)));
                        item(ui, "Close tabs to the left", i > 0, Action::CloseMany(ids(0..i)));
                        item(ui, "Close all tabs", true, Action::CloseMany(tab_ids.clone()));
                    });
                }
                ui.add_space(4.0);
                ui.menu_button(RichText::new("+").size(18.0).color(theme::WHITE), |ui| {
                    if ui.button("New request").clicked() {
                        actions.push(Action::NewTab);
                        ui.close();
                    }
                    if ui.button("New flow").clicked() {
                        actions.push(Action::NewFlow(None));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("New WebSocket").clicked() {
                        actions.push(Action::NewLive(crate::model::Protocol::WebSocket));
                        ui.close();
                    }
                    if ui.button("New gRPC").clicked() {
                        actions.push(Action::NewLive(crate::model::Protocol::Grpc));
                        ui.close();
                    }
                })
                .response
                .on_hover_text("New request (Ctrl+T), flow, WebSocket or gRPC");
            });
        });
    }

    pub(super) fn code_window(&mut self, ctx: &egui::Context) {
        if !self.show_code {
            return;
        }
        let Some(TabBody::Request(st)) = self.tabs.get(self.active).map(|t| &t.body) else {
            self.show_code = false;
            return;
        };
        let method = st.spec.doc.as_ref().and_then(|d| self.doc.lookup(d)).map(|(_, m)| m);
        let snippet = model::curl_snippet(&model::prepare(&st.spec, method));
        let mut open = true;
        egui::Window::new(RichText::new("Code snippet · cURL").family(theme::bold()))
            .open(&mut open)
            .collapsible(false)
            .default_width(720.0)
            .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(RichText::new("Run the same request from a terminal. Your SECRET is never written in the snippet.").color(theme::SILVER));
                ui.add_space(4.0);
                code_block(ui, RichText::new(&snippet).monospace().color(theme::CLOUDS).into());
                ui.add_space(4.0);
                if ui.button("Copy").clicked() {
                    ui.ctx().copy_text(snippet.clone());
                }
            });
        self.show_code = open;
    }

    pub(super) fn toasts_ui(&self, ctx: &egui::Context) {
        if self.toasts.is_empty() {
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(500));
        egui::Area::new(Id::unique("toasts")).anchor(Align2::RIGHT_BOTTOM, vec2(-18.0, -18.0)).show(ctx, |ui| {
            for t in &self.toasts {
                Frame::new().fill(theme::CODE_BG).stroke(Stroke::new(1.0_f32, t.color)).corner_radius(6).inner_margin(Margin::symmetric(14, 8)).show(ui, |ui| {
                    ui.set_max_width(420.0);
                    ui.add(egui::Label::new(RichText::new(&t.text).color(theme::WHITE)).wrap());
                });
                ui.add_space(6.0);
            }
        });
    }

    pub(super) fn shortcuts(&self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        if self.dialog.is_some() {
            return;
        }
        ctx.input_mut(|i| {
            if i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter) && !self.tabs.is_empty() {
                let id = TabId(self.tabs[self.active].id);
                match self.tabs[self.active].body {
                    TabBody::Request(_) => actions.push(Action::Send(id)),
                    TabBody::Flow(_) => actions.push(Action::RunFlow(id)),
                    TabBody::DataMap => {}
                }
            }
            if i.consume_key(egui::Modifiers::COMMAND, egui::Key::S) && !self.tabs.is_empty() {
                actions.push(Action::SaveTab(TabId(self.tabs[self.active].id)));
            }
            if i.consume_key(egui::Modifiers::ALT, egui::Key::ArrowLeft) {
                actions.push(Action::NavBack);
            }
            if i.consume_key(egui::Modifiers::ALT, egui::Key::ArrowRight) {
                actions.push(Action::NavForward);
            }
            if i.consume_key(egui::Modifiers::COMMAND, egui::Key::T) {
                actions.push(Action::NewTab);
            }
            if i.consume_key(egui::Modifiers::COMMAND, egui::Key::W) && !self.tabs.is_empty() {
                actions.push(Action::Close(TabId(self.tabs[self.active].id)));
            }
        });
    }

    /// While an endpoint or a saved request is dragged from the sidebar: what it is, next to the pointer.
    pub(super) fn drag_preview(&self, ctx: &egui::Context) {
        let Some(payload) = egui::DragAndDrop::payload::<BlockDrop>(ctx) else { return };
        let Some(pos) = ctx.pointer_interact_pos() else { return };
        let on_flow = matches!(self.tabs.get(self.active).map(|t| &t.body), Some(TabBody::Flow(_)));
        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        egui::Area::new(Id::unique("block_drop_preview")).order(egui::Order::Tooltip).fixed_pos(pos + egui::vec2(16.0, 12.0)).interactable(false).show(ctx, |ui| {
            Frame::popup(&ctx.global_style()).show(ui, |ui| {
                ui.label(RichText::new(payload.label(&self.doc)).monospace().color(theme::WHITE));
                let hint = if on_flow { "Drop it on a “Drop here” zone of the flow" } else { "Open a flow tab to drop it there" };
                ui.label(RichText::new(hint).size(11.5).color(theme::SILVER));
            });
        });
    }
}
