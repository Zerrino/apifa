//! Small reusable widgets and helpers shared by the request and flow editors.

use egui::{
    vec2, Align, Align2, Button, Color32, CornerRadius, CursorIcon, FontId, Frame, Id, Label, Layout, Margin, Rect, RichText, ScrollArea, Sense, Stroke, TextEdit,
    TextStyle, Ui,
};

use crate::app::{Action, TabId};
use crate::docs::Param;
use crate::model::Kv;
use crate::theme;

/// The first screen: what apifa does, how to start.
/// `counts`: (resources, endpoints) of the imported APIs.
pub fn welcome(ui: &mut Ui, counts: (usize, usize), actions: &mut Vec<Action>) {
    ui.vertical_centered(|ui| {
        ui.add_space(ui.available_height() * 0.10);
        logo(ui, 84.0);
        ui.add_space(10.0);
        ui.label(RichText::new("apifa").family(theme::bold()).size(40.0).color(theme::WHITE));
        ui.label(RichText::new("API For All").size(18.0).color(theme::CYAN));
        ui.add_space(8.0);
        ui.label(
            RichText::new("HTTP · GraphQL · WebSocket · gRPC — collections, environments, scripts, flows and tests, on your computer.").size(15.0).color(theme::SILVER),
        );
        if counts.1 > 0 {
            ui.add_space(6.0);
            ui.label(RichText::new(format!("{} endpoints in {} resources from your APIs", counts.1, counts.0)).size(13.0).color(theme::CONCRETE));
        }
        ui.add_space(24.0);
        Frame::new().fill(theme::WHITE).corner_radius(8).inner_margin(Margin::same(20)).show(ui, |ui| {
            ui.style_mut().visuals = theme::card_visuals();
            ui.set_max_width(600.0);
            ui.label(RichText::new("Get started").family(theme::bold()).size(18.0));
            ui.add_space(4.0);
            for (i, step) in [
                "Import an API definition (OpenAPI / Swagger) — sidebar › Endpoints › APIs — or just type a URL in a new request.",
                "Set where requests go and how they authenticate: ⚙ Settings (base URL, auth), or environments (dev, prod…) in the title bar.",
                "Send, save it in your workspace, add scripts and tests, chain requests in flows.",
            ]
            .iter()
            .enumerate()
            {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(format!("{}.", i + 1)).family(theme::bold()).color(theme::CYAN));
                    ui.label(*step);
                });
            }
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                let primary = Button::new(RichText::new("New HTTP request").family(theme::bold()).color(theme::WHITE)).fill(theme::CYAN);
                if ui.add(primary).clicked() {
                    actions.push(Action::NewTab);
                }
                if ui.button("New WebSocket").clicked() {
                    actions.push(Action::NewLive(crate::model::Protocol::WebSocket));
                }
                if ui.button("New gRPC").clicked() {
                    actions.push(Action::NewLive(crate::model::Protocol::Grpc));
                }
                if ui.button("Import an API…").clicked() {
                    actions.push(Action::ImportApiFile);
                }
            });
        });
    });
}

/// The chrome's background: a quiet graphite gradient (no animation, nothing to repaint).
pub fn paint_space_background(ui: &Ui, _animate: bool) {
    let rect = ui.max_rect().expand(12.0);
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), theme::NAVY_DEEP);
    mesh.colored_vertex(rect.right_top(), theme::NAVY_DEEP);
    mesh.colored_vertex(rect.right_bottom(), theme::NAVY_DARK);
    mesh.colored_vertex(rect.left_bottom(), theme::NAVY_DARK);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    ui.painter().add(egui::Shape::mesh(mesh));
}

/// The apifa logo, drawn as vectors: a rounded violet square with `‹·›` — two chevrons around a dot,
/// what goes in and out of an API.
pub fn paint_logo(painter: &egui::Painter, rect: Rect) {
    let side = rect.width().min(rect.height());
    let r = Rect::from_center_size(rect.center(), vec2(side, side));
    painter.rect_filled(r, CornerRadius::same((side * 0.24) as u8), theme::CYAN);
    let c = r.center();
    let u = side / 10.0;
    let stroke = Stroke::new(u * 0.95, theme::WHITE);
    let left = [c + vec2(-1.4 * u, -2.2 * u), c + vec2(-3.4 * u, 0.0), c + vec2(-1.4 * u, 2.2 * u)];
    let right = [c + vec2(1.4 * u, -2.2 * u), c + vec2(3.4 * u, 0.0), c + vec2(1.4 * u, 2.2 * u)];
    painter.add(egui::Shape::line(left.to_vec(), stroke));
    painter.add(egui::Shape::line(right.to_vec(), stroke));
    painter.circle_filled(c, u * 0.75, theme::WHITE);
}

