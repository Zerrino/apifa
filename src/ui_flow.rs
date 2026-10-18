//! Flow editor (building blocks) and its run console.

use std::sync::mpsc::TryRecvError;
use std::time::Duration;

use egui::{
    vec2, Align, Button, CollapsingHeader, Color32, CornerRadius, FontId, Frame, Label, Layout, Margin, Rect, RichText, ScrollArea, Stroke, TextEdit, TextStyle, Ui,
};

use crate::app::{Action, TabId};
use crate::docs::{ApiDoc, Param};
use crate::flow::{self, Block, BlockKind, CompareOp, Flow, FlowCall, FlowInput, FlowOutput, InputType, LogKind, LogLine, RunEvent, RunHandle, BLOCK_TYPES};
use crate::model::{self, BodyMode, DocRef, Kv, RequestSpec, VERBS};
use crate::theme;
use crate::widgets::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConsoleTab {
    Log,
    Variables,
}

pub struct FlowState {
    pub id: u64,
    pub flow: Flow,
    pub run: Option<RunHandle>,
    pub log: Vec<LogLine>,
    /// Lines left out once the console is full (errors and warnings are always kept).
    pub log_dropped: usize,
    /// The last log search: needle, log length it was made on, matching lines (and whether in their detail).
    log_hits: Option<LogHits>,
    /// Height of each log line when last drawn, and the widest one: lines out of view aren't drawn.
    log_heights: Vec<f32>,
    log_width: f32,
    pub result: Option<(bool, Duration)>,
    pub vars_text: String,
    /// Line starts of `vars_text` (only the visible lines are drawn).
    pub vars_lines: Vec<usize>,
    pub console: ConsoleTab,
    pub picker: String,
    /// Console search (log lines and variables).
    pub search: String,
    /// Variables flattened as (path, value), built on the first search only (big runs have 100 000s).
    pub leaves: Option<Vec<(String, String)>>,
    /// The last search in the variables and the leaves it matched.
    pub leaves_search: Option<(String, Vec<usize>)>,
    /// Edit mode shows every block; otherwise only the inputs to fill before running.
    pub editing: bool,
    /// Variables at the end of the last run (to show the outputs).
    pub last_vars: Option<serde_json::Value>,
    /// When those results were made and where they come from ("this run", "the saved file"…).
    pub results_from: Option<(u64, &'static str)>,
    /// Requests sent and the API's rate limits, live during a run.
    pub rate: Option<flow::Rate>,
    /// The outermost running "for each": items done, time left.
    pub progress: Option<flow::Progress>,
    /// A stopped run of this flow that Resume goes on with.
    pub resume: Option<crate::resume::Info>,
}

/// A log search: needle, log length it was made on, matching lines (index, matched in its detail).
type LogHits = (String, usize, Vec<(usize, bool)>);

impl FlowState {
    pub fn new(id: u64, flow: Flow) -> Self {
        let resume = crate::resume::info(&flow);
        Self {
            id,
            flow,
            run: None,
            log: Vec::new(),
            log_dropped: 0,
            log_hits: None,
            log_heights: Vec::new(),
            log_width: 0.0,
            result: None,
            vars_text: String::new(),
            vars_lines: Vec::new(),
            console: ConsoleTab::Log,
            picker: String::new(),
            search: String::new(),
            leaves: None,
            leaves_search: None,
            editing: false,
            last_vars: None,
            results_from: None,
            rate: None,
            progress: None,
            resume,
        }
    }

    pub fn running(&self) -> bool {
        self.run.is_some()
    }

    pub fn reset_console(&mut self) {
        self.log.clear();
        self.log_dropped = 0;
        self.log_hits = None;
        self.log_heights.clear();
        self.log_width = 0.0;
        self.result = None;
        self.vars_text = String::new();
        self.vars_lines = Vec::new();
        self.leaves = None;
        self.leaves_search = None;
        self.last_vars = None;
        self.results_from = None;
        self.rate = None;
        self.progress = None;
        self.console = ConsoleTab::Log;
    }
}

const MAX_LOG: usize = 5000;

/// Drains the runner's events.
pub fn poll(st: &mut FlowState) {
    let Some(run) = &st.run else { return };
    let mut finished = None;
    loop {
        match run.rx.try_recv() {
            Ok(RunEvent::Log(line)) => {
                // Once full, only what explains the outcome still comes in.
                let important = matches!(line.kind, LogKind::Error | LogKind::Warning);
                if st.log.len() < MAX_LOG || (important && st.log.len() < 2 * MAX_LOG) {
                    st.log.push(line);
                } else {
                    st.log_dropped += 1;
                }
            }
            Ok(RunEvent::Rate(rate)) => st.rate = Some(rate),
            Ok(RunEvent::Progress(p)) => st.progress = p,
            Ok(RunEvent::Tests(_)) => {}
            Ok(RunEvent::Finished { ok, vars }) => {
                finished = Some((ok, run.started.elapsed(), Some(vars)));
                break;
            }
            Err(TryRecvError::Empty) => break,
            Err(TryRecvError::Disconnected) => {
                finished = Some((false, run.started.elapsed(), None));
                break;
            }
        }
    }
    if let Some((ok, elapsed, vars)) = finished {
        if st.log_dropped > 0 {
            let text = format!("… {} console lines not shown (the console keeps {MAX_LOG} lines, then only errors and warnings)", st.log_dropped);
            st.log.push(LogLine { kind: LogKind::Warning, depth: 0, text, detail: None });
            st.log_dropped = 0;
        }
        st.run = None;
        st.progress = None;
        st.resume = crate::resume::info(&st.flow);
        st.result = Some((ok, elapsed));
        if let Some(vars) = vars {
            let at = crate::storage::now();
            set_results(st, ok, elapsed, vars, at, "this run");
            if st.flow.keep_results {
                save_results(st);
            }
        }
    }
}

/// Shows results (from a run or a file): variables tab, outputs, Visualize.
pub fn set_results(st: &mut FlowState, ok: bool, elapsed: Duration, vars: serde_json::Value, at: u64, from: &'static str) {
    st.result = Some((ok, elapsed));
    st.vars_text = serde_json::to_string_pretty(&vars).unwrap_or_default();
    st.vars_lines = line_starts(&st.vars_text);
    st.leaves = None;
    st.leaves_search = None;
    crate::privacy::learn(&vars);
    st.last_vars = Some(vars);
    st.results_from = Some((at, from));
}

/// The current results as a saved run.
pub fn current_run(st: &FlowState) -> Option<crate::runs::SavedRun> {
    Some(crate::runs::SavedRun {
        flow: st.flow.name.clone(),
        ok: st.result.is_some_and(|(ok, _)| ok),
        at: st.results_from.map(|(at, _)| at).unwrap_or_else(crate::storage::now),
        duration_ms: st.result.map(|(_, d)| d.as_millis() as u64).unwrap_or(0),
        vars: st.last_vars.clone()?,
    })
}

/// Writes the results next to the app's data (Keep results); a failure goes to the console.
pub fn save_results(st: &mut FlowState) {
    let Some(run) = current_run(st) else { return };
    let line = match crate::runs::save(st.flow.uid, &run) {
        Ok(()) => {
            LogLine { kind: LogKind::Info, depth: 0, text: "Results kept: they come back when you open this flow (Visualize without running again)".into(), detail: None }
        }
        Err(e) => LogLine { kind: LogKind::Warning, depth: 0, text: format!("⚠ couldn't keep the results: {e}"), detail: None },
    };
    st.log.push(line);
}

/// "Keep results", where the results come from, Export / Load.
fn results_row(ui: &mut Ui, st: &mut FlowState, tab_id: TabId, actions: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        let tip = "Each run writes its results to a file (in the app's data, not the workspace). They come back when you open the flow: change the visualization and Visualize again without calling the API.";
        if ui.checkbox(&mut st.flow.keep_results, "Keep results").on_hover_text(tip).changed() && st.flow.keep_results {
            save_results(st);
        }
        match st.results_from {
            Some((at, from)) => {
                let when = crate::runs::when(at);
                let text = if when.is_empty() { format!("Results: {from}") } else { format!("Results of {when} · {from}") };
                ui.label(RichText::new(text).size(12.0).color(theme::MUTED));
            }
            None => {
                ui.label(RichText::new("No results yet: run the flow, or load results").size(12.0).color(theme::MUTED));
            }
        }
        if ui.add_enabled(st.last_vars.is_some(), Button::new("Export results…").small()).on_hover_text("Save them as a JSON file (to reuse or share)").clicked() {
            actions.push(Action::ExportFlowResults(tab_id));
        }
        if ui.small_button("Load results…").on_hover_text("Use results saved before (Export results, or `apifa run … --vars`)").clicked() {
            actions.push(Action::LoadFlowResults(tab_id));
        }
    });
}

fn block_color(kind: &BlockKind) -> Color32 {
    match kind {
        BlockKind::Request(r) => theme::verb_color(&r.verb),
        BlockKind::Extract { .. } => theme::PURPLE,
        BlockKind::Set { .. } => theme::BLUE,
        BlockKind::ForEach { .. } => theme::ORANGE,
        BlockKind::If { .. } => theme::CYAN,
        BlockKind::Wait { .. } => theme::CONCRETE,
        BlockKind::Log { .. } => theme::GREEN,
        BlockKind::Fail { .. } => theme::RED,
        BlockKind::Flow(_) => theme::NAVY,
        BlockKind::WebSocket(_) => theme::CYAN,
        BlockKind::Grpc(_) => theme::GREEN,
    }
}

// ==================================================================== editor

