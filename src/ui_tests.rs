//! The test runner window (folder menu › Run tests): results as they come, failures open.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{vec2, Align2, Button, CollapsingHeader, RichText, ScrollArea};

use crate::app::Action;
use crate::testrun::{self, Item, TestResult};
use crate::theme;
use crate::widgets::*;

pub struct TestRun {
    /// `Users` or `the workspace`.
    pub title: String,
    pub items: Vec<Item>,
    pub results: Vec<TestResult>,
    pub rx: Receiver<TestResult>,
    pub stop: Arc<AtomicBool>,
    pub started: Instant,
    pub finished: Option<Duration>,
}

impl TestRun {
    pub fn poll(&mut self) {
        if self.finished.is_some() {
            return;
        }
        loop {
            match self.rx.try_recv() {
                Ok(r) => self.results.push(r),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.finished = Some(self.started.elapsed());
                    break;
                }
            }
        }
    }

    fn report(&self) -> String {
        let mut out = String::new();
        for r in &self.results {
            out.push_str(&format!("{} {}   {}{}\n", if r.passed { "✓" } else { "✗" }, r.path, r.what, r.status.map(|s| format!(" · {s}")).unwrap_or_default()));
            if let Some(e) = &r.error {
                out.push_str(&format!("    {e}\n"));
            }
            for o in r.outcomes.iter().filter(|o| !o.passed) {
                out.push_str(&format!("    ✗ {} — {}\n", o.text, o.detail));
            }
        }
        let (passed, total, a_ok, a_total) = testrun::totals(&self.results);
        out.push_str(&format!("\n{passed} passed, {} failed ({total}) · assertions {a_ok}/{a_total}\n", total - passed));
        out
    }
}

/// Returns false when the window is closed.
pub fn window(ctx: &egui::Context, run: &mut TestRun, actions: &mut Vec<Action>) -> bool {
    run.poll();
    let mut open = true;
    let (passed, total, a_ok, a_total) = testrun::totals(&run.results);
    egui::Window::new(RichText::new(format!("Tests · {}", run.title)).family(theme::bold()).size(17.0))
        .id(egui::Id::unique("test_runner"))
        .open(&mut open)
        .collapsible(false)
        .default_size(vec2(760.0, 520.0))
        .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                match run.finished {
                    None => {
                        ui.add(egui::Spinner::new().color(theme::CYAN));
                        ui.label(
                            RichText::new(format!("{} / {} · {:.1}s", run.results.len(), run.items.len(), run.started.elapsed().as_secs_f32())).color(theme::SILVER),
                        );
                        if ui.small_button("Stop").clicked() {
                            run.stop.store(true, Ordering::Relaxed);
                        }
                        ui.ctx().request_repaint_after(Duration::from_millis(200));
                    }
                    Some(d) => {
                        let (text, color) = if passed == total { ("all passed", theme::GREEN) } else { ("failed", theme::RED) };
                        pill(ui, text, theme::WHITE, color);
                        ui.label(RichText::new(format!("in {:.1}s", d.as_secs_f32())).color(theme::SILVER));
                    }
                }
                stat(ui, "Passed", &format!("{passed}/{total}"));
                if a_total > 0 {
                    stat(ui, "Assertions", &format!("{a_ok}/{a_total}"));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if run.finished.is_some() {
                        if ui.small_button("Save JUnit…").on_hover_text("JUnit XML report, for CI tools").clicked() {
                            if let Some(path) = rfd::FileDialog::new().add_filter("JUnit XML", &["xml"]).set_file_name("tests.xml").save_file() {
                                let _ = std::fs::write(path, testrun::junit(&run.title, &run.results, run.finished.unwrap_or_default()));
                            }
                        }
                        if ui.small_button("Copy report").clicked() {
                            ui.ctx().copy_text(run.report());
                        }
                    }
                });
            });
            ui.separator();
            ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                for (i, r) in run.results.iter().enumerate() {
                    let uid = run.items.iter().find(|it| it.path == r.path).map(|it| it.node.uid());
                    let mark = if r.passed { RichText::new("✓").color(theme::GREEN) } else { RichText::new("✗").color(theme::RED) };
                    let status = r.status.map(|s| format!(" · {s}")).unwrap_or_default();
                    let asserts = if r.outcomes.is_empty() { String::new() } else { format!(" · {}", crate::assert::summary(&r.outcomes)) };
                    let detail = format!("{}{status} · {} ms{asserts}", r.what, r.duration.as_millis());
                    let has_more = !r.outcomes.is_empty() || r.error.is_some();
                    ui.horizontal(|ui| {
                        ui.label(mark.family(theme::bold()));
                        ui.label(RichText::new(&r.path).family(theme::bold()).color(theme::WHITE));
                        ui.label(RichText::new(detail).size(12.0).color(theme::SILVER));
                        if let Some(uid) = uid {
                            if ui.add(Button::new("Open").small()).clicked() {
                                actions.push(Action::OpenNode(uid));
                            }
                        }
                    });
                    if has_more {
                        CollapsingHeader::new(RichText::new("details").size(12.0).color(theme::CONCRETE)).id_salt(("tr", i)).default_open(!r.passed).show(ui, |ui| {
                            if let Some(e) = &r.error {
                                ui.label(RichText::new(e).color(theme::RED));
                            }
                            crate::ui_request::outcomes_ui(ui, &r.outcomes);
                        });
                    }
                }
                if run.results.is_empty() && run.finished.is_some() {
                    ui.label(RichText::new("No request or flow here.").color(theme::SILVER));
                }
            });
        });
    if !open {
        run.stop.store(true, Ordering::Relaxed);
    }
    open
}
