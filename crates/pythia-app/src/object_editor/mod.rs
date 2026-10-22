//! Object editors: forms for sequences, views, synonyms, indexes and the
//! grants of any object. Each loads the object (if it exists), shows the
//! statements the edits turn into and applies them on the window's session.

mod grants;
mod index;
mod sequence;
mod synonym;
mod view;

#[cfg(test)]
pub use sequence::SequenceDef;
#[cfg(test)]
pub use view::ViewDef;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::Hash;

use egui::{Color32, RichText, Ui};
use pythia_db::oracle::Connection;
use pythia_db::{DbError, DbEvent, DbRequest, ObjectType};
use pythia_ui::icons::Icon;
use serde::{Deserialize, Serialize};

use crate::documents::{
    DocContext, Effects, ObjectRef, OpenRequest, Target, quote_ident, request_id,
};
use crate::editor::Editor;

/// What an editor works on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EditTarget {
    New {
        owner: String,
        object_type: ObjectType,
    },
    Edit(ObjectRef),
    Grants(ObjectRef),
}

impl EditTarget {
    fn owner(&self) -> &str {
        match self {
            EditTarget::New { owner, .. } => owner,
            EditTarget::Edit(o) | EditTarget::Grants(o) => &o.owner,
        }
    }

    fn object(&self) -> Option<&ObjectRef> {
        match self {
            EditTarget::New { .. } => None,
            EditTarget::Edit(o) | EditTarget::Grants(o) => Some(o),
        }
    }

    fn object_type(&self) -> ObjectType {
        match self {
            EditTarget::New { object_type, .. } => *object_type,
            EditTarget::Edit(o) | EditTarget::Grants(o) => o.object_type,
        }
    }
}

/// Types with a form editor (grants work for any object).
pub fn has_editor(t: ObjectType) -> bool {
    matches!(
        t,
        ObjectType::Sequence | ObjectType::View | ObjectType::Synonym | ObjectType::Index
    )
}

/// Types that take object grants.
pub fn has_grants(t: ObjectType) -> bool {
    matches!(
        t,
        ObjectType::Table
            | ObjectType::View
            | ObjectType::MaterializedView
            | ObjectType::Sequence
            | ObjectType::Package
            | ObjectType::Procedure
            | ObjectType::Function
            | ObjectType::Type
            | ObjectType::Library
    )
}

#[derive(Debug, Clone, PartialEq)]
enum Form {
    Sequence(sequence::SequenceDef),
    View(view::ViewDef),
    Synonym(synonym::SynonymDef),
    Index(index::IndexDef),
    Grants(grants::GrantsDef),
}

impl Form {
    fn name(&self) -> String {
        match self {
            Form::Sequence(d) => ident(&d.name),
            Form::View(d) => ident(&d.name),
            Form::Synonym(d) => ident(&d.name),
            Form::Index(d) => ident(&d.name),
            Form::Grants(_) => String::new(),
        }
    }
}

/// Names for the pickers.
#[derive(Default)]
pub struct Lists {
    /// Owner of the edited object.
    pub schema: String,
    pub owners: Vec<String>,
    /// Objects by owner, loaded when a picker needs them.
    pub objects: HashMap<String, Vec<String>>,
    pub tables: Vec<String>,
    /// Columns by table (of `schema`).
    pub columns: HashMap<String, Vec<String>>,
    pub tablespaces: Vec<String>,
    pub db_links: Vec<String>,
    pub grantees: Vec<String>,
    /// Lists a form asked for this frame.
    pub want_objects: Option<String>,
    pub want_columns: Option<String>,
}

/// The lists loaded with the form.
#[derive(Default)]
struct BaseLists {
    owners: Vec<String>,
    tables: Vec<String>,
    tablespaces: Vec<String>,
    db_links: Vec<String>,
    grantees: Vec<String>,
}

enum ListRequest {
    Objects(String),
    Columns(String),
}

/// A statement that failed while applying.
#[derive(Debug)]
struct ApplyError {
    statement: String,
    error: DbError,
}

#[derive(Serialize, Deserialize)]
pub struct ObjectEditor {
    pub target: EditTarget,
    #[serde(skip)]
    state: State,
}