#[allow(clippy::too_many_arguments)]
pub fn flow_ui(ui: &mut Ui, doc: &ApiDoc, library: &[&Flow], st: &mut FlowState, tab_id: TabId, saved: bool, fav: Option<bool>, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            star_button(ui, fav, tab_id, actions);
            let (text, tip) = if st.editing { ("Done", "Back to filling in the inputs") } else { ("Edit", "Change the inputs and the blocks") };
            let edit = Button::new(RichText::new(text).family(theme::bold()).color(if st.editing { theme::WHITE } else { theme::NAVY }))
                .fill(if st.editing { theme::NAVY } else { theme::CLOUDS })
                .min_size(vec2(64.0, 26.0));
            if ui.add(edit).on_hover_text(tip).clicked() {
                st.editing = !st.editing;
            }
            ui.menu_button("More", |ui| {
                if ui.button("Save as…").clicked() {
                    actions.push(Action::SaveTabAs(tab_id));
                    ui.close();
                }
                if ui.button("Export to file…").clicked() {
                    actions.push(Action::ExportTab(tab_id));
                    ui.close();
                }
                if ui.button("Copy as JSON (share)").clicked() {
                    actions.push(Action::CopyTab(tab_id));
                    ui.close();
                }
            });
            if ui.button(if saved { "Save" } else { "Save…" }).on_hover_text("Save in your workspace (Ctrl+S)").clicked() {
                actions.push(Action::SaveTab(tab_id));
            }
            if st.running() {
                let stop =
                    Button::new(RichText::new("Stop").family(theme::bold()).size(15.0).color(theme::WHITE)).fill(theme::RED).corner_radius(4).min_size(vec2(96.0, 32.0));
                if ui.add(stop).on_hover_text("Stops the calls. What was received is kept: Resume goes on from here later, even after closing the app").clicked() {
                    actions.push(Action::StopFlow(tab_id));
                }
            } else {
                resume_button(ui, st, tab_id, actions, vec2(110.0, 32.0));
                let run = Button::new(RichText::new(if st.resume.is_some() { "▶  Start over" } else { "▶  Run" }).family(theme::bold()).size(15.0).color(theme::WHITE))
                    .fill(if st.resume.is_some() { theme::CONCRETE } else { theme::CYAN })
                    .corner_radius(4)
                    .min_size(vec2(96.0, 32.0));
                let tip = if st.resume.is_some() { "Runs from the beginning (Ctrl+Enter): the stopped run is forgotten" } else { "Ctrl+Enter" };
                if ui.add(run).on_hover_text(tip).clicked() {
                    actions.push(Action::RunFlow(tab_id));
                }
            }
            // What is left of the row, after the buttons on the right (drawn first: they no longer cover it).
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                pill(ui, "FLOW", theme::WHITE, theme::CYAN);
                if st.editing {
                    ui.add(
                        TextEdit::singleline(&mut st.flow.name)
                            .font(FontId::new(17.0, theme::bold()))
                            .desired_width(ui.available_width().min(340.0))
                            .frame(egui::Frame::NONE)
                            .hint_text(hint_rt("Flow name")),
                    )
                    .on_hover_text("Click to rename");
                } else {
                    let name = if st.flow.name.trim().is_empty() { "Untitled flow" } else { st.flow.name.as_str() };
                    ui.add(Label::new(RichText::new(name).family(theme::bold()).size(17.0)).truncate());
                }
            });
        });
    });
    if !st.editing {
        if !st.flow.description.trim().is_empty() {
            ui.add(Label::new(RichText::new(&st.flow.description).color(theme::MUTED)).wrap());
        }
        ui.separator();
        fill_view(ui, doc, st, tab_id, actions);
        return;
    }
    ui.add(
        TextEdit::singleline(&mut st.flow.description)
            .desired_width(f32::INFINITY)
            .frame(egui::Frame::NONE)
            .hint_text(hint_rt("What does this flow do? (shown to people you share it with)")),
    );
    ui.separator();

    ScrollArea::vertical().id_salt(("flow", st.id)).auto_shrink([false, false]).show(ui, |ui| {
        ui.set_width(ui.available_width() - 6.0);
        section(ui, "Inputs", "Values to fill before running. Use them anywhere as {{name}}.");
        inputs_ui(ui, st);
        ui.add_space(10.0);
        section(
            ui,
            "Blocks",
            "Run from top to bottom. Read variables with {{name.path}}: {{user.body.id}}, {{users.body[0].login}}, {{list.length}}, {{users.body.id}} (all ids, comma separated). Dates: {{now}}, {{now-7d}}, {{today}} are computed when each request is sent.",
        );
        let id = st.id;
        let this = st.flow.uid;
        let running = st.running();
        blocks_ui(ui, doc, &mut st.flow.blocks, 0, &mut Cx { picker: &mut st.picker, actions, library, this, tab_id, running }, id);
        ui.add_space(14.0);
        section(
            ui,
            "Outputs",
            "What this flow gives back at the end. When another flow runs it, they are in {{<save as>.<name>}}. Value: an expression like {{user.body.id}}.",
        );
        outputs_ui(ui, &mut st.flow.outputs);
        ui.add_space(14.0);
        visualizer_ui(ui, st, tab_id, actions);
        ui.add_space(30.0);
    });
}

/// Outside edit mode: the inputs as a form, then a read-only outline of the steps.
fn fill_view(ui: &mut Ui, doc: &ApiDoc, st: &mut FlowState, tab_id: TabId, actions: &mut Vec<Action>) {
    ScrollArea::vertical().id_salt(("flow_fill", st.id)).auto_shrink([false, false]).show(ui, |ui| {
        ui.set_width(ui.available_width() - 6.0);
        ui.add_space(4.0);
        if st.flow.inputs.is_empty() {
            ui.label(RichText::new("This flow needs no input: press Run.").color(theme::MUTED));
        } else {
            section(ui, "Fill in", "Then press Run (Ctrl+Enter).");
            let salt = st.id;
            for (i, input) in st.flow.inputs.iter_mut().enumerate().filter(|(_, inp)| !inp.name.trim().is_empty()) {
                row_frame(ui, |ui| {
                    ui.horizontal(|ui| {
                        fixed(ui, 200.0, |ui| {
                            ui.label(RichText::new(input.name.trim()).monospace().family(theme::bold()).color(theme::NAVY));
                            if input.kind != InputType::Auto && input.kind != InputType::String {
                                pill(ui, input.kind.label(), theme::MUTED, theme::CLOUDS);
                            }
                        });
                        fixed(ui, VALUE_W, |ui| input_value(ui, input, (salt, i)));
                        if !input.description.trim().is_empty() {
                            // Wrapped in a few pixels it went one letter per line: cut, the whole text on hover.
                            let label = Label::new(RichText::new(&input.description).size(12.5).color(theme::MUTED));
                            let label = if ui.available_width() < 160.0 { label.truncate() } else { label.wrap() };
                            ui.add(label).on_hover_text(&input.description);
                        }
                    });
                    if let Err(e) = input.to_value() {
                        ui.horizontal(|ui| {
                            ui.add_space(208.0);
                            ui.label(RichText::new(e).size(11.5).color(theme::RED));
                        });
                    }
                });
            }
        }
        ui.add_space(10.0);
        if !st.running() {
            let run =
                Button::new(RichText::new("▶  Run").family(theme::bold()).size(15.0).color(theme::WHITE)).fill(theme::CYAN).corner_radius(4).min_size(vec2(140.0, 34.0));
            resume_banner(ui, st, tab_id, actions);
            ui.horizontal(|ui| {
                resume_button(ui, st, tab_id, actions, vec2(140.0, 34.0));
                let tip = if st.resume.is_some() { "Runs from the beginning (Ctrl+Enter): the stopped run is forgotten" } else { "Ctrl+Enter" };
                if ui.add(run).on_hover_text(tip).clicked() {
                    actions.push(Action::RunFlow(tab_id));
                }
                if st.last_vars.is_some() {
                    let tip = if st.flow.visualizer.trim().is_empty() { "Open the results in your browser" } else { "Open the results with this flow's visualization" };
                    if ui.add(Button::new("Visualize ↗").min_size(vec2(110.0, 34.0))).on_hover_text(tip).clicked() {
                        actions.push(Action::VisualizeFlow(tab_id));
                    }
                }
            });
        }
        ui.add_space(4.0);
        results_row(ui, st, tab_id, actions);
        let outputs: Vec<&FlowOutput> = st.flow.outputs.iter().filter(|o| !o.name.trim().is_empty()).collect();
        if !outputs.is_empty() {
            ui.add_space(14.0);
            let help = if st.last_vars.is_some() { "Values from the last run." } else { "Run the flow to see their values." };
            section(ui, "Outputs", help);
            let vars = st.last_vars.as_ref().and_then(|v| v.as_object());
            for o in outputs {
                row_frame(ui, |ui| {
                    ui.horizontal(|ui| {
                        fixed(ui, 200.0, |ui| {
                            ui.label(RichText::new(o.name.trim()).monospace().family(theme::bold()).color(theme::NAVY));
                        });
                        match vars {
                            Some(vars) => {
                                let v = flow::eval_output(&o.value, vars);
                                let text = match &v {
                                    serde_json::Value::String(s) => s.clone(),
                                    other => other.to_string(),
                                };
                                let shown: String = text.chars().take(300).collect();
                                ui.add(Label::new(RichText::new(shown).monospace().color(theme::CYAN_DARK)).truncate()).on_hover_text("Click to copy");
                                if ui.small_button("Copy").clicked() {
                                    ui.ctx().copy_text(text);
                                }
                            }
                            None => {
                                ui.label(RichText::new("—").color(theme::SILVER));
                            }
                        }
                        if !o.description.trim().is_empty() {
                            ui.label(RichText::new(&o.description).size(12.5).color(theme::MUTED));
                        }
                    });
                });
            }
        }
        ui.add_space(14.0);
        CollapsingHeader::new(RichText::new(format!("Steps ({})", count_blocks(&st.flow.blocks))).family(theme::bold()))
            .id_salt(("steps", st.id))
            .default_open(false)
            .show(ui, |ui| {
                if st.flow.blocks.is_empty() {
                    ui.label(RichText::new("No block yet: press Edit to add some.").color(theme::MUTED));
                }
                outline(ui, &st.flow.blocks, 0);
            });
        // Dropping a request from the sidebar adds it and switches to edit mode.
        if let Some(b) = drop_zone(ui, doc, "Drop here to add the request (opens edit mode)") {
            st.flow.blocks.push(b);
            st.editing = true;
        }
    });
}

fn count_blocks(blocks: &[Block]) -> usize {
    blocks
        .iter()
        .map(|b| {
            1 + match &b.kind {
                BlockKind::ForEach { blocks, .. } => count_blocks(blocks),
                BlockKind::If { then_blocks, else_blocks, .. } => count_blocks(then_blocks) + count_blocks(else_blocks),
                _ => 0,
            }
        })
        .sum()
}

/// One line per block, nested blocks indented.
fn outline(ui: &mut Ui, blocks: &[Block], depth: usize) {
    for b in blocks {
        let text = b.kind.summary();
        ui.horizontal(|ui| {
            ui.add_space(depth as f32 * 18.0);
            let (r, _) = ui.allocate_exact_size(vec2(4.0, 16.0), egui::Sense::hover());
            ui.painter().rect_filled(r, CornerRadius::same(2), block_color(&b.kind));
            ui.label(RichText::new(b.kind.title()).size(11.5).family(theme::bold()).color(block_color(&b.kind)));
            let mut line = RichText::new(text).monospace().size(12.0);
            if !b.enabled {
                line = line.strikethrough().color(theme::SILVER);
            }
            ui.add(Label::new(line).truncate());
            if !b.note.trim().is_empty() {
                ui.label(RichText::new(format!("— {}", b.note.trim())).size(11.5).color(theme::MUTED));
            }
        });
        match &b.kind {
            BlockKind::ForEach { blocks, .. } => outline(ui, blocks, depth + 1),
            BlockKind::If { then_blocks, else_blocks, .. } => {
                outline(ui, then_blocks, depth + 1);
                if !else_blocks.is_empty() {
                    ui.horizontal(|ui| {
                        ui.add_space(depth as f32 * 18.0 + 8.0);
                        ui.label(RichText::new("else").size(11.5).family(theme::bold()).color(theme::MUTED));
                    });
                    outline(ui, else_blocks, depth + 1);
                }
            }
            _ => {}
        }
    }
}

const TYPE_W: f32 = 100.0;
const VALUE_W: f32 = 320.0;

