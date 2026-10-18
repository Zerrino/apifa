//! The AI chat panel on the right (shown with the AI button; hidden while no API key is set).

use std::collections::VecDeque;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use egui::{vec2, Align, Button, CollapsingHeader, Color32, Frame, Key, Label, Layout, Margin, RichText, ScrollArea, Stroke, TextEdit, Ui};
use serde_json::Value;

use crate::ai::{self, Message, Part, Role};
use crate::app::Action;
use crate::theme;

/// A tool call waiting: (id, name, input).
pub type Call = (String, String, Value);

#[derive(Default)]
pub struct Chat {
    pub open: bool,
    pub history: Vec<Message>,
    /// Lines shown (not sent): (shown after this many history messages, text, color).
    pub notices: Vec<(usize, String, Color32)>,
    pub input: String,
    pub waiting: Option<(Receiver<Result<ai::Reply, String>>, Instant)>,
    /// Tool calls of the last answer not run yet.
    pub queue: VecDeque<Call>,
    /// Results gathered for the next message.
    pub results: Vec<Part>,
    /// A call waiting for the user's OK.
    pub confirm: Option<Call>,
    /// A confirmed call running: (id, result channel).
    pub tool_wait: Option<(String, Receiver<(String, bool)>)>,
    pub focus: bool,
    /// Tokens of the last answer (the prompt cache at work: most of the prompt read back).
    pub last_usage: Option<ai::Usage>,
}

impl Chat {
    pub fn busy(&self) -> bool {
        self.waiting.is_some() || self.tool_wait.is_some() || self.confirm.is_some() || !self.queue.is_empty()
    }

    pub fn notice(&mut self, text: impl Into<String>, color: Color32) {
        self.notices.push((self.history.len(), text.into(), color));
    }

    /// Stops what's going on. Tool calls without a result get one, so the conversation stays valid.
    pub fn cancel(&mut self) {
        self.waiting = None;
        self.tool_wait = None;
        let mut pending: Vec<Call> = self.confirm.take().into_iter().collect();
        pending.extend(self.queue.drain(..));
        if !pending.is_empty() || !self.results.is_empty() {
            let mut results = std::mem::take(&mut self.results);
            let answered: Vec<String> = results.iter().filter_map(|p| if let Part::ToolResult { id, .. } = p { Some(id.clone()) } else { None }).collect();
            if let Some(last) = self.history.last().filter(|m| m.role == Role::Assistant) {
                for (id, _, _) in last.tool_calls() {
                    if !answered.contains(&id) {
                        results.push(Part::ToolResult { id, content: "Cancelled by the user.".into(), is_error: true });
                    }
                }
            }
            if !results.is_empty() {
                self.history.push(Message::user(results));
            }
        }
        self.notice("Stopped.", theme::SILVER);
    }

    pub fn clear(&mut self) {
        *self = Chat { open: self.open, ..Default::default() };
    }
}

/// One line describing a tool call, for the chat.
fn describe(name: &str, input: &Value) -> String {
    let input = &crate::privacy::reveal(input);
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
    match name {
        "search_endpoints" => format!("Searching the docs: {}", s("query")),
        "endpoint_details" => format!("Reading the docs of {} {}", s("verb"), s("path")),
        "list_workspace" => "Reading your workspace".into(),
        "read_saved" => format!("Reading “{}”", s("name")),
        "current_tab" => "Looking at the open tab".into(),
        "open_request" => format!("Opening a request: {} {}", s("verb"), s("path")),
        "open_flow" => format!("Opening a flow: {}", input.pointer("/flow/name").and_then(Value::as_str).unwrap_or("new flow")),
        "list_substitutions" => "Reading your substitutions".into(),
        "open_substitution" => format!("Opening a substitution: {}", s("name")),
        "send_request" => format!("Wants to send {} {}", s("verb"), s("path")),
        other => other.to_owned(),
    }
}