/// Allocates a square of `size` and draws the logo in it.
pub fn logo(ui: &mut Ui, size: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::hover());
    paint_logo(ui.painter(), rect);
    resp
}

pub fn endpoint_row(ui: &mut Ui, verb: &str, path: &str, selected: bool, deprecated: bool) -> egui::Response {
    list_row(ui, verb, path, FontId::monospace(12.0), selected, deprecated)
}

/// Workspace item: badge + human name.
pub fn named_row(ui: &mut Ui, badge: &str, name: &str, selected: bool) -> egui::Response {
    list_row(ui, badge, name, FontId::proportional(13.5), selected, false)
}

fn list_row(ui: &mut Ui, verb: &str, text: &str, font: FontId, selected: bool, dimmed: bool) -> egui::Response {
    let w = ui.available_width();
    // Draggable too: requests and endpoints can be dropped on a flow.
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 22.0), Sense::click_and_drag());
    if ui.is_rect_visible(rect) {
        let p = ui.painter_at(rect);
        if selected {
            p.rect_filled(rect, CornerRadius::same(3), theme::CYAN.gamma_multiply(0.22));
            p.rect_filled(Rect::from_min_size(rect.min, vec2(3.0, rect.height())), CornerRadius::ZERO, theme::CYAN);
        } else if resp.hovered() {
            p.rect_filled(rect, CornerRadius::same(3), Color32::from_white_alpha(12));
        }
        let c = rect.left_center();
        p.text(c + vec2(8.0, 0.0), Align2::LEFT_CENTER, theme::short_verb(verb), FontId::new(11.0, theme::bold()), theme::verb_color(verb));
        let col = if dimmed {
            theme::CONCRETE
        } else if selected || resp.hovered() {
            theme::WHITE
        } else {
            theme::SILVER
        };
        p.text(c + vec2(58.0, 0.0), Align2::LEFT_CENTER, text, font, col);
    }
    resp
}

pub fn segment<T: PartialEq + Copy>(ui: &mut Ui, cur: &mut T, val: T, text: &str) {
    let sel = *cur == val;
    let btn = Button::new(RichText::new(text).family(theme::bold()).color(if sel { theme::WHITE } else { theme::SILVER }))
        .fill(if sel { theme::CYAN } else { theme::NAVY_DARK })
        .corner_radius(3);
    if ui.add(btn).clicked() {
        *cur = val;
    }
}

pub fn subtab<T: PartialEq + Copy>(ui: &mut Ui, cur: &mut T, val: T, text: &str) {
    let sel = *cur == val;
    let rt = RichText::new(text).size(14.5).color(if sel { theme::NAVY } else { theme::MUTED });
    let rt = if sel { rt.family(theme::bold()) } else { rt };
    let r = ui.add(Label::new(rt).sense(Sense::click()).selectable(false));
    if sel {
        ui.painter().hline(r.rect.x_range(), r.rect.bottom() + 4.0, Stroke::new(2.5_f32, theme::CYAN));
    }
    if r.clicked() {
        *cur = val;
    }
    ui.add_space(12.0);
}

pub fn dark_subtab<T: PartialEq + Copy>(ui: &mut Ui, cur: &mut T, val: T, text: &str) {
    let sel = *cur == val;
    let rt = RichText::new(text).size(14.0).color(if sel { theme::WHITE } else { theme::CONCRETE });
    let rt = if sel { rt.family(theme::bold()) } else { rt };
    let r = ui.add(Label::new(rt).sense(Sense::click()).selectable(false));
    if sel {
        ui.painter().hline(r.rect.x_range(), r.rect.bottom() + 3.0, Stroke::new(2.0_f32, theme::CYAN));
    }
    if r.clicked() {
        *cur = val;
    }
    ui.add_space(10.0);
}

pub fn pill(ui: &mut Ui, text: &str, fg: Color32, bg: Color32) {
    Frame::new().fill(bg).corner_radius(9).inner_margin(Margin::symmetric(7, 1)).show(ui, |ui| {
        ui.label(RichText::new(text).size(10.5).family(theme::bold()).color(fg));
    });
}

