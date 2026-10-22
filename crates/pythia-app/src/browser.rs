//! Object browser: connection → schema → object type → objects → children.
//!
//! The tree is flattened into visible rows every frame and drawn with a
//! virtualized scroll area, so folders with tens of thousands of objects stay
//! smooth. Folders load lazily when expanded.

use std::collections::{HashMap, HashSet};

use egui::{Color32, RichText, Sense, Ui};
use pythia_db::{
    DbEvent, DbObject, DbRequest, Member, MetaQuery, MetaResult, ObjectType, TableColumn,
};

use pythia_ui::icons::{Icon, IconSize, paint_invalid_badge};

use crate::documents::{Effects, ObjectRef, OpenRequest, Target, quote_ident, request_id};
use crate::object_editor::{EditTarget, can_create, has_editor, has_grants};
use crate::table_editor::TableTab;
use crate::table_parts::{Part, PartKind, TableParts};

enum Loaded<T> {
    Loading,
    Ready(T),
    Failed(String),
}

enum Children {
    Columns(Vec<TableColumn>),
    Members(Vec<Member>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Node {
    Folder(String, ObjectType),
    Object(ObjectRef),
    /// Indexes, constraints or triggers of a table.
    Group(ObjectRef, Group),
    /// Favorites or recently opened objects.
    Section(Section),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Section {
    Favorites,
    Recent,
    Users,
    Roles,
    Profiles,
}

impl Section {
    /// Sections listing the database's own objects, in display order.
    const DATABASE: [Section; 3] = [Section::Users, Section::Roles, Section::Profiles];

    /// The users, roles or profiles a section lists.
    fn accounts(self) -> Option<ObjectType> {
        match self {
            Section::Users => Some(ObjectType::User),
            Section::Roles => Some(ObjectType::Role),
            Section::Profiles => Some(ObjectType::Profile),
            Section::Favorites | Section::Recent => None,
        }
    }

    /// The section listing objects of a type.
    fn listing(object_type: ObjectType) -> Option<Section> {
        Section::DATABASE
            .into_iter()
            .find(|s| s.accounts() == Some(object_type))
    }
}

/// Favorite and recently opened objects per connection; saved with the
/// workspace.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Marks {
    pub favorites: std::collections::BTreeMap<String, Vec<ObjectRef>>,
    pub recent: std::collections::BTreeMap<String, Vec<ObjectRef>>,
}

impl Marks {
    const RECENT: usize = 12;

    /// Remembers an object as just opened.
    pub fn touch(&mut self, connection: &str, object: ObjectRef) {
        let list = self.recent.entry(connection.to_owned()).or_default();
        list.retain(|o| *o != object);
        list.insert(0, object);
        list.truncate(Self::RECENT);
    }

    fn toggle_favorite(&mut self, connection: &str, object: ObjectRef) {
        let list = self.favorites.entry(connection.to_owned()).or_default();
        if let Some(i) = list.iter().position(|o| *o == object) {
            list.remove(i);
        } else {
            list.push(object);
            list.sort_by(|a, b| a.name.cmp(&b.name));
        }
    }

    pub fn is_favorite(&self, connection: &str, object: &ObjectRef) -> bool {
        self.favorites
            .get(connection)
            .is_some_and(|l| l.contains(object))
    }
}

/// Which objects the folders list besides the name filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Changed {
    #[default]
    Any,
    Day,
    Week,
    Month,
}

impl Changed {
    fn days(self) -> Option<i64> {
        match self {
            Changed::Any => None,
            Changed::Day => Some(1),
            Changed::Week => Some(7),
            Changed::Month => Some(30),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Group {
    Indexes,
    Constraints,
    Triggers,
    ReferencedBy,
}

impl Group {
    fn label(self) -> &'static str {
        match self {
            Group::Indexes => "Indexes",
            Group::Constraints => "Constraints",
            Group::Triggers => "Triggers",
            Group::ReferencedBy => "Referenced by",
        }
    }
}

enum Pending {
    Schemas,
    Folder(String, ObjectType),
    Children(ObjectRef),
    /// Indexes, constraints and triggers of a table.
    Parts(ObjectRef),
    Action {
        object: ObjectRef,
        verb: &'static str,
    },
    /// Columns for a generated statement.
    Generate {
        object: ObjectRef,
        kind: Template,
    },
    /// Parameters of `member` (or the standalone unit) for a test script.
    Test {
        object: ObjectRef,
        member: Option<String>,
    },
    TestLink(ObjectRef),
    /// Users or roles of the database.
    Accounts(ObjectType),
}

/// The folders of a schema; `PUBLIC` only owns synonyms and links.
fn folders(owner: &str) -> &'static [ObjectType] {
    if owner == "PUBLIC" {
        ObjectType::PUBLIC
    } else {
        ObjectType::BROWSER
    }
}

/// One visible line of the tree.
enum Row<'a> {
    Folder {
        owner: &'a str,
        object_type: ObjectType,
        count: Option<usize>,
        open: bool,
    },
    Object {
        object: &'a DbObject,
        open: bool,
        expandable: bool,
    },
    /// A table column; whether it's part of a foreign key.
    Column(&'a TableColumn, bool),
    Group {
        table: ObjectRef,
        group: Group,
        count: usize,
        open: bool,
    },
    Part(&'a Part, ObjectRef),
    Member(&'a Member, ObjectRef),
    Section {
        section: Section,
        /// Unknown until loaded (users and roles).
        count: Option<usize>,
        open: bool,
    },
    /// An object listed under Favorites or Recent.
    Shortcut(&'a ObjectRef),
    /// A user or role.
    Account(&'a str, ObjectType),
    Message {
        depth: usize,
        text: String,
        error: bool,
    },
}

#[derive(Default)]
pub struct Browser {
    pub connection: Option<String>,
    schema: Option<String>,
    schemas: Option<Loaded<Vec<String>>>,
    filter: String,
    invalid_only: bool,
    changed: Changed,
    pub marks: Marks,
    folders: HashMap<(String, ObjectType), Loaded<Vec<DbObject>>>,
    /// Users and roles, by type.
    accounts: HashMap<ObjectType, Loaded<Vec<String>>>,
    children: HashMap<ObjectRef, Loaded<Children>>,
    parts: HashMap<ObjectRef, TableParts>,
    expanded: HashSet<Node>,
    pending: HashMap<u64, Pending>,
    selected: Option<ObjectRef>,
    /// Scroll to this object once its row exists.
    reveal: Option<ObjectRef>,
    confirm_drop: Option<ObjectRef>,
}

impl Browser {
    /// Shows a (newly connected) connection, starting in its current schema.
    pub fn attach(&mut self, connection: String, schema: String) -> Effects {
        let marks = std::mem::take(&mut self.marks);
        *self = Self {
            connection: Some(connection),
            schema: Some(schema),
            marks,
            ..Self::default()
        };
        self.load_schemas()
    }

    pub fn detach(&mut self, connection: &str) {
        if self.connection.as_deref() == Some(connection) {
            *self = Self::default();
        }
    }

    #[cfg_attr(not(test), expect(dead_code, reason = "used by screenshot tests"))]
    pub fn is_loading(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Shows another schema, keeping the same folders open.
    fn set_schema(&mut self, schema: String, effects: &mut Effects) {
        if self.schema.as_ref() == Some(&schema) {
            return;
        }
        self.schema = Some(schema);
        self.expanded.retain(|n| matches!(n, Node::Folder(..)));
        let folders: Vec<ObjectType> = self
            .expanded
            .drain()
            .filter_map(|n| {
                if let Node::Folder(_, t) = n {
                    Some(t)
                } else {
                    None
                }
            })
            .collect();
        let owner = self.schema.clone().unwrap_or_default();
        for t in folders {
            self.expanded.insert(Node::Folder(owner.clone(), t));
        }
        self.refresh(effects);
    }

    /// Shows an object in the tree: in its schema, with its folder open,
    /// selected, and scrolled to once the folder has loaded. Bodies show as
    /// their package or type.
    fn reveal(&mut self, object: ObjectRef, effects: &mut Effects) {
        let object_type = match object.object_type {
            ObjectType::PackageBody => ObjectType::Package,
            ObjectType::TypeBody => ObjectType::Type,
            t => t,
        };
        if !ObjectType::BROWSER.contains(&object_type) {
            effects.log.push(format!(
                "{} objects aren't listed in the object browser",
                object_type.dictionary_name().to_lowercase()
            ));
            return;
        }
        let object = ObjectRef {
            object_type,
            ..object
        };
        self.set_schema(object.owner.clone(), effects);
        if !object.name.contains(&self.filter.trim().to_uppercase()) {
            self.filter.clear();
        }
        let node = Node::Folder(object.owner.clone(), object_type);
        if self.expanded.insert(node)
            && !self
                .folders
                .contains_key(&(object.owner.clone(), object_type))
        {
            self.load_folder(effects, &object.owner, object_type);
        }
        self.selected = Some(object.clone());
        self.reveal = Some(object);
    }

    /// Expands the given folders of the current schema.
    #[cfg_attr(not(test), expect(dead_code, reason = "used by screenshot tests"))]
    pub fn expand(&mut self, types: &[ObjectType]) -> Effects {
        let mut effects = Effects::default();
        let owner = self.schema.clone().unwrap_or_default();
        for t in types {
            if self.expanded.insert(Node::Folder(owner.clone(), *t)) {
                self.load_folder(&mut effects, &owner, *t);
            }
        }
        effects
    }

    /// Opens Favorites and Recent, marks `favorite` and filters invalid objects.
    #[cfg(test)]
    pub fn test_marks(&mut self, favorite: ObjectRef) {
        let connection = self.connection.clone().unwrap_or_default();
        self.marks.toggle_favorite(&connection, favorite);
        self.expanded.insert(Node::Section(Section::Favorites));
        self.expanded.insert(Node::Section(Section::Recent));
        self.invalid_only = true;
    }

    #[cfg(test)]
    pub fn set_filter(&mut self, filter: &str) {
        self.filter = filter.to_owned();
    }

    #[cfg(test)]
    pub fn selected(&self) -> Option<&ObjectRef> {
        self.selected.as_ref()
    }

    /// Expands a table with all its groups.
    #[cfg(test)]
    pub fn expand_table(&mut self, table: ObjectRef) -> Effects {
        let mut effects = Effects::default();
        self.load_children(&mut effects, &table);
        for group in [
            Group::Indexes,
            Group::Constraints,
            Group::Triggers,
            Group::ReferencedBy,
        ] {
            self.expanded.insert(Node::Group(table.clone(), group));
        }
        self.expanded.insert(Node::Object(table));
        effects
    }

    /// The schema shown.
    pub fn schema(&self) -> Option<&str> {
        self.schema.as_deref()
    }

    pub fn owns(&self, id: u64) -> bool {
        self.pending.contains_key(&id)
    }

    fn load_schemas(&mut self) -> Effects {
        let mut effects = Effects::default();
        self.schemas = Some(Loaded::Loading);
        self.meta(&mut effects, Pending::Schemas, MetaQuery::Schemas);
        effects
    }

    fn meta(&mut self, effects: &mut Effects, pending: Pending, query: MetaQuery) {
        let id = request_id();
        self.pending.insert(id, pending);
        effects.request(Target::Main, DbRequest::Meta { id, query });
    }

    fn load_folder(&mut self, effects: &mut Effects, owner: &str, object_type: ObjectType) {
        self.folders
            .insert((owner.to_owned(), object_type), Loaded::Loading);
        self.meta(
            effects,
            Pending::Folder(owner.to_owned(), object_type),
            MetaQuery::Objects {
                owner: owner.to_owned(),
                object_type,
                filter: None,
            },
        );
    }

    /// Loads the users, roles or profiles of the database.
    fn load_accounts(&mut self, effects: &mut Effects, object_type: ObjectType) {
        self.accounts.insert(object_type, Loaded::Loading);
        let id = request_id();
        self.pending.insert(id, Pending::Accounts(object_type));
        let task: pythia_db::TaskFn = Box::new(move |conn: &pythia_db::oracle::Connection| {
            let names = account_names(conn, object_type)?;
            Ok(Box::new(names) as Box<dyn std::any::Any + Send>)
        });
        effects.request(Target::Main, DbRequest::Task { id, task });
    }

    fn load_children(&mut self, effects: &mut Effects, object: &ObjectRef) {
        let query = match object.object_type {
            t if t.has_data() => MetaQuery::Columns {
                owner: object.owner.clone(),
                table: object.name.clone(),
                keys: t == ObjectType::Table,
            },
            ObjectType::Package | ObjectType::Type => MetaQuery::Members {
                owner: object.owner.clone(),
                package: object.name.clone(),
            },
            _ => return,
        };
        self.children.insert(object.clone(), Loaded::Loading);
        self.meta(effects, Pending::Children(object.clone()), query);
        if object.object_type == ObjectType::Table {
            let id = request_id();
            self.pending.insert(id, Pending::Parts(object.clone()));
            let (owner, table) = (object.owner.clone(), object.name.clone());
            let task: pythia_db::TaskFn = Box::new(move |conn: &pythia_db::oracle::Connection| {
                let parts = crate::table_parts::load(conn, &owner, &table)?;
                Ok(Box::new(parts) as Box<dyn std::any::Any + Send>)
            });
            effects.request(Target::Main, DbRequest::Task { id, task });
        }
    }

    /// Reloads expanded folders (after compile/drop or on demand).
    fn refresh(&mut self, effects: &mut Effects) {
        self.children.clear();
        self.parts.clear();
        let open: Vec<(String, ObjectType)> = self
            .expanded
            .iter()
            .filter_map(|n| match n {
                Node::Folder(owner, t) => Some((owner.clone(), *t)),
                Node::Object(_) | Node::Group(..) | Node::Section(_) => None,
            })
            .collect();
        self.folders.clear();
        for (owner, t) in open {
            self.load_folder(effects, &owner, t);
        }
        let objects: Vec<ObjectRef> = self
            .expanded
            .iter()
            .filter_map(|n| match n {
                Node::Object(o) => Some(o.clone()),
                Node::Folder(..) | Node::Group(..) | Node::Section(_) => None,
            })
            .collect();
        for o in objects {
            self.load_children(effects, &o);
        }
        let accounts: Vec<ObjectType> = self.accounts.keys().copied().collect();
        for t in accounts {
            self.load_accounts(effects, t);
        }
    }

    /// An object was created or changed elsewhere (in an editor): lists it
    /// as it is now and shows it.
    pub fn show_changed(&mut self, object: ObjectRef) -> Effects {
        let mut effects = Effects::default();
        match Section::listing(object.object_type) {
            Some(section) => {
                self.load_accounts(&mut effects, object.object_type);
                self.expanded.insert(Node::Section(section));
            }
            None => {
                self.refresh(&mut effects);
                self.reveal(object, &mut effects);
            }
        }
        effects
    }

    fn open_test(
        &self,
        answer: crate::catalog::Answer,
        object: &ObjectRef,
        member: Option<&str>,
        effects: &mut Effects,
        log: &mut Vec<String>,
    ) {
        let crate::catalog::Answer::Members(_, _, members) = answer else {
            return;
        };
        let wanted = member.unwrap_or(&object.name);
        let Some(found) = members.iter().find(|m| m.name.eq_ignore_ascii_case(wanted)) else {
            log.push(format!("No parameters found for {wanted}"));
            return;
        };
        let qualified = match member {
            Some(name) => format!(
                "{}.{}",
                object.qualified(),
                crate::documents::quote_ident(name)
            ),
            None => object.qualified(),
        };
        let text = crate::test_script::test_block(&qualified.to_lowercase(), found);
        if let Some(connection) = self.connection.clone() {
            effects.open.push(OpenRequest::Sql {
                connection,
                text,
                run: false,
            });
        }
    }

    pub fn on_event(&mut self, event: DbEvent, log: &mut Vec<String>) -> Effects {
        let mut effects = Effects::default();
        if let DbEvent::TaskDone { id, result } = event {
            if let Some(Pending::Parts(object)) = self.pending.get(&id) {
                let object = object.clone();
                self.pending.remove(&id);
                // Without the parts the table still shows its columns.
                if let Ok(Ok(parts)) = result.map(|any| any.downcast::<TableParts>()) {
                    self.parts.insert(object, *parts);
                }
                return effects;
            }
            if let Some(Pending::Accounts(t)) = self.pending.get(&id) {
                let t = *t;
                self.pending.remove(&id);
                let loaded = match result.map(|any| any.downcast::<Vec<String>>()) {
                    Ok(Ok(names)) => Loaded::Ready(*names),
                    Ok(Err(_)) => Loaded::Failed("unexpected result".into()),
                    Err(e) => Loaded::Failed(e.to_string()),
                };
                self.accounts.insert(t, loaded);
                return effects;
            }
            if let Some(Pending::Test { object, member }) = self.pending.remove(&id) {
                match result.map(|any| any.downcast::<crate::catalog::Answer>()) {
                    Ok(Ok(answer)) => {
                        self.open_test(*answer, &object, member.as_deref(), &mut effects, log)
                    }
                    Ok(Err(_)) => {}
                    Err(e) => log.push(format!("Test script: {e}")),
                }
            }
            return effects;
        }
        if let DbEvent::Meta { id, result } = &event
            && let Some(Pending::Generate { object, kind }) = self.pending.get(id)
        {
            let (object, kind) = (object.clone(), *kind);
            self.pending.remove(id);
            match result {
                Ok(MetaResult::Columns(cols)) => {
                    effects.open.push(OpenRequest::InsertText {
                        text: template(kind, &object, cols),
                    });
                }
                Ok(_) => {}
                Err(e) => log.push(format!("Generate: {e}")),
            }
            return effects;
        }
        let (id, result) = match event {
            DbEvent::Meta { id, result } => (id, result.map(Some)),
            DbEvent::Executed { id, .. } => (id, Ok(None)),
            DbEvent::Failed { id, error, .. } => (id, Err(error)),
            _ => return effects,
        };
        let Some(pending) = self.pending.remove(&id) else {
            return effects;
        };
        match (pending, result) {
            (Pending::Schemas, Ok(Some(MetaResult::Schemas(mut s)))) => {
                // Public synonyms and links belong to PUBLIC, which isn't a user.
                if let Err(at) = s.binary_search_by(|u| u.as_str().cmp("PUBLIC")) {
                    s.insert(at, "PUBLIC".into());
                }
                self.schemas = Some(Loaded::Ready(s))
            }
            (Pending::Schemas, Err(e)) => self.schemas = Some(Loaded::Failed(e.to_string())),
            (Pending::Folder(owner, t), Ok(Some(MetaResult::Objects(objects)))) => {
                self.folders.insert((owner, t), Loaded::Ready(objects));
            }
            (Pending::Folder(owner, t), Err(e)) => {
                self.folders
                    .insert((owner, t), Loaded::Failed(e.to_string()));
            }
            (Pending::Children(o), Ok(Some(MetaResult::Columns(c)))) => {
                self.children.insert(o, Loaded::Ready(Children::Columns(c)));
            }
            (Pending::Children(o), Ok(Some(MetaResult::Members(m)))) => {
                self.children.insert(o, Loaded::Ready(Children::Members(m)));
            }
            (Pending::Children(o), Err(e)) => {
                self.children.insert(o, Loaded::Failed(e.to_string()));
            }
            (Pending::Action { object, verb }, Ok(_)) => {
                let ty = object.object_type.dictionary_name().to_lowercase();
                log.push(format!("{ty} {} {verb}", object.qualified()));
                self.refresh(&mut effects);
                effects.invalid_changed = true;
            }
            (Pending::Action { object, verb }, Err(e)) => {
                log.push(format!("{} {} failed: {e}", verb, object.qualified()));
                self.refresh(&mut effects);
            }
            (Pending::TestLink(object), Ok(Some(MetaResult::Text(remote)))) => {
                log.push(format!("Database link {} works: {remote}", object.name));
            }
            (Pending::TestLink(object), Err(e)) => {
                log.push(format!("Database link {} failed: {e}", object.name));
            }
            _ => {}
        }
        effects
    }

    fn run(&mut self, effects: &mut Effects, object: &ObjectRef, verb: &'static str, sql: String) {
        let id = request_id();
        self.pending.insert(
            id,
            Pending::Action {
                object: object.clone(),
                verb,
            },
        );
        effects.request(
            Target::Main,
            DbRequest::Execute {
                id,
                sql,
                max_rows: 1,
                binds: Vec::new(),
            },
        );
    }

    pub fn ui(&mut self, ui: &mut Ui, connections: &[String]) -> Effects {
        let mut effects = Effects::default();
        let Some(connection) = self.connection.clone() else {
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                ui.weak("Not connected");
                ui.add_space(4.0);
                ui.weak("Connect to browse tables, packages and other objects.");
                ui.add_space(8.0);
                if pythia_ui::icons::icon_text_button(ui, Icon::Connect, "Connect…").clicked() {
                    effects.open.push(OpenRequest::ConnectDialog);
                }
            });
            return effects;
        };

        // Header: connection, schema, refresh, filter.
        ui.horizontal(|ui| {
            if connections.len() > 1 {
                egui::ComboBox::from_id_salt("browser-connection")
                    .selected_text(&connection)
                    .show_ui(ui, |ui| {
                        for c in connections {
                            if ui.selectable_label(*c == connection, c).clicked()
                                && *c != connection
                            {
                                effects.open.push(OpenRequest::Browse {
                                    connection: c.clone(),
                                });
                            }
                        }
                    });
            }
            let schema = self.schema.clone().unwrap_or_default();
            let mut chosen = None;
            egui::ComboBox::from_id_salt("browser-schema")
                .selected_text(&schema)
                .height(400.0)
                .show_ui(ui, |ui| match &self.schemas {
                    Some(Loaded::Ready(list)) => {
                        for s in list {
                            if ui.selectable_label(*s == schema, s).clicked() {
                                chosen = Some(s.clone());
                            }
                        }
                    }
                    Some(Loaded::Failed(e)) => {
                        ui.colored_label(ui.visuals().error_fg_color, e);
                    }
                    _ => {
                        ui.spinner();
                    }
                });
            if let Some(s) = chosen {
                self.set_schema(s, &mut effects);
            }
            if pythia_ui::icons::icon_button(ui, Icon::Refresh, "Refresh").clicked() {
                self.refresh(&mut effects);
            }
            if schema != "PUBLIC"
                && pythia_ui::icons::icon_button(
                    ui,
                    Icon::Compile,
                    "Recompile invalid objects in this schema",
                )
                .clicked()
                && let Some(owner) = self.schema.clone()
            {
                // In the invalid objects window: dependency order, progress.
                effects.open.push(OpenRequest::InvalidObjects {
                    connection: connection.clone(),
                    schema: Some(owner),
                    recompile: true,
                });
            }
        });
        ui.horizontal(|ui| {
            let filtered = self.invalid_only || self.changed != Changed::Any;
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text(if filtered {
                        "Filter objects… (more filters on)"
                    } else {
                        "Filter objects…"
                    })
                    .desired_width(ui.available_width() - 34.0),
            );
            let icon = if filtered {
                Icon::Filter.image(ui).tint(ui.visuals().hyperlink_color)
            } else {
                Icon::Filter.image(ui)
            };
            ui.menu_image_button(icon, |ui| {
                ui.checkbox(&mut self.invalid_only, "Invalid objects only");
                ui.separator();
                ui.label(RichText::new("Changed").strong());
                for (value, label) in [
                    (Changed::Any, "Any time"),
                    (Changed::Day, "Last 24 hours"),
                    (Changed::Week, "Last 7 days"),
                    (Changed::Month, "Last 30 days"),
                ] {
                    ui.radio_value(&mut self.changed, value, label);
                }
            })
            .response
            .on_hover_text("Filter by status and change date");
        });
        ui.add_space(2.0);

        let owner = self.schema.clone().unwrap_or_default();
        let rows = self.rows(&owner);
        let row_height = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        let mut toggles = Vec::new();
        let mut actions = Vec::new();
        let revealed = self.reveal.as_ref().and_then(|target| {
            rows.iter().position(
                |r| matches!(r, Row::Object { object, .. } if object_ref(object) == *target),
            )
        });
        let mut scroll = egui::ScrollArea::both().auto_shrink(false);
        if let Some(row) = revealed {
            let above = (ui.available_height() / row_height / 3.0).floor();
            scroll = scroll.vertical_scroll_offset((row as f32 - above).max(0.0) * row_height);
        }
        scroll.show_rows(ui, row_height, rows.len(), |ui, range| {
            for row in &rows[range] {
                self.row_ui(ui, row, row_height, &mut toggles, &mut actions);
            }
        });
        drop(rows);
        if revealed.is_some() {
            self.reveal = None;
        } else if let Some(target) = self.reveal.clone()
            && let Some(Loaded::Ready(objects)) = self
                .folders
                .get(&(target.owner.clone(), target.object_type))
        {
            // Loaded: either filtered out (show it anyway) or not there.
            if objects.iter().any(|o| object_ref(o) == target) {
                self.filter.clear();
                self.invalid_only = false;
                self.changed = Changed::Any;
            } else {
                self.reveal = None;
                effects
                    .log
                    .push(format!("{} not found", target.qualified()));
            }
        }

        for node in toggles {
            if !self.expanded.remove(&node) {
                match &node {
                    Node::Folder(owner, t) if !self.folders.contains_key(&(owner.clone(), *t)) => {
                        self.load_folder(&mut effects, &owner.clone(), *t);
                    }
                    Node::Object(o) if !self.children.contains_key(o) => {
                        self.load_children(&mut effects, &o.clone())
                    }
                    Node::Section(section) => {
                        if let Some(t) = section.accounts()
                            && !self.accounts.contains_key(&t)
                        {
                            self.load_accounts(&mut effects, t);
                        }
                    }
                    _ => {}
                }
                self.expanded.insert(node);
            }
        }
        for action in actions {
            self.action(action, &connection, ui.ctx(), &mut effects);
        }
        self.drop_confirm_ui(ui.ctx(), &mut effects);
        effects
    }

    fn rows<'a>(&'a self, owner: &'a str) -> Vec<Row<'a>> {
        let filter = self.filter.trim().to_uppercase();
        let mut rows = Vec::new();
        let connection = self.connection.as_deref().unwrap_or_default();
        for (section, list) in [
            (Section::Favorites, self.marks.favorites.get(connection)),
            (Section::Recent, self.marks.recent.get(connection)),
        ] {
            let Some(list) = list.filter(|l| !l.is_empty()) else {
                continue;
            };
            let matching: Vec<&ObjectRef> = list
                .iter()
                .filter(|o| filter.is_empty() || o.name.contains(&filter))
                .collect();
            if matching.is_empty() {
                continue;
            }
            let open = self.expanded.contains(&Node::Section(section));
            rows.push(Row::Section {
                section,
                count: Some(matching.len()),
                open,
            });
            if open {
                rows.extend(matching.into_iter().map(Row::Shortcut));
            }
        }
        // Change date cutoff, compared as text with LAST_DDL_TIME.
        let cutoff = self.changed.days().map(|d| {
            (chrono::Local::now() - chrono::Duration::days(d))
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        });
        let shown = |o: &DbObject| {
            (filter.is_empty() || o.name.contains(&filter))
                && (!self.invalid_only || !o.valid)
                && cutoff.as_deref().is_none_or(|c| o.last_ddl.as_str() >= c)
        };
        let filtering = !filter.is_empty() || self.invalid_only || cutoff.is_some();
        for &object_type in folders(owner) {
            let node = Node::Folder(owner.to_owned(), object_type);
            let open = self.expanded.contains(&node);
            let folder = self.folders.get(&(owner.to_owned(), object_type));
            let matching: Vec<&DbObject> = match folder {
                Some(Loaded::Ready(objects)) => objects.iter().filter(|o| shown(o)).collect(),
                _ => Vec::new(),
            };
            let count = matches!(folder, Some(Loaded::Ready(_))).then_some(matching.len());
            if filtering && count == Some(0) {
                continue;
            }
            rows.push(Row::Folder {
                owner,
                object_type,
                count,
                open,
            });
            if !open {
                continue;
            }
            match folder {
                Some(Loaded::Loading) | None => rows.push(Row::Message {
                    depth: 1,
                    text: "Loading…".into(),
                    error: false,
                }),
                Some(Loaded::Failed(e)) => rows.push(Row::Message {
                    depth: 1,
                    text: e.clone(),
                    error: true,
                }),
                Some(Loaded::Ready(_)) if matching.is_empty() => rows.push(Row::Message {
                    depth: 1,
                    text: "(none)".into(),
                    error: false,
                }),
                Some(Loaded::Ready(_)) => {}
            }
            for object in matching {
                let r = object_ref(object);
                let expandable = object_type.has_data()
                    || matches!(object_type, ObjectType::Package | ObjectType::Type);
                let open = expandable && self.expanded.contains(&Node::Object(r.clone()));
                rows.push(Row::Object {
                    object,
                    open,
                    expandable,
                });
                if !open {
                    continue;
                }
                match self.children.get(&r) {
                    Some(Loaded::Ready(Children::Columns(cols))) => {
                        let parts = self.parts.get(&r);
                        rows.extend(cols.iter().map(|c| {
                            let fk = parts.is_some_and(|p| p.fk_columns.contains(&c.name));
                            Row::Column(c, fk)
                        }));
                        if let Some(parts) = parts {
                            for (group, items) in [
                                (Group::Indexes, &parts.indexes),
                                (Group::Constraints, &parts.constraints),
                                (Group::Triggers, &parts.triggers),
                                (Group::ReferencedBy, &parts.referenced_by),
                            ] {
                                if items.is_empty() {
                                    continue;
                                }
                                let open = self.expanded.contains(&Node::Group(r.clone(), group));
                                rows.push(Row::Group {
                                    table: r.clone(),
                                    group,
                                    count: items.len(),
                                    open,
                                });
                                if open {
                                    rows.extend(items.iter().map(|p| Row::Part(p, r.clone())));
                                }
                            }
                        }
                    }
                    Some(Loaded::Ready(Children::Members(members))) if members.is_empty() => rows
                        .push(Row::Message {
                            depth: 2,
                            text: "(no subprograms)".into(),
                            error: false,
                        }),
                    Some(Loaded::Ready(Children::Members(members))) => {
                        rows.extend(members.iter().map(|m| Row::Member(m, r.clone())))
                    }
                    Some(Loaded::Failed(e)) => rows.push(Row::Message {
                        depth: 2,
                        text: e.clone(),
                        error: true,
                    }),
                    _ => rows.push(Row::Message {
                        depth: 2,
                        text: "Loading…".into(),
                        error: false,
                    }),
                }
            }
        }
        for section in Section::DATABASE {
            let Some(object_type) = section.accounts() else {
                continue;
            };
            let open = self.expanded.contains(&Node::Section(section));
            let list = self.accounts.get(&object_type);
            let matching: Vec<&String> = match list {
                Some(Loaded::Ready(names)) => names
                    .iter()
                    .filter(|n| filter.is_empty() || n.contains(&filter))
                    .collect(),
                _ => Vec::new(),
            };
            let count = matches!(list, Some(Loaded::Ready(_))).then_some(matching.len());
            if filtering && count == Some(0) {
                continue;
            }
            rows.push(Row::Section {
                section,
                count,
                open,
            });
            if !open {
                continue;
            }
            match list {
                Some(Loaded::Loading) | None => rows.push(Row::Message {
                    depth: 1,
                    text: "Loading…".into(),
                    error: false,
                }),
                Some(Loaded::Failed(e)) => rows.push(Row::Message {
                    depth: 1,
                    text: e.clone(),
                    error: true,
                }),
                Some(Loaded::Ready(_)) => rows.extend(
                    matching
                        .into_iter()
                        .map(|name| Row::Account(name.as_str(), object_type)),
                ),
            }
        }
        rows
    }

    fn row_ui(
        &self,
        ui: &mut Ui,
        row: &Row,
        height: f32,
        toggles: &mut Vec<Node>,
        actions: &mut Vec<ObjectAction>,
    ) {
        const INDENT: f32 = 18.0;
        let small = IconSize::Small;
        // Chevron toggle, or the same space when there's nothing to expand.
        let chevron = |ui: &mut Ui, open: bool, expandable: bool| -> bool {
            if !expandable {
                ui.add_space(small.points());
                return false;
            }
            let icon = if open {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            };
            let color = ui.visuals().weak_text_color();
            ui.add(
                icon.mono(color)
                    .fit_to_exact_size(egui::vec2(small.points(), small.points()))
                    .sense(Sense::click()),
            )
            .clicked()
        };
        ui.horizontal(|ui| {
            ui.set_height(height);
            ui.spacing_mut().item_spacing.x = 5.0;
            match row {
                Row::Folder {
                    owner,
                    object_type,
                    count,
                    open,
                } => {
                    let toggle = chevron(ui, *open, true);
                    let folder = if *open {
                        Icon::FolderOpen
                    } else {
                        Icon::Folder
                    };
                    ui.add(folder.sized(ui, small));
                    let label = ui.add(
                        egui::Label::new(RichText::new(object_type.plural()).strong())
                            .sense(Sense::click()),
                    );
                    if let Some(n) = count {
                        ui.weak(n.to_string());
                    }
                    if toggle || label.clicked() {
                        toggles.push(Node::Folder((*owner).to_owned(), *object_type));
                    }
                    if can_create(*object_type) {
                        label.context_menu(|ui| {
                            let kind = object_type.dictionary_name().to_lowercase();
                            let label = if *owner == "PUBLIC" {
                                format!("New public {kind}…")
                            } else {
                                format!("New {kind}…")
                            };
                            if ui.button(label).clicked() {
                                actions.push(ObjectAction::New((*owner).to_owned(), *object_type));
                                ui.close();
                            }
                        });
                    }
                }
                Row::Object {
                    object,
                    open,
                    expandable,
                } => {
                    ui.add_space(INDENT);
                    let r = object_ref(object);
                    if chevron(ui, *open, *expandable) {
                        toggles.push(Node::Object(r.clone()));
                    }
                    let icon = Icon::for_object_type(object.object_type.dictionary_name())
                        .unwrap_or(Icon::Text);
                    let icon_response = ui.add(icon.sized(ui, small));
                    if !object.valid {
                        paint_invalid_badge(ui, icon_response.rect);
                    }
                    let mut text = RichText::new(object.name.to_lowercase()).monospace();
                    if !object.valid {
                        text = text.color(ui.visuals().error_fg_color);
                    }
                    let selected = self.selected.as_ref() == Some(&r);
                    let response = self.drag_source(ui, &r, |ui| {
                        ui.add(
                            egui::Button::selectable(selected, text).sense(Sense::click_and_drag()),
                        )
                    });
                    let response = response.on_hover_ui(|ui| {
                        ui.label(RichText::new(r.qualified()).strong());
                        ui.label(object.object_type.dictionary_name().to_lowercase());
                        if !object.valid {
                            ui.colored_label(ui.visuals().error_fg_color, "Invalid");
                        }
                        ui.weak(format!("Last DDL: {}", object.last_ddl));
                    });
                    if response.clicked() {
                        actions.push(ObjectAction::Select(r.clone()));
                    }
                    if response.double_clicked() {
                        actions.push(ObjectAction::Default(r.clone()));
                    }
                    response.context_menu(|ui| self.context_menu(ui, &r, actions));
                }
                Row::Column(c, fk) => {
                    ui.add_space(INDENT * 2.0 + small.points());
                    let icon = if c.pk_position.is_some() {
                        Icon::PrimaryKey
                    } else if *fk {
                        Icon::ForeignKey
                    } else {
                        Icon::Column
                    };
                    ui.add(icon.sized(ui, small));
                    let name = c.name.to_lowercase();
                    let label = ui.add(
                        egui::Label::new(RichText::new(&name).monospace())
                            .sense(Sense::drag())
                            .selectable(false),
                    );
                    drag_text(ui, &label, name);
                    let null = if c.nullable { "" } else { " not null" };
                    let label = ui.weak(format!("{}{null}", c.data_type.to_lowercase()));
                    if let Some(comment) = &c.comment {
                        label.on_hover_text(comment);
                    }
                }
                Row::Group {
                    table,
                    group,
                    count,
                    open,
                } => {
                    ui.add_space(INDENT * 2.0);
                    if chevron(ui, *open, true) {
                        toggles.push(Node::Group(table.clone(), *group));
                    }
                    let label = ui.add(
                        egui::Label::new(RichText::new(group.label()).strong())
                            .sense(Sense::click()),
                    );
                    if label.clicked() {
                        toggles.push(Node::Group(table.clone(), *group));
                    }
                    if *group != Group::ReferencedBy {
                        label.context_menu(|ui| group_menu(ui, table, *group, actions));
                    }
                    ui.weak(count.to_string());
                }
                Row::Part(part, table) => {
                    ui.add_space(INDENT * 3.0 + small.points());
                    let icon = match part.kind {
                        PartKind::Index { .. } => Icon::Index,
                        PartKind::Constraint { kind: 'P' } => Icon::PrimaryKey,
                        PartKind::Constraint { kind: 'R' } => Icon::ForeignKey,
                        PartKind::Constraint { .. } => Icon::Constraint,
                        PartKind::Trigger => Icon::Trigger,
                    };
                    ui.add(icon.sized(ui, small));
                    let mut text = RichText::new(part.name.to_lowercase()).monospace();
                    if !part.enabled {
                        text = text.strikethrough().color(ui.visuals().weak_text_color());
                    }
                    let label = ui.add(egui::Label::new(text).sense(Sense::click()));
                    let tag = match part.kind {
                        PartKind::Index { unique: true } => "unique ",
                        PartKind::Constraint { kind: 'U' } => "unique ",
                        PartKind::Constraint { kind: 'C' } => "check ",
                        _ => "",
                    };
                    ui.weak(format!("{tag}{}", part.detail));
                    let state = if part.enabled { "" } else { " (disabled)" };
                    let label = label.on_hover_text(format!(
                        "{}{state}\n{tag}{}",
                        part.name.to_lowercase(),
                        part.detail
                    ));
                    let trigger = ObjectRef {
                        owner: table.owner.clone(),
                        name: part.name.clone(),
                        object_type: ObjectType::Trigger,
                    };
                    if part.kind == PartKind::Trigger && label.double_clicked() {
                        actions.push(ObjectAction::Edit(trigger.clone()));
                    }
                    label.context_menu(|ui| {
                        if part.kind == PartKind::Trigger && ui.button("Edit trigger").clicked() {
                            actions.push(ObjectAction::Edit(trigger.clone()));
                            ui.close();
                        }
                        if ui.button("Copy name").clicked() {
                            ui.ctx().copy_text(part.name.to_lowercase());
                            ui.close();
                        }
                    });
                }
                Row::Member(m, package) => {
                    ui.add_space(INDENT * 2.0 + small.points());
                    let icon = if m.is_function {
                        Icon::Function
                    } else {
                        Icon::Procedure
                    };
                    ui.add(icon.sized(ui, small));
                    let label = ui.add(
                        egui::Label::new(RichText::new(m.name.to_lowercase()).monospace())
                            .sense(Sense::click()),
                    );
                    ui.weak(&m.signature);
                    label.context_menu(|ui| {
                        if ui.button("Test…").clicked() {
                            actions.push(ObjectAction::Test(package.clone(), Some(m.name.clone())));
                            ui.close();
                        }
                        if ui.button("Edit package body").clicked() {
                            let body = ObjectRef {
                                object_type: ObjectType::PackageBody,
                                ..package.clone()
                            };
                            actions.push(ObjectAction::Edit(body));
                            ui.close();
                        }
                    });
                }
                Row::Section {
                    section,
                    count,
                    open,
                } => {
                    if chevron(ui, *open, true) {
                        toggles.push(Node::Section(*section));
                    }
                    let (icon, label) = match section {
                        Section::Favorites => (Icon::Star, "Favorites"),
                        Section::Recent => (Icon::History, "Recent"),
                        Section::Users => (Icon::Schema, "Users"),
                        Section::Roles => (Icon::Lock, "Roles"),
                        Section::Profiles => (Icon::Settings, "Profiles"),
                    };
                    ui.add(icon.sized(ui, small));
                    let r = ui
                        .add(egui::Label::new(RichText::new(label).strong()).sense(Sense::click()));
                    if r.clicked() {
                        toggles.push(Node::Section(*section));
                    }
                    if let Some(t) = section.accounts() {
                        r.context_menu(|ui| {
                            let kind = t.dictionary_name().to_lowercase();
                            if ui.button(format!("New {kind}…")).clicked() {
                                actions.push(ObjectAction::New(String::new(), t));
                                ui.close();
                            }
                        });
                    }
                    if let Some(n) = count {
                        ui.weak(n.to_string());
                    }
                }
                Row::Account(name, object_type) => {
                    ui.add_space(INDENT);
                    ui.add_space(small.points());
                    let icon =
                        Icon::for_object_type(object_type.dictionary_name()).unwrap_or(Icon::Text);
                    ui.add(icon.sized(ui, small));
                    let account = ObjectRef {
                        owner: String::new(),
                        name: (*name).to_owned(),
                        object_type: *object_type,
                    };
                    let selected = self.selected.as_ref() == Some(&account);
                    let response = ui.add(egui::Button::selectable(
                        selected,
                        RichText::new(name.to_lowercase()).monospace(),
                    ));
                    if response.clicked() {
                        actions.push(ObjectAction::Select(account.clone()));
                    }
                    if response.double_clicked() {
                        actions.push(ObjectAction::EditObject(account.clone()));
                    }
                    response.context_menu(|ui| account_menu(ui, &account, actions));
                }
                Row::Shortcut(object) => {
                    ui.add_space(INDENT);
                    ui.add_space(small.points());
                    let icon = Icon::for_object_type(object.object_type.dictionary_name())
                        .unwrap_or(Icon::Text);
                    ui.add(icon.sized(ui, small));
                    let selected = self.selected.as_ref() == Some(*object);
                    let mut text = RichText::new(object.name.to_lowercase()).monospace();
                    if Some(object.owner.as_str()) != self.schema.as_deref() {
                        text = RichText::new(format!(
                            "{}.{}",
                            object.owner.to_lowercase(),
                            object.name.to_lowercase()
                        ))
                        .monospace();
                    }
                    let response = self.drag_source(ui, object, |ui| {
                        ui.add(
                            egui::Button::selectable(selected, text).sense(Sense::click_and_drag()),
                        )
                    });
                    let response = response.on_hover_text(format!(
                        "{} {}",
                        object.object_type.dictionary_name().to_lowercase(),
                        object.qualified()
                    ));
                    if response.clicked() {
                        actions.push(ObjectAction::Select((*object).clone()));
                    }
                    if response.double_clicked() {
                        actions.push(ObjectAction::Default((*object).clone()));
                    }
                    response.context_menu(|ui| self.context_menu(ui, object, actions));
                }
                Row::Message { depth, text, error } => {
                    ui.add_space(INDENT * *depth as f32 + small.points());
                    if *error {
                        ui.colored_label(ui.visuals().error_fg_color, text);
                    } else {
                        ui.weak(text);
                    }
                }
            }
        });
    }

    /// Lets an object's row be dragged into an editor, which inserts its
    /// name (qualified when it's in another schema). `add` must sense drags.
    fn drag_source(
        &self,
        ui: &mut Ui,
        object: &ObjectRef,
        add: impl FnOnce(&mut Ui) -> egui::Response,
    ) -> egui::Response {
        let name = if Some(object.owner.as_str()) == self.schema.as_deref() {
            crate::documents::quote_ident(&object.name).to_lowercase()
        } else {
            object.qualified().to_lowercase()
        };
        let response = add(ui);
        drag_text(ui, &response, name);
        response
    }

    fn context_menu(&self, ui: &mut Ui, object: &ObjectRef, actions: &mut Vec<ObjectAction>) {
        let connection = self.connection.as_deref().unwrap_or_default();
        let options = MenuOptions {
            favorite: self.marks.is_favorite(connection, object),
            drop: true,
            reveal: false,
            member: None,
            create: true,
        };
        actions.extend(object_menu(ui, object, &options));
    }

    /// Carries out an action chosen outside the browser, e.g. in an editor's
    /// context menu, on the attached connection.
    pub fn perform(&mut self, action: ObjectAction, ctx: &egui::Context) -> Effects {
        let mut effects = Effects::default();
        if let Some(connection) = self.connection.clone() {
            self.action(action, &connection, ctx, &mut effects);
        }
        effects
    }

    fn action(
        &mut self,
        action: ObjectAction,
        connection: &str,
        ctx: &egui::Context,
        effects: &mut Effects,
    ) {
        let connection = connection.to_owned();
        match action {
            ObjectAction::Select(o) => self.selected = Some(o),
            ObjectAction::Default(o) if o.object_type.has_source() => {
                effects.open.push(OpenRequest::Program {
                    connection,
                    object: o,
                });
            }
            ObjectAction::Default(o) | ObjectAction::Describe(o) => {
                effects.open.push(OpenRequest::Object {
                    connection,
                    object: o,
                })
            }
            ObjectAction::Query(o) => effects.open.push(OpenRequest::Sql {
                connection,
                text: format!("select * from {}", o.qualified()),
                run: true,
            }),
            ObjectAction::Edit(o) => effects.open.push(OpenRequest::Program {
                connection,
                object: o,
            }),
            ObjectAction::Compile(o) => {
                // As the invalid objects window does: keeping each unit's
                // compiler settings, debug information included.
                let id = pythia_db::recompile::ObjectId::new(
                    &o.owner,
                    &o.name,
                    o.object_type.dictionary_name(),
                );
                match pythia_db::recompile::compile_sql(&id) {
                    Some(sql) => self.run(effects, &o, "compiled", sql),
                    None => effects
                        .log
                        .push(format!("{} can't be compiled", o.qualified())),
                }
            }
            ObjectAction::CopyName(o) => ctx.copy_text(o.name.clone()),
            ObjectAction::CopyQualified(o) => ctx.copy_text(o.qualified()),
            ObjectAction::Drop(o) => self.confirm_drop = Some(o),
            ObjectAction::ToggleFavorite(o) => {
                let opening = !self.marks.is_favorite(&connection, &o);
                self.marks.toggle_favorite(&connection, o);
                if opening {
                    self.expanded.insert(Node::Section(Section::Favorites));
                }
            }
            ObjectAction::EditTable(object, tab) => effects.open.push(OpenRequest::EditTable {
                connection,
                object,
                tab,
            }),
            ObjectAction::New(owner, ObjectType::Table) => {
                effects.open.push(OpenRequest::NewTable {
                    connection,
                    owner,
                    like: None,
                })
            }
            ObjectAction::New(owner, object_type) => effects.open.push(OpenRequest::EditObject {
                connection,
                target: EditTarget::New {
                    owner,
                    object_type,
                    from: None,
                },
            }),
            ObjectAction::NewFrom(ObjectType::Table, like) => {
                effects.open.push(OpenRequest::NewTable {
                    connection,
                    owner: like.owner.clone(),
                    like: Some(like),
                })
            }
            ObjectAction::NewFrom(object_type, from) => {
                // A synonym goes to the schema shown, other objects to the
                // one of the object they're on.
                let owner = match object_type {
                    ObjectType::Synonym => {
                        self.schema.clone().unwrap_or_else(|| from.owner.clone())
                    }
                    _ => from.owner.clone(),
                };
                effects.open.push(OpenRequest::EditObject {
                    connection,
                    target: EditTarget::New {
                        owner,
                        object_type,
                        from: Some(from),
                    },
                })
            }
            ObjectAction::ShowSchema(user) => {
                self.set_schema(user, effects);
            }
            ObjectAction::EditObject(object) => effects.open.push(OpenRequest::EditObject {
                connection,
                target: EditTarget::Edit(object),
            }),
            ObjectAction::Grants(object) => effects.open.push(OpenRequest::EditObject {
                connection,
                target: EditTarget::Grants(object),
            }),
            ObjectAction::Generate(object, kind) => {
                let id = request_id();
                let query = MetaQuery::Columns {
                    owner: object.owner.clone(),
                    table: object.name.clone(),
                    keys: object.object_type == ObjectType::Table,
                };
                self.pending.insert(id, Pending::Generate { object, kind });
                effects.request(Target::Main, DbRequest::Meta { id, query });
            }
            ObjectAction::EditAt(object, member) => effects.open.push(OpenRequest::ProgramMember {
                connection,
                object,
                member,
                parameter: None,
            }),
            ObjectAction::Reveal(object) => self.reveal(object, effects),
            ObjectAction::TestLink(object) => {
                let id = request_id();
                let query = MetaQuery::TestLink {
                    owner: object.owner.clone(),
                    name: object.name.clone(),
                };
                self.pending.insert(id, Pending::TestLink(object));
                effects.request(Target::Meta, DbRequest::Meta { id, query });
            }
            ObjectAction::Test(object, member) => {
                let id = request_id();
                let request = pythia_lsp_core::CatalogRequest::Members {
                    owner: object.owner.clone(),
                    name: object.name.clone(),
                };
                self.pending.insert(id, Pending::Test { object, member });
                effects.request(
                    Target::Main,
                    DbRequest::Task {
                        id,
                        task: crate::catalog::task(request),
                    },
                );
            }
        }
    }

    fn drop_confirm_ui(&mut self, ctx: &egui::Context, effects: &mut Effects) {
        let Some(object) = self.confirm_drop.clone() else {
            return;
        };
        let mut decision = None;
        let modal = egui::Modal::new(egui::Id::new("drop-object")).show(ctx, |ui| {
            ui.set_max_width(380.0);
            ui.heading("Drop object");
            ui.add_space(6.0);
            ui.label(format!(
                "Drop {} {}? This cannot be undone.",
                object.object_type.dictionary_name().to_lowercase(),
                object.qualified()
            ));
            match object.object_type {
                ObjectType::User => {
                    ui.weak("A user who owns objects isn't dropped (that needs CASCADE).");
                }
                ObjectType::Profile => {
                    ui.weak("Users with this profile get the DEFAULT profile.");
                }
                _ => {}
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let drop = egui::Button::new(RichText::new("Drop").color(Color32::WHITE))
                    .fill(Color32::from_rgb(0xc0, 0x30, 0x30));
                if ui.add(drop).clicked() {
                    decision = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    decision = Some(false);
                }
            });
        });
        if modal.should_close() && decision.is_none() {
            decision = Some(false);
        }
        if let Some(yes) = decision {
            self.confirm_drop = None;
            if yes {
                self.run(effects, &object, "dropped", drop_sql(&object));
            }
        }
    }
}

/// The New submenu of an object: another object of its type, and objects
/// on it (label, action).
fn new_entries(object: &ObjectRef) -> Vec<(String, ObjectAction)> {
    let t = object.object_type;
    let on = |kind: ObjectType| ObjectAction::NewFrom(kind, object.clone());
    let mut out = Vec::new();
    if can_create(t) {
        let kind = t.dictionary_name().to_lowercase();
        let mut label = format!("{kind}…");
        label[..1].make_ascii_uppercase();
        out.push((label, ObjectAction::New(object.owner.clone(), t)));
    }
    match t {
        ObjectType::Table => out.extend([
            ("Table like this…".to_owned(), on(ObjectType::Table)),
            ("Index on this table…".to_owned(), on(ObjectType::Index)),
            ("Trigger on this table…".to_owned(), on(ObjectType::Trigger)),
            ("View on this table…".to_owned(), on(ObjectType::View)),
        ]),
        ObjectType::View => out.extend([
            ("Trigger on this view…".to_owned(), on(ObjectType::Trigger)),
            ("View on this view…".to_owned(), on(ObjectType::View)),
        ]),
        _ => {}
    }
    if matches!(
        t,
        ObjectType::Table
            | ObjectType::View
            | ObjectType::MaterializedView
            | ObjectType::Sequence
            | ObjectType::Package
            | ObjectType::Procedure
            | ObjectType::Function
            | ObjectType::Type
    ) {
        out.push(("Synonym for this…".to_owned(), on(ObjectType::Synonym)));
    }
    out
}

/// The context menu of a table's Indexes, Constraints or Triggers.
fn group_menu(ui: &mut Ui, table: &ObjectRef, group: Group, actions: &mut Vec<ObjectAction>) {
    let (label, action) = match group {
        Group::Indexes => (
            "New index…",
            ObjectAction::NewFrom(ObjectType::Index, table.clone()),
        ),
        Group::Constraints => (
            "Add constraint…",
            ObjectAction::EditTable(table.clone(), TableTab::Keys),
        ),
        Group::Triggers => (
            "New trigger…",
            ObjectAction::NewFrom(ObjectType::Trigger, table.clone()),
        ),
        Group::ReferencedBy => return,
    };
    if ui.button(label).clicked() {
        actions.push(action);
        ui.close();
    }
}

/// The context menu of a user, role or profile.
fn account_menu(ui: &mut Ui, account: &ObjectRef, actions: &mut Vec<ObjectAction>) {
    let kind = account.object_type.dictionary_name().to_lowercase();
    let mut item = |ui: &mut Ui, label: &str, action: ObjectAction| {
        if ui.button(label).clicked() {
            actions.push(action);
            ui.close();
        }
    };
    item(
        ui,
        &format!("Edit {kind}…"),
        ObjectAction::EditObject(account.clone()),
    );
    if account.object_type == ObjectType::User {
        item(
            ui,
            "Show schema",
            ObjectAction::ShowSchema(account.name.clone()),
        );
    }
    item(
        ui,
        &format!("New {kind}…"),
        ObjectAction::New(String::new(), account.object_type),
    );
    ui.separator();
    item(ui, "Copy name", ObjectAction::CopyName(account.clone()));
    // The DEFAULT profile can't be dropped.
    if !(account.object_type == ObjectType::Profile && account.name == "DEFAULT") {
        ui.separator();
        item(ui, "Drop…", ObjectAction::Drop(account.clone()));
    }
}

/// Names of the database's users, of its roles (all of them with catalog
/// access, else those granted to the session's user), or of its profiles
/// (with catalog access).
fn account_names(
    conn: &pythia_db::oracle::Connection,
    object_type: ObjectType,
) -> Result<Vec<String>, pythia_db::DbError> {
    let read = |sql: &str| -> Result<Vec<String>, pythia_db::DbError> {
        let mut out = Vec::new();
        for row in conn.query_as::<String>(sql, &[])? {
            out.push(row?);
        }
        Ok(out)
    };
    match object_type {
        ObjectType::User => read("select username from all_users order by username"),
        ObjectType::Profile => read("select distinct profile from dba_profiles order by profile")
            .map_err(|_| pythia_db::DbError::new("Listing profiles needs access to DBA_PROFILES")),
        _ => read("select role from dba_roles order by role")
            .or_else(|_| read("select granted_role from user_role_privs order by granted_role")),
    }
}

/// `DROP` for an object. Public synonyms and links have their own syntax,
/// and a link's name can't be qualified: its dots belong to the name.
fn drop_sql(o: &ObjectRef) -> String {
    let kind = o.object_type.dictionary_name().to_lowercase();
    match (o.owner.as_str(), o.object_type) {
        ("PUBLIC", _) => format!("drop public {kind} {}", quote_ident(&o.name)),
        (_, ObjectType::DatabaseLink) => format!("drop {kind} {}", quote_ident(&o.name)),
        // Users with the profile get DEFAULT.
        (_, ObjectType::Profile) => format!("drop {kind} {} cascade", o.qualified()),
        _ => format!("drop {kind} {}", o.qualified()),
    }
}

/// Entries of an object's context menu besides those for its type.
pub struct MenuOptions {
    /// The object is a favorite: the entry removes it.
    pub favorite: bool,
    /// Offer Drop… (left out in editors, where it's too easy to hit).
    pub drop: bool,
    /// Offer "Show in object browser".
    pub reveal: bool,
    /// A subprogram of the package, e.g. `PROC` in `pkg.proc`.
    pub member: Option<String>,
    /// Offer the New submenu (new objects of the type, or on this one).
    pub create: bool,
}

/// The context menu of a database object, shared by the object browser and
/// the editors. Returns the chosen action.
pub fn object_menu(ui: &mut Ui, object: &ObjectRef, options: &MenuOptions) -> Option<ObjectAction> {
    let t = object.object_type;
    let mut chosen = None;
    let mut item = |ui: &mut Ui, label: &str, action: ObjectAction| {
        if ui.button(label).clicked() {
            chosen = Some(action);
            ui.close();
        }
    };
    if t.has_data() {
        item(ui, "Query data", ObjectAction::Query(object.clone()));
    }
    if t.has_source() {
        match &options.member {
            Some(member) => item(
                ui,
                &format!("Edit {}", member.to_lowercase()),
                ObjectAction::EditAt(object.clone(), member.clone()),
            ),
            None => item(ui, "Edit", ObjectAction::Edit(object.clone())),
        }
    }
    let testable = matches!(t, ObjectType::Procedure | ObjectType::Function)
        || (t == ObjectType::Package && options.member.is_some());
    if testable {
        item(
            ui,
            "Test…",
            ObjectAction::Test(object.clone(), options.member.clone()),
        );
    }
    item(ui, "Describe", ObjectAction::Describe(object.clone()));
    if t == ObjectType::Table {
        item(
            ui,
            "Edit table…",
            ObjectAction::EditTable(object.clone(), TableTab::Columns),
        );
    }
    if has_editor(t) {
        item(ui, "Edit…", ObjectAction::EditObject(object.clone()));
    }
    if has_grants(t) {
        item(ui, "Grants…", ObjectAction::Grants(object.clone()));
    }
    if options.create {
        let news = new_entries(object);
        if !news.is_empty() {
            ui.menu_button("New", |ui| {
                for (label, action) in news {
                    item(ui, &label, action);
                }
            });
        }
    }
    if t.has_data() {
        ui.menu_button("Generate", |ui| {
            for kind in [
                Template::Select,
                Template::Insert,
                Template::Update,
                Template::ColumnList,
            ] {
                item(
                    ui,
                    kind.label(),
                    ObjectAction::Generate(object.clone(), kind),
                );
            }
        });
    }
    ui.separator();
    if t.has_source() || matches!(t, ObjectType::View | ObjectType::MaterializedView) {
        item(ui, "Compile", ObjectAction::Compile(object.clone()));
    }
    if t == ObjectType::DatabaseLink {
        item(ui, "Test link", ObjectAction::TestLink(object.clone()));
    }
    let favorite = if options.favorite {
        "Remove from favorites"
    } else {
        "Add to favorites"
    };
    item(ui, favorite, ObjectAction::ToggleFavorite(object.clone()));
    if options.reveal {
        item(
            ui,
            "Show in object browser",
            ObjectAction::Reveal(object.clone()),
        );
    }
    item(ui, "Copy name", ObjectAction::CopyName(object.clone()));
    item(
        ui,
        "Copy qualified name",
        ObjectAction::CopyQualified(object.clone()),
    );
    if options.drop {
        ui.separator();
        item(ui, "Drop…", ObjectAction::Drop(object.clone()));
    }
    chosen
}

/// What can be done with a database object, from the browser or an editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectAction {
    Select(ObjectRef),
    Default(ObjectRef),
    Query(ObjectRef),
    Edit(ObjectRef),
    Describe(ObjectRef),
    Compile(ObjectRef),
    CopyName(ObjectRef),
    CopyQualified(ObjectRef),
    Drop(ObjectRef),
    Test(ObjectRef, Option<String>),
    /// Open the package at this subprogram.
    EditAt(ObjectRef, String),
    /// Show the object in the browser's tree.
    Reveal(ObjectRef),
    Generate(ObjectRef, Template),
    EditTable(ObjectRef, TableTab),
    /// Form editor (or table builder) for a new object of this type in this
    /// schema.
    New(String, ObjectType),
    /// A new object of this type on (or like) an object: an index on a
    /// table, a synonym for it, a table like it.
    NewFrom(ObjectType, ObjectRef),
    /// Browse a user's schema.
    ShowSchema(String),
    EditObject(ObjectRef),
    Grants(ObjectRef),
    ToggleFavorite(ObjectRef),
    /// Query over a database link and report whether it works.
    TestLink(ObjectRef),
}

/// Statements generated from a table's columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Template {
    Select,
    Insert,
    Update,
    ColumnList,
}

impl Template {
    fn label(self) -> &'static str {
        match self {
            Template::Select => "SELECT statement",
            Template::Insert => "INSERT statement",
            Template::Update => "UPDATE statement",
            Template::ColumnList => "Column list",
        }
    }
}

