//! Code assistance for an editor: completion, signature help, hover,
//! go to definition and references, backed by `pythia_lsp_core`.
//!
//! Lookups never block. When the engine needs dictionary data it reports
//! [`CatalogRequest`]s; the app fetches them in the background and bumps the
//! catalog revision, and the pending request is recomputed then.

use pythia_db::ObjectType;
use pythia_lsp_core::{
    Analysis, Catalog, CatalogRequest, CompletionKind as LspKind, Definition, EmptyCatalog,
    OutlineItem, Severity,
};
use pythia_ui::code_editor::{CompletionEntry, CompletionKind, EditorOutput};

use crate::documents::{DocContext, Effects, ObjectRef, OpenRequest};
use crate::editor::{Editor, Marker, MarkerKind};

#[derive(Default)]
pub struct Assist {
    analysis: Analysis,
    /// Editor revision the analysis (diagnostics, outline) reflects.
    analyzed_revision: Option<u64>,
    /// When the text last changed without being analyzed yet (egui time).
    changed_at: Option<f64>,
    outline: Vec<OutlineItem>,
    /// Request waiting for dictionary data.
    pending: Option<Pending>,
    /// Catalog revision the pending request last ran against.
    catalog_revision: u64,
    /// What the last right-click was on, for the context menu.
    context: Option<Context>,
}

/// What a right-click in the editor was on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Context {
    pub offset: usize,
    pub target: ContextTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextTarget {
    /// Waiting for dictionary data.
    Looking,
    /// A database object, possibly a member of it (`pkg.proc`).
    Object {
        object: ObjectRef,
        member: Option<String>,
    },
    /// A declaration in this text.
    Local(std::ops::Range<usize>),
    Nothing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    /// Completion requested with the cursor at this byte offset.
    Complete(usize),
    Signature(usize),
    Hover(usize),
    Goto(usize),
    /// What's at this offset, for the context menu.
    Context(usize),
}

