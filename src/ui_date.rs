//! Calendar popup for date fields: pick a day + time, or a dynamic value resolved at send time.

use egui::{vec2, Button, CornerRadius, Id, RichText, Sense, Stroke, Ui};

use crate::{dates, theme};

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

/// Dynamic choices: (label, expression).
const DYNAMIC: [(&str, &str); 6] =
    [("now", "now"), ("now − 1 hour", "now-1h"), ("now − 1 day", "now-1d"), ("now − 7 days", "now-7d"), ("now + 1 day", "now+1d"), ("today 00:00", "today")];

#[derive(Clone)]
struct PickerState {
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    /// Range fields: the date goes to the end bound.
    end: bool,
}

impl PickerState {
    fn from_value(current: &str, range: bool) -> Self {
        let part = if range { current.split(',').next().unwrap_or_default() } else { current };
        let (year, month, day, hour, minute) = dates::parse(part).map(|d| dates::parts(&d)).unwrap_or_else(dates::now_parts);
        Self { year, month, day, hour, minute, end: false }
    }

    fn value(&self) -> Option<String> {
        dates::format_local(self.year, self.month, self.day.min(dates::days_in_month(self.year, self.month)), self.hour, self.minute)
    }
}

pub enum Pick {
    Set(String),
    Start(String),
    End(String),
    Clear,
}

/// Writes a pick into a field (`min,max` for range fields).
pub fn apply_pick(value: &mut String, pick: Pick) {
    match pick {
        Pick::Set(s) => *value = s,
        Pick::Clear => value.clear(),
        Pick::Start(s) => {
            let end = value.split_once(',').map(|(_, e)| e.trim().to_owned()).unwrap_or_default();
            *value = format!("{s},{end}");
        }
        Pick::End(s) => {
            let start = value.split_once(',').map(|(a, _)| a.trim().to_owned()).unwrap_or_else(|| value.trim().to_owned());
            *value = format!("{start},{s}");
        }
    }
}

/// Calendar button + popup editing `value` in place.
pub fn date_field(ui: &mut Ui, value: &mut String, id: Id, range: bool, dynamic: bool) {
    let current = value.clone();
    if let Some(pick) = calendar_button(ui, id, &current, range, dynamic) {
        apply_pick(value, pick);
    }
    if dates::is_dynamic(value) {
        ui.label(RichText::new("⟳ resolved when sent").size(11.0).color(theme::CYAN_DARK)).on_hover_text(format!("Right now: {}", dates::expand(value)));
    }
}

fn paint_calendar(ui: &Ui, rect: egui::Rect, color: egui::Color32) {
    let p = ui.painter();
    let r = egui::Rect::from_center_size(rect.center() + vec2(0.0, 1.0), vec2(14.0, 12.0));
    let s = Stroke::new(1.4_f32, color);
    p.rect_stroke(r, CornerRadius::same(2), s, egui::StrokeKind::Middle);
    p.line_segment([egui::pos2(r.left(), r.top() + 3.5), egui::pos2(r.right(), r.top() + 3.5)], s);
    for x in [r.left() + 4.0, r.right() - 4.0] {
        p.line_segment([egui::pos2(x, r.top() - 2.5), egui::pos2(x, r.top() + 1.0)], s);
    }
    for (dx, dy) in [(3.5, 6.5), (7.0, 6.5), (10.5, 6.5), (3.5, 9.5), (7.0, 9.5)] {
        p.circle_filled(egui::pos2(r.left() + dx, r.top() + dy), 0.9, color);
    }
}

/// A small calendar button; returns what the user picked in its popup.
pub fn calendar_button(ui: &mut Ui, id: Id, current: &str, range: bool, dynamic: bool) -> Option<Pick> {
    let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 24.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(3), theme::CYAN.gamma_multiply(0.18));
    }
    paint_calendar(ui, rect, theme::CYAN_DARK);
    let resp = resp.on_hover_text("Pick a date");
    let popup_id = id.with("date_popup");
    let state_id = id.with("date_state");
    let clicked = resp.clicked();
    if clicked {
        ui.data_mut(|d| d.insert_temp(state_id, PickerState::from_value(current, range)));
    }
    let mut out = None;
    let popup = egui::Popup::from_response(&resp)
        .id(popup_id)
        .open_memory(clicked.then_some(egui::SetOpenCommand::Toggle))
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
    popup.show(|ui| {
        ui.set_min_width(300.0);
        // Same light theme as the white card the popup's frame is drawn with.
        ui.style_mut().visuals = theme::card_visuals();
        let mut st = ui.data(|d| d.get_temp::<PickerState>(state_id)).unwrap_or_else(|| PickerState::from_value(current, range));
        out = picker_body(ui, &mut st, range, dynamic);
        ui.data_mut(|d| d.insert_temp(state_id, st));
    });
    if out.is_some() {
        egui::Popup::close_id(ui.ctx(), popup_id);
    }
    out
}

