//! SQL Server's database users and roles: a user is for a server login
//! (new or existing), without one, or (in a contained database) has a
//! password of its own; its default schema; a login's password and whether
//! it's disabled. Roles are joined with `ALTER ROLE … ADD MEMBER`;
//! permissions on the database, a schema or an object are granted (with
//! grant option) or denied. Without `VIEW DEFINITION` the catalog views
//! show only some principals and permissions.

use std::collections::BTreeMap;

use egui::Ui;
use pythia_db::{Client, DbError, DbKind, Dialect};

use super::{
    Choices, Grant, Loaded, Login, RoleGrant, Scope, ScopeKind, UserDef, by_scope, list, text,
    texts,
};
use crate::object_editor::{form_row, picker};

/// An `N'…'` literal.
pub fn literal(s: &str) -> String {
    format!("N'{}'", s.replace('\'', "''"))
}

fn q(name: &str) -> String {
    Dialect::SQLSERVER.quote_ident(name)
}

/// `on schema::s`, `on s.o`; database permissions have no `on`.
fn on(scope: &Scope) -> String {
    match scope {
        Scope::Global | Scope::Database(_) => String::new(),
        Scope::Schema(s) => format!(" on schema::{}", q(s)),
        Scope::Object(s, o) => format!(" on {}.{}", q(s), q(o)),
    }
}

pub(super) fn load(
    client: &mut dyn Client,
    name: Option<&str>,
    role: bool,
) -> Result<Loaded, DbError> {
    let permissions = |client: &mut dyn Client, class: &str| {
        texts(
            client,
            "select distinct permission_name from sys.fn_builtin_permissions(@P1) \
              order by permission_name",
            &[class],
        )
    };
    let choices = Choices {
        roles: texts(
            client,
            "select name from sys.database_principals \
              where type = 'R' and name <> 'public' and name <> @P1 order by name",
            &[name.unwrap_or_default()],
        )?,
        privileges: BTreeMap::from([
            (ScopeKind::Global, permissions(client, "DATABASE")?),
            (ScopeKind::Schema, permissions(client, "SCHEMA")?),
            (ScopeKind::Object, permissions(client, "OBJECT")?),
        ]),
        schemas: texts(client, "select name from sys.schemas order by name", &[])?,
        logins: texts(
            client,
            "select name from sys.server_principals \
              where type in ('S', 'U', 'G', 'E', 'X') and name not like '##%' order by name",
            &[],
        )?,
        contained: texts(
            client,
            "select cast(containment as varchar) from sys.databases where name = db_name()",
            &[],
        )?
        .first()
        .is_some_and(|c| c != "0"),
        ..Choices::default()
    };
    let original = match name {
        Some(name) => Some(load_principal(client, name, role)?),
        None => None,
    };
    let start = original
        .clone()
        .unwrap_or_else(|| UserDef::new(DbKind::SqlServer, role, false));
    Ok((original, start, choices))
}

fn load_principal(client: &mut dyn Client, name: &str, role: bool) -> Result<UserDef, DbError> {
    let mut def = UserDef {
        name: name.to_owned(),
        ..UserDef::new(DbKind::SqlServer, role, false)
    };
    let what = if role { "role" } else { "user" };
    let row = client
        .query(
            "select type_desc, authentication_type_desc, default_schema_name, suser_sname(sid) \
               from sys.database_principals where name = @P1",
            &[name],
        )?
        .into_iter()
        .next()
        .ok_or_else(|| {
            DbError::new(format!(
                "The {what} {} doesn't exist, or this session may not see it (VIEW DEFINITION)",
                q(name)
            ))
        })?;
    if !role {
        def.default_schema = text(&row, 2);
        def.login_name = text(&row, 3);
        def.login = match text(&row, 1).as_str() {
            "INSTANCE" => Login::Mapped,
            "NONE" => Login::None,
            "DATABASE" => Login::Password,
            _ => Login::Other,
        };
        let kind = text(&row, 0);
        if def.login == Login::Other || kind != "SQL_USER" {
            def.info
                .push(("Type".to_owned(), kind.to_lowercase().replace('_', " ")));
        }
        def.locked = None;
        if def.login == Login::Mapped {
            def.locked = texts(
                client,
                "select cast(is_disabled as varchar) from sys.server_principals where name = @P1",
                &[&def.login_name],
            )?
            .first()
            .map(|d| d == "1");
            def.unread.attributes = def.locked.is_none();
        }
    }
    for role in texts(
        client,
        "select r.name from sys.database_role_members m \
           join sys.database_principals r on r.principal_id = m.role_principal_id \
          where m.member_principal_id = database_principal_id(@P1)",
        &[name],
    )? {
        def.roles.insert(role, RoleGrant::default());
    }
    for row in client.query(
        "select cast(p.class as varchar), p.permission_name, p.state, s.name, os.name, o.name \
           from sys.database_permissions p \
           left join sys.schemas s on p.class = 3 and s.schema_id = p.major_id \
           left join sys.objects o on p.class = 1 and o.object_id = p.major_id \
           left join sys.schemas os on os.schema_id = o.schema_id \
          where p.grantee_principal_id = database_principal_id(@P1) \
            and p.class in (0, 1, 3) and p.minor_id = 0",
        &[name],
    )? {
        let scope = match text(&row, 0).as_str() {
            "0" => Scope::Global,
            "3" => Scope::Schema(text(&row, 3)),
            _ => Scope::Object(text(&row, 4), text(&row, 5)),
        };
        let grant = match text(&row, 2).as_str() {
            "D" => Grant::Deny,
            "W" => Grant::WithGrantOption,
            _ => Grant::Plain,
        };
        def.privileges.insert((scope, text(&row, 1)), grant);
    }
    Ok(def)
}