#[derive(Default)]
struct State {
    loading: Option<u64>,
    lists_loading: BTreeMap<u64, ListRequest>,
    requested: BTreeSet<String>,
    original: Option<Form>,
    edited: Option<Form>,
    lists: Lists,
    /// The view's query.
    query: Option<Editor>,
    applying: Option<u64>,
    load_error: Option<String>,
    /// Result of the last apply: message and success.
    outcome: Option<(String, bool)>,
    /// Text range of an apply error in the query editor.
    error_at: Option<std::ops::Range<usize>>,
}

impl ObjectEditor {
    pub fn new(target: EditTarget) -> Self {
        Self {
            target,
            state: State::default(),
        }
    }

    pub fn title(&self) -> String {
        // The tab adds the unsaved-changes dot.
        let kind = self.target.object_type().dictionary_name().to_lowercase();
        match &self.target {
            EditTarget::New { .. } => format!("New {kind}"),
            EditTarget::Edit(o) => format!("{} ({kind})", o.name.to_lowercase()),
            EditTarget::Grants(o) => format!("{} grants", o.name.to_lowercase()),
        }
    }

    pub fn is_busy(&self) -> bool {
        self.state.loading.is_some()
            || self.state.applying.is_some()
            || !self.state.lists_loading.is_empty()
    }

    pub fn is_dirty(&self) -> bool {
        match (&self.state.original, &self.state.edited) {
            (Some(a), Some(b)) => a != b,
            (None, Some(b)) => !b.name().is_empty(),
            _ => false,
        }
    }

    #[cfg(test)]
    pub fn statements_for_test(&mut self) -> Result<Vec<String>, String> {
        self.sync_query();
        self.statements()
    }

    #[cfg(test)]
    pub fn outcome(&self) -> Option<(&str, bool)> {
        self.state.outcome.as_ref().map(|(m, ok)| (m.as_str(), *ok))
    }

    /// Changes the form in tests, e.g. `|f| f.cache = 50` on a sequence.
    #[cfg(test)]
    pub fn edit_for_test(&mut self, f: impl FnOnce(&mut dyn std::any::Any)) {
        match self.state.edited.as_mut() {
            Some(Form::Sequence(d)) => f(d),
            Some(Form::View(d)) => {
                f(d);
                if let Some(q) = self.state.query.as_mut() {
                    q.set_text(d.query.clone());
                }
            }
            Some(Form::Synonym(d)) => f(d),
            Some(Form::Index(d)) => f(d),
            Some(Form::Grants(d)) => f(d),
            None => {}
        }
    }

    #[cfg(test)]
    pub fn apply_for_test(&mut self) -> Effects {
        self.apply()
    }

    /// Loads the object and the picker lists.
    pub fn load(&mut self) -> Effects {
        let mut effects = Effects::default();
        let id = request_id();
        self.state.loading = Some(id);
        self.state.load_error = None;
        let target = self.target.clone();
        let task: pythia_db::TaskFn = Box::new(move |conn: &Connection| {
            let loaded = load(conn, &target)?;
            Ok(Box::new(loaded) as Box<dyn std::any::Any + Send>)
        });
        effects.request(Target::Main, DbRequest::Task { id, task });
        effects
    }