fn picker_body(ui: &mut Ui, st: &mut PickerState, range: bool, dynamic: bool) -> Option<Pick> {
    let mut out = None;
    let wrap = |st: &PickerState, s: String| {
        if !range {
            Pick::Set(s)
        } else if st.end {
            Pick::End(s)
        } else {
            Pick::Start(s)
        }
    };

    if range {
        ui.horizontal(|ui| {
            ui.label("Bound:");
            ui.radio_value(&mut st.end, false, "start (min)");
            ui.radio_value(&mut st.end, true, "end (max)");
        });
    }
    // Month navigation
    ui.horizontal(|ui| {
        if ui.small_button(" < ").clicked() {
            if st.month == 1 {
                st.month = 12;
                st.year -= 1;
            } else {
                st.month -= 1;
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button(" > ").clicked() {
                if st.month == 12 {
                    st.month = 1;
                    st.year += 1;
                } else {
                    st.month += 1;
                }
            }
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new(format!("{} {}", MONTHS[(st.month - 1) as usize], st.year)).family(theme::bold()).size(15.0));
            });
        });
    });
    // Day grid, weeks start on Monday
    let (ty, tm, td, _, _) = dates::now_parts();
    let first = dates::first_weekday(st.year, st.month);
    let days = dates::days_in_month(st.year, st.month);
    egui::Grid::new(ui.scope_id().with("days")).spacing([2.0, 2.0]).show(ui, |ui| {
        for w in ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"] {
            ui.centered_and_justified(|ui| ui.label(RichText::new(w).size(11.5).color(theme::CONCRETE)));
        }
        ui.end_row();
        let mut col = 0;
        for _ in 0..first {
            ui.label("");
            col += 1;
        }
        for d in 1..=days {
            let selected = d == st.day;
            let is_today = (st.year, st.month, d) == (ty, tm, td);
            let mut text = RichText::new(format!("{d}"));
            if is_today && !selected {
                text = text.color(theme::CYAN).family(theme::bold());
            }
            if ui.add(Button::new(text).selected(selected).min_size(vec2(36.0, 26.0))).clicked() {
                st.day = d;
            }
            col += 1;
            if col % 7 == 0 {
                ui.end_row();
            }
        }
    });
    st.day = st.day.min(days);
    // Time
    ui.horizontal(|ui| {
        ui.label("Time");
        ui.add(egui::DragValue::new(&mut st.hour).range(0..=23).custom_formatter(|v, _| format!("{v:02}")));
        ui.label(":");
        ui.add(egui::DragValue::new(&mut st.minute).range(0..=59).custom_formatter(|v, _| format!("{v:02}")));
        ui.label(RichText::new(format!("local (UTC{})", dates::local_offset())).size(11.5).color(theme::CONCRETE));
    });
    let value = st.value();
    if let Some(v) = &value {
        ui.label(RichText::new(v).monospace().size(12.0).color(theme::MUTED));
    }
    ui.horizontal(|ui| {
        let apply = Button::new(RichText::new("Apply").family(theme::bold()).color(theme::WHITE)).fill(theme::CYAN);
        if ui.add_enabled(value.is_some(), apply).clicked() {
            out = value.clone().map(|v| wrap(st, v));
        }
        if ui.button("Now").on_hover_text("Select the current date and time").clicked() {
            let (y, m, d, h, min) = dates::now_parts();
            (st.year, st.month, st.day, st.hour, st.minute) = (y, m, d, h, min);
        }
        if ui.button("Clear").clicked() {
            out = Some(Pick::Clear);
        }
    });
    if dynamic {
        ui.separator();
        ui.label(RichText::new("Dynamic — computed each time the request is sent").size(12.0).family(theme::bold()).color(theme::CYAN));
        ui.horizontal_wrapped(|ui| {
            for (label, expr) in DYNAMIC {
                if ui.button(label).on_hover_text(format!("{{{{{expr}}}}}")).clicked() {
                    out = Some(wrap(st, format!("{{{{{expr}}}}}")));
                }
            }
        });
        ui.label(RichText::new("Or type it: {{now-30m}}, {{now+2w}}, {{today-1d}} (s, m, h, d, w)").size(11.0).color(theme::CONCRETE));
    }
    out
}

/// Inserts a date into a JSON text at the last cursor position (quoted when outside a string).
pub fn insert_json_date(body: &mut String, cursor: Option<usize>, value: &str) {
    let chars = body.chars().count();
    let at = cursor.unwrap_or(chars).min(chars);
    let byte = body.char_indices().nth(at).map(|(b, _)| b).unwrap_or(body.len());
    let mut in_string = false;
    let mut escaped = false;
    for c in body[..byte].chars() {
        if escaped {
            escaped = false;
        } else if c == '\\' && in_string {
            escaped = true;
        } else if c == '"' {
            in_string = !in_string;
        }
    }
    let text = if in_string { value.to_owned() } else { format!("\"{value}\"") };
    body.insert_str(byte, &text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_picks() {
        let mut v = String::new();
        apply_pick(&mut v, Pick::Start("A".into()));
        assert_eq!(v, "A,");
        apply_pick(&mut v, Pick::End("B".into()));
        assert_eq!(v, "A,B");
        apply_pick(&mut v, Pick::Start("C".into()));
        assert_eq!(v, "C,B");
    }

    #[test]
    fn json_insertion() {
        let mut b = String::from(r#"{"begin_at": }"#);
        insert_json_date(&mut b, Some(13), "{{now}}");
        assert_eq!(b, r#"{"begin_at": "{{now}}"}"#);
        let mut b = String::from(r#"{"begin_at": ""}"#);
        insert_json_date(&mut b, Some(14), "{{now}}");
        assert_eq!(b, r#"{"begin_at": "{{now}}"}"#);
    }
}