/// What a confirmation shows: the request, the reason.
pub fn confirm_text(input: &Value) -> String {
    // What will really be sent: the fingerprints the AI wrote, as their values.
    let input = &crate::privacy::reveal(input);
    let verb = input.get("verb").and_then(Value::as_str).unwrap_or("GET");
    let path = input.get("path").and_then(Value::as_str).unwrap_or_default();
    let query: Vec<String> = ai::pairs(input.get("query")).into_iter().map(|(k, v)| format!("{k}={v}")).collect();
    let mut text = format!("{verb} {path}{}{}", if query.is_empty() { "" } else { "?" }, query.join("&"));
    let body = ai::body_text(input.get("body"));
    if !body.trim().is_empty() {
        text.push('\n');
        text.push_str(&ai::clip(&body, 1500));
    }
    text
}

pub fn panel(ui: &mut Ui, chat: &mut Chat, model: &str, provider: &str, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Assistant").family(theme::bold()).size(17.0).color(theme::WHITE));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.small_button("×").on_hover_text("Hide the assistant").clicked() {
                actions.push(Action::AiToggle);
            }
            if ui.small_button("New chat").clicked() {
                actions.push(Action::AiNewChat);
            }
        });
    });
    let tip = match chat.last_usage {
        Some(u) if u.input > 0 => format!(
            "Change it in Settings › AI assistant\n\nLast answer: {} prompt tokens, {} of them from the cache ({}%), {} written",
            u.input,
            u.cached,
            u.cached * 100 / u.input,
            u.output
        ),
        _ => "Change it in Settings › AI assistant".to_owned(),
    };
    ui.label(RichText::new(format!("{provider} · {model}")).size(11.5).color(theme::CONCRETE)).on_hover_text(tip);
    ui.label(RichText::new("Anonymized: it sees the structure of your data, never a value").size(11.5).color(theme::GREEN)).on_hover_text(
        "Responses, examples and the values of your saved requests reach the assistant as types and fingerprints \
         (\"login\": \"string#5d1e…\"): equal values give equal fingerprints, nothing can be read back. \
         What you type in the chat is sent as you type it.",
    );
    ui.separator();

    // Separator + input (at most INPUT_MAX tall, it scrolls beyond) + the Send / Stop row: always in the panel.
    const INPUT_MAX: f32 = 80.0;
    let input_h = INPUT_MAX + 50.0;
    let list_h = (ui.available_height() - input_h).max(80.0);
    ScrollArea::vertical().id_salt("ai_chat").max_height(list_h).auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
        ui.set_width(ui.available_width());
        if chat.history.is_empty() && chat.notices.is_empty() {
            ui.add_space(10.0);
            ui.label(RichText::new("Ask for a request, a flow or a substitution, e.g.:").color(theme::SILVER));
            for example in [
                "Make me a request listing the users created this week",
                "Create a flow that logs in, keeps the token and lists my orders",
                "Add a substitution so I can type a team name where team_id is expected",
            ] {
                if ui.add(Button::new(RichText::new(example).size(12.5).color(theme::CLOUDS)).wrap().fill(theme::NAVY)).clicked() {
                    chat.input = example.to_owned();
                    chat.focus = true;
                }
            }
            ui.add_space(6.0);
            ui.label(
                RichText::new("It reads the API docs and your workspace, and opens what it makes in a tab for you to check. It asks before sending anything to the API.")
                    .size(11.5)
                    .color(theme::CONCRETE),
            );
        }
        let notices_at = |ui: &mut Ui, at: usize, notices: &[(usize, String, Color32)]| {
            for (_, text, color) in notices.iter().filter(|(n, _, _)| *n == at) {
                ui.add(Label::new(RichText::new(text).size(12.0).color(*color)).wrap());
            }
        };
        for (i, m) in chat.history.iter().enumerate() {
            notices_at(ui, i, &chat.notices);
            for (j, p) in m.parts.iter().enumerate() {
                match (m.role, p) {
                    (Role::User, Part::Text(t)) => {
                        ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                            Frame::new().fill(theme::CYAN_DARK).corner_radius(8).inner_margin(Margin::symmetric(10, 6)).show(ui, |ui| {
                                ui.set_max_width(ui.available_width() * 0.85);
                                ui.add(Label::new(RichText::new(crate::privacy::reveal_text(t)).color(theme::WHITE)).wrap().selectable(true));
                            });
                        });
                        ui.add_space(4.0);
                    }
                    (Role::Assistant, Part::Text(t)) => {
                        // Shown with the values the fingerprints stand for (only here, on this computer).
                        ui.add(Label::new(RichText::new(crate::privacy::reveal_text(t)).color(theme::CLOUDS)).wrap().selectable(true));
                        ui.add_space(4.0);
                    }
                    (_, Part::ToolCall { name, input, .. }) => {
                        ui.label(RichText::new(format!("→ {}", describe(name, input))).size(12.0).color(theme::CYAN));
                    }
                    (_, Part::ToolResult { content, is_error, .. }) => {
                        let color = if *is_error { theme::ORANGE } else { theme::CONCRETE };
                        let first: String = content.lines().next().unwrap_or_default().chars().take(70).collect();
                        CollapsingHeader::new(RichText::new(first).size(11.5).color(color)).id_salt(("ai_res", i, j)).show(ui, |ui| {
                            ui.add(
                                Label::new(RichText::new(crate::privacy::reveal_text(&ai::clip(content, 4000))).monospace().size(11.0).color(theme::SILVER))
                                    .wrap()
                                    .selectable(true),
                            );
                        });
                    }
                }
            }
        }
        notices_at(ui, chat.history.len(), &chat.notices);
        if let Some((_, since)) = &chat.waiting {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().color(theme::CYAN));
                ui.label(RichText::new(format!("thinking… {:.0}s", since.elapsed().as_secs_f32())).color(theme::SILVER));
            });
        }
        if chat.tool_wait.is_some() {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().color(theme::CYAN));
                ui.label(RichText::new("sending the request…").color(theme::SILVER));
            });
        }
        if let Some((_, _, input)) = &chat.confirm {
            Frame::new().fill(theme::NAVY).stroke(Stroke::new(1.5_f32, theme::ORANGE)).corner_radius(6).inner_margin(Margin::same(10)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("Send this request?").family(theme::bold()).color(theme::ORANGE));
                if let Some(why) = input.get("why").and_then(Value::as_str) {
                    ui.add(Label::new(RichText::new(why).size(12.5).color(theme::CLOUDS)).wrap());
                }
                ui.add(Label::new(RichText::new(confirm_text(input)).monospace().size(12.0).color(theme::WHITE)).wrap().selectable(true));
                ui.horizontal(|ui| {
                    if ui.add(Button::new(RichText::new("Send it").family(theme::bold()).color(theme::WHITE)).fill(theme::CYAN)).clicked() {
                        actions.push(Action::AiConfirm(true));
                    }
                    if ui.button("Don't send").clicked() {
                        actions.push(Action::AiConfirm(false));
                    }
                });
            });
        }
    });

    ui.separator();
    let edit = ScrollArea::vertical()
        .id_salt("ai_input_scroll")
        .max_height(INPUT_MAX)
        .show(ui, |ui| {
            ui.add(
                TextEdit::multiline(&mut chat.input)
                    .id(egui::Id::unique("ai_input"))
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .hint_text("Ask… (Enter sends, Shift+Enter: new line)"),
            )
        })
        .inner;
    if chat.focus {
        edit.request_focus();
        chat.focus = false;
    }
    let enter = edit.has_focus() && ui.input(|i| i.key_pressed(Key::Enter) && !i.modifiers.shift);
    ui.horizontal(|ui| {
        if chat.busy() {
            if ui.button("Stop").clicked() {
                actions.push(Action::AiStop);
            }
        } else {
            let send = ui.add_enabled(
                !chat.input.trim().is_empty(),
                Button::new(RichText::new("Send").family(theme::bold()).color(theme::WHITE)).fill(theme::CYAN).min_size(vec2(70.0, 26.0)),
            );
            if send.clicked() || (enter && !chat.input.trim().is_empty()) {
                // Enter added a newline to the text: drop it.
                let text = chat.input.trim().to_owned();
                chat.input.clear();
                actions.push(Action::AiSend(text));
            }
        }
    });
}