    pub fn on_event(&mut self, event: DbEvent) -> Effects {
        let DbEvent::TaskDone { id, result } = event else {
            return Effects::default();
        };
        if self.state.loading == Some(id) {
            self.state.loading = None;
            match result.map(|any| any.downcast::<(Option<Form>, BaseLists)>()) {
                Ok(Ok(loaded)) => {
                    let (form, base) = *loaded;
                    let edited = form.clone().unwrap_or_else(|| new_form(&self.target));
                    self.state.query = match &edited {
                        Form::View(v) => Some(Editor::new(v.query.clone())),
                        _ => None,
                    };
                    self.state.original = form;
                    self.state.edited = Some(edited);
                    let lists = &mut self.state.lists;
                    lists.schema = self.target.owner().to_owned();
                    lists.owners = base.owners;
                    lists.tables = base.tables;
                    lists.tablespaces = base.tablespaces;
                    lists.db_links = base.db_links;
                    lists.grantees = base.grantees;
                }
                Ok(Err(_)) => self.state.load_error = Some("unexpected result".into()),
                Err(e) => self.state.load_error = Some(e.to_string()),
            }
        } else if let Some(request) = self.state.lists_loading.remove(&id) {
            let names = result
                .ok()
                .and_then(|any| any.downcast::<Vec<String>>().ok())
                .map(|n| *n)
                .unwrap_or_default();
            match request {
                ListRequest::Objects(owner) => {
                    self.state.lists.objects.insert(owner, names);
                }
                ListRequest::Columns(table) => {
                    self.state.lists.columns.insert(table, names);
                }
            }
        } else if self.state.applying == Some(id) {
            self.state.applying = None;
            self.state.error_at = None;
            match result.map(|any| any.downcast::<Result<Vec<String>, ApplyError>>()) {
                Ok(Ok(outcome)) => match *outcome {
                    Ok(warnings) => {
                        let mut message = "Applied".to_owned();
                        for w in &warnings {
                            message.push('\n');
                            message.push_str(w);
                        }
                        self.state.outcome = Some((message, warnings.is_empty()));
                        if let EditTarget::New { owner, object_type } = &self.target
                            && let Some(form) = &self.state.edited
                        {
                            self.target = EditTarget::Edit(ObjectRef {
                                owner: owner.clone(),
                                name: form.name(),
                                object_type: *object_type,
                            });
                        }
                        return self.load();
                    }
                    Err(e) => {
                        self.locate_error(&e);
                        self.state.outcome = Some((
                            format!(
                                "{}\n\nin: {}",
                                error_text(&e.error),
                                first_line(&e.statement)
                            ),
                            false,
                        ));
                    }
                },
                Ok(Err(_)) => self.state.outcome = Some(("unexpected result".into(), false)),
                Err(e) => self.state.outcome = Some((e.to_string(), false)),
            }
        }
        Effects::default()
    }

    /// Selects the error position in the view's query editor.
    fn locate_error(&mut self, e: &ApplyError) {
        let (Some(offset), Some(query)) = (e.error.offset, self.state.query.as_mut()) else {
            return;
        };
        let Some(start) = e.statement.find(" as\n").map(|i| i + 4) else {
            return;
        };
        let Some(at) = (offset as usize).checked_sub(start) else {
            return;
        };
        let lead = query.text().len() - query.text().trim_start().len();
        let pos = (lead + at).min(query.text().len());
        query.select(pos..pos);
        self.state.error_at = Some(pos..pos);
    }

    fn sync_query(&mut self) {
        if let (Some(Form::View(v)), Some(q)) = (self.state.edited.as_mut(), &self.state.query) {
            v.query = q.text().to_owned();
        }
    }

    fn statements(&self) -> Result<Vec<String>, String> {
        let Some(edited) = &self.state.edited else {
            return Ok(Vec::new());
        };
        let original = self.state.original.as_ref();
        if original == Some(edited) {
            return Ok(Vec::new());
        }
        let owner = self.target.owner();
        match (original, edited) {
            (o, Form::Sequence(d)) => {
                let o = o.and_then(|f| match f {
                    Form::Sequence(s) => Some(s),
                    _ => None,
                });
                sequence::statements(owner, o, d)
            }
            (_, Form::View(d)) => view::statements(owner, d),
            (_, Form::Synonym(d)) => synonym::statements(owner, d),
            (o, Form::Index(d)) => {
                let o = o.and_then(|f| match f {
                    Form::Index(i) => Some(i),
                    _ => None,
                });
                index::statements(owner, o, d)
            }
            (o, Form::Grants(d)) => {
                let empty = grants::GrantsDef::default();
                let o = match o {
                    Some(Form::Grants(g)) => g,
                    _ => &empty,
                };
                let object = self
                    .target
                    .object()
                    .map(ObjectRef::qualified)
                    .unwrap_or_default();
                Ok(grants::statements(&object, o, d))
            }
        }
    }