fn inputs_ui(ui: &mut Ui, st: &mut FlowState) {
    let mut remove = None;
    if !st.flow.inputs.is_empty() {
        ui.horizontal(|ui| {
            for (title, w) in [("Type", TYPE_W), ("Name", 160.0), ("Value", VALUE_W), ("Description", 0.0)] {
                fixed(ui, w, |ui| {
                    ui.label(RichText::new(title).family(theme::bold()));
                });
            }
        });
    }
    let salt = st.id;
    for (i, input) in st.flow.inputs.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            fixed(ui, TYPE_W, |ui| {
                let before = input.kind;
                egui::ComboBox::from_id_salt(("itype", salt, i))
                    .width(TYPE_W - 10.0)
                    .selected_text(RichText::new(input.kind.label()).monospace().color(theme::CYAN_DARK))
                    .show_ui(ui, |ui| {
                        card_popup(ui);
                        for t in InputType::ALL {
                            ui.selectable_value(&mut input.kind, t, RichText::new(t.label()).monospace()).on_hover_text(t.help());
                        }
                    })
                    .response
                    .on_hover_text(input.kind.help());
                // Switching to enum: the current value becomes the first choice.
                if before != input.kind && input.kind == InputType::Enum && input.options.is_empty() && !input.value.trim().is_empty() {
                    input.options.push(input.value.trim().to_owned());
                }
            });
            ui.add_sized(
                [160.0, 24.0],
                TextEdit::singleline(&mut input.name).id(egui::Id::unique(("iname", salt, i))).font(TextStyle::Monospace).hint_text(hint_rt("login")),
            );
            fixed(ui, VALUE_W, |ui| input_value(ui, input, (salt, i)));
            ui.add_sized([300.0, 24.0], TextEdit::singleline(&mut input.description).id(egui::Id::unique(("idesc", salt, i))).hint_text(hint_rt("what is it?")));
            if ui.small_button("×").on_hover_text("Remove").clicked() {
                remove = Some(i);
            }
        });
        if input.kind == InputType::Enum {
            enum_choices(ui, input, (salt, i));
        }
        if let Err(e) = input.to_value() {
            ui.horizontal(|ui| {
                ui.add_space(TYPE_W + 168.0);
                ui.label(RichText::new(e).size(11.5).color(theme::RED));
            });
        }
    }
    if let Some(i) = remove {
        st.flow.inputs.remove(i);
    }
    if ui.button("+ Add input").clicked() {
        st.flow.inputs.push(FlowInput { kind: InputType::String, ..Default::default() });
    }
}

fn input_value(ui: &mut Ui, input: &mut FlowInput, salt: (u64, usize)) {
    let id = egui::Id::unique(("ivalue", salt));
    let text = |ui: &mut Ui, v: &mut String, w: f32, hint: &str| {
        ui.add_sized([w, 24.0], TextEdit::singleline(v).id(id).font(TextStyle::Monospace).hint_text(hint_rt(hint)));
    };
    match input.kind {
        InputType::Boolean => {
            egui::ComboBox::from_id_salt(("ibool", salt))
                .width(VALUE_W - 90.0)
                .selected_text(if input.value.trim().is_empty() { "— empty —" } else { input.value.as_str() })
                .show_ui(ui, |ui| {
                    card_popup(ui);
                    for v in ["true", "false"] {
                        ui.selectable_value(&mut input.value, v.to_owned(), v);
                    }
                });
        }
        InputType::Enum if !input.options.is_empty() => {
            egui::ComboBox::from_id_salt(("ienum", salt))
                .width(VALUE_W - 90.0)
                .selected_text(if input.value.trim().is_empty() { "— pick one —" } else { input.value.as_str() })
                .show_ui(ui, |ui| {
                    card_popup(ui);
                    for o in &input.options {
                        ui.selectable_value(&mut input.value, o.clone(), o);
                    }
                });
        }
        InputType::Enum => {
            ui.label(RichText::new("add choices below").size(12.0).color(theme::MUTED));
        }
        InputType::Date => {
            text(ui, &mut input.value, VALUE_W - 40.0, "2026-10-15T09:00:00Z or {{now-7d}}");
            crate::ui_date::date_field(ui, &mut input.value, egui::Id::unique(("idate", salt)), false, true);
        }
        InputType::File => {
            text(ui, &mut input.value, VALUE_W - 80.0, "~/photos/me.jpg");
            browse_file(ui, &mut input.value);
        }
        InputType::Integer => text(ui, &mut input.value, VALUE_W - 8.0, "17"),
        InputType::Number => text(ui, &mut input.value, VALUE_W - 8.0, "3.5"),
        InputType::Json => text(ui, &mut input.value, VALUE_W - 8.0, "[1, 2, 3]  or  {\"a\": 1}"),
        InputType::String | InputType::Auto => text(ui, &mut input.value, VALUE_W - 8.0, "value"),
    }
}

/// The values an `enum` input can take: removable chips plus a field to add one.
fn enum_choices(ui: &mut Ui, input: &mut FlowInput, salt: (u64, usize)) {
    let new_id = egui::Id::unique(("ienum_new", salt));
    ui.horizontal_wrapped(|ui| {
        ui.add_space(TYPE_W + 8.0);
        ui.label(RichText::new("Choices").size(12.0).family(theme::bold()).color(theme::MUTED));
        let mut remove = None;
        for (j, o) in input.options.iter().enumerate() {
            if ui.small_button(format!("{o}  ×")).on_hover_text("Remove this choice").clicked() {
                remove = Some(j);
            }
        }
        if let Some(j) = remove {
            let gone = input.options.remove(j);
            if input.value.trim() == gone {
                input.value.clear();
            }
        }
        let mut new: String = ui.data(|d| d.get_temp(new_id)).unwrap_or_default();
        let out = ui.add(TextEdit::singleline(&mut new).desired_width(140.0).font(TextStyle::Monospace).hint_text(hint_rt("new choice")));
        let enter = out.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ui.small_button("+ Add").clicked() || enter) && !new.trim().is_empty() {
            for choice in new.split(',').map(str::trim).filter(|c| !c.is_empty()) {
                if !input.options.iter().any(|o| o == choice) {
                    input.options.push(choice.to_owned());
                }
            }
            // "," alone adds nothing: options can still be empty.
            if input.value.trim().is_empty() {
                if let Some(first) = input.options.first() {
                    input.value = first.clone();
                }
            }
            new.clear();
            if enter {
                out.request_focus();
            }
        }
        ui.data_mut(|d| d.insert_temp(new_id, new));
    });
}

/// What the block editors share while drawing.
struct Cx<'a> {
    picker: &'a mut String,
    actions: &'a mut Vec<Action>,
    /// The workspace's flows, for "Run a flow" blocks.
    library: &'a [&'a Flow],
    /// The flow being edited (it can't run itself).
    this: u64,
    /// Its tab, and whether it runs now (for "Run to here").
    tab_id: TabId,
    running: bool,
}

/// Something dragged from the sidebar that becomes an API request block when dropped on a flow.
#[derive(Clone)]
pub enum BlockDrop {
    Endpoint(DocRef),
    Request(Box<RequestSpec>),
    Flow { uid: u64, name: String },
}

impl BlockDrop {
    pub fn label(&self, doc: &ApiDoc) -> String {
        match self {
            BlockDrop::Endpoint(d) => match doc.lookup(d).and_then(|(_, m)| m.routes.get(d.route)) {
                Some(r) => format!("{} {}", r.verb, r.path),
                None => "API request".into(),
            },
            BlockDrop::Request(spec) => format!("{} {}", spec.verb, spec.path),
            BlockDrop::Flow { name, .. } => format!("Run flow “{name}”"),
        }
    }

    pub fn to_block(&self, doc: &ApiDoc) -> Block {
        match self {
            BlockDrop::Endpoint(d) => block_from_endpoint(doc, d),
            BlockDrop::Request(spec) => block_from_spec(doc, spec),
            BlockDrop::Flow { uid, name } => {
                Block::new_kind(BlockKind::Flow(FlowCall { flow: name.clone(), flow_uid: Some(*uid), save_as: "sub".into(), ..Default::default() }))
            }
        }
    }
}

enum BlockOp {
    Up,
    Down,
    Duplicate,
    Delete,
}

/// While a request is dragged from the sidebar: a zone that receives it.
fn drop_zone(ui: &mut Ui, doc: &ApiDoc, text: &str) -> Option<Block> {
    if !egui::DragAndDrop::has_payload_of_type::<BlockDrop>(ui.ctx()) {
        return None;
    }
    let frame = Frame::new().inner_margin(Margin::same(10)).corner_radius(6);
    let (_, dropped) = ui.dnd_drop_zone::<BlockDrop, ()>(frame, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(text).family(theme::bold()));
    });
    ui.add_space(4.0);
    dropped.map(|d| d.to_block(doc))
}

fn blocks_ui(ui: &mut Ui, doc: &ApiDoc, blocks: &mut Vec<Block>, depth: usize, cx: &mut Cx, salt: u64) {
    if depth == 0 && blocks.len() > 2 {
        if let Some(b) = drop_zone(ui, doc, "Drop here to add the request at the start") {
            blocks.insert(0, b);
        }
    }
    let mut op = None;
    let n = blocks.len();
    for (i, b) in blocks.iter_mut().enumerate() {
        if let Some(o) = block_ui(ui, doc, b, i, n, depth, cx, salt) {
            op = Some((i, o));
        }
    }
    match op {
        Some((i, BlockOp::Up)) if i > 0 => blocks.swap(i, i - 1),
        Some((i, BlockOp::Down)) if i + 1 < n => blocks.swap(i, i + 1),
        Some((i, BlockOp::Duplicate)) => {
            let mut copy = blocks[i].clone();
            copy.refresh_uids();
            blocks.insert(i + 1, copy);
        }
        Some((i, BlockOp::Delete)) => {
            blocks.remove(i);
        }
        _ => {}
    }
    if let Some(b) = drop_zone(ui, doc, if depth == 0 { "Drop here to add the request at the end" } else { "Drop here to add the request in this branch" }) {
        blocks.push(b);
    }
    add_block_menu(ui, doc, blocks, cx, if depth == 0 { "+ Add block" } else { "+ Add block here" });
}

