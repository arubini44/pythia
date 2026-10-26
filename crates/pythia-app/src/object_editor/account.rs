//! Users and roles: authentication, a user's tablespaces, quotas, profile
//! and account state; the roles and system privileges granted (with admin
//! option, and a user's default roles) and the object privileges. New ones
//! are created, existing ones altered, grants granted or revoked. Reading
//! another user needs the DBA views (`SELECT_CATALOG_ROLE`).

use std::collections::{BTreeMap, BTreeSet};

use egui::{RichText, Ui};
use pythia_db::DbError;
use pythia_db::oracle::{Connection, RowValue};

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

/// A granted role's options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoleGrant {
    /// `WITH ADMIN OPTION`: may grant it on.
    pub admin: bool,
    /// A user's role that's enabled at login.
    pub default: bool,
}

impl Default for RoleGrant {
    fn default() -> Self {
        Self {
            admin: false,
            default: true,
        }
    }
}

/// An object privilege: owner (empty for a directory), object, privilege.
pub type ObjectPrivilege = (String, String, String);

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AccountDef {
    pub role: bool,
    pub name: String,
    pub auth: Auth,
    /// A new password; empty keeps the current one.
    pub password: String,
    pub default_tablespace: String,
    pub temporary_tablespace: String,
    /// Quotas: tablespace (as typed) and `unlimited` or a size (`100M`).
    pub quotas: Vec<(String, String)>,
    pub profile: String,
    pub locked: bool,
    /// The password must be changed at the next login.
    pub expire: bool,
    pub roles: BTreeMap<String, RoleGrant>,
    /// System privileges, with admin option or not.
    pub privileges: BTreeMap<String, bool>,
    /// Object privileges, with grant option or not.
    pub objects: BTreeMap<ObjectPrivilege, bool>,
}