    fn apply(&mut self) -> Effects {
        let mut effects = Effects::default();
        let Ok(statements) = self.statements() else {
            return effects;
        };
        if statements.is_empty() {
            return effects;
        }
        let view = match (&self.state.edited, self.target.object_type()) {
            (Some(form), ObjectType::View) => Some((self.target.owner().to_owned(), form.name())),
            _ => None,
        };
        let id = request_id();
        self.state.applying = Some(id);
        self.state.outcome = None;
        let task: pythia_db::TaskFn = Box::new(move |conn: &Connection| {
            let outcome = apply(conn, &statements, view);
            Ok(Box::new(outcome) as Box<dyn std::any::Any + Send>)
        });
        effects.request(Target::Own, DbRequest::Task { id, task });
        effects
    }

    /// Loads lists the form asked for.
    fn request_lists(&mut self, effects: &mut Effects) {
        let schema = self.state.lists.schema.clone();
        let wants = [
            self.state
                .lists
                .want_objects
                .take()
                .map(ListRequest::Objects),
            self.state
                .lists
                .want_columns
                .take()
                .map(ListRequest::Columns),
        ];
        for want in wants.into_iter().flatten() {
            let key = match &want {
                ListRequest::Objects(o) => format!("objects:{o}"),
                ListRequest::Columns(t) => format!("columns:{t}"),
            };
            if !self.state.requested.insert(key) {
                continue;
            }
            let id = request_id();
            let (sql, binds): (&str, Vec<String>) = match &want {
                ListRequest::Objects(owner) => (
                    "select object_name from all_objects
                      where owner = :1 and object_type in ('TABLE', 'VIEW', 'MATERIALIZED VIEW',
                            'SEQUENCE', 'SYNONYM', 'PACKAGE', 'PROCEDURE', 'FUNCTION', 'TYPE')
                      order by object_name",
                    vec![owner.clone()],
                ),
                ListRequest::Columns(table) => (
                    "select column_name from all_tab_columns
                      where owner = :1 and table_name = :2 order by column_id",
                    vec![schema.clone(), table.clone()],
                ),
            };
            self.state.lists_loading.insert(id, want);
            let task: pythia_db::TaskFn = Box::new(move |conn: &Connection| {
                let refs: Vec<&dyn pythia_db::oracle::sql_type::ToSql> = binds
                    .iter()
                    .map(|b| b as &dyn pythia_db::oracle::sql_type::ToSql)
                    .collect();
                let names = names(conn, sql, &refs)?;
                Ok(Box::new(names) as Box<dyn std::any::Any + Send>)
            });
            effects.request(Target::Main, DbRequest::Task { id, task });
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &DocContext) -> Effects {
        let mut effects = Effects::default();
        if let Some(e) = &self.state.load_error {
            ui.colored_label(ui.visuals().error_fg_color, e);
            if ui.button("Retry").clicked() {
                effects.extend(self.load());
            }
            return effects;
        }
        if self.state.edited.is_none() {
            ui.spinner();
            return effects;
        }
        self.sync_query();
        let statements = self.statements();
        let existing = self.state.original.is_some();
        ui.horizontal(|ui| {
            let t = self.target.object_type();
            if let Some(icon) = Icon::for_object_type(t.dictionary_name()) {
                ui.add(icon.sized(ui, pythia_ui::icons::IconSize::Small));
            }
            let heading = match &self.target {
                EditTarget::New { owner, .. } => {
                    format!("New {} in {owner}", t.dictionary_name().to_lowercase())
                }
                EditTarget::Edit(o) => format!("{}.{}", o.owner, o.name),
                EditTarget::Grants(o) => format!("Grants on {}.{}", o.owner, o.name),
            };
            ui.label(RichText::new(heading).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let ready = matches!(&statements, Ok(s) if !s.is_empty());
                let apply = egui::Button::image_and_text(Icon::Execute.image(ui), "Apply");
                if ui
                    .add_enabled(ready && cx.connected && !self.is_busy(), apply)
                    .on_hover_text("Run the statements below in this window's session")
                    .clicked()
                {
                    effects.extend(self.apply());
                }
                if self.state.applying.is_some() {
                    ui.spinner();
                }
                if ui
                    .add_enabled(ready, egui::Button::new("Open in SQL window"))
                    .clicked()
                    && let (Some(connection), Ok(s)) = (cx.connection.clone(), &statements)
                {
                    effects.open.push(OpenRequest::Sql {
                        connection,
                        text: script(s),
                        run: false,
                    });
                }
                if existing {
                    if ui
                        .add_enabled(self.is_dirty(), egui::Button::new("Revert"))
                        .clicked()
                    {
                        self.state.edited = self.state.original.clone();
                        if let (Some(Form::View(v)), Some(q)) =
                            (&self.state.edited, self.state.query.as_mut())
                        {
                            q.set_text(v.query.clone());
                        }
                    }
                    if ui.button("Reload").clicked() {
                        effects.extend(self.load());
                    }
                }
            });
        });
        ui.separator();
        if let Some((message, ok)) = &self.state.outcome {
            let color = if *ok {
                Color32::from_rgb(0x2f, 0xa0, 0x4c)
            } else {
                ui.visuals().error_fg_color
            };
            ui.label(RichText::new(message).color(color));
            ui.add_space(4.0);
        }

        // The DDL preview keeps a fixed share at the bottom.
        let preview_height = (ui.available_height() * 0.3).clamp(90.0, 220.0);
        let form_height = (ui.available_height() - preview_height - 30.0).max(120.0);
        let lists = &mut self.state.lists;
        let editor_id = egui::Id::new(("object-editor-query", cx.id));
        ui.allocate_ui(
            egui::vec2(ui.available_width(), form_height),
            |ui| match self.state.edited.as_mut().expect("loaded") {
                Form::View(def) => {
                    view::ui(ui, def, existing);
                    ui.add_space(6.0);
                    ui.label(RichText::new("Query").strong());
                    if let Some(query) = self.state.query.as_mut() {
                        let height = ui.available_height().max(80.0);
                        ui.allocate_ui(egui::vec2(ui.available_width(), height), |ui| {
                            query.ui(ui, editor_id, &cx.settings.editor);
                        });
                    }
                }
                form => {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, true])
                        .show(ui, |ui| match form {
                            Form::Sequence(def) => sequence::ui(ui, def, existing),
                            Form::Synonym(def) => synonym::ui(ui, def, existing, lists),
                            Form::Index(def) => index::ui(ui, def, existing, lists),
                            Form::Grants(def) => grants::ui(ui, def, lists),
                            Form::View(_) => {}
                        })
                        .inner
                }
            },
        );
        self.request_lists(&mut effects);

        ui.separator();
        ui.label(RichText::new("Statements").strong());
        match &statements {
            Err(problem) => {
                ui.label(RichText::new(problem).color(ui.visuals().warn_fg_color));
            }
            Ok(s) if s.is_empty() => {
                ui.weak("No changes");
            }
            Ok(s) => {
                let theme = pythia_ui::SyntaxTheme::for_visuals(ui.visuals());
                let font = egui::FontId::monospace(cx.settings.editor.font_size.min(14.0));
                egui::Frame::new()
                    .fill(ui.visuals().extreme_bg_color)
                    .corner_radius(4.0)
                    .inner_margin(6.0)
                    .show(ui, |ui| {
                        egui::ScrollArea::both()
                            .id_salt("ddl")
                            .auto_shrink([false, true])
                            .max_height(ui.available_height())
                            .show(ui, |ui| {
                                let job = pythia_ui::highlight(&script(s), font, &theme);
                                ui.add(egui::Label::new(job).extend());
                            });
                    });
            }
        }
        effects
    }
}