fn add_block_menu(ui: &mut Ui, doc: &ApiDoc, blocks: &mut Vec<Block>, cx: &mut Cx, label: &str) {
    ui.menu_button(RichText::new(label).color(theme::CYAN_DARK).family(theme::bold()), |ui| {
        const W: f32 = 360.0;
        ui.set_width(W);
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.menu_button(RichText::new("API request from the docs…").family(theme::bold()).color(theme::CYAN), |ui| {
            if let Some((d, _, _)) = endpoint_picker(ui, doc, cx.picker) {
                blocks.push(block_from_endpoint(doc, &d));
                ui.close();
            }
        })
        .response
        .on_hover_text("Pick an endpoint (or drag one from the sidebar onto the flow)");
        ui.separator();
        for (id, title, help) in BLOCK_TYPES {
            let color = block_color(&BlockKind::from_id(id));
            let (rect, resp) = ui.allocate_exact_size(vec2(W, 44.0), egui::Sense::click());
            let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
            let painter = ui.painter();
            if resp.hovered() {
                painter.rect_filled(rect, CornerRadius::same(4), theme::NAVY_HOVER);
            }
            let bar = Rect::from_min_size(rect.min + vec2(6.0, 7.0), vec2(4.0, rect.height() - 14.0));
            painter.rect_filled(bar, CornerRadius::same(2), color);
            let x = bar.max.x + 10.0;
            painter.text(egui::pos2(x, rect.min.y + 6.0), egui::Align2::LEFT_TOP, title, FontId::new(14.5, theme::bold()), theme::WHITE);
            painter.text(egui::pos2(x, rect.min.y + 25.0), egui::Align2::LEFT_TOP, help, FontId::proportional(12.0), theme::SILVER);
            if resp.clicked() {
                blocks.push(Block::new_kind(BlockKind::from_id(id)));
                ui.close();
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn block_ui(ui: &mut Ui, doc: &ApiDoc, b: &mut Block, i: usize, n: usize, depth: usize, cx: &mut Cx, salt: u64) -> Option<BlockOp> {
    let color = block_color(&b.kind);
    let mut op = None;
    let resp =
        Frame::new().fill(theme::WHITE).stroke(Stroke::new(1.0_f32, theme::LINE)).corner_radius(6).inner_margin(Margin { left: 14, right: 10, top: 8, bottom: 8 }).show(
            ui,
            |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(b.kind.title()).family(theme::bold()).size(12.5).color(color));
                    ui.checkbox(&mut b.enabled, "").on_hover_text("Enabled (disabled blocks are skipped)");
                    let mut logs = b.logs();
                    if ui.checkbox(&mut logs, RichText::new("log").size(12.0)).on_hover_text("Show this block in the console").changed() {
                        b.log = Some(logs);
                    }
                    // ↑ ↓ Duplicate ▶ to here ✕ keep their room (nested blocks are narrower).
                    let note_w = (ui.available_width() - 230.0).clamp(60.0, 260.0);
                    ui.add(TextEdit::singleline(&mut b.note).frame(egui::Frame::NONE).desired_width(note_w).hint_text(hint_rt("comment…")));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, Icon::Close, vec2(22.0, 22.0), theme::MUTED, theme::RED).on_hover_text("Delete block").clicked() {
                            op = Some(BlockOp::Delete);
                        }
                        let to_here = ui.add_enabled(!cx.running && b.enabled, Button::new("▶ to here").small()).on_hover_text(
                            "Runs the flow and stops once this block is done (in a loop: the first time). \
                             The Variables tab then shows them at this point; Resume goes on from here.",
                        );
                        if to_here.clicked() {
                            cx.actions.push(Action::RunFlowTo(cx.tab_id, b.uid));
                        }
                        if ui.small_button("Duplicate").clicked() {
                            op = Some(BlockOp::Duplicate);
                        }
                        if ui.add_enabled(i + 1 < n, Button::new("↓").small()).on_hover_text("Move down").clicked() {
                            op = Some(BlockOp::Down);
                        }
                        if ui.add_enabled(i > 0, Button::new("↑").small()).on_hover_text("Move up").clicked() {
                            op = Some(BlockOp::Up);
                        }
                    });
                });
                if !b.enabled {
                    ui.multiply_opacity(0.45);
                }
                block_body(ui, doc, b, depth, cx, salt);
            },
        );
    let r = resp.response.rect;
    ui.painter().rect_filled(Rect::from_min_size(r.min, vec2(5.0, r.height())), CornerRadius { nw: 6, sw: 6, ne: 0, se: 0 }, color);
    ui.add_space(6.0);
    op
}

fn field(ui: &mut Ui, value: &mut String, width: f32, hint: &str) -> egui::Response {
    ui.add(TextEdit::singleline(value).font(TextStyle::Monospace).desired_width(width).hint_text(hint_rt(hint)))
}

fn nested(ui: &mut Ui, doc: &ApiDoc, label: &str, blocks: &mut Vec<Block>, depth: usize, cx: &mut Cx, salt: u64) {
    ui.label(RichText::new(label).family(theme::bold()).size(12.5).color(theme::MUTED));
    ui.horizontal(|ui| {
        ui.add_space(14.0);
        ui.vertical(|ui| {
            if blocks.is_empty() {
                ui.label(RichText::new("(empty)").size(12.0).color(theme::SILVER));
            }
            blocks_ui(ui, doc, blocks, depth + 1, cx, salt);
        });
    });
}

/// A block's auth and API profile (requests, WebSocket, gRPC).
fn live_extras(ui: &mut Ui, auth: &mut crate::auth::Auth, profile: &mut String, uid: u64) {
    let label = if auth.is_inherit() { "Auth".to_owned() } else { format!("Auth · {}", auth.label()) };
    CollapsingHeader::new(RichText::new(label).size(13.0)).id_salt(("bauth", uid)).show(ui, |ui| {
        let mut show = false;
        let mut get = false;
        crate::ui_auth::auth_editor(ui, auth, Some("its profile's, else the workspace's"), &mut show, &mut get, ("bauth_ed", uid));
    });
    profile_row(ui, profile, uid, "the flow's");
}

/// Which API profile a block uses — a flow can read from one API and write to another.
fn profile_row(ui: &mut Ui, profile: &mut String, uid: u64, default: &str) {
    let profiles = crate::profiles::all();
    if !profiles.is_empty() {
        ui.horizontal(|ui| {
            ui.label(RichText::new("API profile").size(12.5));
            let shown = profiles.iter().find(|(id, _)| id == profile).map(|(_, n)| n.clone()).unwrap_or_else(|| default.into());
            egui::ComboBox::from_id_salt(("bprofile", uid)).selected_text(shown).width(200.0).show_ui(ui, |ui| {
                if ui.selectable_label(profile.is_empty(), default).clicked() {
                    profile.clear();
                }
                for (id, name) in &profiles {
                    if ui.selectable_label(profile == id, name).clicked() {
                        *profile = id.clone();
                    }
                }
            });
        });
    }
}