impl AccountDef {
    pub fn new_user() -> Self {
        Self {
            privileges: [("CREATE SESSION".to_owned(), false)].into(),
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

/// Object privileges offered when adding one.
const OBJECT_PRIVILEGES: &[&str] = &[
    "SELECT",
    "READ",
    "INSERT",
    "UPDATE",
    "DELETE",
    "EXECUTE",
    "REFERENCES",
    "ALTER",
    "INDEX",
    "DEBUG",
    "WRITE",
];

/// Rows from the DBA view, or (for the session's own user or roles) from
/// the USER_/ROLE_ one.
fn rows<T: RowValue>(
    conn: &Connection,
    dba: &str,
    own: Option<&str>,
    binds: &[&dyn pythia_db::oracle::sql_type::ToSql],
) -> Result<Vec<T>, DbError> {
    let read = |sql: &str, binds: &[&dyn pythia_db::oracle::sql_type::ToSql]| {
        conn.query_as::<T>(sql, binds)?
            .collect::<Result<Vec<T>, _>>()
    };
    read(dba, binds).or_else(|e| match own {
        Some(sql) => read(sql, binds).map_err(DbError::from),
        None => Err(DbError::new(format!(
            "{} (reading users and roles needs SELECT_CATALOG_ROLE)",
            DbError::from(e).message
        ))),
    })
}

/// Object privileges by grantee; a privilege on a directory has no owner.
fn object_privileges(
    conn: &Connection,
    dba: &str,
    own: Option<&str>,
    binds: &[&dyn pythia_db::oracle::sql_type::ToSql],
) -> Result<BTreeMap<ObjectPrivilege, bool>, DbError> {
    let directories: BTreeSet<String> =
        names(conn, "select directory_name from all_directories", &[])
            .unwrap_or_default()
            .into_iter()
            .collect();
    let mut out = BTreeMap::new();
    for (owner, object, privilege, grantable) in
        rows::<(String, String, String, String)>(conn, dba, own, binds)?
    {
        // `INHERIT PRIVILEGES` is granted on a user, not an object.
        if privilege.starts_with("INHERIT") {
            continue;
        }
        let owner = if owner == "SYS" && directories.contains(&object) {
            String::new()
        } else {
            owner
        };
        out.insert((owner, object, privilege), grantable == "YES");
    }
    Ok(out)
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
                       from user_users where username = :1",
                    &[&name],
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
    let roles = rows::<(String, String, String)>(
        conn,
        "select granted_role, admin_option, default_role from dba_role_privs
          where grantee = :1",
        own(
            "select granted_role, admin_option, default_role from user_role_privs
              where username = :1",
        ),
        &[&name],
    )?;
    let privileges = rows::<(String, String)>(
        conn,
        "select privilege, admin_option from dba_sys_privs where grantee = :1",
        own("select privilege, admin_option from user_sys_privs where username = :1"),
        &[&name],
    )?;
    let quotas = rows::<(String, i64)>(
        conn,
        "select tablespace_name, max_bytes from dba_ts_quotas
          where username = :1 and max_bytes <> 0 order by tablespace_name",
        own("select tablespace_name, max_bytes from user_ts_quotas
              where :1 is not null and max_bytes <> 0 order by tablespace_name"),
        &[&name],
    )?;
    let objects = object_privileges(
        conn,
        "select owner, table_name, privilege, grantable from dba_tab_privs where grantee = :1",
        own(
            "select owner, table_name, privilege, grantable from user_tab_privs_recd
              where :1 is not null",
        ),
        &[&name],
    )?;
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
        quotas: quotas
            .into_iter()
            .map(|(ts, bytes)| (quote_ident(&ts), size_text(bytes)))
            .collect(),
        profile: quote_ident(&profile),
        locked: status.contains("LOCKED"),
        expire: status.contains("EXPIRED"),
        roles: roles
            .into_iter()
            .map(|(role, admin, default)| {
                (
                    role,
                    RoleGrant {
                        admin: admin == "YES",
                        default: default == "YES",
                    },
                )
            })
            .collect(),
        privileges: privileges
            .into_iter()
            .map(|(p, admin)| (p, admin == "YES"))
            .collect(),
        objects,
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
    let roles = rows::<(String, String)>(
        conn,
        "select granted_role, admin_option from dba_role_privs where grantee = :1",
        Some("select granted_role, admin_option from role_role_privs where role = :1"),
        &[&name],
    )?;
    let privileges = rows::<(String, String)>(
        conn,
        "select privilege, admin_option from dba_sys_privs where grantee = :1",
        Some("select privilege, admin_option from role_sys_privs where role = :1"),
        &[&name],
    )?;
    let objects = object_privileges(
        conn,
        "select owner, table_name, privilege, grantable from dba_tab_privs where grantee = :1",
        Some(
            "select owner, table_name, privilege, grantable from role_tab_privs
              where role = :1 and column_name is null",
        ),
        &[&name],
    )?;
    Ok(AccountDef {
        role: true,
        name: quote_ident(name),
        auth: match required.as_deref() {
            Some("YES") => Auth::Password,
            Some("EXTERNAL") => Auth::External,
            _ => Auth::None,
        },
        roles: roles
            .into_iter()
            .map(|(role, admin)| {
                (
                    role,
                    RoleGrant {
                        admin: admin == "YES",
                        default: true,
                    },
                )
            })
            .collect(),
        privileges: privileges
            .into_iter()
            .map(|(p, admin)| (p, admin == "YES"))
            .collect(),
        objects,
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
    lists.owners = names(
        conn,
        "select username from all_users order by username",
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

/// A quota as written: `unlimited`, `100M`.
fn size_sql(size: &str) -> String {
    match size.trim() {
        s if s.eq_ignore_ascii_case("unlimited") => "unlimited".into(),
        s => s.to_uppercase(),
    }
}

/// A role or system privilege as written in GRANT and REVOKE.
fn grantable_name(n: &str) -> String {
    let plain = n.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && n.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || "_$#".contains(c));
    if n.contains(' ') {
        // A system privilege: keywords (`CREATE TABLE`).
        n.to_lowercase()
    } else if plain {
        // A role; `CONNECT` and `RESOURCE` need no quotes.
        n.to_owned()
    } else {
        quote_ident(n)
    }
}

fn name_list<'a>(names: impl IntoIterator<Item = &'a String>) -> String {
    names
        .into_iter()
        .map(|n| grantable_name(n))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `revoke …` / `grant … [with admin option]` turning `before` into `after`
/// (name → admin option). Taking the admin option away means revoking
/// and granting again.
fn grants(
    name: &str,
    before: &BTreeMap<String, bool>,
    after: &BTreeMap<String, bool>,
    out: &mut Vec<String>,
) {
    let revoke: Vec<&String> = before
        .iter()
        .filter(|(n, admin)| match after.get(*n) {
            None => true,
            Some(now) => **admin && !now,
        })
        .map(|(n, _)| n)
        .collect();
    let grant = |with_admin: bool| -> Vec<&String> {
        after
            .iter()
            .filter(|(n, admin)| {
                **admin == with_admin
                    && match before.get(*n) {
                        None => true,
                        Some(was) => *was != **admin,
                    }
            })
            .map(|(n, _)| n)
            .collect()
    };
    if !revoke.is_empty() {
        out.push(format!("revoke {} from {name}", name_list(revoke)));
    }
    let plain = grant(false);
    if !plain.is_empty() {
        out.push(format!("grant {} to {name}", name_list(plain)));
    }
    let admin = grant(true);
    if !admin.is_empty() {
        out.push(format!(
            "grant {} to {name} with admin option",
            name_list(admin)
        ));
    }
}

/// `owner.object`, or `directory x`.
fn object_sql((owner, object, _): &ObjectPrivilege) -> String {
    if owner.is_empty() {
        format!("directory {}", quote_ident(object))
    } else {
        format!("{}.{}", quote_ident(owner), quote_ident(object))
    }
}

/// Object privilege changes, per object: revokes, then grants (with grant
/// option or not).
fn object_grants(
    name: &str,
    before: &BTreeMap<ObjectPrivilege, bool>,
    after: &BTreeMap<ObjectPrivilege, bool>,
    out: &mut Vec<String>,
) {
    // By object: privileges to revoke, to grant, to grant with grant option.
    let mut by_object: BTreeMap<String, [Vec<String>; 3]> = BTreeMap::new();
    for (key, grantable) in before {
        let revoke = match after.get(key) {
            None => true,
            Some(now) => *grantable && !now,
        };
        if revoke {
            by_object.entry(object_sql(key)).or_default()[0].push(key.2.clone());
        }
    }
    for (key, grantable) in after {
        let grant = match before.get(key) {
            None => true,
            Some(was) => was != grantable,
        };
        if grant {
            by_object.entry(object_sql(key)).or_default()[if *grantable { 2 } else { 1 }]
                .push(key.2.clone());
        }
    }
    let list = |privileges: &[String]| privileges.join(", ").to_lowercase();
    for (object, [revoke, _, _]) in &by_object {
        if !revoke.is_empty() {
            out.push(format!("revoke {} on {object} from {name}", list(revoke)));
        }
    }
    for (object, [_, plain, with_grant]) in &by_object {
        if !plain.is_empty() {
            out.push(format!("grant {} on {object} to {name}", list(plain)));
        }
        if !with_grant.is_empty() {
            out.push(format!(
                "grant {} on {object} to {name} with grant option",
                list(with_grant)
            ));
        }
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

/// Quota clauses turning `before` into `after`; a quota taken away is 0.
fn quota_clauses(before: &[(String, String)], after: &[(String, String)]) -> Vec<String> {
    let find = |list: &[(String, String)], ts: &str| {
        list.iter()
            .find(|(t, _)| ident(t) == ident(ts))
            .map(|(_, size)| size_sql(size))
    };
    let mut out: Vec<String> = after
        .iter()
        .filter(|(ts, size)| find(before, ts) != Some(size_sql(size)))
        .map(|(ts, size)| format!("quota {} on {}", size_sql(size), tablespace(ts)))
        .collect();
    out.extend(
        before
            .iter()
            .filter(|(ts, _)| find(after, ts).is_none())
            .map(|(ts, _)| format!("quota 0 on {}", tablespace(ts))),
    );
    out
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
    let mut seen = BTreeSet::new();
    for (ts, size) in &def.quotas {
        if ts.trim().is_empty() {
            return Err("A quota needs its tablespace".into());
        }
        if !valid_size(size) {
            return Err(format!(
                "Quota on {}: unlimited, or a size like 100M",
                ts.trim()
            ));
        }
        if !seen.insert(ident(ts)) {
            return Err(format!("Two quotas on {}", ts.trim()));
        }
    }
    if def.role && def.objects.values().any(|grantable| *grantable) {
        return Err("A role can't get privileges with grant option".into());
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
        clauses.extend(quota_clauses(&before.quotas, &def.quotas));
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
    let roles = |list: &BTreeMap<String, RoleGrant>| -> BTreeMap<String, bool> {
        list.iter()
            .filter(|(r, _)| **r != ident(&def.name))
            .map(|(r, g)| (r.clone(), g.admin))
            .collect()
    };
    grants(&name, &roles(&before.roles), &roles(&def.roles), &mut out);
    object_grants(&name, &before.objects, &def.objects, &mut out);
    if !def.role {
        out.extend(default_roles(original, def, &name));
    }
    Ok(out)
}

/// `alter user … default role …` when the default roles change. A new
/// user's granted roles are all default; a role granted later may not be,
/// so the statement follows any grant.
fn default_roles(original: Option<&AccountDef>, def: &AccountDef, name: &str) -> Option<String> {
    if def.roles.is_empty() {
        return None;
    }
    let defaults: BTreeSet<&String> = def
        .roles
        .iter()
        .filter(|(_, g)| g.default)
        .map(|(r, _)| r)
        .collect();
    let needed = match original {
        None => defaults.len() != def.roles.len(),
        Some(o) => {
            let added = def.roles.keys().any(|r| !o.roles.contains_key(r));
            let changed = def
                .roles
                .iter()
                .any(|(r, g)| o.roles.get(r).is_some_and(|was| was.default != g.default));
            added || changed
        }
    };
    if !needed {
        return None;
    }
    let roles = if defaults.is_empty() {
        "none".to_owned()
    } else if defaults.len() == def.roles.len() {
        "all".to_owned()
    } else {
        name_list(defaults)
    };
    Some(format!("alter user {name} default role {roles}"))
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

/// Quotas: tablespace and size per row.
fn quotas_ui(ui: &mut Ui, quotas: &mut Vec<(String, String)>, tablespaces: &[String]) {
    let sizes: Vec<String> = ["unlimited", "100M", "1G", "10G"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let mut remove = None;
    let height = ui.spacing().interact_size.y;
    egui::Grid::new("quotas")
        .num_columns(3)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            for (i, (ts, size)) in quotas.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [180.0, height],
                        egui::TextEdit::singleline(ts).hint_text("tablespace"),
                    );
                    if let Some(t) = picker(ui, ("quota-ts", i), tablespaces) {
                        *ts = quote_ident(&t);
                    }
                });
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [100.0, height],
                        egui::TextEdit::singleline(size).hint_text("100M"),
                    );
                    if let Some(s) = picker(ui, ("quota-size", i), &sizes) {
                        *size = s;
                    }
                });
                if ui.small_button("×").on_hover_text("No quota").clicked() {
                    remove = Some(i);
                }
                ui.end_row();
            }
        });
    if let Some(i) = remove {
        quotas.remove(i);
    }
    if ui.button("Add quota").clicked() {
        quotas.push((String::new(), "unlimited".into()));
    }
}

/// Granted roles: admin option and (users) default.
fn roles_ui(ui: &mut Ui, def: &mut AccountDef, choices: &[String]) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Roles").strong());
        let open: Vec<String> = choices
            .iter()
            .filter(|c| !def.roles.contains_key(*c) && **c != ident(&def.name))
            .cloned()
            .collect();
        if let Some(role) = picker(ui, "add-role", &open) {
            def.roles.insert(role, RoleGrant::default());
        }
    });
    if def.roles.is_empty() {
        ui.weak("none");
        return;
    }
    let mut remove = None;
    let user = !def.role;
    egui::Grid::new("roles")
        .num_columns(4)
        .spacing([10.0, 2.0])
        .show(ui, |ui| {
            ui.label("");
            ui.weak("admin")
                .on_hover_text("With admin option: may grant it on");
            if user {
                ui.weak("default")
                    .on_hover_text("Enabled at login; others need SET ROLE");
            } else {
                ui.label("");
            }
            ui.label("");
            ui.end_row();
            for (role, grant) in def.roles.iter_mut() {
                ui.label(RichText::new(role.to_lowercase()).monospace());
                ui.checkbox(&mut grant.admin, "");
                if user {
                    ui.checkbox(&mut grant.default, "");
                } else {
                    ui.label("");
                }
                if ui.small_button("×").on_hover_text("Revoke").clicked() {
                    remove = Some(role.clone());
                }
                ui.end_row();
            }
        });
    if let Some(role) = remove {
        def.roles.remove(&role);
    }
}