impl Assist {
    /// Re-analyzes the text shortly after typing stops: refreshes syntax
    /// markers and the outline. Call every frame after `Editor::ui`.
    pub fn background(&mut self, editor: &mut Editor, ui: &egui::Ui) {
        const IDLE: f64 = 0.35;
        if self.analyzed_revision == Some(editor.revision()) {
            return;
        }
        let now = ui.input(|i| i.time);
        // Analyze right away the first time (just opened / loaded).
        let changed_at = *self
            .changed_at
            .get_or_insert(if self.analyzed_revision.is_none() {
                now - IDLE
            } else {
                now
            });
        if now - changed_at < IDLE {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f64(IDLE));
            return;
        }
        self.changed_at = None;
        self.analyzed_revision = Some(editor.revision());
        self.analysis.update(editor.text());
        let text = editor.text();
        let markers = self
            .analysis
            .diagnostics()
            .iter()
            .map(|d| Marker {
                line: text[..d.span.start.min(text.len())].matches('\n').count() + 1,
                kind: match d.severity {
                    Severity::Error => MarkerKind::Error,
                    Severity::Warning => MarkerKind::Warning,
                },
                message: d.message.clone(),
            })
            .collect();
        self.outline = self.analysis.outline();
        editor.set_syntax_markers(markers);
    }

    pub fn outline(&self) -> &[OutlineItem] {
        &self.outline
    }

    /// What the last right-click was on.
    pub fn context(&self) -> Option<&Context> {
        self.context.as_ref()
    }

    /// Marks the references to the symbol at `offset`.
    pub fn references(&mut self, editor: &mut Editor, offset: usize) {
        self.analysis.update(editor.text());
        let refs = self.analysis.references(offset);
        editor.inner().highlight_references(refs);
    }

    /// Handles the editor's requests of this frame; call after `Editor::ui`.
    pub fn handle(&mut self, editor: &mut Editor, out: &EditorOutput, cx: &DocContext) -> Effects {
        let mut effects = Effects::default();
        let empty = EmptyCatalog;
        let catalog: &dyn Catalog = match cx.catalog {
            Some(c) => c,
            None => &empty,
        };

        let mut work: Vec<Pending> = Vec::new();
        // A context menu still looking up its name retries on its own: a
        // hover over the same word must not take its place.
        if cx.catalog_revision != self.catalog_revision
            && let Some(context) = &self.context
            && context.target == ContextTarget::Looking
        {
            work.push(Pending::Context(context.offset));
        }
        if let Some(p) = self.pending
            && cx.catalog_revision != self.catalog_revision
        {
            // New dictionary data arrived: retry, unless the user moved on.
            let still_relevant = match p {
                Pending::Complete(at) => {
                    // Still at the same place, or only typed more of the word.
                    let cursor = editor.cursor();
                    cursor >= at
                        && editor.text().get(at..cursor).is_some_and(|typed| {
                            typed
                                .chars()
                                .all(|c| c.is_alphanumeric() || c == '_' || c == '$' || c == '#')
                        })
                }
                _ => true,
            };
            self.pending = None;
            if still_relevant {
                work.push(p);
            }
        }
        if out.completion_request.is_some() {
            work.push(Pending::Complete(editor.cursor()));
        }
        work.extend(out.signature_request.map(Pending::Signature));
        work.extend(out.hover_request.map(Pending::Hover));
        work.extend(out.goto_request.map(Pending::Goto));
        if let Some(offset) = out.context_request {
            self.context = Some(Context {
                offset,
                target: ContextTarget::Looking,
            });
            work.push(Pending::Context(offset));
        }
        if work.is_empty() && out.references_request.is_none() {
            return effects;
        }
        self.analysis.update(editor.text());
        self.catalog_revision = cx.catalog_revision;

        if let Some(offset) = out.references_request {
            let refs = self.analysis.references(offset);
            editor.inner().highlight_references(refs);
        }
        for request in work {
            let missing = self.run(request, editor, catalog, cx, &mut effects);
            if !missing.is_empty() {
                if !matches!(request, Pending::Context(_)) {
                    self.pending = Some(request);
                }
                effects.catalog_requests.extend(missing);
            }
        }
        effects
    }

    fn run(
        &mut self,
        request: Pending,
        editor: &mut Editor,
        catalog: &dyn Catalog,
        cx: &DocContext,
        effects: &mut Effects,
    ) -> Vec<CatalogRequest> {
        match request {
            Pending::Complete(_) => {
                let completion = self.analysis.complete(editor.cursor(), catalog);
                let mut entries: Vec<CompletionEntry> =
                    completion.items.into_iter().map(entry).collect();
                // Snippets for a plain word, not after `alias.` or `pkg.`.
                let text = editor.text();
                let replace = completion.replace.clone();
                let member = text[..replace.start.min(text.len())].ends_with('.');
                if let Some(prefix) = text.get(replace.clone()).map(str::to_owned)
                    && !prefix.is_empty()
                    && !member
                {
                    entries.extend(editor.inner().snippet_completions(&prefix));
                }
                editor.inner().show_completions(completion.replace, entries);
                completion.missing
            }
            Pending::Signature(offset) => match self.analysis.signature_help(offset, catalog) {
                Some(help) => {
                    match help.signatures.get(help.active_signature) {
                        Some(sig) => {
                            let active = sig.params.get(help.active_param).cloned();
                            editor.inner().show_signature(sig.label.clone(), active);
                        }
                        None => editor.inner().hide_signature(),
                    }
                    help.missing
                }
                None => {
                    editor.inner().hide_signature();
                    Vec::new()
                }
            },
            Pending::Hover(offset) => match self.analysis.hover(offset, catalog) {
                Some(hover) => {
                    editor
                        .inner()
                        .show_hover(hover.range, hover.title, hover.detail);
                    hover.missing
                }
                None => Vec::new(),
            },
            Pending::Goto(offset) => {
                let result = self.analysis.definition(offset, catalog);
                match result.target {
                    Some(Definition::Local(range)) => editor.select(range),
                    Some(Definition::Object {
                        owner,
                        name,
                        object_type,
                        member,
                    }) => {
                        if let (Some(connection), Some(t)) = (
                            cx.connection.clone(),
                            ObjectType::from_dictionary(&object_type),
                        ) {
                            let object = ObjectRef {
                                owner,
                                name,
                                object_type: t,
                            };
                            effects.open.push(match member {
                                // `pkg.proc`: the package at `proc`.
                                Some(member) if t.has_source() => OpenRequest::ProgramMember {
                                    connection,
                                    object,
                                    member,
                                },
                                _ if t.has_source() => OpenRequest::Program { connection, object },
                                _ => OpenRequest::Object { connection, object },
                            });
                        }
                    }
                    None => {}
                }
                result.missing
            }
            Pending::Context(offset) => {
                let result = self.analysis.definition(offset, catalog);
                let target = match result.target {
                    Some(Definition::Local(range)) => ContextTarget::Local(range),
                    Some(Definition::Object {
                        owner,
                        name,
                        object_type,
                        member,
                    }) => match ObjectType::from_dictionary(&object_type) {
                        Some(t) => ContextTarget::Object {
                            object: ObjectRef {
                                owner,
                                name,
                                object_type: t,
                            },
                            // Columns are no subprograms to open at.
                            member: member.filter(|_| t.has_source()),
                        },
                        None => ContextTarget::Nothing,
                    },
                    None if result.missing.is_empty() => ContextTarget::Nothing,
                    None => ContextTarget::Looking,
                };
                if let Some(context) = self.context.as_mut().filter(|c| c.offset == offset) {
                    context.target = target;
                }
                result.missing
            }
        }
    }
}

fn entry(item: pythia_lsp_core::CompletionItem) -> CompletionEntry {
    let kind = match item.kind {
        LspKind::Keyword => CompletionKind::Keyword,
        LspKind::Snippet => CompletionKind::Snippet,
        LspKind::Variable
        | LspKind::Constant
        | LspKind::Cursor
        | LspKind::Exception
        | LspKind::Alias => CompletionKind::Variable,
        LspKind::Parameter => CompletionKind::Parameter,
        LspKind::Type => CompletionKind::Type,
        LspKind::Procedure => CompletionKind::Procedure,
        LspKind::Function | LspKind::Builtin => CompletionKind::Function,
        LspKind::Field | LspKind::Column => CompletionKind::Column,
        LspKind::Table => CompletionKind::Table,
        LspKind::View => CompletionKind::View,
        LspKind::Package => CompletionKind::Package,
        LspKind::Schema => CompletionKind::Schema,
        LspKind::Sequence | LspKind::Synonym | LspKind::Object => CompletionKind::Other,
    };
    CompletionEntry {
        label: item.label,
        detail: item.detail,
        kind,
        insert_text: item.insert_text,
    }
}