fn block_body(ui: &mut Ui, doc: &ApiDoc, b: &mut Block, depth: usize, cx: &mut Cx, salt: u64) {
    let uid = b.uid;
    match &mut b.kind {
        BlockKind::Request(r) => {
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt(("bverb", uid))
                    .width(84.0)
                    .selected_text(RichText::new(&r.verb).family(theme::bold()).color(theme::verb_color(&r.verb)))
                    .show_ui(ui, |ui| {
                        card_popup(ui);
                        for v in VERBS {
                            ui.selectable_value(&mut r.verb, v.to_owned(), RichText::new(v).family(theme::bold()).color(theme::verb_color(v)));
                        }
                    });
                let w = (ui.available_width() - 90.0).max(160.0);
                field(ui, &mut r.path, w, "/users/{{username}}  or  /users/:user_id/orders");
                ui.menu_button("Docs…", |ui| {
                    if let Some((d, verb, path)) = endpoint_picker(ui, doc, cx.picker) {
                        r.verb = verb;
                        r.path = path;
                        // Its API's profile (another one than the flow's: the flow mixes APIs).
                        if let Some((res, _)) = doc.lookup(&d) {
                            let p = crate::profiles::of_api_id(&res.api);
                            if !p.is_empty() {
                                r.profile = p;
                            }
                        }
                        r.doc = Some(d);
                        ui.close();
                    }
                })
                .response
                .on_hover_text("Pick a documented endpoint");
            });
            // The same verb + path as a documented endpoint: link it (its fields show up below).
            if !r.doc.as_ref().is_some_and(|d| doc.route_fits(d, &r.verb, &r.path)) {
                let found = doc.match_route(&r.verb, &r.path);
                if found != r.doc {
                    r.doc = found;
                }
            }
            ui.horizontal(|ui| {
                ui.label(RichText::new("Save response as").size(12.5));
                field(ui, &mut r.save_as, 110.0, "res");
                ui.add_enabled(r.verb == "GET", egui::Checkbox::new(&mut r.all_pages, "Fetch all pages"))
                    .on_hover_text("Follows page[number] until the list is complete (page limit: below, or in Settings)");
                if r.all_pages && r.verb == "GET" {
                    let mut own = r.max_pages.is_some();
                    if ui.checkbox(&mut own, "max pages").on_hover_text("This block's own page limit (unchecked: the one in Settings)").changed() {
                        r.max_pages = own.then_some(100);
                    }
                    if let Some(n) = &mut r.max_pages {
                        ui.add(egui::DragValue::new(n).range(0..=100_000).custom_formatter(|v, _| if v == 0.0 { "no limit".into() } else { format!("{v}") }))
                            .on_hover_text("0 = no limit. 100 items per page, 2 requests per second.");
                    }
                }
                ui.checkbox(&mut r.stop_on_error, "Stop on HTTP error");
            });
            ui.label(
                RichText::new(format!(
                    "Then use {{{{{0}.status}}}}, {{{{{0}.body}}}}, {{{{{0}.body.id}}}}… Path variables like :user_id are filled from variables with the same name.",
                    r.save_as.trim()
                ))
                .size(11.5)
                .color(theme::MUTED),
            );
            if let Some(d) = r.doc.clone() {
                documented_fields(ui, doc, &d, r, uid, cx);
            }
            let nq = r.query.iter().filter(|q| q.enabled && !q.key.is_empty()).count();
            CollapsingHeader::new(with_count("Query parameters", nq)).id_salt(("bq", uid)).default_open(nq > 0).show(ui, |ui| kv_editor(ui, &mut r.query, ("bqkv", uid)));
            let nh = r.headers.iter().filter(|q| q.enabled && !q.key.is_empty()).count();
            CollapsingHeader::new(with_count("Headers", nh)).id_salt(("bh", uid)).show(ui, |ui| kv_editor(ui, &mut r.headers, ("bhkv", uid)));
            if model::has_body(&r.verb) {
                // What the body is: JSON (default), text of a type, or GraphQL.
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Body").family(theme::bold()).size(12.5));
                    let mode = if !r.graphql.is_empty() {
                        2
                    } else if !r.content_type.is_empty() {
                        1
                    } else {
                        0
                    };
                    for (i, label) in ["JSON", "Text / XML", "GraphQL"].into_iter().enumerate() {
                        if ui.selectable_label(mode == i, label).clicked() && mode != i {
                            r.content_type = if i == 1 { "text/plain".into() } else { String::new() };
                            r.graphql = if i == 2 { "query {\n  \n}".into() } else { String::new() };
                        }
                    }
                    if mode == 1 {
                        field(ui, &mut r.content_type, 200.0, "application/xml");
                    }
                });
            }
            if model::has_body(&r.verb) && !r.graphql.is_empty() {
                ui.add(
                    TextEdit::multiline(&mut r.graphql)
                        .id(egui::Id::unique(("flow_gql", uid)))
                        .code_editor()
                        .desired_rows(6)
                        .desired_width(f32::INFINITY)
                        .hint_text(hint_rt("query User($id: ID!) { user(id: $id) { id name } }")),
                );
                ui.label(RichText::new("Variables (JSON; templates allowed)").size(12.0).color(theme::MUTED));
                ui.add(
                    TextEdit::multiline(&mut r.graphql_variables)
                        .id(egui::Id::unique(("flow_gqlv", uid)))
                        .code_editor()
                        .desired_rows(3)
                        .desired_width(f32::INFINITY)
                        .hint_text(hint_rt("{ \"id\": \"{{user_id}}\" }")),
                );
            } else if model::has_body(&r.verb) {
                let body_id = egui::Id::unique(("flow_body", uid));
                let cursor_id = body_id.with("cursor");
                ui.horizontal(|ui| {
                    ui.label(RichText::new(if r.content_type.is_empty() { "JSON body" } else { "Text body" }).family(theme::bold()).size(12.5));
                    ui.add_space(12.0);
                    ui.label(RichText::new("Insert a date at the cursor").size(11.5).color(theme::MUTED));
                    if let Some(crate::ui_date::Pick::Set(v) | crate::ui_date::Pick::Start(v) | crate::ui_date::Pick::End(v)) =
                        crate::ui_date::calendar_button(ui, body_id.with("date"), "", false, true)
                    {
                        let cursor = ui.data(|d| d.get_temp::<usize>(cursor_id));
                        crate::ui_date::insert_json_date(&mut r.body, cursor, &v);
                    }
                });
                let out = TextEdit::multiline(&mut r.body)
                    .id(body_id)
                    .code_editor()
                    .desired_rows(5)
                    .desired_width(f32::INFINITY)
                    .hint_text(hint_rt("{ \"user_id\": {{user.body.id}}, \"begin_at\": \"{{now}}\" }"))
                    .show(ui);
                if let Some(range) = out.cursor_range {
                    ui.data_mut(|d| d.insert_temp(cursor_id, range.primary.index.0));
                }
                let nf = model::file_rows(&r.files).count();
                CollapsingHeader::new(with_count("Files", nf)).id_salt(("bf", uid)).default_open(nf > 0).show(ui, |ui| {
                    ui.label(
                        RichText::new("Upload files as multipart/form-data. The path accepts variables: ~/photos/{{login}}.jpg, /x/y/{{p.login}}.png")
                            .size(11.5)
                            .color(theme::MUTED),
                    );
                    files_editor(ui, &mut r.files, "~/photos/{{login}}.jpg");
                });
            }
            let na = r.assertions.iter().filter(|a| a.enabled).count();
            CollapsingHeader::new(with_count("Assertions", na)).id_salt(("ba", uid)).default_open(na > 0).show(ui, |ui| {
                ui.label(
                    RichText::new(format!(
                        "Checked on the response: res.status, res.body…, res.headers.x-total, res.time. Values can use {{{{variables}}}}. \
                         With assertions, they decide whether the block fails (a 404 can be expected); “Stop on HTTP error” then stops the flow on a failed one, else it's a warning. \
                         The response is also in {{{{{}}}}}.",
                        r.save_as.trim()
                    ))
                    .size(11.5)
                    .color(theme::MUTED),
                );
                crate::ui_request::assertions_editor(ui, &mut r.assertions, ("flow_asserts", uid));
            });
            live_extras(ui, &mut r.auth, &mut r.profile, uid);
            let ns = [&r.pre_script, &r.post_script].iter().filter(|x| !x.trim().is_empty()).count();
            CollapsingHeader::new(with_count("Scripts", ns)).id_salt(("bs", uid)).default_open(ns > 0).show(ui, |ui| {
                ui.label(
                    RichText::new(
                        "JavaScript with Postman's pm: pm.variables are this flow's variables (pm.variables.set makes one), pm.test results count like assertions.",
                    )
                    .size(11.5)
                    .color(theme::MUTED),
                );
                for (label, script, hint) in [
                    ("Before sending", &mut r.pre_script, "pm.request.headers.upsert({ key: \"X-Trace\", value: pm.variables.get(\"run_id\") });"),
                    (
                        "On the response",
                        &mut r.post_script,
                        "pm.test(\"ok\", () => pm.response.to.have.status(200));\npm.variables.set(\"token\", pm.response.json().token);",
                    ),
                ] {
                    ui.label(RichText::new(label).size(12.0).family(theme::bold()));
                    ui.add(
                        TextEdit::multiline(script)
                            .id(egui::Id::unique(("bscript", label, uid)))
                            .code_editor()
                            .desired_rows(3)
                            .desired_width(f32::INFINITY)
                            .hint_text(hint_rt(hint)),
                    );
                }
            });
        }
        BlockKind::WebSocket(w) => {
            ui.horizontal(|ui| {
                ui.label(RichText::new("URL").family(theme::bold()).size(12.5));
                let width = (ui.available_width() - 8.0).max(160.0);
                field(ui, &mut w.url, width, "wss://echo.websocket.org  or  /path (the base URL as ws:// / wss://)");
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Save as").size(12.5));
                field(ui, &mut w.save_as, 100.0, "ws");
                ui.label("wait");
                ui.add(egui::DragValue::new(&mut w.wait_ms).range(1..=600_000).speed(50).suffix(" ms"))
                    .on_hover_text("How long to wait for answers after the last message or answer");
                ui.label("at most");
                ui.add(egui::DragValue::new(&mut w.max_messages).range(0..=100_000).custom_formatter(|v, _| if v == 0.0 { "any".into() } else { format!("{v}") }))
                    .on_hover_text("Stop after this many answers (any = no limit)");
                ui.label("until it says");
                field(ui, &mut w.until, 120.0, "\"done\"");
                ui.checkbox(&mut w.stop_on_error, "Stop on error");
            });
            ui.label(RichText::new("Messages, sent in order once connected (templates allowed):").size(12.0).color(theme::MUTED));
            let mut remove = None;
            for (i, m) in w.messages.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    if ui.small_button("✕").clicked() {
                        remove = Some(i);
                    }
                    ui.add(TextEdit::multiline(m).id(egui::Id::unique(("wsm", i, uid))).code_editor().desired_rows(1).desired_width(f32::INFINITY));
                });
            }
            if let Some(i) = remove {
                w.messages.remove(i);
            }
            if ui.small_button("+ Message").clicked() {
                w.messages.push(String::new());
            }
            let nh = w.headers.iter().filter(|q| q.enabled && !q.key.is_empty()).count();
            CollapsingHeader::new(with_count("Headers", nh)).id_salt(("wsh", uid)).show(ui, |ui| kv_editor(ui, &mut w.headers, ("wshkv", uid)));
            live_extras(ui, &mut w.auth, &mut w.profile, uid);
            ui.label(
                RichText::new(format!(
                    "Then {{{{{0}.received}}}} (the answers, JSON parsed), {{{{{0}.received[-1]}}}}, {{{{{0}.connected}}}}, {{{{{0}.sent}}}}.",
                    w.save_as.trim()
                ))
                .size(11.5)
                .color(theme::MUTED),
            );
        }
        BlockKind::Grpc(g) => {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Server").family(theme::bold()).size(12.5));
                field(ui, &mut g.url, 260.0, "https://grpc.example.com:443 (empty: the profile's)");
                ui.label(RichText::new("Method").family(theme::bold()).size(12.5));
                let width = (ui.available_width() - 8.0).max(160.0);
                field(ui, &mut g.method, width, "/package.Service/Method");
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Save as").size(12.5));
                field(ui, &mut g.save_as, 100.0, "rpc");
                ui.label("timeout");
                ui.add(egui::DragValue::new(&mut g.timeout_ms).range(1..=600_000).speed(100).suffix(" ms"));
                ui.checkbox(&mut g.stop_on_error, "Stop on a non-OK status");
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new(".proto files").size(12.5));
                let mut files = g.proto_files.join(", ");
                if ui
                    .add(TextEdit::singleline(&mut files).hint_text("empty: server reflection · ~/protos/a.proto, …").desired_width(ui.available_width().max(160.0)))
                    .changed()
                {
                    g.proto_files = files.split(',').map(|x| x.trim().to_owned()).filter(|x| !x.is_empty()).collect();
                }
            });
            if !g.proto_files.is_empty() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Include folders").size(12.5));
                    let mut inc = g.proto_includes.join(", ");
                    if ui.add(TextEdit::singleline(&mut inc).hint_text("~/protos").desired_width(ui.available_width().max(160.0))).changed() {
                        g.proto_includes = inc.split(',').map(|x| x.trim().to_owned()).filter(|x| !x.is_empty()).collect();
                    }
                });
            }
            ui.label(RichText::new("Message (JSON, templates allowed; client streaming: a JSON array)").size(12.0).color(theme::MUTED));
            ui.add(TextEdit::multiline(&mut g.message).id(egui::Id::unique(("grpcm", uid))).code_editor().desired_rows(4).desired_width(f32::INFINITY));
            let nh = g.metadata.iter().filter(|q| q.enabled && !q.key.is_empty()).count();
            CollapsingHeader::new(with_count("Metadata", nh)).id_salt(("grpch", uid)).show(ui, |ui| kv_editor(ui, &mut g.metadata, ("grpchkv", uid)));
            live_extras(ui, &mut g.auth, &mut g.profile, uid);
            ui.label(
                RichText::new(format!(
                    "Then {{{{{0}.message}}}} (the first answer), {{{{{0}.messages}}}}, {{{{{0}.status}}}} (0 = OK), {{{{{0}.status_name}}}}.",
                    g.save_as.trim()
                ))
                .size(11.5)
                .color(theme::MUTED),
            );
        }
        BlockKind::Extract { from, into } => {
            ui.horizontal(|ui| {
                ui.label("Take");
                field(ui, from, 260.0, "user.body.id");
                ui.label("→ into variable");
                field(ui, into, 140.0, "user_id");
            });
        }
        BlockKind::Set { name, value } => {
            ui.horizontal(|ui| {
                field(ui, name, 140.0, "name");
                ui.label("=");
                field(ui, value, (ui.available_width() - 120.0).max(160.0), "text, number, JSON, {{expr}} or {{now}}");
                crate::ui_date::date_field(ui, value, egui::Id::unique(("setdate", uid)), false, true);
            });
        }
        BlockKind::ForEach { list, item, blocks, parallel } => {
            ui.horizontal(|ui| {
                ui.label("For each");
                field(ui, item, 100.0, "item");
                ui.label("in");
                field(ui, list, 260.0, "projects.body");
                ui.label(RichText::new("({{index}} is the position)").size(11.5).color(theme::MUTED));
            });
            ui.horizontal(|ui| {
                ui.checkbox(parallel, "Parallel").on_hover_text(
                    "Runs several items at once, as fast as the API's per-second limit allows (Settings › Parallel loops). \
                     Each item starts from the variables before the loop and can't see what the others set; \
                     the last item's variables are kept after the loop. The console still shows the items in order.",
                );
                if *parallel {
                    ui.label(RichText::new("items run at the same time, at the API's rate").size(11.5).color(theme::MUTED));
                }
            });
            nested(ui, doc, "Do:", blocks, depth, cx, salt);
        }
        BlockKind::If { left, op, right, then_blocks, else_blocks } => {
            ui.horizontal(|ui| {
                field(ui, left, 220.0, "{{p.status}}");
                egui::ComboBox::from_id_salt(("op", uid)).width(110.0).selected_text(op.label()).show_ui(ui, |ui| {
                    card_popup(ui);
                    for o in CompareOp::ALL {
                        ui.selectable_value(op, o, o.label());
                    }
                });
                if !op.unary() {
                    field(ui, right, 200.0, "finished");
                    crate::ui_date::date_field(ui, right, egui::Id::unique(("ifdate", uid)), false, true);
                }
            });
            nested(ui, doc, "Then:", then_blocks, depth, cx, salt);
            nested(ui, doc, "Else:", else_blocks, depth, cx, salt);
        }
        BlockKind::Wait { ms } => {
            ui.horizontal(|ui| {
                ui.label("Wait");
                ui.add(egui::DragValue::new(ms).range(0..=600_000).speed(50).suffix(" ms"));
            });
        }
        BlockKind::Log { message } => {
            field(ui, message, f32::INFINITY, "Hello {{user.body.login}}");
        }
        BlockKind::Fail { message } => {
            field(ui, message, f32::INFINITY, "Stops the flow with this message");
        }
        BlockKind::Flow(c) => flow_call_ui(ui, c, uid, cx),
    }
}