/// Granted system privileges with their admin option.
fn privileges_ui(ui: &mut Ui, privileges: &mut BTreeMap<String, bool>, choices: &[String]) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("System privileges").strong());
        let open: Vec<String> = choices
            .iter()
            .filter(|c| !privileges.contains_key(*c))
            .cloned()
            .collect();
        if let Some(p) = picker(ui, "add-privilege", &open) {
            privileges.insert(p, false);
        }
    });
    let mut remove = None;
    egui::ScrollArea::vertical()
        .id_salt("privileges")
        .max_height(200.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            if privileges.is_empty() {
                ui.weak("none");
                return;
            }
            egui::Grid::new("privileges-grid")
                .num_columns(3)
                .spacing([10.0, 2.0])
                .show(ui, |ui| {
                    ui.label("");
                    ui.weak("admin")
                        .on_hover_text("With admin option: may grant it on");
                    ui.label("");
                    ui.end_row();
                    for (p, admin) in privileges.iter_mut() {
                        ui.label(RichText::new(p.to_lowercase()).monospace());
                        ui.checkbox(admin, "");
                        if ui.small_button("×").on_hover_text("Revoke").clicked() {
                            remove = Some(p.clone());
                        }
                        ui.end_row();
                    }
                });
        });
    if let Some(p) = remove {
        privileges.remove(&p);
    }
    if ui
        .button("Add what a developer needs")
        .on_hover_text(DEVELOPER.join(", ").to_lowercase())
        .clicked()
    {
        for p in DEVELOPER {
            privileges.entry((*p).to_owned()).or_insert(false);
        }
    }
}