/// A new, empty form.
fn new_form(target: &EditTarget) -> Form {
    match target.object_type() {
        ObjectType::View => Form::View(view::ViewDef::new()),
        ObjectType::Synonym => Form::Synonym(synonym::SynonymDef::default()),
        ObjectType::Index => Form::Index(index::IndexDef::new()),
        _ => Form::Sequence(sequence::SequenceDef::new()),
    }
}

fn load(conn: &Connection, target: &EditTarget) -> Result<(Option<Form>, BaseLists), DbError> {
    let owner = target.owner();
    let mut base = BaseLists::default();
    let form = match target {
        EditTarget::Grants(o) => {
            base.grantees = grantees(conn)?;
            Some(Form::Grants(grants::load(
                conn,
                &o.owner,
                &o.name,
                o.object_type,
            )?))
        }
        _ => {
            match target.object_type() {
                ObjectType::Synonym => {
                    base.owners = names(
                        conn,
                        "select username from all_users order by username",
                        &[],
                    )?;
                    base.db_links = names(
                        conn,
                        "select db_link from all_db_links order by db_link",
                        &[],
                    )?;
                }
                ObjectType::Index => {
                    base.tables = names(
                        conn,
                        "select table_name from all_tables where owner = :1 order by table_name",
                        &[&owner],
                    )?;
                    base.tablespaces = names(
                        conn,
                        "select tablespace_name from user_tablespaces
                          where contents = 'PERMANENT' order by tablespace_name",
                        &[],
                    )?;
                }
                _ => {}
            }
            match target.object() {
                None => None,
                Some(o) => Some(match o.object_type {
                    ObjectType::Sequence => Form::Sequence(sequence::load(conn, owner, &o.name)?),
                    ObjectType::View => Form::View(view::load(conn, owner, &o.name)?),
                    ObjectType::Synonym => Form::Synonym(synonym::load(conn, owner, &o.name)?),
                    ObjectType::Index => Form::Index(index::load(conn, owner, &o.name)?),
                    t => {
                        return Err(DbError::new(format!(
                            "No editor for {}",
                            t.dictionary_name().to_lowercase()
                        )));
                    }
                }),
            }
        }
    };
    Ok((form, base))
}