/// A "Run a flow" block: the other flow's inputs to fill and the outputs it gives back.
fn flow_call_ui(ui: &mut Ui, c: &mut FlowCall, uid: u64, cx: &mut Cx) {
    let sub = flow::find_flow(cx.library.iter().copied(), c.flow_uid, &c.flow);
    profile_row(ui, &mut c.profile, uid, "this flow's");
    ui.horizontal(|ui| {
        ui.label(RichText::new("Flow").size(12.5));
        let shown = if c.flow.trim().is_empty() { "— pick a flow —".to_owned() } else { c.flow.clone() };
        egui::ComboBox::from_id_salt(("subflow", uid)).width(280.0).selected_text(RichText::new(shown).family(theme::bold())).show_ui(ui, |ui| {
            card_popup(ui);
            let mut any = false;
            for f in cx.library.iter().filter(|f| f.uid != cx.this) {
                any = true;
                let picked = c.flow_uid == Some(f.uid);
                let resp = ui.selectable_label(picked, &f.name);
                let resp = if f.description.is_empty() { resp } else { resp.on_hover_text(&f.description) };
                if resp.clicked() {
                    c.flow = f.name.clone();
                    c.flow_uid = Some(f.uid);
                }
            }
            if !any {
                ui.label(RichText::new("No other saved flow in your workspace").color(theme::MUTED));
            }
        });
        ui.label(RichText::new("Save outputs as").size(12.5));
        field(ui, &mut c.save_as, 110.0, "sub");
        if let Some(f) = sub {
            if ui.small_button("Open").on_hover_text("Open this flow in a tab").clicked() {
                cx.actions.push(Action::OpenNode(f.uid));
            }
        }
    });
    let Some(sub) = sub else {
        if !c.flow.trim().is_empty() {
            ui.label(RichText::new(format!("No flow named “{}” in your workspace (save it first).", c.flow.trim())).size(12.0).color(theme::RED));
        }
        return;
    };
    // Keep the name in sync when the flow was renamed.
    if c.flow != sub.name {
        c.flow = sub.name.clone();
    }
    if c.flow_uid != Some(sub.uid) {
        c.flow_uid = Some(sub.uid);
    }
    if !sub.description.trim().is_empty() {
        ui.add(Label::new(RichText::new(&sub.description).size(12.0).color(theme::MUTED)).wrap());
    }
    let inputs: Vec<&FlowInput> = sub.inputs.iter().filter(|i| !i.name.trim().is_empty()).collect();
    if inputs.is_empty() {
        ui.label(RichText::new("No input to fill.").size(12.0).color(theme::MUTED));
    } else {
        group_title(ui, "Inputs — empty keeps the flow's own value", theme::CYAN);
        for input in inputs {
            let name = input.name.trim().to_owned();
            let v = c.inputs.entry(name.clone()).or_default();
            ui.horizontal(|ui| {
                fixed(ui, 170.0, |ui| {
                    ui.label(RichText::new(&name).monospace().family(theme::bold()).color(theme::NAVY));
                    if input.kind != InputType::Auto && input.kind != InputType::String {
                        pill(ui, input.kind.label(), theme::MUTED, theme::CLOUDS);
                    }
                });
                let hint = if input.value.trim().is_empty() { format!("{{{{{name}}}}} or a value") } else { format!("default: {}", input.value.trim()) };
                ui.add(
                    TextEdit::singleline(v)
                        .id(egui::Id::unique(("subinput", uid, &name)))
                        .font(TextStyle::Monospace)
                        .desired_width(VALUE_W - 40.0)
                        .hint_text(hint_rt(&hint)),
                );
                match input.kind {
                    InputType::Boolean | InputType::Enum => {
                        let choices: Vec<String> = if input.kind == InputType::Boolean { vec!["true".into(), "false".into()] } else { input.options.clone() };
                        if !choices.is_empty() {
                            ui.menu_button("▾", |ui| {
                                for o in choices {
                                    if ui.button(&o).clicked() {
                                        *v = o;
                                        ui.close();
                                    }
                                }
                            })
                            .response
                            .on_hover_text("Pick a value");
                        }
                    }
                    InputType::Date => crate::ui_date::date_field(ui, v, egui::Id::unique(("subdate", uid, &name)), false, true),
                    InputType::File => browse_file(ui, v),
                    _ => {}
                }
                if !input.description.trim().is_empty() {
                    ui.add(Label::new(RichText::new(&input.description).size(12.0).color(theme::MUTED)).wrap());
                }
            });
        }
    }
    // The form adds an empty entry per input drawn: keep only what was filled.
    c.inputs.retain(|_, v| !v.trim().is_empty());
    let save_as = if c.save_as.trim().is_empty() { "sub" } else { c.save_as.trim() };
    let outputs: Vec<&FlowOutput> = sub.outputs.iter().filter(|o| !o.name.trim().is_empty()).collect();
    if outputs.is_empty() {
        ui.label(RichText::new("This flow exposes no output (add some at the end of it, in Outputs).").size(12.0).color(theme::MUTED));
    } else {
        group_title(ui, "Outputs — use them in the next blocks", theme::GREEN);
        for o in outputs {
            ui.horizontal(|ui| {
                let var = format!("{{{{{save_as}.{}}}}}", o.name.trim());
                if ui.add(Label::new(RichText::new(&var).monospace().color(theme::CYAN_DARK)).sense(egui::Sense::click())).on_hover_text("Click to copy").clicked() {
                    ui.ctx().copy_text(var.clone());
                }
                if !o.description.trim().is_empty() {
                    ui.label(RichText::new(&o.description).size(12.0).color(theme::MUTED));
                }
            });
        }
    }
}

/// The edit-mode visualization script of the flow.
fn visualizer_ui(ui: &mut Ui, st: &mut FlowState, tab_id: TabId, actions: &mut Vec<Action>) {
    let has = !st.flow.visualizer.trim().is_empty();
    CollapsingHeader::new(RichText::new(if has { "VISUALIZATION ●" } else { "VISUALIZATION" }).family(theme::bold()).color(theme::NAVY))
        .id_salt(("flow_vis", st.id))
        .default_open(has)
        .show(ui, |ui| {
            ui.label(
                RichText::new("HTML + JavaScript opened in your browser after a run (Visualize). It gets flow (name, ok), outputs, vars, and body: the only output, or the outputs, or every variable — so the request templates work here too. Postman scripts work as well (pm.response.json() = body, pm.variables.get(name), pm.visualizer.set).")
                    .size(12.0)
                    .color(theme::MUTED),
            );
            results_row(ui, st, tab_id, actions);
            ui.horizontal(|ui| {
                ui.menu_button("Start from a template", |ui| {
                    for (name, script) in crate::visualize::FLOW_TEMPLATES {
                        if ui.button(name).clicked() {
                            st.flow.visualizer = script.to_owned();
                            ui.close();
                        }
                    }
                });
                let open = Button::new(RichText::new("Open with the last run ↗").family(theme::bold()).color(theme::WHITE)).fill(theme::CYAN);
                if ui.add_enabled(st.last_vars.is_some(), open).on_disabled_hover_text("Run the flow first").clicked() {
                    actions.push(Action::VisualizeFlow(tab_id));
                }
                if has && ui.small_button("Clear").clicked() {
                    st.flow.visualizer.clear();
                }
                if !has {
                    ui.label(RichText::new("Empty: Visualize shows the outputs (or the variables)").size(12.0).color(theme::MUTED));
                }
            });
            ui.add(
                TextEdit::multiline(&mut st.flow.visualizer)
                    .id(egui::Id::unique(("flow_visualizer", st.id)))
                    .code_editor()
                    .desired_rows(14)
                    .desired_width(f32::INFINITY)
                    .hint_text(hint_rt("<h3 id=\"t\"></h3>\n<script>\n  document.getElementById(\"t\").textContent = Object.keys(outputs).join(\", \");\n</script>")),
            );
        });
}

/// The edit-mode list of outputs.
fn outputs_ui(ui: &mut Ui, outputs: &mut Vec<FlowOutput>) {
    let mut remove = None;
    if !outputs.is_empty() {
        ui.horizontal(|ui| {
            for (title, w) in [("Name", 160.0), ("Value", VALUE_W), ("Description", 0.0)] {
                fixed(ui, w, |ui| {
                    ui.label(RichText::new(title).family(theme::bold()));
                });
            }
        });
    }
    for (i, o) in outputs.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.add_sized([160.0, 24.0], TextEdit::singleline(&mut o.name).font(TextStyle::Monospace).hint_text(hint_rt("user_id")));
            ui.add_sized([VALUE_W - 8.0, 24.0], TextEdit::singleline(&mut o.value).font(TextStyle::Monospace).hint_text(hint_rt("{{user.body.id}}")));
            ui.add_space(8.0);
            ui.add_sized([300.0, 24.0], TextEdit::singleline(&mut o.description).hint_text(hint_rt("what is it?")));
            if ui.small_button("×").on_hover_text("Remove").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        outputs.remove(i);
    }
    if ui.button("+ Add output").clicked() {
        outputs.push(FlowOutput::default());
    }
}

/// The documented endpoint's fields, filled like in a request tab (templates allowed).
fn documented_fields(ui: &mut Ui, doc: &ApiDoc, d: &DocRef, r: &mut flow::RequestBlock, uid: u64, cx: &mut Cx) {
    let Some((res, m)) = doc.lookup(d) else { return };
    let Some(route) = m.routes.get(d.route) else { return };
    let fields = model::field_params(m, &route.path);
    ui.horizontal(|ui| {
        ui.label(RichText::new("Documented endpoint:").size(12.0).color(theme::MUTED));
        ui.label(RichText::new(format!("{} › {}", res.name, m.name)).size(12.0).family(theme::bold()).color(theme::CYAN_DARK))
            .on_hover_text(format!("{} {}", route.verb, route.path));
        if ui.small_button("Docs").on_hover_text("Open this endpoint in a tab").clicked() {
            cx.actions.push(Action::Open(d.clone()));
        }
    });
    if fields.is_empty() {
        return;
    }
    let set: usize = fields.iter().map(|p| p.count_set(&r.values)).sum();
    let leaves: usize = fields.iter().map(|p| p.leaf_count()).sum();
    let has_body = model::has_body(&r.verb);
    let title = format!("{} ({set} of {leaves} filled)", if has_body { "Body fields" } else { "Query parameters from the docs" });
    CollapsingHeader::new(RichText::new(title).family(theme::bold())).id_salt(("bdoc", uid)).default_open(set > 0).show(ui, |ui| {
        let help = if has_body {
            "Sent as body fields (a file field uploads the file). Variables work: {{user_id}}, {{now}}. The JSON body below wins for the same key."
        } else {
            "Sent as query parameters. Variables work: {{login}}, {{now-7d}}."
        };
        ui.label(RichText::new(help).size(11.5).color(theme::MUTED));
        let filter_id = egui::Id::unique(("bdoc_filter", uid));
        let mut filter: String = ui.data(|dd| dd.get_temp(filter_id)).unwrap_or_default();
        if leaves > 6 {
            ui.horizontal(|ui| {
                ui.add(TextEdit::singleline(&mut filter).id(filter_id.with("edit")).hint_text(hint_rt("Filter fields…")).desired_width(240.0));
                if !filter.is_empty() && ui.small_button("×").clicked() {
                    filter.clear();
                }
                if ui.add_enabled(set > 0, Button::new("Clear all").small()).clicked() {
                    r.values.clear();
                }
            });
        }
        let needle = filter.trim().to_lowercase();
        ui.data_mut(|dd| dd.insert_temp(filter_id, filter));
        let links = crate::ui_request::Links { doc, resource: Some(res.key.as_str()) };
        let (required, optional): (Vec<&Param>, Vec<&Param>) = fields.into_iter().partition(|p| p.has_required());
        if !required.is_empty() {
            group_title(ui, "Required — you must add", theme::RED);
            for p in &required {
                crate::ui_request::param_ui(ui, p, &mut r.values, 0, &needle, uid, &links, cx.actions);
            }
        }
        if !optional.is_empty() {
            group_title(ui, "Optional — you can add", theme::CYAN);
            for p in &optional {
                crate::ui_request::param_ui(ui, p, &mut r.values, 0, &needle, uid, &links, cx.actions);
            }
        }
    });
    // The form adds an empty entry for every field it draws: keep only what was filled.
    r.values.retain(|_, v| !v.is_empty());
}