/// The object privilege being added (kept between frames).
#[derive(Clone, Default)]
struct NewObjectPrivilege {
    owner: String,
    object: String,
    privilege: String,
    grantable: bool,
}

/// Object privileges: per object, the privileges; a row to add one.
fn objects_ui(ui: &mut Ui, def: &mut AccountDef, lists: &mut Lists) {
    ui.label(RichText::new("Object privileges").strong());
    let user = !def.role;
    let mut remove = None;
    if def.objects.is_empty() {
        ui.weak("none");
    } else {
        egui::Grid::new("object-privileges")
            .striped(true)
            .num_columns(4)
            .spacing([12.0, 2.0])
            .show(ui, |ui| {
                ui.weak("Object");
                ui.weak("Privilege");
                if user {
                    ui.weak("grantable")
                        .on_hover_text("With grant option: may grant it on");
                } else {
                    ui.label("");
                }
                ui.label("");
                ui.end_row();
                for (key, grantable) in def.objects.iter_mut() {
                    ui.label(RichText::new(object_sql(key).to_lowercase()).monospace());
                    ui.label(key.2.to_lowercase());
                    if user {
                        ui.checkbox(grantable, "");
                    } else {
                        ui.label("");
                    }
                    if ui.small_button("×").on_hover_text("Revoke").clicked() {
                        remove = Some(key.clone());
                    }
                    ui.end_row();
                }
            });
    }
    if let Some(key) = remove {
        def.objects.remove(&key);
    }
    let id = egui::Id::new("account-new-object-privilege");
    let mut new: NewObjectPrivilege = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    ui.horizontal_wrapped(|ui| {
        ui.label("Add");
        ui.add(
            egui::TextEdit::singleline(&mut new.owner)
                .hint_text("owner")
                .desired_width(120.0),
        );
        if let Some(o) = picker(ui, "object-owner", &lists.owners) {
            new.owner = quote_ident(&o);
            new.object.clear();
        }
        ui.label(".");
        ui.add(
            egui::TextEdit::singleline(&mut new.object)
                .hint_text("object")
                .desired_width(180.0),
        );
        let owner = ident(&new.owner);
        if !owner.is_empty() {
            match lists.objects.get(&owner) {
                Some(objects) => {
                    if let Some(o) = picker(ui, "object-name", objects) {
                        new.object = quote_ident(&o);
                    }
                }
                None => lists.want_objects = Some(owner),
            }
        }
        egui::ComboBox::from_id_salt("object-privilege")
            .selected_text(if new.privilege.is_empty() {
                "privilege".to_owned()
            } else {
                new.privilege.to_lowercase()
            })
            .show_ui(ui, |ui| {
                for p in OBJECT_PRIVILEGES {
                    ui.selectable_value(&mut new.privilege, (*p).to_owned(), p.to_lowercase());
                }
            });
        if user {
            ui.checkbox(&mut new.grantable, "with grant option");
        }
        let complete = !new.owner.trim().is_empty()
            && !new.object.trim().is_empty()
            && !new.privilege.is_empty();
        if ui.add_enabled(complete, egui::Button::new("Add")).clicked() {
            def.objects.insert(
                (ident(&new.owner), ident(&new.object), new.privilege.clone()),
                new.grantable && user,
            );
            new = NewObjectPrivilege::default();
        }
    });
    ui.data_mut(|d| d.insert_temp(id, new));
}