pub fn type_pill(ui: &mut Ui, p: &Param) {
    Frame::new()
        .stroke(Stroke::new(1.0_f32, theme::CYAN))
        .corner_radius(9)
        .inner_margin(Margin::symmetric(7, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(p.kind.label()).size(10.5).color(theme::CYAN_DARK));
        })
        .response
        .on_hover_text(&p.validator);
    if p.allow_nil {
        ui.label(RichText::new("nullable").size(10.5).color(theme::MUTED));
    }
}

pub fn stat(ui: &mut Ui, label: &str, value: &str) {
    ui.label(RichText::new(label).size(12.0).color(theme::CONCRETE));
    ui.label(RichText::new(value).size(13.0).family(theme::bold()).color(theme::GREEN));
}

pub fn section(ui: &mut Ui, title: &str, help: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(title.to_uppercase()).family(theme::bold()).size(12.5).color(theme::NAVY));
    ui.label(RichText::new(help).size(12.0).color(theme::MUTED));
    ui.add_space(2.0);
}

pub fn group_title(ui: &mut Ui, title: &str, color: Color32) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(4.0, 16.0), Sense::hover());
        ui.painter().rect_filled(r, CornerRadius::same(2), color);
        ui.label(RichText::new(title).family(theme::bold()).size(14.0).color(color));
    });
    ui.add_space(2.0);
}

pub fn row_frame(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    Frame::new().fill(theme::ROW).corner_radius(4).inner_margin(Margin::symmetric(10, 6)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui);
    });
    ui.add_space(2.0);
}

pub fn fixed(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui)) {
    ui.allocate_ui_with_layout(vec2(width, 22.0), Layout::left_to_right(Align::Center), |ui| {
        ui.set_width(width);
        add(ui);
    });
}

pub fn description(ui: &mut Ui, text: &str) {
    ui.add(Label::new(RichText::new(text).size(12.5).color(theme::MUTED)).wrap());
}

pub fn notice(ui: &mut Ui, color: Color32, text: &str) {
    Frame::new().fill(color.gamma_multiply(0.12)).stroke(Stroke::new(1.0_f32, color)).corner_radius(4).inner_margin(Margin::symmetric(10, 4)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.add(Label::new(RichText::new(text).color(color.gamma_multiply(0.9)).size(12.5)).wrap());
    });
}

pub fn code_block(ui: &mut Ui, text: egui::WidgetText) {
    Frame::new().fill(theme::CODE_BG).corner_radius(4).inner_margin(Margin::same(10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ScrollArea::horizontal().id_salt(ui.next_auto_id()).show(ui, |ui| {
            ui.add(Label::new(text).selectable(true).extend());
        });
    });
}

pub fn kv_editor(ui: &mut Ui, rows: &mut Vec<Kv>, salt: impl std::hash::Hash + std::fmt::Debug) {
    let base = Id::unique(salt);
    let mut remove = None;
    if !rows.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(26.0);
            fixed(ui, 228.0, |ui| {
                ui.label(RichText::new("Key").family(theme::bold()));
            });
            ui.label(RichText::new("Value").family(theme::bold()));
        });
    }
    for (i, r) in rows.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.checkbox(&mut r.enabled, "").on_hover_text("Enabled");
            ui.add_sized([220.0, 24.0], TextEdit::singleline(&mut r.key).id(base.with(("k", i))).font(TextStyle::Monospace).hint_text(hint_rt("key")));
            let date = is_date_key(&r.key);
            let hint = if date { "2026-10-15T09:00:00Z or {{now}}" } else { "value" };
            ui.add_sized([340.0, 24.0], TextEdit::singleline(&mut r.value).id(base.with(("v", i))).font(TextStyle::Monospace).hint_text(hint_rt(hint)));
            if date {
                let range = r.key.trim().starts_with("range[");
                crate::ui_date::date_field(ui, &mut r.value, ui.scope_id().with(("kvdate", i)), range, true);
            }
            subst_badge(ui, &r.key, &r.value);
            if ui.small_button("×").on_hover_text("Remove").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        rows.remove(i);
    }
    if ui.button("+ Add").clicked() {
        rows.push(Kv { enabled: true, ..Default::default() });
    }
}

/// Inside a ComboBox popup opened from the white card: egui gives the popup the card's frame but the
/// app's dark text colors, so the choices would be light grey on white.
pub fn card_popup(ui: &mut Ui) {
    ui.style_mut().visuals = theme::card_visuals();
}

/// Where each line of `text` starts (byte offsets).
pub fn line_starts(text: &str) -> Vec<usize> {
    std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).filter(|&i| i <= text.len()).collect()
}