fn endpoint_picker(ui: &mut Ui, doc: &ApiDoc, filter: &mut String) -> Option<(DocRef, String, String)> {
    ui.set_min_width(520.0);
    let searched = ui.add(TextEdit::singleline(filter).hint_text(hint_rt("Search endpoints… (e.g. users projects)")).desired_width(f32::INFINITY)).changed();
    let mut picked = None;
    let mut list = ScrollArea::vertical().max_height(380.0);
    if searched {
        list = list.vertical_scroll_offset(0.0);
    }
    list.show(ui, |ui| {
        let mut shown = 0;
        'outer: for hit in doc.search(filter) {
            let res = &doc.resources[hit.resource];
            {
                for (mi, ri) in hit.routes {
                    let m = &res.methods[mi];
                    let r = &m.routes[ri];
                    shown += 1;
                    if shown > 150 {
                        ui.label(RichText::new("… refine the search").color(theme::MUTED));
                        break 'outer;
                    }
                    ui.horizontal(|ui| {
                        fixed(ui, 54.0, |ui| {
                            ui.label(RichText::new(theme::short_verb(&r.verb)).family(theme::bold()).size(11.5).color(theme::verb_color(&r.verb)));
                        });
                        let resp = ui.selectable_label(false, RichText::new(&r.path).monospace()).on_hover_text(format!("{} › {}", res.name, m.name));
                        if resp.clicked() {
                            picked = Some((DocRef { resource: res.key.clone(), method: m.name.clone(), route: ri }, r.verb.clone(), r.path.clone()));
                        }
                    });
                }
            }
        }
    });
    picked
}

// ==================================================================== console

fn log_color(kind: LogKind) -> Color32 {
    match kind {
        LogKind::Info => theme::CLOUDS,
        LogKind::Request => theme::SILVER,
        LogKind::Response => theme::GREEN,
        LogKind::Error => theme::RED,
        LogKind::Success => theme::CYAN,
        LogKind::Var => theme::YELLOW,
        LogKind::Warning => theme::ORANGE,
    }
}

/// The lines matching `needle` as (index, in its detail): computed again only when the search or the log changes
/// (lowercasing 5000 lines and their details every frame was the console's main cost).
fn log_hits<'a>(st: &'a mut FlowState, needle: &str) -> &'a [(usize, bool)] {
    let fresh = st.log_hits.as_ref().is_some_and(|(n, len, _)| n == needle && *len == st.log.len());
    if !fresh {
        let hits = st.log.iter().enumerate().filter_map(|(i, l)| {
            let (matched, in_detail) = line_matches(l, needle);
            matched.then_some((i, in_detail))
        });
        st.log_hits = Some((needle.to_owned(), st.log.len(), hits.collect()));
    }
    &st.log_hits.as_ref().expect("set above").2
}

fn line_matches(line: &LogLine, needle: &str) -> (bool, bool) {
    if needle.is_empty() {
        return (true, false);
    }
    let in_text = line.text.to_lowercase().contains(needle);
    let in_detail = line.detail.as_deref().is_some_and(|d| d.to_lowercase().contains(needle));
    (in_text || in_detail, in_detail)
}

pub fn console_ui(ui: &mut Ui, st: &mut FlowState, tab_id: TabId, actions: &mut Vec<Action>) {
    let needle = st.search.trim().to_lowercase();
    // The controls first, from the right; the title and the run's state in what's left (they used to be drawn under the controls).
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if !st.log.is_empty() && st.run.is_none() && ui.small_button("Clear").on_hover_text("Clear the console").clicked() {
                st.reset_console();
            }
            if st.last_vars.is_some() && ui.small_button("Visualize ↗").on_hover_text("Open the results in your browser").clicked() {
                actions.push(Action::VisualizeFlow(tab_id));
            }
            if ui.small_button("Copy").on_hover_text("Copy what is shown (search applied)").clicked() {
                let text = match st.console {
                    ConsoleTab::Log => {
                        let hits: Vec<usize> = log_hits(st, &needle).iter().map(|&(i, _)| i).collect();
                        hits.iter().map(|&i| format!("{}{}", "  ".repeat(st.log[i].depth), st.log[i].text)).collect::<Vec<_>>().join("\n")
                    }
                    ConsoleTab::Variables if needle.is_empty() => st.vars_text.clone(),
                    ConsoleTab::Variables => {
                        let (leaves, hits) = search_leaves(st, &needle);
                        hits.iter().map(|&i| format!("{} = {}", leaves[i].0, leaves[i].1)).collect::<Vec<_>>().join("\n")
                    }
                };
                ui.ctx().copy_text(text);
            }
            if !st.search.is_empty() && ui.small_button("×").on_hover_text("Clear the search").clicked() {
                st.search.clear();
            }
            // What the search may take: the tabs and the title keep their room.
            let search_w = (ui.available_width() - 440.0).clamp(80.0, 220.0);
            ui.add(TextEdit::singleline(&mut st.search).desired_width(search_w).hint_text(if st.console == ConsoleTab::Log {
                "Search logs…"
            } else {
                "Search variables (path or value)…"
            }));
            ui.add_space(6.0);
            dark_subtab(ui, &mut st.console, ConsoleTab::Variables, "Variables");
            dark_subtab(ui, &mut st.console, ConsoleTab::Log, "Log");
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                // Never under the controls: what doesn't fit is cut.
                ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                ui.label(RichText::new("Console").family(theme::bold()).size(17.0).color(theme::WHITE));
                ui.add_space(8.0);
                if let Some(run) = &st.run {
                    ui.add(egui::Spinner::new().color(theme::CYAN));
                    ui.label(RichText::new(format!("running… {:.1}s", run.started.elapsed().as_secs_f32())).color(theme::SILVER));
                    if ui.small_button("Stop").clicked() {
                        actions.push(Action::StopFlow(tab_id));
                    }
                } else if let Some((ok, d)) = st.result {
                    let (text, color) = if ok { ("finished", theme::GREEN) } else { ("failed", theme::RED) };
                    pill(ui, &format!("{text} · {:.1}s", d.as_secs_f32()), theme::WHITE, color);
                }
            });
        });
    });
    // The run's numbers on their own line: next to the controls they ran under the search field.
    if st.progress.is_some() || st.rate.is_some() {
        ui.horizontal_wrapped(|ui| {
            // Same numbers as a request's response, updated after each call of the run.
            if let Some(p) = &st.progress {
                let label = if p.parallel { "Loop ⇉" } else { "Loop" };
                stat(ui, label, &p.text());
                ui.add(egui::ProgressBar::new(p.done as f32 / p.total.max(1) as f32).desired_width(90.0).desired_height(8.0).fill(theme::CYAN)).on_hover_text(format!(
                    "for each {}{}",
                    p.label,
                    if p.parallel { " (parallel)" } else { "" }
                ));
                // The rest of the loop won't fit in what's left of the hour.
                if let (Some(need), Some(left)) = (p.requests_left, st.rate.and_then(|r| r.hourly_left)) {
                    if need > left {
                        ui.label(RichText::new(format!("⚠ ~{need} requests left, {left} this hour")).size(12.0).color(theme::ORANGE))
                            .on_hover_text("The hourly limit will be reached before the end: the API will answer 429 until the next hour");
                    }
                }
            }
            if let Some(rate) = st.rate {
                ui.add_space(6.0);
                stat(ui, "Requests", &rate.requests.to_string());
                if let Some(run) = &st.run {
                    let secs = run.started.elapsed().as_secs_f64();
                    if secs > 1.0 && rate.requests > 1 {
                        stat(ui, "Speed", &format!("{:.1}/s", rate.requests as f64 / secs));
                    }
                }
                if let Some(left) = rate.per_second_left {
                    let of = rate.per_second_limit.map(|l| format!(" of {l}")).unwrap_or_default();
                    stat(ui, "Rate limit", &format!("{left}{of}/s left"));
                }
                if rate.reused > 0 {
                    stat(ui, "Reused", &rate.reused.to_string());
                }
                let asserts = rate.asserts_passed + rate.asserts_failed;
                if asserts > 0 {
                    stat(ui, "Tests", &format!("{}/{asserts}", rate.asserts_passed));
                }
                if rate.throttled > 0 {
                    stat(ui, "429", &format!("{} retried", rate.throttled));
                }
                if let Some(left) = rate.hourly_left {
                    let of = rate.hourly_limit.map(|l| format!(" of {l}")).unwrap_or_default();
                    stat(ui, "Hourly", &format!("{left}{of} left"));
                }
            }
        });
    }
    // Every variable: only the lines in view are drawn (a big run is megabytes of JSON).
    if st.console == ConsoleTab::Variables && needle.is_empty() {
        if st.last_vars.is_none() {
            ui.label(RichText::new("Variables appear here after a run.").color(theme::SILVER));
        } else {
            code_view(
                ui,
                ("console_vars", st.id),
                CodeView { text: &st.vars_text, lines: &st.vars_lines, json: true, marks: &[], mark_len: 0, current: 0, scroll_to: None },
            );
        }
        return;
    }
    if st.console == ConsoleTab::Log {
        log_hits(st, &needle);
    }
    let area = ScrollArea::both().id_salt(("console", st.id)).auto_shrink([false, false]).stick_to_bottom(st.console == ConsoleTab::Log && needle.is_empty());
    match st.console {
        ConsoleTab::Log => {
            area.show_viewport(ui, |ui, viewport| log_lines(ui, st, &needle, viewport));
        }
        ConsoleTab::Variables => {
            area.show(ui, |ui| {
                if st.last_vars.is_none() {
                    ui.label(RichText::new("Variables appear here after a run.").color(theme::SILVER));
                } else {
                    vars_matches(ui, st, &needle);
                }
            });
        }
    }
}

