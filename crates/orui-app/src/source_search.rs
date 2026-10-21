//! Find in database source: searches ALL_SOURCE (text or regular
//! expression) and opens hits in program windows at the matching line.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use egui::text::{LayoutJob, TextFormat};
use egui::{RichText, Ui};
use orui_db::oracle::Connection;
use orui_db::{DbError, DbEvent, DbRequest, ObjectType};
use serde::{Deserialize, Serialize};

use crate::documents::{DocContext, Effects, ObjectRef, OpenRequest, Target, request_id};

/// One ALL_SOURCE line: owner, name, type, line, text.
type SourceRow = (String, String, String, u32, String);

/// More hits than this are cut off (the query fetches one extra to know).
const MAX_HITS: usize = 2000;

#[derive(Default, Serialize, Deserialize)]
pub struct SourceSearch {
    pub query: String,
    pub regex: bool,
    pub match_case: bool,
    /// Restrict to one schema (empty: all visible schemas).
    pub owner: String,
    pub include_oracle: bool,
    #[serde(skip)]
    state: State,
}

#[derive(Default)]
struct State {
    pending: Option<(u64, Instant)>,
    result: Option<Result<SearchResult, String>>,
    focus: bool,
}

struct SearchResult {
    /// Hits grouped by object, in dictionary order.
    objects: BTreeMap<(String, String, String), Vec<Hit>>,
    total: usize,
    truncated: bool,
    elapsed: Duration,
    /// The pattern that produced these hits, for highlighting.
    pattern: String,
}

#[derive(Debug, Clone)]
struct Hit {
    line: u32,
    text: String,
}

impl SourceSearch {
    pub fn new() -> Self {
        Self {
            state: State {
                focus: true,
                ..State::default()
            },
            ..Self::default()
        }
    }

    pub fn title(&self) -> String {
        if self.query.is_empty() {
            "Find in source".into()
        } else {
            format!("Find: {}", self.query)
        }
    }

    pub fn is_busy(&self) -> bool {
        self.state.pending.is_some()
    }

    pub fn search(&mut self) -> Effects {
        let mut effects = Effects::default();
        let pattern = self.query.trim().to_owned();
        if pattern.is_empty() || self.is_busy() {
            return effects;
        }
        let (regex, match_case, include_oracle) =
            (self.regex, self.match_case, self.include_oracle);
        let owner = Some(self.owner.trim().to_uppercase()).filter(|o| !o.is_empty());
        let id = request_id();
        self.state.pending = Some((id, Instant::now()));
        let task = Box::new(move |conn: &Connection| {
            let hits = query(
                conn,
                &pattern,
                regex,
                match_case,
                owner.as_deref(),
                include_oracle,
            )?;
            Ok(Box::new((pattern, hits)) as Box<dyn std::any::Any + Send>)
        });
        effects.request(Target::Own, DbRequest::Task { id, task });
        effects
    }

    pub fn on_event(&mut self, event: DbEvent) -> Effects {
        let DbEvent::TaskDone { id, result } = event else {
            return Effects::default();
        };
        let Some((_, started)) = self.state.pending.filter(|(p, _)| *p == id) else {
            return Effects::default();
        };
        self.state.pending = None;
        type Found = (String, Vec<SourceRow>);
        self.state.result = Some(match result.map(|any| any.downcast::<Found>()) {
            Ok(Ok(found)) => {
                let (pattern, rows) = *found;
                let truncated = rows.len() > MAX_HITS;
                let mut objects: BTreeMap<_, Vec<Hit>> = BTreeMap::new();
                for (owner, name, ty, line, text) in rows.into_iter().take(MAX_HITS) {
                    objects
                        .entry((owner, name, ty))
                        .or_default()
                        .push(Hit { line, text });
                }
                let total = objects.values().map(Vec::len).sum();
                Ok(SearchResult {
                    objects,
                    total,
                    truncated,
                    elapsed: started.elapsed(),
                    pattern,
                })
            }
            Ok(Err(_)) => Err("unexpected result".into()),
            Err(e) => Err(e.to_string()),
        });
        Effects::default()
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &DocContext) -> Effects {
        let mut effects = Effects::default();
        let mut run = false;
        ui.horizontal(|ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text(if self.regex {
                        "Regular expression, e.g. raise_application_error\\(-20\\d{3}"
                    } else {
                        "Text to find in PL/SQL source"
                    })
                    .desired_width(420.0),
            );
            if std::mem::take(&mut self.state.focus) {
                edit.request_focus();
            }
            run |= edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let can = cx.connected && !self.is_busy() && !self.query.trim().is_empty();
            if ui
                .add_enabled(
                    can,
                    egui::Button::image_and_text(orui_ui::icons::Icon::Search.image(ui), "Find"),
                )
                .clicked()
            {
                run = true;
            }
            if self.is_busy() {
                ui.spinner();
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.match_case, "Match case");
            ui.checkbox(&mut self.regex, "Regular expression");
            ui.checkbox(&mut self.include_oracle, "Include Oracle schemas");
            ui.label("Schema");
            ui.add(
                egui::TextEdit::singleline(&mut self.owner)
                    .hint_text("all")
                    .desired_width(120.0),
            );
        });
        if run && cx.connected {
            effects.extend(self.search());
        }
        ui.separator();

        match &self.state.result {
            None => {
                ui.weak("Searches the source of packages, procedures, functions, types and triggers you can see.");
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            Some(Ok(result)) => {
                let summary = format!(
                    "{} match{} in {} object{} · {} ms{}",
                    result.total,
                    if result.total == 1 { "" } else { "es" },
                    result.objects.len(),
                    if result.objects.len() == 1 { "" } else { "s" },
                    result.elapsed.as_millis(),
                    if result.truncated {
                        format!(" · first {MAX_HITS} shown")
                    } else {
                        String::new()
                    },
                );
                ui.weak(summary);
                ui.add_space(4.0);
                let highlight = (!self.regex).then(|| (result.pattern.clone(), self.match_case));
                let mut open = None;
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        for ((owner, name, ty), hits) in &result.objects {
                            let header = format!(
                                "{}.{}  ·  {}  ·  {}",
                                owner,
                                name,
                                ty.to_lowercase(),
                                hits.len()
                            );
                            egui::CollapsingHeader::new(RichText::new(header).strong())
                                .id_salt((owner, name, ty))
                                .default_open(result.objects.len() <= 30)
                                .show(ui, |ui| {
                                    for hit in hits {
                                        let job = line_job(ui, hit, highlight.as_ref());
                                        if ui
                                            .add(egui::Button::new(job).frame_when_inactive(false))
                                            .clicked()
                                        {
                                            open = Some((
                                                owner.clone(),
                                                name.clone(),
                                                ty.clone(),
                                                hit.line,
                                            ));
                                        }
                                    }
                                });
                        }
                    });
                if let (Some((owner, name, ty, line)), Some(connection)) =
                    (open, cx.connection.clone())
                    && let Some(object_type) = ObjectType::from_dictionary(&ty)
                {
                    effects.open.push(OpenRequest::ProgramAt {
                        connection,
                        object: ObjectRef {
                            owner,
                            name,
                            object_type,
                        },
                        line: line as usize,
                    });
                }
            }
        }
        effects
    }
}