pub(super) fn statements(original: Option<&UserDef>, def: &UserDef) -> Result<Vec<String>, String> {
    let name = q(def.name.trim());
    let login = match def.login_name.trim() {
        "" if def.login == Login::New => def.name.trim(),
        login => login,
    };
    let schema = def.default_schema.trim();
    let mut out = Vec::new();
    match original {
        None if def.role => out.push(format!("create role {name}")),
        None => {
            let schema = if schema.is_empty() {
                String::new()
            } else {
                format!("default_schema = {}", q(schema))
            };
            let with = |items: &[&str]| {
                let items: Vec<&str> = items.iter().copied().filter(|i| !i.is_empty()).collect();
                if items.is_empty() {
                    String::new()
                } else {
                    format!(" with {}", items.join(", "))
                }
            };
            match def.login {
                Login::Mapped if login.is_empty() => return Err("Pick the login".to_owned()),
                Login::Mapped => out.push(format!(
                    "create user {name} for login {}{}",
                    q(login),
                    with(&[&schema])
                )),
                Login::New | Login::Password if def.password.is_empty() => {
                    return Err("Enter a password".to_owned());
                }
                Login::New => {
                    out.push(format!(
                        "create login {} with password = {}",
                        q(login),
                        literal(&def.password)
                    ));
                    out.push(format!(
                        "create user {name} for login {}{}",
                        q(login),
                        with(&[&schema])
                    ));
                    if def.locked == Some(true) {
                        out.push(format!("alter login {} disable", q(login)));
                    }
                }
                Login::None => out.push(format!(
                    "create user {name} without login{}",
                    with(&[&schema])
                )),
                Login::Password => {
                    let password = format!("password = {}", literal(&def.password));
                    out.push(format!("create user {name}{}", with(&[&password, &schema])));
                }
                Login::Other => return Err("Pick how the user logs in".to_owned()),
            }
        }
        Some(_) if def.role => {}
        Some(o) => {
            let mut with = Vec::new();
            if schema != o.default_schema.trim() {
                with.push(format!(
                    "default_schema = {}",
                    q(if schema.is_empty() { "dbo" } else { schema })
                ));
            }
            if def.login == Login::Mapped && login != o.login_name.trim() {
                if login.is_empty() {
                    return Err("Pick the login".to_owned());
                }
                with.push(format!("login = {}", q(login)));
            }
            if def.login == Login::Password && !def.password.is_empty() {
                with.push(format!("password = {}", literal(&def.password)));
            }
            if !with.is_empty() {
                out.push(format!("alter user {name} with {}", with.join(", ")));
            }
            if def.login == Login::Mapped {
                if !def.password.is_empty() {
                    out.push(format!(
                        "alter login {} with password = {}",
                        q(login),
                        literal(&def.password)
                    ));
                }
                if def.locked != o.locked {
                    match def.locked {
                        Some(true) => out.push(format!("alter login {} disable", q(login))),
                        Some(false) => out.push(format!("alter login {} enable", q(login))),
                        None => {}
                    }
                }
            }
        }
    }

    let empty = UserDef::default();
    let before = original.unwrap_or(&empty);
    for role in before.roles.keys().filter(|r| !def.roles.contains_key(*r)) {
        out.push(format!("alter role {} drop member {name}", q(role)));
    }
    for role in def.roles.keys().filter(|r| !before.roles.contains_key(*r)) {
        out.push(format!("alter role {} add member {name}", q(role)));
    }

    for (scope, (before, after)) in by_scope(&before.privileges, &def.privileges) {
        let on = on(&scope);
        // Revoking what may be granted on takes it from whom it was granted
        // to as well (CASCADE).
        let mut groups: BTreeMap<(u8, bool), Vec<&String>> = BTreeMap::new();
        for (privilege, was) in &before {
            let cascade = *was == Grant::WithGrantOption;
            match after.get(privilege) {
                None => groups.entry((0, cascade)).or_default().push(privilege),
                Some(Grant::Plain) if cascade => {
                    groups.entry((1, true)).or_default().push(privilege)
                }
                _ => {}
            }
        }
        for (privilege, now) in &after {
            let was = before.get(privilege);
            let cascade = was == Some(&Grant::WithGrantOption);
            let action = match (was, now) {
                (Some(w), n) if w == n => continue,
                (Some(Grant::WithGrantOption), Grant::Plain) => continue,
                (_, Grant::Plain) => 2,
                (_, Grant::WithGrantOption) => 3,
                (_, Grant::Deny) => 4,
            };
            groups
                .entry((action, cascade && action == 4))
                .or_default()
                .push(privilege);
        }
        for ((action, cascade), privileges) in groups {
            let privileges = list(&privileges);
            let cascade = if cascade { " cascade" } else { "" };
            out.push(match action {
                0 => format!("revoke {privileges}{on} from {name}{cascade}"),
                1 => format!("revoke grant option for {privileges}{on} from {name} cascade"),
                2 => format!("grant {privileges}{on} to {name}"),
                3 => format!("grant {privileges}{on} to {name} with grant option"),
                _ => format!("deny {privileges}{on} to {name}{cascade}"),
            });
        }
    }
    Ok(out)
}

