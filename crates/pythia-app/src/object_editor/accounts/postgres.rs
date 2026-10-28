//! PostgreSQL's roles: users are the roles that can log in. A role's
//! options (`SUPERUSER`, `CREATEDB`, …), password, its end and the
//! connection limit; the roles it's a member of, with the admin option;
//! privileges on databases, schemas and tables (and sequences), each with
//! its grant option. They come from `pg_roles`, `pg_auth_members` and the
//! objects' ACLs, which every role may read; an owner's own privileges
//! aren't listed.

use std::collections::BTreeMap;

use egui::Ui;
use pythia_db::{Client, DbError, DbKind, Dialect};

use super::{
    Choices, Grant, Loaded, RoleGrant, Scope, ScopeKind, UserDef, by_scope, list, role_changes,
    text, texts, yes,
};
use crate::object_editor::{form_row, is_integer};

/// The role options: keyword, label, what it allows. `INHERIT` is on
/// unless switched off.
const OPTIONS: &[(&str, &str, &str)] = &[
    ("SUPERUSER", "superuser", "Bypasses all permission checks"),
    ("CREATEDB", "create databases", "May create databases"),
    (
        "CREATEROLE",
        "create roles",
        "May create, alter and drop roles",
    ),
    (
        "INHERIT",
        "inherit",
        "Has the privileges of the roles it's a member of",
    ),
    (
        "REPLICATION",
        "replication",
        "May connect for streaming replication",
    ),
    (
        "BYPASSRLS",
        "bypass RLS",
        "Row-level security policies don't apply",
    ),
];

/// A string literal, also with `\` in it.
pub fn literal(value: &str) -> String {
    let quoted = value.replace('\'', "''");
    if value.contains('\\') {
        format!("E'{}'", quoted.replace('\\', "\\\\"))
    } else {
        format!("'{quoted}'")
    }
}

fn q(name: &str) -> String {
    Dialect::POSTGRES.quote_ident(name)
}

fn names(roles: &[&String]) -> String {
    roles.iter().map(|r| q(r)).collect::<Vec<_>>().join(", ")
}

/// What a grant is on; sequences' `USAGE` needs `sequence`.
fn scope_sql(scope: &Scope, privilege: &str) -> String {
    match scope {
        Scope::Global => String::new(),
        Scope::Database(d) => format!("database {}", q(d)),
        Scope::Schema(s) => format!("schema {}", q(s)),
        Scope::Object(s, o) if privilege == "USAGE" => format!("sequence {}.{}", q(s), q(o)),
        Scope::Object(s, o) => format!("table {}.{}", q(s), q(o)),
    }
}

pub(super) fn load(
    client: &mut dyn Client,
    name: Option<&str>,
    role: bool,
) -> Result<Loaded, DbError> {
    let version: u32 = texts(client, "show server_version_num", &[])?
        .first()
        .and_then(|v| v.parse().ok())
        .unwrap_or_default();
    let mut table: Vec<String> = [
        "SELECT",
        "INSERT",
        "UPDATE",
        "DELETE",
        "TRUNCATE",
        "REFERENCES",
        "TRIGGER",
        "USAGE",
    ]
    .iter()
    .map(|p| (*p).to_owned())
    .collect();
    if version >= 170_000 {
        table.push("MAINTAIN".to_owned());
    }
    let to_vec = |l: &[&str]| l.iter().map(|p| (*p).to_owned()).collect::<Vec<_>>();
    let choices = Choices {
        roles: texts(
            client,
            "select rolname from pg_roles where rolname <> $1 order by rolname",
            &[name.unwrap_or_default()],
        )?,
        privileges: BTreeMap::from([
            (
                ScopeKind::Database,
                to_vec(&["CONNECT", "CREATE", "TEMPORARY"]),
            ),
            (ScopeKind::Schema, to_vec(&["USAGE", "CREATE"])),
            (ScopeKind::Object, table),
        ]),
        databases: texts(
            client,
            "select datname from pg_database where not datistemplate order by datname",
            &[],
        )?,
        schemas: texts(
            client,
            "select nspname from pg_namespace \
              where nspname !~ '^pg_' and nspname <> 'information_schema' order by nspname",
            &[],
        )?,
        ..Choices::default()
    };
    let original = match name {
        Some(name) => Some(load_role(client, name, role)?),
        None => None,
    };
    let start = original
        .clone()
        .unwrap_or_else(|| UserDef::new(DbKind::Postgres, role, false));
    Ok((original, start, choices))
}