/// Builds the statement for `kind` with binds named after the columns.
fn template(kind: Template, object: &ObjectRef, cols: &[TableColumn]) -> String {
    let name = |c: &TableColumn| crate::documents::quote_ident(&c.name).to_lowercase();
    let table = object.qualified().to_lowercase();
    let names: Vec<String> = cols.iter().map(name).collect();
    match kind {
        Template::ColumnList => names.join(", "),
        Template::Select => {
            let alias = object
                .name
                .chars()
                .next()
                .unwrap_or('t')
                .to_ascii_lowercase();
            let list: Vec<String> = names.iter().map(|n| format!("{alias}.{n}")).collect();
            format!("select {}\n  from {table} {alias}", list.join(",\n       "))
        }
        Template::Insert => {
            let binds: Vec<String> = cols
                .iter()
                .map(|c| format!(":{}", c.name.to_lowercase()))
                .collect();
            format!(
                "insert into {table}\n  ({})\nvalues\n  ({})",
                names.join(", "),
                binds.join(", ")
            )
        }
        Template::Update => {
            let mut keys: Vec<&TableColumn> =
                cols.iter().filter(|c| c.pk_position.is_some()).collect();
            keys.sort_by_key(|c| c.pk_position);
            let sets: Vec<String> = cols
                .iter()
                .filter(|c| c.pk_position.is_none())
                .map(|c| format!("{} = :{}", name(c), c.name.to_lowercase()))
                .collect();
            let wheres: Vec<String> = if keys.is_empty() {
                vec!["1 = 0 -- TODO: no primary key".into()]
            } else {
                keys.iter()
                    .map(|c| format!("{} = :{}", name(c), c.name.to_lowercase()))
                    .collect()
            };
            format!(
                "update {table}\n   set {}\n where {}",
                sets.join(",\n       "),
                wheres.join("\n   and ")
            )
        }
    }
}