/// The log lines matching the search; only those in `viewport` are laid out (a run has thousands, each
/// with its collapsible detail). The others take the height they had when last shown.
fn log_lines(ui: &mut Ui, st: &mut FlowState, needle: &str, viewport: Rect) {
    if st.log.is_empty() {
        ui.add_space(16.0);
        ui.label(RichText::new("Press Run: calls, extracted values and logs show up here. Untick “log” on a block to hide it.").color(theme::SILVER));
        return;
    }
    let spacing = ui.spacing().item_spacing.y;
    let row_h = ui.spacing().interact_size.y;
    st.log_heights.resize(st.log.len(), row_h);
    let hits = st.log_hits.as_ref().map(|h| h.2.as_slice()).unwrap_or_default();
    let total: f32 = hits.iter().map(|&(i, _)| st.log_heights[i] + spacing).sum();
    let origin = ui.max_rect().min;
    let visible = viewport.translate(origin.to_vec2());
    let width = st.log_width.max(ui.available_width());
    let (area, _) = ui.allocate_exact_size(vec2(width, total), egui::Sense::hover());
    let mut y = area.top();
    let mut changed = false;
    for &(i, in_detail) in hits {
        let h = st.log_heights[i];
        let row = Rect::from_min_size(egui::pos2(area.left(), y), vec2(width, h));
        y += h + spacing;
        if row.bottom() < visible.top() || row.top() > visible.bottom() {
            continue;
        }
        let line = &st.log[i];
        let slot = Rect::from_min_size(row.min, vec2(f32::INFINITY, f32::INFINITY));
        let drawn = ui
            .scope_builder(egui::UiBuilder::new().max_rect(slot).layout(Layout::left_to_right(Align::Min)), |ui| {
                // As tall as a collapsible line: the console keeps an even rhythm.
                ui.set_min_height(row_h);
                ui.add_space(line.depth as f32 * 18.0);
                let text = RichText::new(&line.text).monospace().color(log_color(line.kind));
                match &line.detail {
                    Some(detail) => {
                        let mut header = CollapsingHeader::new(text).id_salt(("log", st.id, i));
                        if in_detail {
                            header = header.open(Some(true));
                        }
                        header.show(ui, |ui| {
                            ui.add(Label::new(RichText::new(detail).monospace().size(12.0).color(theme::SILVER)).selectable(true).extend());
                        });
                    }
                    None => {
                        ui.add(Label::new(text).selectable(true).extend());
                    }
                }
            })
            .response
            .rect;
        if (drawn.height() - h).abs() > 0.5 {
            st.log_heights[i] = drawn.height();
            changed = true;
        }
        if drawn.right() - area.left() > st.log_width {
            st.log_width = drawn.right() - area.left();
            changed = true;
        }
    }
    // A detail opened or closed: the lines below move, next frame.
    if changed {
        ui.ctx().request_repaint();
    }
    if !needle.is_empty() {
        ui.add_space(6.0);
        ui.label(RichText::new(format!("{} of {} lines match", hits.len(), st.log.len())).size(12.0).color(theme::CONCRETE));
    }
}

/// The variables as (path, value) and the ones matching `needle`: built on the first search, then per new search only.
fn search_leaves<'a>(st: &'a mut FlowState, needle: &str) -> (&'a [(String, String)], &'a [usize]) {
    if st.leaves.is_none() {
        let mut leaves = Vec::new();
        if let Some(vars) = &st.last_vars {
            flow::flatten("", vars, &mut leaves);
        }
        st.leaves = Some(leaves);
    }
    if st.leaves_search.as_ref().is_none_or(|(n, _)| n != needle) {
        let leaves = st.leaves.as_deref().unwrap_or_default();
        let hits = leaves.iter().enumerate().filter(|(_, (p, v))| p.to_lowercase().contains(needle) || v.to_lowercase().contains(needle)).map(|(i, _)| i).collect();
        st.leaves_search = Some((needle.to_owned(), hits));
    }
    (st.leaves.as_deref().unwrap_or_default(), &st.leaves_search.as_ref().expect("set above").1)
}

/// Search results in the variables: one row per matching value, with its `{{path}}`.
fn vars_matches(ui: &mut Ui, st: &mut FlowState, needle: &str) {
    const MAX: usize = 400;
    let (leaves, hits) = search_leaves(st, needle);
    let matches: Vec<&(String, String)> = hits.iter().map(|&i| &leaves[i]).collect();
    ui.label(RichText::new(format!("{} matches — click a path to copy it as {{{{…}}}}", matches.len())).size(12.0).color(theme::CONCRETE));
    for (path, value) in matches.iter().take(MAX) {
        ui.horizontal(|ui| {
            let r = ui.add(Label::new(RichText::new(path).monospace().color(theme::CYAN)).sense(egui::Sense::click())).on_hover_text("Copy as {{…}}");
            if r.clicked() {
                ui.ctx().copy_text(format!("{{{{{path}}}}}"));
            }
            ui.label(RichText::new("=").monospace().color(theme::CONCRETE));
            let shown = if value.chars().count() > 200 { format!("{}…", value.chars().take(200).collect::<String>()) } else { value.clone() };
            ui.add(Label::new(RichText::new(shown).monospace().color(theme::GREEN)).selectable(true));
        });
    }
    if matches.len() > MAX {
        ui.label(RichText::new(format!("… {} more, refine the search", matches.len() - MAX)).color(theme::CONCRETE));
    }
}

/// A new request block for a documented endpoint.
/// A documented endpoint as a request block, bound to its API's profile (a flow can mix APIs).
pub fn block_from_endpoint(doc: &ApiDoc, d: &DocRef) -> Block {
    let found = doc.lookup(d);
    let route = found.and_then(|(_, m)| m.routes.get(d.route));
    Block::new_kind(BlockKind::Request(flow::RequestBlock {
        verb: route.map_or_else(|| "GET".into(), |r| r.verb.clone()),
        path: route.map_or_else(|| "/".into(), |r| r.path.clone()),
        doc: Some(d.clone()),
        profile: found.map(|(res, _)| crate::profiles::of_api_id(&res.api)).unwrap_or_default(),
        ..Default::default()
    }))
}

/// A request (tab or saved) turned into a flow block; its documented fields stay editable in the block's form.
pub fn block_from_spec(doc: &ApiDoc, spec: &RequestSpec) -> Block {
    let method = spec.doc.as_ref().and_then(|d| doc.lookup(d)).map(|(_, m)| m);
    let form = method.is_some() && (!model::has_body(&spec.verb) || spec.body_mode == BodyMode::Form);
    let mut bare = spec.clone();
    if form {
        bare.values.clear();
    }
    let mut b = block_from_request(&model::prepare(&bare, method), &spec.headers, spec.doc.clone());
    if let BlockKind::Request(r) = &mut b.kind {
        if form {
            r.values = spec.values.iter().filter(|(_, v)| !v.trim().is_empty()).map(|(k, v)| (k.clone(), v.clone())).collect();
        }
        r.assertions = spec.assertions.clone();
    }
    b
}

/// A request tab turned into a flow block.
pub fn block_from_request(p: &model::Prepared, headers: &[Kv], doc: Option<DocRef>) -> Block {
    let body = p
        .body
        .as_deref()
        .map(|b| serde_json::from_str::<serde_json::Value>(b).ok().and_then(|v| serde_json::to_string_pretty(&v).ok()).unwrap_or_else(|| b.to_owned()))
        .unwrap_or_default();
    Block::new_kind(BlockKind::Request(flow::RequestBlock {
        save_as: "res".into(),
        verb: p.verb.clone(),
        path: p.path.clone(),
        query: p.query.iter().map(|(k, v)| Kv { enabled: true, key: k.clone(), value: v.clone() }).collect(),
        headers: headers.to_vec(),
        body,
        files: p.files.iter().map(|(k, v)| Kv { enabled: true, key: k.clone(), value: v.clone() }).collect(),
        values: Default::default(),
        all_pages: false,
        max_pages: None,
        stop_on_error: true,
        doc,
        assertions: Vec::new(),
        auth: p.auth.clone(),
        content_type: p.content_type.clone().filter(|t| t != "application/json").unwrap_or_default(),
        profile: p.profile.clone(),
        ..Default::default()
    }))
}

/// "⟳ Resume" when a stopped run of this flow was kept.
fn resume_button(ui: &mut Ui, st: &FlowState, tab_id: TabId, actions: &mut Vec<Action>, size: egui::Vec2) {
    let Some(info) = &st.resume else { return };
    let b = Button::new(RichText::new("⟳  Resume").family(theme::bold()).size(15.0).color(theme::WHITE)).fill(theme::CYAN).corner_radius(4).min_size(size);
    let tip = format!(
        "Goes on with the run stopped on {}: its {} responses are reused (not sent again), with the same inputs{}",
        crate::runs::when(info.stopped_at),
        info.responses,
        if info.changed { ".\n⚠ The flow was changed since: blocks moved or edited may not find their responses." } else { "" }
    );
    if ui.add(b).on_hover_text(tip).clicked() {
        actions.push(Action::ResumeFlow(tab_id));
    }
}

/// One line above Run: when the stopped run was, how far it got, with which inputs.
fn resume_banner(ui: &mut Ui, st: &FlowState, tab_id: TabId, actions: &mut Vec<Action>) {
    let Some(info) = &st.resume else { return };
    ui.horizontal_wrapped(|ui| {
        let inputs: Vec<String> = info.header.inputs.iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| format!("{k}={v}")).collect();
        let with = if inputs.is_empty() { String::new() } else { format!(" · inputs {}", inputs.join(", ")) };
        ui.label(
            RichText::new(format!("‖ Stopped run from {} · {} responses kept{with}", crate::runs::when(info.stopped_at), info.responses)).size(12.5).color(theme::ORANGE),
        );
        if info.changed {
            ui.label(RichText::new("(the flow was changed since)").size(12.0).color(theme::MUTED));
        }
        if ui.small_button("Forget").on_hover_text("Deletes the kept responses").clicked() {
            actions.push(Action::DiscardResume(tab_id));
        }
    });
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use egui_kittest::kittest::Queryable;
    use egui_kittest::Harness;

    /// The console header at different widths: title, state, run numbers and controls never overlap
    /// (the stats used to run under the search field, the state under the Log / Variables tabs).
    #[test]
    fn console_header_never_overlaps() {
        for width in [700.0, 1000.0, 1600.0] {
            let flow: Flow = serde_json::from_str(r#"{"name": "F", "blocks": []}"#).unwrap();
            let mut st = FlowState::new(1, flow);
            st.result = Some((true, Duration::from_millis(1500)));
            st.rate = Some(flow::Rate {
                requests: 1234,
                per_second_left: Some(7),
                per_second_limit: Some(8),
                hourly_left: Some(1100),
                hourly_limit: Some(1200),
                reused: 3,
                ..Default::default()
            });
            st.log.push(LogLine { kind: LogKind::Info, depth: 0, text: "▶ Running".into(), detail: None });
            st.last_vars = Some(serde_json::json!({}));
            let fonts_ready = std::cell::Cell::new(false);
            let mut actions = Vec::new();
            let harness = Harness::builder().with_size(vec2(width, 300.0)).build_ui(|ui| {
                // The app's fonts (bold is a named family) are there from the next frame.
                if !fonts_ready.replace(true) {
                    theme::install(ui.ctx());
                    return;
                }
                console_ui(ui, &mut st, TabId(1), &mut actions);
            });
            let rect = |label: &str| harness.get_by_label(label).rect();
            let header = ["Console", "Log", "Variables", "Copy", "Visualize ↗", "Clear"];
            let rects: Vec<(&str, egui::Rect)> = header.iter().map(|l| (*l, rect(l))).collect();
            for (i, (a, ra)) in rects.iter().enumerate() {
                for (b, rb) in &rects[i + 1..] {
                    assert!(!ra.shrink(0.5).intersects(rb.shrink(0.5)), "at {width} px, “{a}” {ra:?} overlaps “{b}” {rb:?}");
                }
            }
            // The run's numbers are on their own line, below every control.
            let stats = rect("Requests");
            let header_bottom = rects.iter().map(|(_, r)| r.bottom()).fold(f32::MIN, f32::max);
            assert!(stats.top() >= header_bottom - 0.5, "at {width} px, the stats ({stats:?}) are not below the header (bottom {header_bottom})");
        }
    }
}