/// Users and roles that can receive grants.
fn grantees(conn: &Connection) -> Result<Vec<String>, DbError> {
    let mut out = names(
        conn,
        "select username from all_users order by username",
        &[],
    )?;
    // DBA_ROLES needs catalog access; the session's roles are the fallback.
    let roles = names(conn, "select role from dba_roles order by role", &[])
        .or_else(|_| names(conn, "select role from session_roles order by role", &[]))?;
    out.extend(roles);
    out.push("PUBLIC".into());
    out.sort();
    out.dedup();
    Ok(out)
}

fn names(
    conn: &Connection,
    sql: &str,
    binds: &[&dyn pythia_db::oracle::sql_type::ToSql],
) -> Result<Vec<String>, DbError> {
    let mut out = Vec::new();
    for row in conn.query_as::<String>(sql, binds)? {
        out.push(row?);
    }
    Ok(out)
}

/// Runs the statements in order, stopping at the first error. A view's
/// compile errors (with `FORCE`) come back as warnings.
fn apply(
    conn: &Connection,
    statements: &[String],
    view: Option<(String, String)>,
) -> Result<Vec<String>, ApplyError> {
    for s in statements {
        conn.execute(s, &[]).map_err(|e| ApplyError {
            statement: s.clone(),
            error: e.into(),
        })?;
    }
    let mut warnings = Vec::new();
    if let Some((owner, name)) = view
        && let Ok(rows) = conn.query_as::<String>(
            "select text from all_errors where owner = :1 and name = :2 and type = 'VIEW'
              order by sequence",
            &[&owner, &name],
        )
    {
        for text in rows.flatten() {
            warnings.push(format!("Created with errors: {}", text.trim()));
        }
    }
    Ok(warnings)
}

fn error_text(e: &DbError) -> String {
    match e.code {
        Some(code) if !e.message.starts_with("ORA-") => format!("ORA-{code:05}: {}", e.message),
        _ => e.message.clone(),
    }
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or_default()
}

/// The statements as a runnable script.
fn script(statements: &[String]) -> String {
    statements
        .iter()
        .map(|s| format!("{s};"))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The dictionary form of a name as typed: quoted names verbatim, others
/// upper case.
pub fn ident(input: &str) -> String {
    let s = input.trim();
    match s.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        Some(quoted) => quoted.replace("\"\"", "\""),
        None => s.to_uppercase(),
    }
}

/// `owner.name` with `name` as typed.
pub fn qualified(owner: &str, name: &str) -> String {
    format!("{}.{}", quote_ident(owner), quote_ident(&ident(name)))
}

pub fn is_integer(s: &str) -> bool {
    let digits = s.strip_prefix('-').unwrap_or(s);
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

pub fn form_grid(ui: &mut Ui, id: &str, add: impl FnOnce(&mut Ui)) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([16.0, 8.0])
        .min_col_width(110.0)
        .show(ui, add);
}

pub fn form_row(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.label(label);
    ui.horizontal(add);
    ui.end_row();
}

