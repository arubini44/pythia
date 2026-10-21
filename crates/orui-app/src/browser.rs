//! Object browser: connection → schema → object type → objects → children.
//!
//! The tree is flattened into visible rows every frame and drawn with a
//! virtualized scroll area, so folders with tens of thousands of objects stay
//! smooth. Folders load lazily when expanded.

use std::collections::{HashMap, HashSet};

use egui::{Color32, RichText, Sense, Ui};
use orui_db::{
    DbEvent, DbObject, DbRequest, Member, MetaQuery, MetaResult, ObjectType, TableColumn,
};

use orui_ui::icons::{Icon, IconSize, paint_invalid_badge};

use crate::documents::{Effects, ObjectRef, OpenRequest, Target, request_id};

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
}

enum Pending {
    Schemas,
    Folder(String, ObjectType),
    Children(ObjectRef),
    Action {
        object: ObjectRef,
        verb: &'static str,
    },
    /// Columns for a generated statement.
    Generate {
        object: ObjectRef,
        kind: Template,
    },
    RecompileSchema(String),
    /// Parameters of `member` (or the standalone unit) for a test script.
    Test {
        object: ObjectRef,
        member: Option<String>,
    },
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
    Column(&'a TableColumn),
    Member(&'a Member, ObjectRef),
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
    folders: HashMap<(String, ObjectType), Loaded<Vec<DbObject>>>,
    children: HashMap<ObjectRef, Loaded<Children>>,
    expanded: HashSet<Node>,
    pending: HashMap<u64, Pending>,
    selected: Option<ObjectRef>,
    confirm_drop: Option<ObjectRef>,
}

impl Browser {
    /// Shows a (newly connected) connection, starting in its current schema.
    pub fn attach(&mut self, connection: String, schema: String) -> Effects {
        *self = Self {
            connection: Some(connection),
            schema: Some(schema),
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

    fn load_children(&mut self, effects: &mut Effects, object: &ObjectRef) {
        let query = match object.object_type {
            t if t.has_data() => MetaQuery::Columns {
                owner: object.owner.clone(),
                table: object.name.clone(),
            },
            ObjectType::Package | ObjectType::Type => MetaQuery::Members {
                owner: object.owner.clone(),
                package: object.name.clone(),
            },
            _ => return,
        };
        self.children.insert(object.clone(), Loaded::Loading);
        self.meta(effects, Pending::Children(object.clone()), query);
    }

    /// Reloads expanded folders (after compile/drop or on demand).
    fn refresh(&mut self, effects: &mut Effects) {
        self.children.clear();
        let open: Vec<(String, ObjectType)> = self
            .expanded
            .iter()
            .filter_map(|n| match n {
                Node::Folder(owner, t) => Some((owner.clone(), *t)),
                Node::Object(_) => None,
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
                Node::Folder(..) => None,
            })
            .collect();
        for o in objects {
            self.load_children(effects, &o);
        }
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
            (Pending::Schemas, Ok(Some(MetaResult::Schemas(s)))) => {
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
            }
            (Pending::RecompileSchema(owner), Ok(_)) => {
                log.push(format!("Recompiled invalid objects in {owner}"));
                self.refresh(&mut effects);
            }
            (Pending::RecompileSchema(owner), Err(e)) => {
                log.push(format!("Recompiling {owner} failed: {e}"));
            }
            (Pending::Action { object, verb }, Err(e)) => {
                log.push(format!("{} {} failed: {e}", verb, object.qualified()));
                self.refresh(&mut effects);
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
                if orui_ui::icons::icon_text_button(ui, Icon::Connect, "Connect…").clicked() {
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
            if let Some(s) = chosen
                && Some(&s) != self.schema.as_ref()
            {
                self.schema = Some(s);
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
                self.refresh(&mut effects);
            }
            if orui_ui::icons::icon_button(ui, Icon::Refresh, "Refresh").clicked() {
                self.refresh(&mut effects);
            }
            if orui_ui::icons::icon_button(
                ui,
                Icon::Compile,
                "Recompile invalid objects in this schema",
            )
            .clicked()
                && let Some(owner) = self.schema.clone()
            {
                let id = request_id();
                let sql = format!(
                    "begin dbms_utility.compile_schema(schema => '{}', compile_all => false); end;",
                    owner.replace('\'', "''")
                );
                self.pending.insert(id, Pending::RecompileSchema(owner));
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
        });
        ui.add(
            egui::TextEdit::singleline(&mut self.filter)
                .hint_text("Filter objects…")
                .desired_width(f32::INFINITY),
        );
        ui.add_space(2.0);

        let owner = self.schema.clone().unwrap_or_default();
        let rows = self.rows(&owner);
        let row_height = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        let mut toggles = Vec::new();
        let mut actions = Vec::new();
        egui::ScrollArea::both().auto_shrink(false).show_rows(
            ui,
            row_height,
            rows.len(),
            |ui, range| {
                for row in &rows[range] {
                    self.row_ui(ui, row, row_height, &mut toggles, &mut actions);
                }
            },
        );
        drop(rows);

        for node in toggles {
            if !self.expanded.remove(&node) {
                match &node {
                    Node::Folder(owner, t) if !self.folders.contains_key(&(owner.clone(), *t)) => {
                        self.load_folder(&mut effects, &owner.clone(), *t);
                    }
                    Node::Object(o) if !self.children.contains_key(o) => {
                        self.load_children(&mut effects, &o.clone())
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
        for &object_type in ObjectType::BROWSER {
            let node = Node::Folder(owner.to_owned(), object_type);
            let open = self.expanded.contains(&node);
            let folder = self.folders.get(&(owner.to_owned(), object_type));
            let matching: Vec<&DbObject> = match folder {
                Some(Loaded::Ready(objects)) => objects
                    .iter()
                    .filter(|o| filter.is_empty() || o.name.contains(&filter))
                    .collect(),
                _ => Vec::new(),
            };
            let count = matches!(folder, Some(Loaded::Ready(_))).then_some(matching.len());
            if !filter.is_empty() && count == Some(0) {
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
                        rows.extend(cols.iter().map(Row::Column))
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
        rows
    }

    fn row_ui(
        &self,
        ui: &mut Ui,
        row: &Row,
        height: f32,
        toggles: &mut Vec<Node>,
        actions: &mut Vec<Action>,
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
                    let response =
                        ui.add(egui::Button::selectable(selected, text).frame_when_inactive(false));
                    let response = response.on_hover_ui(|ui| {
                        ui.label(RichText::new(r.qualified()).strong());
                        ui.label(object.object_type.dictionary_name().to_lowercase());
                        if !object.valid {
                            ui.colored_label(ui.visuals().error_fg_color, "Invalid");
                        }
                        ui.weak(format!("Last DDL: {}", object.last_ddl));
                    });
                    if response.clicked() {
                        actions.push(Action::Select(r.clone()));
                    }
                    if response.double_clicked() {
                        actions.push(Action::Default(r.clone()));
                    }
                    response.context_menu(|ui| self.context_menu(ui, &r, actions));
                }
                Row::Column(c) => {
                    ui.add_space(INDENT * 2.0 + small.points());
                    let icon = if c.pk_position.is_some() {
                        Icon::PrimaryKey
                    } else {
                        Icon::Column
                    };
                    ui.add(icon.sized(ui, small));
                    ui.label(RichText::new(c.name.to_lowercase()).monospace());
                    let null = if c.nullable { "" } else { " not null" };
                    let label = ui.weak(format!("{}{null}", c.data_type.to_lowercase()));
                    if let Some(comment) = &c.comment {
                        label.on_hover_text(comment);
                    }
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
                            actions.push(Action::Test(package.clone(), Some(m.name.clone())));
                            ui.close();
                        }
                        if ui.button("Edit package body").clicked() {
                            let body = ObjectRef {
                                object_type: ObjectType::PackageBody,
                                ..package.clone()
                            };
                            actions.push(Action::Edit(body));
                            ui.close();
                        }
                    });
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

    fn context_menu(&self, ui: &mut Ui, object: &ObjectRef, actions: &mut Vec<Action>) {
        let t = object.object_type;
        let mut item = |ui: &mut Ui, label: &str, action: Action| {
            if ui.button(label).clicked() {
                actions.push(action);
                ui.close();
            }
        };
        if t.has_data() {
            item(ui, "Query data", Action::Query(object.clone()));
        }
        if t.has_source() {
            item(ui, "Edit", Action::Edit(object.clone()));
        }
        if matches!(t, ObjectType::Procedure | ObjectType::Function) {
            item(ui, "Test…", Action::Test(object.clone(), None));
        }
        item(ui, "Describe", Action::Describe(object.clone()));
        if t == ObjectType::Table {
            item(ui, "Edit table…", Action::EditTable(object.clone()));
        }
        let mut generate = None;
        if t.has_data() {
            ui.menu_button("Generate", |ui| {
                for kind in [
                    Template::Select,
                    Template::Insert,
                    Template::Update,
                    Template::ColumnList,
                ] {
                    if ui.button(kind.label()).clicked() {
                        generate = Some(Action::Generate(object.clone(), kind));
                        ui.close();
                    }
                }
            });
        }
        ui.separator();
        if t.has_source() || matches!(t, ObjectType::View | ObjectType::MaterializedView) {
            item(ui, "Compile", Action::Compile(object.clone()));
        }
        item(ui, "Copy name", Action::CopyName(object.clone()));
        item(
            ui,
            "Copy qualified name",
            Action::CopyQualified(object.clone()),
        );
        ui.separator();
        item(ui, "Drop…", Action::Drop(object.clone()));
        actions.extend(generate);
    }

    fn action(
        &mut self,
        action: Action,
        connection: &str,
        ctx: &egui::Context,
        effects: &mut Effects,
    ) {
        let connection = connection.to_owned();
        match action {
            Action::Select(o) => self.selected = Some(o),
            Action::Default(o) if o.object_type.has_source() => {
                effects.open.push(OpenRequest::Program {
                    connection,
                    object: o,
                });
            }
            Action::Default(o) | Action::Describe(o) => effects.open.push(OpenRequest::Object {
                connection,
                object: o,
            }),
            Action::Query(o) => effects.open.push(OpenRequest::Sql {
                connection,
                text: format!("select * from {}", o.qualified()),
                run: true,
            }),
            Action::Edit(o) => effects.open.push(OpenRequest::Program {
                connection,
                object: o,
            }),
            Action::Compile(o) => {
                let sql = match o.object_type {
                    ObjectType::PackageBody => {
                        format!("alter package {} compile body", o.qualified())
                    }
                    ObjectType::TypeBody => format!("alter type {} compile body", o.qualified()),
                    ObjectType::JavaSource => {
                        format!("alter java source {} compile", o.qualified())
                    }
                    t => format!(
                        "alter {} {} compile",
                        t.dictionary_name().to_lowercase(),
                        o.qualified()
                    ),
                };
                self.run(effects, &o, "compiled", sql);
            }
            Action::CopyName(o) => ctx.copy_text(o.name.clone()),
            Action::CopyQualified(o) => ctx.copy_text(o.qualified()),
            Action::Drop(o) => self.confirm_drop = Some(o),
            Action::EditTable(object) => effects
                .open
                .push(OpenRequest::EditTable { connection, object }),
            Action::Generate(object, kind) => {
                let id = request_id();
                let query = MetaQuery::Columns {
                    owner: object.owner.clone(),
                    table: object.name.clone(),
                };
                self.pending.insert(id, Pending::Generate { object, kind });
                effects.request(Target::Main, DbRequest::Meta { id, query });
            }
            Action::Test(object, member) => {
                let id = request_id();
                let request = orui_lsp_core::CatalogRequest::Members {
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
                let sql = format!(
                    "drop {} {}",
                    object.object_type.dictionary_name().to_lowercase(),
                    object.qualified()
                );
                self.run(effects, &object, "dropped", sql);
            }
        }
    }
}

enum Action {
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
    Generate(ObjectRef, Template),
    EditTable(ObjectRef),
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
}