/// Longest part of one line that is drawn (a minified JSON body is a single huge line).
const MAX_LINE: usize = 20_000;

/// A long text (a response, variables) shown line by line: only the lines in view are laid out, so a 6 MB
/// body costs about what a short one does. `json` colours the lines; `marks` are the byte offsets of the search
/// matches (`mark_len` long, `current` stands out); `scroll_to` brings a byte offset into view.
pub struct CodeView<'a> {
    pub text: &'a str,
    pub lines: &'a [usize],
    pub json: bool,
    pub marks: &'a [usize],
    pub mark_len: usize,
    pub current: usize,
    pub scroll_to: Option<usize>,
}

pub fn code_view(ui: &mut Ui, salt: impl std::hash::Hash + std::fmt::Debug, v: CodeView) {
    let font = FontId::monospace(13.0);
    let row_h = ui.ctx().fonts_mut(|f| f.row_height(&font));
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        // Each row exactly `row_h` tall, as `show_rows` counts them (else rows were 24 px apart: wrong scrollbar, jumps).
        ui.spacing_mut().interact_size.y = row_h;
        let mut area = ScrollArea::both().id_salt(salt).auto_shrink([false, false]);
        if let Some(byte) = v.scroll_to {
            let line = v.lines.partition_point(|&s| s <= byte).saturating_sub(1);
            area = area.vertical_scroll_offset((line as f32 * row_h - ui.available_height() / 2.0).max(0.0));
        }
        area.show_rows(ui, row_h, v.lines.len(), |ui, rows| {
            for i in rows {
                let start = v.lines[i];
                let mut end = v.lines.get(i + 1).map(|e| e - 1).unwrap_or(v.text.len()).max(start);
                if v.text[..end].ends_with('\r') {
                    end -= 1;
                }
                let mut shown_end = end.min(start + MAX_LINE);
                while !v.text.is_char_boundary(shown_end) {
                    shown_end -= 1;
                }
                let line = &v.text[start..shown_end];
                let mut job = if v.json { crate::json_view::highlight(line) } else { crate::json_view::plain(line, f32::INFINITY) };
                job.wrap.max_width = f32::INFINITY;
                // The search matches inside this line.
                let first = v.marks.partition_point(|&m| m + v.mark_len <= start);
                let ranges: Vec<(usize, usize, bool)> = v.marks[first..]
                    .iter()
                    .enumerate()
                    .take_while(|(_, &m)| m < shown_end)
                    .map(|(k, &m)| (m.max(start) - start, (m + v.mark_len).min(shown_end) - start, first + k == v.current))
                    .filter(|(a, b, _)| a < b)
                    .collect();
                if !ranges.is_empty() {
                    job = crate::json_view::mark_ranges(&job, &ranges);
                }
                ui.horizontal(|ui| {
                    ui.add(Label::new(job).selectable(true).extend());
                    if shown_end < end {
                        ui.label(RichText::new(format!("… +{} characters (Copy for everything)", end - shown_end)).size(11.0).color(theme::CONCRETE));
                    }
                });
            }
        });
    });
}

/// When a substitution will replace what was typed in `key` (a login in user_id…): a small hint after the field.
pub fn subst_badge(ui: &mut Ui, key: &str, value: &str) {
    let rules = crate::subst::rules_in(ui.ctx());
    if let Some(rule) = crate::subst::rule_for(&rules, key, value) {
        ui.label(RichText::new(format!("→ {}", rule.name)).size(11.5).color(theme::ORANGE)).on_hover_text(format!(
            "“{}” is looked up before sending:\n{}",
            value.trim(),
            rule.summary()
        ));
    }
}

/// "Browse…" button that puts a picked local file's path in `path`.
pub fn browse_file(ui: &mut Ui, path: &mut String) {
    if ui.button("Browse…").on_hover_text("Pick a file on this computer").clicked() {
        let mut dialog = rfd::FileDialog::new();
        if let Some(dir) = crate::model::expand_home(path).parent().filter(|d| d.is_dir()) {
            dialog = dialog.set_directory(dir);
        }
        if let Some(picked) = dialog.pick_file() {
            *path = picked.display().to_string();
        }
    }
}