pub fn ui(ui: &mut Ui, def: &mut AccountDef, original: Option<&AccountDef>, lists: &mut Lists) {
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
    if !def.role {
        ui.add_space(8.0);
        ui.label(RichText::new("Quotas").strong())
            .on_hover_text("Space the user's objects may take per tablespace");
        quotas_ui(ui, &mut def.quotas, &lists.tablespaces);
    }
    ui.add_space(8.0);
    let roles = lists.roles.clone();
    let privileges = lists.privileges.clone();
    ui.columns(2, |columns| {
        roles_ui(&mut columns[0], def, &roles);
        privileges_ui(&mut columns[1], &mut def.privileges, &privileges);
    });
    ui.add_space(8.0);
    objects_ui(ui, def, lists);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quota(ts: &str, size: &str) -> (String, String) {
        (ts.into(), size.into())
    }

    #[test]
    fn creates_users() {
        let mut def = AccountDef::new_user();
        def.name = "app_owner".into();
        assert_eq!(statements(None, &def), Err("Enter a password".into()));
        def.password = "Secret_1".into();
        def.default_tablespace = "users".into();
        def.temporary_tablespace = "TEMP".into();
        def.quotas = vec![quota("users", "unlimited"), quota("DATA", "100m")];
        def.expire = true;
        def.roles.insert("CONNECT".into(), RoleGrant::default());
        def.roles.insert(
            "APP_ADMIN".into(),
            RoleGrant {
                admin: true,
                default: false,
            },
        );
        def.objects
            .insert(("HR".into(), "EMP".into(), "SELECT".into()), false);
        def.objects
            .insert(("HR".into(), "EMP".into(), "UPDATE".into()), true);
        def.objects
            .insert((String::new(), "EXPORTS".into(), "READ".into()), false);
        let s = statements(None, &def).unwrap();
        assert_eq!(
            s,
            [
                "create user APP_OWNER\n  identified by \"Secret_1\"\n  default tablespace USERS\n  \
                 temporary tablespace TEMP\n  quota unlimited on USERS\n  quota 100M on DATA\n  \
                 password expire",
                "grant create session to APP_OWNER",
                "grant CONNECT to APP_OWNER",
                "grant APP_ADMIN to APP_OWNER with admin option",
                "grant select on HR.EMP to APP_OWNER",
                "grant update on HR.EMP to APP_OWNER with grant option",
                "grant read on directory EXPORTS to APP_OWNER",
                "alter user APP_OWNER default role CONNECT",
            ]
        );
        assert!(masked(&s, &def)[0].contains("identified by \"********\""));
        def.quotas.push(quota("users", "1G"));
        assert!(
            statements(None, &def).is_err(),
            "two quotas on one tablespace"
        );
        def.quotas.pop();
        def.quotas[0].1 = "lots".into();
        assert!(statements(None, &def).is_err());
    }

    #[test]
    fn alters_users() {
        let original = AccountDef {
            name: "APP".into(),
            default_tablespace: "USERS".into(),
            temporary_tablespace: "TEMP".into(),
            quotas: vec![quota("USERS", "100M"), quota("DATA", "1G")],
            profile: "DEFAULT".into(),
            privileges: [
                ("CREATE SESSION".to_owned(), false),
                ("CREATE TABLE".to_owned(), true),
            ]
            .into(),
            roles: [("CONNECT".to_owned(), RoleGrant::default())].into(),
            objects: [(
                ("HR".to_owned(), "EMP".to_owned(), "SELECT".to_owned()),
                true,
            )]
            .into(),
            ..AccountDef::default()
        };
        let mut def = original.clone();
        assert_eq!(statements(Some(&original), &def), Ok(Vec::new()));
        def.locked = true;
        def.quotas = vec![quota("users", "unlimited")];
        def.privileges.insert("CREATE TABLE".into(), false);
        def.privileges.insert("CREATE VIEW".into(), true);
        def.roles.get_mut("CONNECT").unwrap().default = false;
        def.objects
            .insert(("HR".into(), "EMP".into(), "SELECT".into()), false);
        assert_eq!(
            statements(Some(&original), &def).unwrap(),
            [
                "alter user APP\n  quota unlimited on USERS\n  quota 0 on DATA\n  account lock",
                // Taking the admin option away: revoke and grant again.
                "revoke create table from APP",
                "grant create table to APP",
                "grant create view to APP with admin option",
                "revoke select on HR.EMP from APP",
                "grant select on HR.EMP to APP",
                "alter user APP default role none",
            ]
        );
        def = original.clone();
        def.password = "new".into();
        assert_eq!(
            statements(Some(&original), &def).unwrap(),
            ["alter user APP\n  identified by \"new\""]
        );
        // A role granted later: all default, said explicitly.
        def = original.clone();
        def.roles.insert("RESOURCE".into(), RoleGrant::default());
        assert_eq!(
            statements(Some(&original), &def).unwrap(),
            ["grant RESOURCE to APP", "alter user APP default role all"]
        );
    }

    #[test]
    fn roles() {
        let mut def = AccountDef::new_role();
        def.name = "app_reader".into();
        def.privileges.insert("CREATE SESSION".into(), false);
        def.roles.insert("APP_READER".into(), RoleGrant::default());
        def.objects
            .insert(("HR".into(), "EMP".into(), "SELECT".into()), false);
        assert_eq!(
            statements(None, &def).unwrap(),
            [
                "create role APP_READER\n  not identified",
                "grant create session to APP_READER",
                "grant select on HR.EMP to APP_READER",
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
        def.objects
            .insert(("HR".into(), "EMP".into(), "SELECT".into()), true);
        assert!(statements(Some(&original), &def).is_err(), "grant option");
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