pub(super) fn ui(ui: &mut Ui, def: &mut UserDef, original: Option<&UserDef>, choices: &Choices) {
    let existing = original.is_some();
    if def.role {
        return;
    }
    form_row(ui, "Authentication", |ui| {
        if existing {
            ui.label(match def.login {
                Login::Mapped | Login::New => "server login",
                Login::None => "without login",
                Login::Password => "password (contained database)",
                Login::Other => "Windows or external",
            });
            return;
        }
        ui.radio_value(&mut def.login, Login::Mapped, "login");
        ui.radio_value(&mut def.login, Login::New, "new login")
            .on_hover_text("Creates a server login with a password first (ALTER ANY LOGIN)");
        ui.radio_value(&mut def.login, Login::None, "without login")
            .on_hover_text("Nobody logs in as this user");
        ui.add_enabled_ui(choices.contained, |ui| {
            ui.radio_value(&mut def.login, Login::Password, "password")
                .on_hover_text("A user of a contained database, with a password of its own")
                .on_disabled_hover_text("Only in a contained database");
        });
    });
    if matches!(def.login, Login::Mapped | Login::New) {
        form_row(ui, "Login", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut def.login_name)
                    .hint_text(if def.login == Login::New {
                        "the user's name"
                    } else {
                        "login"
                    })
                    .desired_width(200.0),
            );
            if def.login == Login::Mapped
                && let Some(login) = picker(ui, "login", &choices.logins)
            {
                def.login_name = login;
            }
        });
    }
    // A new user for an existing login leaves the login as it is.
    let login = def.login == Login::New || existing && def.login == Login::Mapped;
    if login || def.login == Login::Password {
        let label = if def.login == Login::Password {
            "Password"
        } else {
            "Login password"
        };
        form_row(ui, label, |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut def.password)
                    .password(true)
                    .hint_text(if existing { "unchanged" } else { "" })
                    .desired_width(200.0),
            );
        });
    }
    if login {
        form_row(ui, "Login state", |ui| {
            if def.unread.attributes {
                ui.radio_value(&mut def.locked, None, "as it is");
                ui.radio_value(&mut def.locked, Some(true), "disabled");
                ui.radio_value(&mut def.locked, Some(false), "enabled");
            } else {
                let mut disabled = def.locked == Some(true);
                if ui
                    .checkbox(&mut disabled, "disabled")
                    .on_hover_text("Nobody can log in with the login until it's enabled")
                    .changed()
                {
                    def.locked = Some(disabled);
                }
            }
        });
    }
    form_row(ui, "Default schema", |ui| {
        ui.add(
            egui::TextEdit::singleline(&mut def.default_schema)
                .hint_text("dbo")
                .desired_width(200.0),
        );
        if let Some(s) = picker(ui, "default-schema", &choices.schemas) {
            def.default_schema = s;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user() -> UserDef {
        UserDef {
            name: "app".into(),
            login_name: "app_login".into(),
            ..UserDef::new(DbKind::SqlServer, false, false)
        }
    }

    #[test]
    fn creates_users() {
        let mut def = user();
        def.roles
            .insert("db_datareader".into(), RoleGrant::default());
        def.privileges
            .insert((Scope::Global, "CREATE TABLE".into()), Grant::Plain);
        def.privileges.insert(
            (Scope::Schema("sales".into()), "SELECT".into()),
            Grant::WithGrantOption,
        );
        def.privileges.insert(
            (
                Scope::Object("dbo".into(), "order items".into()),
                "DELETE".into(),
            ),
            Grant::Deny,
        );
        assert_eq!(
            statements(None, &def).unwrap(),
            [
                "create user app for login app_login with default_schema = dbo",
                "alter role db_datareader add member app",
                "grant create table to app",
                "grant select on schema::sales to app with grant option",
                "deny delete on dbo.[order items] to app",
            ]
        );

        let mut new = UserDef {
            login: Login::New,
            login_name: String::new(),
            password: "it's".into(),
            default_schema: String::new(),
            ..user()
        };
        new.locked = Some(true);
        assert_eq!(
            statements(None, &new).unwrap(),
            [
                "create login app with password = N'it''s'",
                "create user app for login app",
                "alter login app disable",
            ]
        );
        new.login = Login::None;
        assert_eq!(
            statements(None, &new).unwrap(),
            ["create user app without login"]
        );
        new.login = Login::Password;
        new.default_schema = "sales".into();
        assert_eq!(
            statements(None, &new).unwrap(),
            ["create user app with password = N'it''s', default_schema = sales"]
        );
        let role = UserDef {
            name: "readers".into(),
            ..UserDef::new(DbKind::SqlServer, true, false)
        };
        assert_eq!(statements(None, &role).unwrap(), ["create role readers"]);
    }

    #[test]
    fn alters_users() {
        let mut loaded = user();
        loaded.locked = Some(false);
        loaded.roles.insert("readers".into(), RoleGrant::default());
        let select = (Scope::Schema("dbo".into()), "SELECT".to_owned());
        let update = (Scope::Schema("dbo".into()), "UPDATE".to_owned());
        loaded
            .privileges
            .insert(select.clone(), Grant::WithGrantOption);
        loaded.privileges.insert(update.clone(), Grant::Plain);
        assert_eq!(statements(Some(&loaded), &loaded), Ok(Vec::new()));

        let mut edited = loaded.clone();
        edited.default_schema = "sales".into();
        edited.password = "new".into();
        edited.locked = Some(true);
        edited.roles.clear();
        edited.privileges.insert(select.clone(), Grant::Plain);
        edited.privileges.insert(update, Grant::Deny);
        assert_eq!(
            statements(Some(&loaded), &edited).unwrap(),
            [
                "alter user app with default_schema = sales",
                "alter login app_login with password = N'new'",
                "alter login app_login disable",
                "alter role readers drop member app",
                "revoke grant option for select on schema::dbo from app cascade",
                "deny update on schema::dbo to app",
            ]
        );
        edited.privileges.insert(select.clone(), Grant::Deny);
        edited
            .privileges
            .remove(&(Scope::Schema("dbo".into()), "UPDATE".to_owned()));
        let statements = statements(Some(&loaded), &edited).unwrap();
        assert!(statements.contains(&"revoke update on schema::dbo from app".to_owned()));
        assert!(statements.contains(&"deny select on schema::dbo to app cascade".to_owned()));
    }
}