/// Files to upload: field name + local path (multipart/form-data).
pub fn files_editor(ui: &mut Ui, rows: &mut Vec<Kv>, path_hint: &str) {
    let mut remove = None;
    if !rows.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(26.0);
            fixed(ui, 168.0, |ui| {
                ui.label(RichText::new("Field").family(theme::bold()));
            });
            ui.label(RichText::new("File path").family(theme::bold()));
        });
    }
    for (i, r) in rows.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.checkbox(&mut r.enabled, "").on_hover_text("Enabled");
            ui.add_sized([160.0, 24.0], TextEdit::singleline(&mut r.key).font(TextStyle::Monospace).hint_text(hint_rt("user[image]")));
            ui.add_sized([340.0, 24.0], TextEdit::singleline(&mut r.value).font(TextStyle::Monospace).hint_text(hint_rt(path_hint)));
            browse_file(ui, &mut r.value);
            if ui.small_button("×").on_hover_text("Remove").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        rows.remove(i);
    }
    if ui.button("+ Add file").clicked() {
        rows.push(Kv { enabled: true, key: if rows.is_empty() { "user[image]".into() } else { String::new() }, value: String::new() });
    }
}

/// `begin_at`, `range[created_at]`, `filter[updated_at]`, `date`… look like dates.
pub fn is_date_key(key: &str) -> bool {
    let last = key.trim().trim_end_matches(']').rsplit('[').next().unwrap_or_default().to_lowercase();
    last.ends_with("_at") || last.contains("date") || last == "since" || last == "until"
}

pub fn has_param(params: &[Param], full_name: &str) -> bool {
    params.iter().any(|p| p.full_name == full_name || has_param(&p.children, full_name))
}

pub fn with_count(label: &str, n: usize) -> String {
    if n == 0 {
        label.to_owned()
    } else {
        format!("{label} ({n})")
    }
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

pub fn first_lines(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut out = lines.iter().take(n).copied().collect::<Vec<_>>().join(" ");
    if lines.len() > n {
        out.push_str(" …");
    }
    truncate(&out, 260)
}

pub fn cap(s: &str) -> String {
    if s.len() > 20_000 {
        let mut end = 20_000;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}\n… (truncated)", &s[..end])
    } else {
        s.to_owned()
    }
}

pub fn mask(token: &str) -> String {
    let head: String = token.chars().take(6).collect();
    format!("{head}••••••••")
}

pub fn human_size(n: usize) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / 1024.0 / 1024.0)
    }
}

pub fn hint_rt(text: impl Into<String>) -> RichText {
    RichText::new(text).color(theme::SILVER)
}

/// Splits `full` vertically with a draggable handle. `frac` is the bottom part's share, always kept valid.
pub fn splitter(ui: &mut Ui, full: Rect, frac: &mut f32, id: Id, min_top: f32, min_bottom: f32) -> (Rect, Rect) {
    const HANDLE: f32 = 10.0;
    let h = full.height();
    let max_top = (h - min_bottom - HANDLE).max(min_top.min(h));
    let lo = min_top.min(max_top);
    let mut top_h = (h * (1.0 - *frac)).clamp(lo, max_top);
    let handle = Rect::from_min_size(egui::pos2(full.left(), full.top() + top_h), vec2(full.width(), HANDLE));
    let resp = ui.interact(handle, id, Sense::drag());
    if resp.dragged() {
        top_h = (top_h + resp.drag_delta().y).clamp(lo, max_top);
    }
    if h > 0.0 {
        *frac = (1.0 - top_h / h).clamp(0.05, 0.95);
    }
    let active = resp.hovered() || resp.dragged();
    if active {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
    }
    let handle = Rect::from_min_size(egui::pos2(full.left(), full.top() + top_h), vec2(full.width(), HANDLE));
    let grip = Rect::from_center_size(handle.center(), vec2(56.0, 4.0));
    ui.painter().rect_filled(grip, CornerRadius::same(2), if active { theme::CYAN } else { Color32::from_white_alpha(70) });
    (Rect::from_min_max(full.min, egui::pos2(full.right(), handle.top())), Rect::from_min_max(egui::pos2(full.left(), handle.bottom()), full.max))
}

/// A child Ui confined (and clipped) to `rect`.
pub fn child_in(ui: &mut Ui, rect: Rect) -> Ui {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)));
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    child
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Minimize,
    Maximize,
    Restore,
    Close,
    Plus,
    Gear,
    Back,
    Forward,
}