fn load_role(client: &mut dyn Client, name: &str, role: bool) -> Result<UserDef, DbError> {
    let mut def = UserDef {
        name: name.to_owned(),
        ..UserDef::new(DbKind::Postgres, role, false)
    };
    def.options.clear();
    let row = client
        .query(
            "select rolsuper, rolcreatedb, rolcreaterole, rolinherit, rolreplication, \
                    rolbypassrls, rolconnlimit, rolvaliduntil \
               from pg_roles where rolname = $1",
            &[name],
        )?
        .into_iter()
        .next()
        .ok_or_else(|| DbError::new(format!("The role {} doesn't exist", q(name))))?;
    for (i, (option, ..)) in OPTIONS.iter().enumerate() {
        if yes(&row, i) {
            def.options.insert((*option).to_owned());
        }
    }
    def.connection_limit = match text(&row, 6).as_str() {
        "-1" => String::new(),
        n => n.to_owned(),
    };
    def.valid_until = match text(&row, 7).as_str() {
        "infinity" => String::new(),
        v => v.to_owned(),
    };

    for row in client.query(
        "select r.rolname, bool_or(m.admin_option) from pg_auth_members m \
           join pg_roles r on r.oid = m.roleid \
          where m.member = (select oid from pg_roles where rolname = $1) \
          group by r.rolname",
        &[name],
    )? {
        def.roles.insert(
            text(&row, 0),
            RoleGrant {
                admin: yes(&row, 1),
                default: true,
            },
        );
    }

    let grantee = "(select oid from pg_roles where rolname = $1)";
    let grant = |grantable: bool| {
        if grantable {
            Grant::WithGrantOption
        } else {
            Grant::Plain
        }
    };
    for row in client.query(
        &format!(
            "select d.datname, a.privilege_type, bool_or(a.is_grantable) \
               from pg_database d, aclexplode(d.datacl) a \
              where a.grantee = {grantee} and a.grantee <> d.datdba group by 1, 2"
        ),
        &[name],
    )? {
        def.privileges.insert(
            (Scope::Database(text(&row, 0)), text(&row, 1)),
            grant(yes(&row, 2)),
        );
    }
    for row in client.query(
        &format!(
            "select n.nspname, a.privilege_type, bool_or(a.is_grantable) \
               from pg_namespace n, aclexplode(n.nspacl) a \
              where a.grantee = {grantee} and a.grantee <> n.nspowner group by 1, 2"
        ),
        &[name],
    )? {
        def.privileges.insert(
            (Scope::Schema(text(&row, 0)), text(&row, 1)),
            grant(yes(&row, 2)),
        );
    }
    for row in client.query(
        &format!(
            "select n.nspname, c.relname, a.privilege_type, bool_or(a.is_grantable) \
               from pg_class c join pg_namespace n on n.oid = c.relnamespace, \
                    aclexplode(c.relacl) a \
              where a.grantee = {grantee} and a.grantee <> c.relowner \
                and c.relkind in ('r', 'p', 'v', 'm', 'f', 'S') group by 1, 2, 3"
        ),
        &[name],
    )? {
        def.privileges.insert(
            (Scope::Object(text(&row, 0), text(&row, 1)), text(&row, 2)),
            grant(yes(&row, 3)),
        );
    }
    Ok(def)
}

pub(super) fn statements(original: Option<&UserDef>, def: &UserDef) -> Result<Vec<String>, String> {
    let name = q(def.name.trim());
    let limit = def.connection_limit.trim();
    if !limit.is_empty() && !is_integer(limit) {
        return Err("The connection limit is a number".to_owned());
    }
    let mut out = Vec::new();
    let mut clauses = Vec::new();
    let empty = UserDef::new(DbKind::Postgres, def.role, false);
    let before = original.unwrap_or(&empty);
    for (option, ..) in OPTIONS {
        match (
            before.options.contains(*option),
            def.options.contains(*option),
        ) {
            (false, true) => clauses.push(option.to_lowercase()),
            (true, false) => clauses.push(format!("no{}", option.to_lowercase())),
            _ => {}
        }
    }
    if !def.password.is_empty() {
        clauses.push(format!("password {}", literal(&def.password)));
    }
    if limit != before.connection_limit.trim() {
        clauses.push(format!(
            "connection limit {}",
            if limit.is_empty() { "-1" } else { limit }
        ));
    }
    let until = def.valid_until.trim();
    if until != before.valid_until.trim() {
        clauses.push(format!(
            "valid until {}",
            literal(if until.is_empty() { "infinity" } else { until })
        ));
    }
    let with = if clauses.is_empty() {
        String::new()
    } else {
        format!(" with {}", clauses.join(" "))
    };
    match original {
        None if def.role => out.push(format!("create role {name}{with}")),
        None => out.push(format!("create user {name}{with}")),
        Some(_) if !with.is_empty() => out.push(format!("alter role {name}{with}")),
        Some(_) => {}
    }

    // Only the admin option taken away: no need to revoke the membership.
    let mut kept = def.roles.clone();
    for (role, was) in &before.roles {
        if let Some(now) = kept.get_mut(role)
            && was.admin
            && !now.admin
        {
            out.push(format!("revoke admin option for {} from {name}", q(role)));
            now.admin = true;
        }
    }
    role_changes(
        &before.roles,
        &kept,
        |roles| format!("revoke {} from {name}", names(roles)),
        |roles, admin| {
            let option = if admin { " with admin option" } else { "" };
            format!("grant {} to {name}{option}", names(roles))
        },
        &mut out,
    );

    for (scope, (before, after)) in by_scope(&before.privileges, &def.privileges) {
        // Sequences' USAGE is granted on `sequence`, the rest on `table`.
        let mut groups: BTreeMap<(String, &str), Vec<&String>> = BTreeMap::new();
        for (privilege, was) in &before {
            let on = scope_sql(&scope, privilege);
            match after.get(privilege) {
                None => groups.entry((on, "revoke")).or_default().push(privilege),
                Some(Grant::Plain) if *was == Grant::WithGrantOption => {
                    groups.entry((on, "option")).or_default().push(privilege)
                }
                _ => {}
            }
        }
        for (privilege, now) in &after {
            let on = scope_sql(&scope, privilege);
            match (before.get(privilege), now) {
                (None, Grant::Plain) => groups.entry((on, "grant")).or_default().push(privilege),
                (None | Some(Grant::Plain), Grant::WithGrantOption) => {
                    groups.entry((on, "grantable")).or_default().push(privilege)
                }
                _ => {}
            }
        }
        for action in ["revoke", "option", "grant", "grantable"] {
            for ((on, a), privileges) in &groups {
                if *a != action {
                    continue;
                }
                let privileges = list(privileges);
                out.push(match action {
                    "revoke" => format!("revoke {privileges} on {on} from {name}"),
                    "option" => format!("revoke grant option for {privileges} on {on} from {name}"),
                    "grant" => format!("grant {privileges} on {on} to {name}"),
                    _ => format!("grant {privileges} on {on} to {name} with grant option"),
                });
            }
        }
    }
    Ok(out)
}

