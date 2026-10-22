//! Users and roles: authentication, a user's tablespaces, quota, profile
//! and account state, and the roles and system privileges granted. New
//! ones are created, existing ones altered, grants granted or revoked.
//! Reading another user needs the DBA views (`SELECT_CATALOG_ROLE`).

use std::collections::BTreeSet;

use egui::{RichText, Ui};
use pythia_db::DbError;
use pythia_db::oracle::Connection;

use super::{Lists, form_grid, form_row, ident, name_field, names, picker};
use crate::documents::quote_ident;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Auth {
    #[default]
    Password,
    /// By the operating system (`IDENTIFIED EXTERNALLY`).
    External,
    /// A user without login (schema only, 18c+), or a role enabled
    /// without a password (`NOT IDENTIFIED`).
    None,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AccountDef {
    pub role: bool,
    pub name: String,
    pub auth: Auth,
    /// A new password; empty keeps the current one.
    pub password: String,
    pub default_tablespace: String,
    pub temporary_tablespace: String,
    /// Quota on the default tablespace: empty for none, `unlimited`, or a
    /// size (`100M`).
    pub quota: String,
    pub profile: String,
    pub locked: bool,
    /// The password must be changed at the next login.
    pub expire: bool,
    pub roles: BTreeSet<String>,
    pub privileges: BTreeSet<String>,
}

impl AccountDef {
    pub fn new_user() -> Self {
        Self {
            privileges: ["CREATE SESSION".to_owned()].into(),
            ..Self::default()
        }
    }

    pub fn new_role() -> Self {
        Self {
            role: true,
            auth: Auth::None,
            ..Self::default()
        }
    }
}

/// What a developer's schema usually needs.
const DEVELOPER: &[&str] = &[
    "CREATE SESSION",
    "CREATE TABLE",
    "CREATE VIEW",
    "CREATE SEQUENCE",
    "CREATE PROCEDURE",
    "CREATE TRIGGER",
    "CREATE TYPE",
    "CREATE SYNONYM",
    "CREATE MATERIALIZED VIEW",
];

/// Reads with the DBA view, or (for the session's own user) the USER_ one.
fn read(
    conn: &Connection,
    dba: &str,
    own: Option<&str>,
    binds: &[&dyn pythia_db::oracle::sql_type::ToSql],
) -> Result<Vec<String>, DbError> {
    names(conn, dba, binds).or_else(|e| match own {
        Some(sql) => names(conn, sql, &[]),
        None => Err(DbError::new(format!(
            "{} (reading users and roles needs SELECT_CATALOG_ROLE)",
            e.message
        ))),
    })
}

pub fn load_user(conn: &Connection, name: &str) -> Result<AccountDef, DbError> {
    let me: String = conn.query_row_as("select user from dual", &[])?;
    let own = |sql| (me == name).then_some(sql);
    let row = conn
        .query_row_as::<(String, String, String, String, Option<String>)>(
            "select default_tablespace, temporary_tablespace, profile, account_status,
                    authentication_type
               from dba_users where username = :1",
            &[&name],
        )
        .or_else(|e| {
            if me == name {
                conn.query_row_as(
                    "select default_tablespace, temporary_tablespace, 'DEFAULT', account_status,
                            null
                       from user_users",
                    &[],
                )
            } else {
                Err(e)
            }
        })
        .map_err(|e| match e.db_error().map(|d| d.code()) {
            Some(1403) => DbError::new(format!("User {name} not found")),
            Some(942) => DbError::new(
                "Reading another user needs SELECT_CATALOG_ROLE (DBA_USERS)".to_owned(),
            ),
            _ => e.into(),
        })?;
    let (default_tablespace, temporary_tablespace, profile, status, auth) = row;
    let roles = read(
        conn,
        "select granted_role from dba_role_privs where grantee = :1",
        own("select granted_role from user_role_privs"),
        &[&name],
    )?;
    let privileges = read(
        conn,
        "select privilege from dba_sys_privs where grantee = :1",
        own("select privilege from user_sys_privs"),
        &[&name],
    )?;
    let quota = conn
        .query_as::<i64>(
            "select max_bytes from dba_ts_quotas where username = :1 and tablespace_name = :2",
            &[&name, &default_tablespace],
        )
        .or_else(|e| {
            if me == name {
                conn.query_as(
                    "select max_bytes from user_ts_quotas where tablespace_name = :1",
                    &[&default_tablespace],
                )
            } else {
                Err(e)
            }
        })?
        .next()
        .transpose()?;
    Ok(AccountDef {
        role: false,
        name: quote_ident(name),
        auth: match auth.as_deref() {
            Some("EXTERNAL") => Auth::External,
            Some("NONE") => Auth::None,
            _ => Auth::Password,
        },
        password: String::new(),
        default_tablespace: quote_ident(&default_tablespace),
        temporary_tablespace: quote_ident(&temporary_tablespace),
        quota: quota.map(size_text).unwrap_or_default(),
        profile: quote_ident(&profile),
        locked: status.contains("LOCKED"),
        expire: status.contains("EXPIRED"),
        roles: roles.into_iter().collect(),
        privileges: privileges.into_iter().collect(),
    })
}

pub fn load_role(conn: &Connection, name: &str) -> Result<AccountDef, DbError> {
    let required: Option<String> = conn
        .query_as::<Option<String>>(
            "select password_required from dba_roles where role = :1",
            &[&name],
        )
        .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        .map(|rows| rows.into_iter().next().flatten())
        .unwrap_or(None);
    let roles = names(
        conn,
        "select granted_role from dba_role_privs where grantee = :1",
        &[&name],
    )
    .or_else(|_| {
        names(
            conn,
            "select granted_role from role_role_privs where role = :1",
            &[&name],
        )
    })?;
    let privileges = names(
        conn,
        "select privilege from dba_sys_privs where grantee = :1",
        &[&name],
    )
    .or_else(|_| {
        names(
            conn,
            "select privilege from role_sys_privs where role = :1",
            &[&name],
        )
    })?;
    Ok(AccountDef {
        role: true,
        name: quote_ident(name),
        auth: match required.as_deref() {
            Some("YES") => Auth::Password,
            Some("EXTERNAL") => Auth::External,
            _ => Auth::None,
        },
        roles: roles.into_iter().collect(),
        privileges: privileges.into_iter().collect(),
        ..AccountDef::default()
    })
}

/// The account form's pickers.
pub fn lists(conn: &Connection, lists: &mut super::BaseLists) -> Result<(), DbError> {
    let any = |dba: &str, fallback: &str| -> Result<Vec<String>, DbError> {
        names(conn, dba, &[]).or_else(|_| names(conn, fallback, &[]))
    };
    lists.tablespaces = any(
        "select tablespace_name from dba_tablespaces where contents = 'PERMANENT' order by 1",
        "select tablespace_name from user_tablespaces where contents = 'PERMANENT' order by 1",
    )?;
    lists.temp_tablespaces = any(
        "select tablespace_name from dba_tablespaces where contents = 'TEMPORARY' order by 1",
        "select tablespace_name from user_tablespaces where contents = 'TEMPORARY' order by 1",
    )?;
    lists.profiles = any(
        "select distinct profile from dba_profiles order by 1",
        "select 'DEFAULT' from dual",
    )?;
    lists.roles = any(
        "select role from dba_roles order by 1",
        "select granted_role from user_role_privs order by 1",
    )?;
    lists.privileges = names(
        conn,
        "select name from system_privilege_map order by name",
        &[],
    )?;
    lists.session_privileges = names(conn, "select privilege from session_privs", &[])?;
    Ok(())
}

/// Bytes as a quota: `unlimited`, `100M`, `512K`.
fn size_text(bytes: i64) -> String {
    const K: i64 = 1024;
    match bytes {
        b if b < 0 => "unlimited".into(),
        b if b % (K * K * K) == 0 && b > 0 => format!("{}G", b / (K * K * K)),
        b if b % (K * K) == 0 && b > 0 => format!("{}M", b / (K * K)),
        b if b % K == 0 && b > 0 => format!("{}K", b / K),
        b => b.to_string(),
    }
}

fn valid_size(s: &str) -> bool {
    let s = s.trim();
    s.eq_ignore_ascii_case("unlimited")
        || s.trim_end_matches(['K', 'M', 'G', 'T', 'k', 'm', 'g', 't'])
            .chars()
            .all(|c| c.is_ascii_digit())
            && !s.is_empty()
            && s.chars().next().is_some_and(|c| c.is_ascii_digit())
}

fn name_list(names: &BTreeSet<String>) -> String {
    names
        .iter()
        .map(|n| {
            let plain = n.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && n.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || "_$#".contains(c));
            if n.contains(' ') {
                // A system privilege: keywords (`CREATE TABLE`).
                n.to_lowercase()
            } else if plain {
                // A role; `CONNECT` and `RESOURCE` need no quotes.
                n.clone()
            } else {
                quote_ident(n)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `grant …` / `revoke …` turning `before` into `after`.
fn grants(name: &str, before: &BTreeSet<String>, after: &BTreeSet<String>, out: &mut Vec<String>) {
    let added: BTreeSet<String> = after.difference(before).cloned().collect();
    let removed: BTreeSet<String> = before.difference(after).cloned().collect();
    if !removed.is_empty() {
        out.push(format!("revoke {} from {name}", name_list(&removed)));
    }
    if !added.is_empty() {
        out.push(format!("grant {} to {name}", name_list(&added)));
    }
}

fn identified(def: &AccountDef) -> String {
    match def.auth {
        Auth::Password => format!("identified by \"{}\"", def.password),
        Auth::External => "identified externally".into(),
        Auth::None if def.role => "not identified".into(),
        Auth::None => "no authentication".into(),
    }
}

fn tablespace(s: &str) -> String {
    quote_ident(&ident(s))
}

pub fn statements(original: Option<&AccountDef>, def: &AccountDef) -> Result<Vec<String>, String> {
    if def.name.trim().is_empty() {
        return Err("Enter a name".into());
    }
    if def.password.contains('"') {
        return Err("The password can't contain double quotes".into());
    }
    let auth_changed = original.is_none_or(|o| o.auth != def.auth);
    if def.auth == Auth::Password && def.password.is_empty() && auth_changed {
        return Err("Enter a password".into());
    }
    if !def.quota.trim().is_empty() && !valid_size(&def.quota) {
        return Err("Quota: unlimited, or a size like 100M".into());
    }
    if !def.quota.trim().is_empty() && def.default_tablespace.trim().is_empty() {
        return Err("A quota needs the default tablespace".into());
    }
    let name = quote_ident(&ident(&def.name));
    let kind = if def.role { "role" } else { "user" };
    let mut clauses = Vec::new();
    if auth_changed || !def.password.is_empty() {
        clauses.push(identified(def));
    }
    let empty = AccountDef::default();
    let before = original.unwrap_or(&empty);
    if !def.role {
        let changed = |a: &str, b: &str| ident(a) != ident(b) && !a.trim().is_empty();
        if changed(&def.default_tablespace, &before.default_tablespace) {
            clauses.push(format!(
                "default tablespace {}",
                tablespace(&def.default_tablespace)
            ));
        }
        if changed(&def.temporary_tablespace, &before.temporary_tablespace) {
            clauses.push(format!(
                "temporary tablespace {}",
                tablespace(&def.temporary_tablespace)
            ));
        }
        let quota_changed = !def.quota.trim().eq_ignore_ascii_case(before.quota.trim())
            || original.is_some_and(|_| {
                ident(&def.default_tablespace) != ident(&before.default_tablespace)
                    && !def.quota.trim().is_empty()
            });
        if quota_changed && !def.default_tablespace.trim().is_empty() {
            let quota = match def.quota.trim() {
                "" => "0".to_owned(),
                q if q.eq_ignore_ascii_case("unlimited") => "unlimited".to_owned(),
                q => q.to_uppercase(),
            };
            clauses.push(format!(
                "quota {quota} on {}",
                tablespace(&def.default_tablespace)
            ));
        }
        if changed(&def.profile, &before.profile) {
            clauses.push(format!("profile {}", quote_ident(&ident(&def.profile))));
        }
        if def.expire && !before.expire {
            clauses.push("password expire".into());
        }
        if def.locked != before.locked {
            clauses.push(if def.locked {
                "account lock".into()
            } else {
                "account unlock".into()
            });
        }
    }
    let mut out = Vec::new();
    match original {
        None => {
            let mut sql = format!("create {kind} {name}");
            for c in &clauses {
                sql.push_str("\n  ");
                sql.push_str(c);
            }
            out.push(sql);
        }
        Some(_) if !clauses.is_empty() => {
            out.push(format!("alter {kind} {name}\n  {}", clauses.join("\n  ")));
        }
        Some(_) => {}
    }
    grants(&name, &before.privileges, &def.privileges, &mut out);
    // A role isn't granted to itself.
    let roles = |list: &BTreeSet<String>| -> BTreeSet<String> {
        list.iter()
            .filter(|r| **r != ident(&def.name))
            .cloned()
            .collect()
    };
    grants(&name, &roles(&before.roles), &roles(&def.roles), &mut out);
    Ok(out)
}

/// The statements with the password hidden, for showing them.
pub fn masked(statements: &[String], def: &AccountDef) -> Vec<String> {
    if def.password.is_empty() {
        return statements.to_vec();
    }
    let secret = format!("identified by \"{}\"", def.password);
    statements
        .iter()
        .map(|s| s.replace(&secret, "identified by \"********\""))
        .collect()
}

/// What the session can't do that the form needs.
pub fn missing_privilege(def: &AccountDef, existing: bool, lists: &Lists) -> Option<String> {
    if lists.session_privileges.is_empty() {
        return None;
    }
    let needed = match (def.role, existing) {
        (false, false) => "CREATE USER",
        (false, true) => "ALTER USER",
        (true, false) => "CREATE ROLE",
        (true, true) => return None,
    };
    (!lists.session_privileges.iter().any(|p| p == needed))
        .then(|| format!("This session doesn't have {needed}."))
}

/// A list of names with a picker to add and × to remove.
fn names_ui(
    ui: &mut Ui,
    id: &str,
    title: &str,
    granted: &mut BTreeSet<String>,
    choices: &[String],
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).strong());
        let open: Vec<String> = choices
            .iter()
            .filter(|c| !granted.contains(*c))
            .cloned()
            .collect();
        if let Some(name) = picker(ui, (id, "add"), &open) {
            granted.insert(name);
        }
    });
    let mut remove = None;
    egui::ScrollArea::vertical()
        .id_salt(id)
        .max_height(160.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            if granted.is_empty() {
                ui.weak("none");
            }
            for name in granted.iter() {
                ui.horizontal(|ui| {
                    if ui.small_button("×").on_hover_text("Revoke").clicked() {
                        remove = Some(name.clone());
                    }
                    ui.label(RichText::new(name.to_lowercase()).monospace());
                });
            }
        });
    if let Some(name) = remove {
        granted.remove(&name);
    }
}