fn object_ref(o: &DbObject) -> ObjectRef {
    ObjectRef {
        owner: o.owner.clone(),
        name: o.name.clone(),
        object_type: o.object_type,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_public_objects_and_links() {
        let o = |owner: &str, name: &str, object_type| ObjectRef {
            owner: owner.into(),
            name: name.into(),
            object_type,
        };
        assert_eq!(
            drop_sql(&o("PUBLIC", "DUAL2", ObjectType::Synonym)),
            "drop public synonym DUAL2"
        );
        assert_eq!(
            drop_sql(&o("PUBLIC", "REMOTE.EXAMPLE.COM", ObjectType::DatabaseLink)),
            "drop public database link \"REMOTE.EXAMPLE.COM\""
        );
        assert_eq!(
            drop_sql(&o("HR", "REMOTE", ObjectType::DatabaseLink)),
            "drop database link REMOTE"
        );
        assert_eq!(
            drop_sql(&o("HR", "EMP", ObjectType::Table)),
            "drop table HR.EMP"
        );
        assert_eq!(folders("PUBLIC"), ObjectType::PUBLIC);
    }

    fn col(name: &str, pk: Option<u32>) -> TableColumn {
        TableColumn {
            name: name.into(),
            data_type: "NUMBER".into(),
            nullable: pk.is_none(),
            default: None,
            comment: None,
            pk_position: pk,
        }
    }

    #[test]
    fn templates() {
        let object = ObjectRef {
            owner: "HR".into(),
            name: "EMP".into(),
            object_type: ObjectType::Table,
        };
        let cols = [col("ID", Some(1)), col("NAME", None), col("SAL", None)];
        assert_eq!(
            template(Template::ColumnList, &object, &cols),
            "id, name, sal"
        );
        assert_eq!(
            template(Template::Select, &object, &cols),
            "select e.id,\n       e.name,\n       e.sal\n  from hr.emp e"
        );
        assert_eq!(
            template(Template::Insert, &object, &cols),
            "insert into hr.emp\n  (id, name, sal)\nvalues\n  (:id, :name, :sal)"
        );
        assert_eq!(
            template(Template::Update, &object, &cols),
            "update hr.emp\n   set name = :name,\n       sal = :sal\n where id = :id"
        );
    }

    #[test]
    fn new_submenu() {
        let o = |owner: &str, name: &str, object_type| ObjectRef {
            owner: owner.into(),
            name: name.into(),
            object_type,
        };
        let labels = |t: ObjectType| -> Vec<String> {
            new_entries(&o("HR", "EMP", t))
                .into_iter()
                .map(|(label, _)| label)
                .collect()
        };
        assert_eq!(
            labels(ObjectType::Table),
            [
                "Table…",
                "Table like this…",
                "Index on this table…",
                "Trigger on this table…",
                "View on this table…",
                "Synonym for this…"
            ]
        );
        assert_eq!(
            labels(ObjectType::View),
            [
                "View…",
                "Trigger on this view…",
                "View on this view…",
                "Synonym for this…"
            ]
        );
        assert_eq!(
            labels(ObjectType::Procedure),
            ["Procedure…", "Synonym for this…"]
        );
        assert_eq!(labels(ObjectType::JavaSource), Vec::<String>::new());
        let (_, action) = new_entries(&o("HR", "EMP", ObjectType::Table)).remove(2);
        assert_eq!(
            action,
            ObjectAction::NewFrom(ObjectType::Index, o("HR", "EMP", ObjectType::Table))
        );
    }
}

/// Starts dragging `text` (for editors to insert) when `response` is
/// dragged, with the text following the pointer.
fn drag_text(ui: &Ui, response: &egui::Response, text: String) {
    if response.drag_started() {
        egui::DragAndDrop::set_payload(ui.ctx(), crate::editor::DroppedText(text.clone()));
    }
    if response.dragged()
        && let Some(pos) = ui.ctx().pointer_interact_pos()
    {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        egui::Area::new(response.id.with("drag"))
            .order(egui::Order::Tooltip)
            .fixed_pos(pos + egui::vec2(12.0, 4.0))
            .interactable(false)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(RichText::new(text).monospace());
                });
            });
    }
}

#[cfg(test)]
mod marks_tests {
    use super::*;

    fn object(name: &str) -> ObjectRef {
        ObjectRef {
            owner: "HR".into(),
            name: name.into(),
            object_type: ObjectType::Table,
        }
    }

    #[test]
    fn recent_and_favorites() {
        let mut marks = Marks::default();
        for i in 0..20 {
            marks.touch("db", object(&format!("T{i}")));
        }
        marks.touch("db", object("T5"));
        let recent = &marks.recent["db"];
        assert_eq!(recent.len(), Marks::RECENT);
        assert_eq!(recent[0].name, "T5");
        assert_eq!(recent.iter().filter(|o| o.name == "T5").count(), 1);

        marks.toggle_favorite("db", object("B"));
        marks.toggle_favorite("db", object("A"));
        assert!(marks.is_favorite("db", &object("A")));
        assert!(!marks.is_favorite("other", &object("A")));
        assert_eq!(marks.favorites["db"][0].name, "A");
        marks.toggle_favorite("db", object("A"));
        assert!(!marks.is_favorite("db", &object("A")));
    }
}