/// `line  text` with the plain-text match highlighted.
fn line_job(ui: &Ui, hit: &Hit, highlight: Option<&(String, bool)>) -> LayoutJob {
    let font = egui::TextStyle::Monospace.resolve(ui.style());
    let weak = TextFormat {
        font_id: font.clone(),
        color: ui.visuals().weak_text_color(),
        ..Default::default()
    };
    let normal = TextFormat {
        font_id: font.clone(),
        color: ui.visuals().text_color(),
        ..Default::default()
    };
    let mark = TextFormat {
        background: ui.visuals().selection.bg_fill,
        ..normal.clone()
    };
    let mut job = LayoutJob::default();
    job.append(&format!("{:>6}  ", hit.line), 0.0, weak);
    let text = hit.text.trim_end().replace('\t', "    ");
    // Highlight every occurrence of a plain-text pattern.
    let mut pos = 0;
    if let Some((pattern, case)) = highlight.filter(|(p, _)| !p.is_empty()) {
        let hay = if *case {
            text.clone()
        } else {
            text.to_lowercase()
        };
        let needle = if *case {
            pattern.clone()
        } else {
            pattern.to_lowercase()
        };
        // Lowercasing can change byte lengths; only highlight when it didn't.
        if hay.len() == text.len() {
            while let Some(found) = hay[pos..].find(&needle) {
                let start = pos + found;
                let end = start + needle.len();
                job.append(&text[pos..start], 0.0, normal.clone());
                job.append(&text[start..end], 0.0, mark.clone());
                pos = end;
            }
        }
    }
    job.append(&text[pos..], 0.0, normal);
    job
}

fn query(
    conn: &Connection,
    pattern: &str,
    regex: bool,
    match_case: bool,
    owner: Option<&str>,
    include_oracle: bool,
) -> Result<Vec<SourceRow>, DbError> {
    let condition = match (regex, match_case) {
        (true, true) => "regexp_like(s.text, :pattern, 'c')",
        (true, false) => "regexp_like(s.text, :pattern, 'i')",
        (false, true) => "instr(s.text, :pattern) > 0",
        (false, false) => "instr(upper(s.text), upper(:pattern)) > 0",
    };
    let oracle_filter = if include_oracle {
        ""
    } else {
        " and s.owner not in (select username from all_users where oracle_maintained = 'Y')"
    };
    let sql = format!(
        "select s.owner, s.name, s.type, s.line, s.text from all_source s \
          where {condition} and (:owner is null or s.owner = :owner){oracle_filter} \
          order by s.owner, s.name, s.type, s.line \
          fetch first {} rows only",
        MAX_HITS + 1
    );
    let rows = conn.query_named(&sql, &[("pattern", &pattern), ("owner", &owner)])?;
    let mut out = Vec::new();
    for row in rows {
        let row = row?;
        let (owner, name, ty, line, text): (String, String, String, u32, Option<String>) =
            row.get_as()?;
        out.push((owner, name, ty, line, text.unwrap_or_default()));
    }
    Ok(out)
}