pub(super) fn ui(ui: &mut Ui, def: &mut UserDef, original: Option<&UserDef>) {
    let existing = original.is_some();
    if !def.role {
        form_row(ui, "Password", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut def.password)
                    .password(true)
                    .hint_text(if existing { "unchanged" } else { "none" })
                    .desired_width(200.0),
            );
        });
        form_row(ui, "Valid until", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut def.valid_until)
                    .hint_text("always")
                    .desired_width(200.0),
            )
            .on_hover_text("When the password stops working, e.g. 2027-01-31");
        });
        form_row(ui, "Connection limit", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut def.connection_limit)
                    .hint_text("none")
                    .desired_width(80.0),
            );
        });
    }
    form_row(ui, "Options", |ui| {
        ui.horizontal_wrapped(|ui| {
            for (option, label, hover) in OPTIONS {
                let mut on = def.options.contains(*option);
                if ui.checkbox(&mut on, *label).on_hover_text(*hover).changed() {
                    if on {
                        def.options.insert((*option).to_owned());
                    } else {
                        def.options.remove(*option);
                    }
                }
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_roles() {
        let mut def = UserDef {
            name: "App".into(),
            password: "it's".into(),
            connection_limit: "5".into(),
            ..UserDef::new(DbKind::Postgres, false, false)
        };
        def.options.insert("CREATEDB".into());
        def.options.remove("INHERIT");
        def.roles.insert("readers".into(), RoleGrant::default());
        def.privileges.insert(
            (Scope::Database("shop".into()), "CONNECT".into()),
            Grant::Plain,
        );
        def.privileges.insert(
            (
                Scope::Object("public".into(), "orders".into()),
                "SELECT".into(),
            ),
            Grant::WithGrantOption,
        );
        def.privileges.insert(
            (
                Scope::Object("public".into(), "orders_id".into()),
                "USAGE".into(),
            ),
            Grant::Plain,
        );
        assert_eq!(
            statements(None, &def).unwrap(),
            [
                "create user \"App\" with createdb noinherit password 'it''s' connection limit 5",
                "grant readers to \"App\"",
                "grant connect on database shop to \"App\"",
                "grant select on table public.orders to \"App\" with grant option",
                "grant usage on sequence public.orders_id to \"App\"",
            ]
        );
        let role = UserDef {
            name: "readers".into(),
            ..UserDef::new(DbKind::Postgres, true, false)
        };
        assert_eq!(statements(None, &role).unwrap(), ["create role readers"]);
    }

    #[test]
    fn alters_roles() {
        let mut loaded = UserDef {
            name: "app".into(),
            valid_until: "2030-01-01 00:00:00+00".into(),
            ..UserDef::new(DbKind::Postgres, false, false)
        };
        loaded.roles.insert(
            "admins".into(),
            RoleGrant {
                admin: true,
                default: true,
            },
        );
        let schema = (Scope::Schema("public".into()), "USAGE".to_owned());
        loaded
            .privileges
            .insert(schema.clone(), Grant::WithGrantOption);
        assert_eq!(statements(Some(&loaded), &loaded), Ok(Vec::new()));

        let mut edited = loaded.clone();
        edited.valid_until.clear();
        edited.options.insert("SUPERUSER".into());
        edited.roles.get_mut("admins").unwrap().admin = false;
        edited.privileges.insert(schema, Grant::Plain);
        assert_eq!(
            statements(Some(&loaded), &edited).unwrap(),
            [
                "alter role app with superuser valid until 'infinity'",
                "revoke admin option for admins from app",
                "revoke grant option for usage on schema public from app",
            ]
        );
        edited.connection_limit = "many".into();
        assert!(statements(Some(&loaded), &edited).is_err());
    }
}
