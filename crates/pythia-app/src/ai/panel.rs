//! The assistant's side panel: the conversation, code from answers to
//! insert or apply, and the input for questions.

use egui::{Key, Modifiers, RichText, Ui};
use pythia_ai::Task;
use pythia_ai::answer::{Block, spans};
use pythia_config::AiSettings;

use super::{Assistant, Mode, Origin, Status};

pub struct PanelContext<'a> {
    pub ai: &'a AiSettings,
    /// Title of the window questions are about.
    pub focused: Option<String>,
}

/// What the panel asks the app to do.
#[derive(Debug, Clone, PartialEq)]
pub enum PanelAction {
    /// Ask about (or generate SQL for) the focused window.
    Submit(Task),
    /// Ask entry `n` again.
    Retry(usize),
    /// Insert code at the cursor of the request's editor (else the focused
    /// one).
    Insert {
        origin: Option<Origin>,
        text: String,
    },
    /// Replace the statement or selection entry `entry` was about.
    Replace {
        entry: usize,
        origin: Origin,
        text: String,
    },
    NewSqlWindow(String),
    OpenSettings,
}

impl Assistant {
    pub fn ui(&mut self, ui: &mut Ui, cx: &PanelContext) -> Vec<PanelAction> {
        let mut actions = Vec::new();
        self.poll();
        if cx.ai.backends.is_empty() {
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("AI assistant").heading());
                ui.add_space(6.0);
                ui.label(
                    "Set up a model first: a cloud API (Anthropic, OpenAI-compatible), \
                     a local model server (Ollama, llama.cpp) or a command line tool.",
                );
                ui.add_space(8.0);
                if ui.button("Open Settings → AI").clicked() {
                    actions.push(PanelAction::OpenSettings);
                }
            });
            return actions;
        }
        ui.horizontal(|ui| {
            let current = self.backend(cx.ai).cloned();
            let name = current.as_ref().map_or("", |b| b.name.as_str());
            egui::ComboBox::from_id_salt("assistant-backend")
                .selected_text(name)
                .show_ui(ui, |ui| {
                    for b in &cx.ai.backends {
                        let selected = current.as_ref().is_some_and(|c| c.id == b.id);
                        if ui.selectable_label(selected, &b.name).clicked() {
                            self.backend = Some(b.id.clone());
                        }
                    }
                })
                .response
                .on_hover_text("The model that answers");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(!self.entries.is_empty(), egui::Button::new("New chat"))
                    .on_hover_text("Start over; earlier questions are no longer sent")
                    .clicked()
                {
                    self.clear();
                }
            });
        });
        ui.separator();
        egui::Panel::bottom(ui.id().with("assistant-input"))
            .resizable(false)
            .show(ui, |ui| self.input_ui(ui, cx, &mut actions));
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    if self.entries.is_empty() {
                        ui.add_space(8.0);
                        ui.weak(
                            "Ask about SQL, PL/SQL or tuning, or describe a query to generate. \
                             Explain, fix and rewrite statements from an editor's context menu \
                             (AI), or analyze plans and slow SQL where they are shown.",
                        );
                    }
                    for i in 0..self.entries.len() {
                        self.entry_ui(ui, i, &mut actions);
                        ui.add_space(10.0);
                    }
                });
        });
        actions
    }

    fn input_ui(&mut self, ui: &mut Ui, cx: &PanelContext, actions: &mut Vec<PanelAction>) {
        ui.add_space(4.0);
        if let Some(notice) = self.notice.clone() {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(ui.visuals().warn_fg_color, notice);
                if ui.small_button("×").clicked() {
                    self.notice = None;
                }
            });
        }
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.mode, Mode::Ask, "Ask");
            ui.selectable_value(&mut self.mode, Mode::Generate, "Generate SQL");
            let about = match &cx.focused {
                Some(title) => format!("About {title}"),
                None => "No window to take context from".into(),
            };
            ui.weak(about).on_hover_text(
                "The focused window's connection and statement go with the question",
            );
        });
        let id = ui.make_persistent_id("assistant-question");
        self.input_id = Some(id);
        // Enter (or Ctrl+Enter) sends, Shift+Enter starts a new line.
        let enter = ui.memory(|m| m.has_focus(id))
            && ui.input_mut(|i| {
                !i.modifiers.shift
                    && (i.consume_key(Modifiers::NONE, Key::Enter)
                        || i.consume_key(Modifiers::COMMAND, Key::Enter))
            });
        let hint = match self.mode {
            Mode::Ask => "Ask about SQL, PL/SQL or tuning…",
            Mode::Generate => "Describe the query, e.g. orders from last month without an invoice",
        };
        let response = ui.add(
            egui::TextEdit::multiline(&mut self.input)
                .id(id)
                .hint_text(hint)
                .desired_rows(3)
                .desired_width(f32::INFINITY),
        );
        if std::mem::take(&mut self.focus_input) {
            response.request_focus();
        }
        ui.horizontal(|ui| {
            let busy = self.is_busy();
            let ready = !busy && !self.input.trim().is_empty();
            let send = ui
                .add_enabled(ready, egui::Button::new("Send"))
                .on_hover_text("Enter (Shift+Enter for a new line)");
            if (send.clicked() || enter) && ready {
                let text = std::mem::take(&mut self.input).trim().to_owned();
                actions.push(PanelAction::Submit(match self.mode {
                    Mode::Ask => Task::Ask(text),
                    Mode::Generate => Task::Generate(text),
                }));
            }
            if busy && ui.button("Stop").clicked() {
                self.stop();
            }
        });
        ui.add_space(2.0);
    }

    fn entry_ui(&mut self, ui: &mut Ui, i: usize, actions: &mut Vec<PanelAction>) {
        let e = &self.entries[i];
        let visuals = ui.visuals().clone();
        egui::Frame::new()
            .fill(visuals.faint_bg_color)
            .corner_radius(6.0)
            .inner_margin(6.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(e.turn.task.title()).strong());
                let mut about = vec![e.backend.clone()];
                if !e.source.is_empty() {
                    about.insert(0, e.source.clone());
                }
                ui.label(RichText::new(about.join(" · ")).small().weak());
                if let Some(sent) = &e.sent {
                    if !sent.withheld.is_empty() {
                        ui.label(
                            RichText::new(format!(
                                "Not sent: {} (allowed in Settings → AI)",
                                sent.withheld.join(", ")
                            ))
                            .small()
                            .color(visuals.warn_fg_color),
                        );
                    }
                    if !sent.shortened.is_empty() {
                        ui.label(
                            RichText::new(format!(
                                "Shortened to fit the model: {}",
                                sent.shortened.join(", ")
                            ))
                            .small()
                            .weak(),
                        );
                    }
                    egui::CollapsingHeader::new(RichText::new("What was sent").small())
                        .id_salt(("assistant-sent", i))
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt(("assistant-sent-text", i))
                                .max_height(220.0)
                                .show(ui, |ui| {
                                    ui.label(RichText::new(&sent.text).monospace().small());
                                });
                        });
                }
            });
        ui.add_space(4.0);
        match &e.status {
            Status::LookingUp => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Looking up tables and plans…");
                });
            }
            Status::Asking => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("Waiting for {}…", e.backend));
                });
            }
            Status::Stopped => {
                ui.weak("Stopped");
            }
            Status::Failed(error) => {
                let error = error.clone();
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(visuals.error_fg_color, error);
                    if !self.is_busy() && ui.small_button("Retry").clicked() {
                        actions.push(PanelAction::Retry(i));
                    }
                });
            }
            Status::Done(elapsed) => {
                let elapsed = *elapsed;
                let answer = e.turn.answer.clone().unwrap_or_default();
                answer_ui(ui, i, &answer, e.origin.as_ref(), actions);
                ui.label(
                    RichText::new(format!("{:.1} s", elapsed.as_secs_f64()))
                        .small()
                        .weak(),
                );
            }
        }
    }
}