/// The name: editable for new objects, fixed for existing ones.
pub fn name_field(ui: &mut Ui, name: &mut String, existing: bool) {
    form_row(ui, "Name", |ui| {
        if existing {
            ui.label(RichText::new(name.as_str()).monospace().strong());
        } else {
            ui.add(
                egui::TextEdit::singleline(name)
                    .hint_text("name")
                    .desired_width(240.0),
            );
        }
    });
}

/// A "▾" button listing `items` with a filter; returns the one picked.
pub fn picker(ui: &mut Ui, id: impl Hash + std::fmt::Debug, items: &[String]) -> Option<String> {
    let filter_id = ui.id().with(("picker-filter", &id));
    let mut picked = None;
    ui.menu_button("▾", |ui| {
        let mut filter: String = ui.data(|d| d.get_temp(filter_id)).unwrap_or_default();
        let response = ui.add(
            egui::TextEdit::singleline(&mut filter)
                .hint_text("Filter…")
                .desired_width(220.0),
        );
        if !response.has_focus() && filter.is_empty() {
            response.request_focus();
        }
        let needle = filter.to_uppercase();
        egui::ScrollArea::vertical()
            .max_height(280.0)
            .show(ui, |ui| {
                let mut shown = 0;
                for item in items.iter().filter(|i| i.to_uppercase().contains(&needle)) {
                    if shown == 300 {
                        ui.weak("… type to narrow down");
                        break;
                    }
                    shown += 1;
                    if ui.button(RichText::new(item).monospace()).clicked() {
                        picked = Some(item.clone());
                        ui.close();
                    }
                }
                if shown == 0 {
                    ui.weak("Nothing found");
                }
            });
        ui.data_mut(|d| d.insert_temp(filter_id, filter));
    });
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_names() {
        assert_eq!(ident(" orders "), "ORDERS");
        assert_eq!(ident("\"Mixed\""), "Mixed");
        assert_eq!(qualified("HR", "emp"), "HR.EMP");
        assert_eq!(qualified("HR", "\"my emp\""), "HR.\"my emp\"");
        assert!(is_integer("-12") && !is_integer("1.5") && !is_integer("-"));
    }

    /// Creates, alters and drops real objects through the editors' SQL.
    #[test]
    fn edits_live_objects() {
        let Ok(spec) = std::env::var("PYTHIA_TEST_DB") else {
            return;
        };
        let (cred, db) = spec.rsplit_once('@').unwrap();
        let (user, pw) = cred.split_once('/').unwrap();
        let conn = Connection::connect(user, pw, db).unwrap();
        let owner = conn
            .query_row_as::<String>("select user from dual", &[])
            .unwrap();
        conn.execute(
            "begin
               for o in (select object_name, object_type from user_objects
                          where object_name like 'PYTHIA\\_OE\\_%' escape '\\'
                            and object_type in ('TABLE', 'VIEW', 'SEQUENCE', 'SYNONYM')) loop
                 execute immediate 'drop ' || o.object_type || ' ' || o.object_name
                   || case o.object_type when 'TABLE' then ' purge' end;
               end loop;
             end;",
            &[],
        )
        .unwrap();
        let run = |statements: Vec<String>| {
            apply(&conn, &statements, None).unwrap_or_else(|e| {
                panic!("{}: {}", e.statement, e.error);
            })
        };
        conn.execute(
            "create table pythia_oe_t (id number primary key, name varchar2(30), created date)",
            &[],
        )
        .unwrap();

        // Sequence: create, reload, alter.
        let mut seq = sequence::SequenceDef::new();
        seq.name = "pythia_oe_seq".into();
        seq.start = "100".into();
        run(sequence::statements(&owner, None, &seq).unwrap());
        let loaded = sequence::load(&conn, &owner, "PYTHIA_OE_SEQ").unwrap();
        assert_eq!(loaded.increment, "1");
        assert_eq!(loaded.min, "1");
        assert_eq!(loaded.max, "");
        assert_eq!(loaded.cache, 20);
        let mut edited = loaded.clone();
        edited.increment = "5".into();
        edited.cache = 0;
        edited.start = "500".into();
        run(sequence::statements(&owner, Some(&loaded), &edited).unwrap());
        let next: i64 = conn
            .query_row_as("select pythia_oe_seq.nextval from dual", &[])
            .unwrap();
        assert_eq!(next, 500);
        assert_eq!(
            sequence::load(&conn, &owner, "PYTHIA_OE_SEQ")
                .unwrap()
                .cache,
            0
        );

        // View with check option, reloaded without the clause in the query.
        let mut v = view::ViewDef::new();
        v.name = "pythia_oe_v".into();
        v.query = "select id, name from pythia_oe_t where id > 0".into();
        v.check_option = true;
        run(view::statements(&owner, &v).unwrap());
        let loaded = view::load(&conn, &owner, "PYTHIA_OE_V").unwrap();
        assert!(loaded.check_option && !loaded.read_only);
        assert_eq!(
            loaded.query,
            "select id, name from pythia_oe_t where id > 0"
        );
        // A broken query with FORCE: created with errors.
        let mut broken = loaded.clone();
        broken.query = "select nope from pythia_oe_t".into();
        broken.force = true;
        broken.check_option = false;
        let warnings = apply(
            &conn,
            &view::statements(&owner, &broken).unwrap(),
            Some((owner.clone(), "PYTHIA_OE_V".into())),
        )
        .unwrap();
        assert!(!warnings.is_empty(), "expected compile errors");

        // Synonym.
        let syn = synonym::SynonymDef {
            name: "pythia_oe_syn".into(),
            target_name: "pythia_oe_t".into(),
            ..Default::default()
        };
        run(synonym::statements(&owner, &syn).unwrap());
        let loaded = synonym::load(&conn, &owner, "PYTHIA_OE_SYN").unwrap();
        assert_eq!(loaded.target_name, "PYTHIA_OE_T");
        assert_eq!(loaded.target_owner, quote_ident(&owner));

        // Index: create, then recreate with another column order.
        let mut ix = index::IndexDef::new();
        ix.name = "pythia_oe_ix".into();
        ix.table = "pythia_oe_t".into();
        ix.columns = vec![
            index::IndexColumn {
                expr: "name".into(),
                desc: false,
            },
            index::IndexColumn {
                expr: "created".into(),
                desc: true,
            },
        ];
        run(index::statements(&owner, None, &ix).unwrap());
        let loaded = index::load(&conn, &owner, "PYTHIA_OE_IX").unwrap();
        assert_eq!(loaded.columns.len(), 2);
        assert_eq!(loaded.columns[0].expr, "NAME");
        assert_eq!(loaded.columns[1].expr, "CREATED");
        assert!(loaded.columns[1].desc);
        let mut edited = loaded.clone();
        edited.columns.swap(0, 1);
        edited.unique = true;
        run(index::statements(&owner, Some(&loaded), &edited).unwrap());
        let reloaded = index::load(&conn, &owner, "PYTHIA_OE_IX").unwrap();
        assert!(reloaded.unique);
        assert_eq!(reloaded.columns[0].expr, "CREATED");

        // Grants to PUBLIC: add with grant option is refused for PUBLIC, so
        // grant plain select and revoke it again.
        let object = format!("{}.PYTHIA_OE_T", quote_ident(&owner));
        let before = grants::load(&conn, &owner, "PYTHIA_OE_T", ObjectType::Table).unwrap();
        let mut after = before.clone();
        after.rows.push(grants::GrantRow {
            grantee: "PUBLIC".into(),
            granted: ["SELECT".to_owned()].into(),
            grantable: BTreeSet::new(),
        });
        run(grants::statements(&object, &before, &after));
        let loaded = grants::load(&conn, &owner, "PYTHIA_OE_T", ObjectType::Table).unwrap();
        assert!(
            loaded
                .rows
                .iter()
                .any(|r| r.grantee == "PUBLIC" && r.granted.contains("SELECT"))
        );
        run(grants::statements(&object, &loaded, &before));
        let loaded = grants::load(&conn, &owner, "PYTHIA_OE_T", ObjectType::Table).unwrap();
        assert!(!loaded.rows.iter().any(|r| r.grantee == "PUBLIC"));

        for s in [
            "drop synonym pythia_oe_syn",
            "drop view pythia_oe_v",
            "drop sequence pythia_oe_seq",
            "drop table pythia_oe_t purge",
        ] {
            conn.execute(s, &[]).unwrap();
        }
    }
}