pub fn ui(ui: &mut Ui, def: &mut AccountDef, original: Option<&AccountDef>, lists: &Lists) {
    let existing = original.is_some();
    if let Some(missing) = missing_privilege(def, existing, lists) {
        ui.colored_label(ui.visuals().warn_fg_color, missing);
        ui.add_space(4.0);
    }
    form_grid(ui, "account", |ui| {
        name_field(ui, &mut def.name, existing);
        form_row(ui, "Authentication", |ui| {
            ui.radio_value(&mut def.auth, Auth::Password, "password");
            if def.role {
                ui.radio_value(&mut def.auth, Auth::None, "none")
                    .on_hover_text("NOT IDENTIFIED: enabled without a password");
            } else {
                ui.radio_value(&mut def.auth, Auth::External, "external")
                    .on_hover_text("IDENTIFIED EXTERNALLY: by the operating system");
                ui.add_enabled_ui(lists.release >= (18, 0), |ui| {
                    ui.radio_value(&mut def.auth, Auth::None, "none (schema only)")
                        .on_hover_text("NO AUTHENTICATION: nobody logs in as this user")
                        .on_disabled_hover_text("Needs Oracle 18c or later");
                });
            }
        });
        if def.auth == Auth::Password {
            form_row(ui, "Password", |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut def.password)
                        .password(true)
                        .hint_text(
                            if existing && original.is_some_and(|o| o.auth == Auth::Password) {
                                "unchanged"
                            } else {
                                ""
                            },
                        )
                        .desired_width(200.0),
                );
                if !def.role {
                    ui.checkbox(&mut def.expire, "expired")
                        .on_hover_text("Must be changed at the next login");
                }
            });
        }
        if def.role {
            return;
        }
        let tablespace_row = |ui: &mut Ui, label: &str, value: &mut String, items: &[String]| {
            form_row(ui, label, |ui| {
                ui.add(
                    egui::TextEdit::singleline(value)
                        .hint_text("database default")
                        .desired_width(200.0),
                );
                if let Some(ts) = picker(ui, label, items) {
                    *value = quote_ident(&ts);
                }
            });
        };
        tablespace_row(
            ui,
            "Default tablespace",
            &mut def.default_tablespace,
            &lists.tablespaces,
        );
        tablespace_row(
            ui,
            "Temporary tablespace",
            &mut def.temporary_tablespace,
            &lists.temp_tablespaces,
        );
        form_row(ui, "Quota", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut def.quota)
                    .hint_text("none")
                    .desired_width(100.0),
            );
            let sizes: Vec<String> = ["unlimited", "100M", "1G", "10G"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect();
            if let Some(q) = picker(ui, "quota", &sizes) {
                def.quota = q;
            }
            ui.weak("on the default tablespace");
        });
        form_row(ui, "Profile", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut def.profile)
                    .hint_text("DEFAULT")
                    .desired_width(200.0),
            );
            if let Some(p) = picker(ui, "profile", &lists.profiles) {
                def.profile = quote_ident(&p);
            }
        });
        form_row(ui, "Account", |ui| {
            ui.checkbox(&mut def.locked, "locked")
                .on_hover_text("Nobody can log in until it's unlocked");
        });
    });
    ui.add_space(8.0);
    ui.columns(2, |columns| {
        let roles: Vec<String> = lists
            .roles
            .iter()
            .filter(|r| **r != ident(&def.name))
            .cloned()
            .collect();
        names_ui(&mut columns[0], "roles", "Roles", &mut def.roles, &roles);
        names_ui(
            &mut columns[1],
            "privileges",
            "System privileges",
            &mut def.privileges,
            &lists.privileges,
        );
        if columns[1]
            .button("Add what a developer needs")
            .on_hover_text(DEVELOPER.join(", ").to_lowercase())
            .clicked()
        {
            def.privileges
                .extend(DEVELOPER.iter().map(|p| (*p).to_owned()));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_users() {
        let mut def = AccountDef::new_user();
        def.name = "app_owner".into();
        assert_eq!(statements(None, &def), Err("Enter a password".into()));
        def.password = "Secret_1".into();
        def.default_tablespace = "users".into();
        def.temporary_tablespace = "TEMP".into();
        def.quota = "unlimited".into();
        def.expire = true;
        def.roles.insert("CONNECT".into());
        let s = statements(None, &def).unwrap();
        assert_eq!(
            s,
            [
                "create user APP_OWNER\n  identified by \"Secret_1\"\n  default tablespace USERS\n  \
                 temporary tablespace TEMP\n  quota unlimited on USERS\n  password expire",
                "grant create session to APP_OWNER",
                "grant CONNECT to APP_OWNER",
            ]
        );
        assert!(masked(&s, &def)[0].contains("identified by \"********\""));
        def.default_tablespace.clear();
        assert!(statements(None, &def).is_err(), "quota without tablespace");
        def.quota = "lots".into();
        assert!(statements(None, &def).is_err());
    }

    #[test]
    fn alters_users() {
        let original = AccountDef {
            name: "APP".into(),
            default_tablespace: "USERS".into(),
            temporary_tablespace: "TEMP".into(),
            quota: "100M".into(),
            profile: "DEFAULT".into(),
            privileges: ["CREATE SESSION".to_owned(), "CREATE TABLE".to_owned()].into(),
            roles: ["CONNECT".to_owned()].into(),
            ..AccountDef::default()
        };
        let mut def = original.clone();
        assert_eq!(statements(Some(&original), &def), Ok(Vec::new()));
        def.locked = true;
        def.quota = "unlimited".into();
        def.privileges.remove("CREATE TABLE");
        def.privileges.insert("CREATE VIEW".into());
        def.roles.clear();
        assert_eq!(
            statements(Some(&original), &def).unwrap(),
            [
                "alter user APP\n  quota unlimited on USERS\n  account lock",
                "revoke create table from APP",
                "grant create view to APP",
                "revoke CONNECT from APP",
            ]
        );
        def = original.clone();
        def.password = "new".into();
        assert_eq!(
            statements(Some(&original), &def).unwrap(),
            ["alter user APP\n  identified by \"new\""]
        );
        def = original.clone();
        def.quota.clear();
        assert_eq!(
            statements(Some(&original), &def).unwrap(),
            ["alter user APP\n  quota 0 on USERS"]
        );
    }

    #[test]
    fn roles() {
        let mut def = AccountDef::new_role();
        def.name = "app_reader".into();
        def.privileges.insert("CREATE SESSION".into());
        def.roles.insert("APP_READER".into());
        assert_eq!(
            statements(None, &def).unwrap(),
            [
                "create role APP_READER\n  not identified",
                "grant create session to APP_READER",
            ]
        );
        let original = def.clone();
        def.auth = Auth::Password;
        assert!(statements(Some(&original), &def).is_err());
        def.password = "pw".into();
        assert_eq!(
            statements(Some(&original), &def).unwrap(),
            ["alter role APP_READER\n  identified by \"pw\""]
        );
    }

    #[test]
    fn sizes() {
        assert_eq!(size_text(-1), "unlimited");
        assert_eq!(size_text(100 * 1024 * 1024), "100M");
        assert_eq!(size_text(2 * 1024 * 1024 * 1024), "2G");
        assert_eq!(size_text(1000), "1000");
        assert!(valid_size("100M") && valid_size("unlimited") && valid_size("5"));
        assert!(!valid_size("M") && !valid_size("lots") && !valid_size(""));
    }
}