/// An answer: prose, and code blocks with what can be done with them.
fn answer_ui(
    ui: &mut Ui,
    entry: usize,
    answer: &str,
    origin: Option<&Origin>,
    actions: &mut Vec<PanelAction>,
) {
    for (n, block) in pythia_ai::answer::blocks(answer).into_iter().enumerate() {
        match block {
            Block::Text(text) => prose(ui, text),
            Block::Code { code, .. } => {
                egui::Frame::new()
                    .fill(ui.visuals().extreme_bg_color)
                    .corner_radius(4.0)
                    .inner_margin(6.0)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        egui::ScrollArea::horizontal()
                            .id_salt(("assistant-code", entry, n))
                            .show(ui, |ui| {
                                ui.add(egui::Label::new(RichText::new(code).monospace()).extend());
                            });
                    });
                ui.horizontal(|ui| {
                    if let Some(origin) = origin.filter(|o| o.target.is_some())
                        && ui
                            .small_button("Replace")
                            .on_hover_text("Replace the statement or selection this was about")
                            .clicked()
                    {
                        actions.push(PanelAction::Replace {
                            entry,
                            origin: origin.clone(),
                            text: code.to_owned(),
                        });
                    }
                    if ui
                        .small_button("Insert")
                        .on_hover_text("Insert at the cursor of the editor")
                        .clicked()
                    {
                        actions.push(PanelAction::Insert {
                            origin: origin.cloned(),
                            text: code.to_owned(),
                        });
                    }
                    if ui.small_button("Copy").clicked() {
                        ui.ctx().copy_text(code.to_owned());
                    }
                    if ui
                        .small_button("New SQL window")
                        .on_hover_text("Open the code in a new SQL window")
                        .clicked()
                    {
                        actions.push(PanelAction::NewSqlWindow(code.to_owned()));
                    }
                });
            }
        }
    }
}

/// Markdown prose: headings, bullets, **bold** and `code`.
fn prose(ui: &mut Ui, text: &str) {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            ui.add_space(4.0);
            continue;
        }
        let heading = trimmed.trim_start_matches('#');
        if heading.len() < trimmed.len() && heading.starts_with(' ') {
            ui.label(RichText::new(heading.trim()).strong());
            continue;
        }
        let indent = (line.len() - line.trim_start().len()) as f32 * 4.0;
        let (bullet, rest) = match trimmed.strip_prefix("- ").or(trimmed.strip_prefix("* ")) {
            Some(rest) => ("• ", rest),
            None => ("", trimmed),
        };
        let mut job = egui::text::LayoutJob::default();
        let style = ui.style().clone();
        let mut append = |text: RichText| {
            text.append_to(
                &mut job,
                &style,
                egui::FontSelection::Default,
                egui::Align::Center,
            )
        };
        append(RichText::new(bullet));
        for span in spans(rest) {
            let mut text = RichText::new(span.text);
            if span.code {
                text = text.code();
            }
            if span.strong {
                text = text.strong();
            }
            append(text);
        }
        ui.horizontal_wrapped(|ui| {
            ui.add_space(indent);
            ui.label(job);
        });
    }
}