/// Icons drawn with lines, so they never depend on a font glyph.
pub fn paint_icon(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let c = rect.center();
    let s = Stroke::new(1.6_f32, color);
    let r = 5.0;
    match icon {
        Icon::Minimize => {
            painter.line_segment([c + vec2(-r, 0.0), c + vec2(r, 0.0)], s);
        }
        Icon::Maximize => {
            painter.rect_stroke(Rect::from_center_size(c, vec2(2.0 * r, 2.0 * r)), CornerRadius::same(1), s, egui::StrokeKind::Middle);
        }
        Icon::Restore => {
            painter.rect_stroke(Rect::from_center_size(c + vec2(-1.5, 1.5), vec2(8.0, 8.0)), CornerRadius::same(1), s, egui::StrokeKind::Middle);
            painter.line_segment([c + vec2(-1.0, -5.5), c + vec2(5.5, -5.5)], s);
            painter.line_segment([c + vec2(5.5, -5.5), c + vec2(5.5, 1.0)], s);
        }
        Icon::Plus => {
            painter.line_segment([c + vec2(-r, 0.0), c + vec2(r, 0.0)], s);
            painter.line_segment([c + vec2(0.0, -r), c + vec2(0.0, r)], s);
        }
        Icon::Back | Icon::Forward => {
            let d = if icon == Icon::Back { -1.0 } else { 1.0 };
            painter.line_segment([c + vec2(-r * d, 0.0), c + vec2(r * d, 0.0)], s);
            painter.line_segment([c + vec2(r * d, 0.0), c + vec2(0.0, -r)], s);
            painter.line_segment([c + vec2(r * d, 0.0), c + vec2(0.0, r)], s);
        }
        Icon::Close => {
            painter.line_segment([c + vec2(-r, -r), c + vec2(r, r)], s);
            painter.line_segment([c + vec2(-r, r), c + vec2(r, -r)], s);
        }
        Icon::Gear => {
            painter.circle_stroke(c, 3.2, s);
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::TAU / 8.0;
                let d = vec2(a.cos(), a.sin());
                painter.line_segment([c + d * 5.0, c + d * 7.5], Stroke::new(2.2_f32, color));
            }
        }
    }
}

/// A flat square button showing a painted icon.
pub fn icon_button(ui: &mut Ui, icon: Icon, size: egui::Vec2, color: Color32, hover_fill: Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(3), hover_fill);
    }
    let col = if resp.hovered() { theme::WHITE } else { color };
    paint_icon(ui.painter(), rect, icon, col);
    resp
}

/// ★ / ☆ toggle for the item shown in a tab (`None`: nothing to star yet).
pub fn star_button(ui: &mut Ui, fav: Option<bool>, tab_id: TabId, actions: &mut Vec<Action>) {
    let (text, color, tip) = match fav {
        Some(true) => ("★", theme::YELLOW, "Remove from favorites"),
        Some(false) => ("☆", theme::MUTED, "Add to favorites"),
        None => ("☆", theme::SILVER, "Save it in the workspace to add it to favorites"),
    };
    let resp = ui.add(Button::new(RichText::new(text).size(18.0).color(color)).frame(false)).on_hover_text(tip);
    if resp.clicked() {
        actions.push(Action::ToggleFavoriteTab(tab_id));
    }
}

/// "−" / "+" buttons: `Some(false)` = collapse every folder, `Some(true)` = expand them all.
/// What's left on the current line of a wrapped row (there `available_width` is the whole width).
pub fn left_on_line(ui: &Ui) -> f32 {
    ui.max_rect().right() - ui.cursor().left()
}

/// The − / + buttons at the right end of a wrapped toolbar row, on a row of their own when
/// the toolbar left them too little room (they were drawn over its last button).
pub fn fold_row(ui: &mut Ui) -> Option<bool> {
    // Two 22 px buttons, the spacing between them and before them.
    let needed = 2.0 * 22.0 + 2.0 * ui.spacing().item_spacing.x + 4.0;
    if left_on_line(ui) < needed {
        ui.end_row();
    }
    ui.with_layout(Layout::right_to_left(Align::Center), fold_buttons).inner
}

pub fn fold_buttons(ui: &mut Ui) -> Option<bool> {
    let mut out = None;
    let size = vec2(22.0, 22.0);
    // Laid out right to left by the callers: "+" ends up on the right.
    if icon_button(ui, Icon::Plus, size, theme::SILVER, theme::NAVY_HOVER).on_hover_text("Expand all folders").clicked() {
        out = Some(true);
    }
    if icon_button(ui, Icon::Minimize, size, theme::SILVER, theme::NAVY_HOVER).on_hover_text("Collapse all folders").clicked() {
        out = Some(false);
    }
    out
}
