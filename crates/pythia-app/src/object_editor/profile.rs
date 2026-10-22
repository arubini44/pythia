//! Profile: password rules and resource limits. Each limit is DEFAULT
//! (whatever the DEFAULT profile says), UNLIMITED or a value; a change
//! alters the limits that changed.

use egui::{RichText, Ui};
use pythia_db::DbError;
use pythia_db::oracle::Connection;

use super::{Lists, ident, name_field, names, picker};
use crate::documents::quote_ident;

/// One limit of a profile.
#[derive(Debug, Clone, PartialEq)]
pub struct Limit {
    /// `RESOURCE_NAME`, e.g. `FAILED_LOGIN_ATTEMPTS`.
    pub resource: String,
    /// A password limit (else a resource limit).
    pub password: bool,
    /// As written: `default`, `unlimited`, `10`, a function's name.
    pub value: String,
    /// The DEFAULT profile's value, which `default` stands for.
    pub inherited: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProfileDef {
    pub name: String,
    pub limits: Vec<Limit>,
    /// Users with the profile (shown, not edited).
    pub users: Vec<String>,
}

/// The limits in display order: unit and what they mean.
const RESOURCES: &[(&str, &str, &str)] = &[
    (
        "FAILED_LOGIN_ATTEMPTS",
        "attempts",
        "Failed logins in a row that lock the account",
    ),
    (
        "PASSWORD_LOCK_TIME",
        "days",
        "How long failed logins lock the account",
    ),
    ("PASSWORD_LIFE_TIME", "days", "How long a password works"),
    (
        "PASSWORD_GRACE_TIME",
        "days",
        "Logins with a warning once the password expired",
    ),
    (
        "PASSWORD_REUSE_TIME",
        "days",
        "Before a password can be used again",
    ),
    (
        "PASSWORD_REUSE_MAX",
        "changes",
        "Password changes before one can be used again",
    ),
    (
        "PASSWORD_VERIFY_FUNCTION",
        "function",
        "Checks new passwords; a function owned by SYS, or null for none",
    ),
    (
        "INACTIVE_ACCOUNT_TIME",
        "days",
        "Days without a login that lock the account (12.2 and later)",
    ),
    (
        "PASSWORD_ROLLOVER_TIME",
        "days",
        "After a change, how long the old password still works too (19.12 and later)",
    ),
    ("SESSIONS_PER_USER", "sessions", "Concurrent sessions"),
    ("IDLE_TIME", "minutes", "Idle time that ends a session"),
    ("CONNECT_TIME", "minutes", "How long a session lasts"),
    ("CPU_PER_SESSION", "1/100 s", "CPU time of a session"),
    (
        "CPU_PER_CALL",
        "1/100 s",
        "CPU time of a call (parse, execute or fetch)",
    ),
    (
        "LOGICAL_READS_PER_SESSION",
        "blocks",
        "Blocks a session reads",
    ),
    ("LOGICAL_READS_PER_CALL", "blocks", "Blocks a call reads"),
    (
        "PRIVATE_SGA",
        "bytes",
        "Private SGA space of a shared server session; K or M for kilo- or megabytes",
    ),
    (
        "COMPOSITE_LIMIT",
        "units",
        "Weighted total of CPU time, connect time, reads and private SGA",
    ),
];

/// Resources whose values are days (fractions allowed: `1/24` is an hour).
const DAYS: &[&str] = &[
    "PASSWORD_LOCK_TIME",
    "PASSWORD_LIFE_TIME",
    "PASSWORD_GRACE_TIME",
    "PASSWORD_REUSE_TIME",
    "INACTIVE_ACCOUNT_TIME",
    "PASSWORD_ROLLOVER_TIME",
];

const VERIFY_FUNCTION: &str = "PASSWORD_VERIFY_FUNCTION";

fn position(resource: &str) -> usize {
    RESOURCES
        .iter()
        .position(|(r, ..)| *r == resource)
        .unwrap_or(RESOURCES.len())
}

/// A limit as the dictionary has it, as written in the form.
fn value_text(limit: &str) -> String {
    match limit {
        "DEFAULT" | "UNLIMITED" | "NULL" => limit.to_lowercase(),
        l if l.starts_with('.') => format!("0{l}"),
        l => l.to_owned(),
    }
}

fn limits(rows: Vec<(String, String, String)>, inherited: &[(String, String)]) -> Vec<Limit> {
    let mut out: Vec<Limit> = rows
        .into_iter()
        .map(|(resource, kind, value)| Limit {
            inherited: inherited
                .iter()
                .find(|(r, _)| *r == resource)
                .map(|(_, v)| value_text(v))
                .unwrap_or_default(),
            password: kind == "PASSWORD",
            value: value_text(&value),
            resource,
        })
        .collect();
    out.sort_by(|a, b| {
        (!a.password, position(&a.resource), &a.resource).cmp(&(
            !b.password,
            position(&b.resource),
            &b.resource,
        ))
    });
    out
}

fn rows(conn: &Connection, profile: &str) -> Result<Vec<(String, String, String)>, DbError> {
    let mut out = Vec::new();
    for row in conn.query_as::<(String, String, String)>(
        "select resource_name, resource_type, limit from dba_profiles where profile = :1",
        &[&profile],
    )? {
        out.push(row?);
    }
    Ok(out)
}

fn inherited(rows: &[(String, String, String)]) -> Vec<(String, String)> {
    rows.iter()
        .map(|(r, _, v)| (r.clone(), v.clone()))
        .collect()
}

pub fn load(conn: &Connection, name: &str) -> Result<ProfileDef, DbError> {
    let own = rows(conn, name)?;
    if own.is_empty() {
        return Err(DbError::new(format!("Profile {name} not found")));
    }
    let defaults = rows(conn, "DEFAULT")?;
    Ok(ProfileDef {
        name: name.to_owned(),
        limits: limits(own, &inherited(&defaults)),
        users: names(
            conn,
            "select username from dba_users where profile = :1 order by username",
            &[&name],
        )?,
    })
}

/// A new profile: every limit DEFAULT. Without access to DBA_PROFILES, the
/// limits every release has.
pub fn new_def(conn: &Connection) -> ProfileDef {
    let defaults = rows(conn, "DEFAULT").unwrap_or_default();
    let inherited = inherited(&defaults);
    let mut def = ProfileDef {
        limits: limits(defaults, &inherited),
        ..Default::default()
    };
    if def.limits.is_empty() {
        def.limits = RESOURCES
            .iter()
            .filter(|(r, ..)| !matches!(*r, "INACTIVE_ACCOUNT_TIME" | "PASSWORD_ROLLOVER_TIME"))
            .map(|(r, ..)| Limit {
                resource: (*r).to_owned(),
                password: position(r) < position("SESSIONS_PER_USER"),
                value: String::new(),
                inherited: String::new(),
            })
            .collect();
    }
    for limit in &mut def.limits {
        limit.value = "default".into();
    }
    def
}

/// Password verify functions and the session's privileges.
pub fn lists(conn: &Connection, lists: &mut super::BaseLists) -> Result<(), DbError> {
    lists.verify_functions = names(
        conn,
        "select object_name from dba_objects
          where owner = 'SYS' and object_type = 'FUNCTION' and object_name like '%VERIFY%'
          order by object_name",
        &[],
    )
    .unwrap_or_default();
    lists.session_privileges = names(conn, "select privilege from session_privs", &[])?;
    Ok(())
}

/// A limit's value as SQL.
fn value_sql(limit: &Limit) -> Result<String, String> {
    let value = limit.value.trim();
    let label = limit.resource.to_lowercase().replace('_', " ");
    let lower = value.to_lowercase();
    if lower == "default" {
        return Ok(lower);
    }
    if limit.resource == VERIFY_FUNCTION {
        if lower == "null" {
            return Ok(lower);
        }
        let plain = value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_$#".contains(c))
            && value
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic());
        let quoted = value.len() > 2 && value.starts_with('"') && value.ends_with('"');
        if lower == "unlimited" || !plain && !quoted {
            return Err(format!("{label}: a function's name, null or default"));
        }
        return Ok(quote_ident(&ident(value)));
    }
    if lower == "unlimited" {
        return Ok(lower);
    }
    let digits = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit());
    let valid = if DAYS.contains(&limit.resource.as_str()) {
        // Days, fractions too: 0.5, 1/24.
        value.starts_with(|c: char| c.is_ascii_digit() || c == '.' || c == '(')
            && value
                .chars()
                .all(|c| c.is_ascii_digit() || " ./*()".contains(c))
    } else if limit.resource == "PRIVATE_SGA" {
        digits(value.trim_end_matches(['K', 'M', 'G', 'T', 'k', 'm', 'g', 't']))
    } else {
        digits(value)
    };
    if !valid {
        let what = if DAYS.contains(&limit.resource.as_str()) {
            "days (like 7 or 1/24)"
        } else if limit.resource == "PRIVATE_SGA" {
            "a size (like 64K or 1M)"
        } else {
            "a whole number"
        };
        return Err(format!("{label}: default, unlimited or {what}"));
    }
    Ok(value.to_uppercase())
}

fn same(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

pub fn statements(original: Option<&ProfileDef>, def: &ProfileDef) -> Result<Vec<String>, String> {
    if def.name.trim().is_empty() {
        return Err("Enter a name".into());
    }
    let default_profile = ident(&def.name) == "DEFAULT";
    let mut clauses = Vec::new();
    for limit in &def.limits {
        let value = value_sql(limit)?;
        if default_profile && value == "default" {
            return Err("The DEFAULT profile's limits can't be DEFAULT".into());
        }
        let unchanged = match original {
            Some(o) => o
                .limits
                .iter()
                .find(|l| l.resource == limit.resource)
                .is_some_and(|l| same(&l.value, &limit.value)),
            None => value == "default",
        };
        if !unchanged {
            clauses.push(format!("{} {value}", limit.resource.to_lowercase()));
        }
    }
    let name = quote_ident(&ident(&def.name));
    match original {
        Some(_) if clauses.is_empty() => Ok(Vec::new()),
        Some(_) => Ok(vec![format!(
            "alter profile {name} limit\n  {}",
            clauses.join("\n  ")
        )]),
        None => {
            // CREATE PROFILE needs at least one limit.
            if clauses.is_empty() {
                clauses.push("sessions_per_user default".into());
            }
            Ok(vec![format!(
                "create profile {name} limit\n  {}",
                clauses.join("\n  ")
            )])
        }
    }
}

/// What the session can't do that the form needs.
pub fn missing_privilege(existing: bool, lists: &Lists) -> Option<String> {
    if lists.session_privileges.is_empty() {
        return None;
    }
    let needed = if existing {
        "ALTER PROFILE"
    } else {
        "CREATE PROFILE"
    };
    (!lists.session_privileges.iter().any(|p| p == needed))
        .then(|| format!("This session doesn't have {needed}."))
}

/// One limit's row: name, value with a picker, unit and what DEFAULT
/// stands for.
fn limit_row(ui: &mut Ui, limit: &mut Limit, choices: &[String], verify: &[String]) {
    let (unit, help) = RESOURCES
        .iter()
        .find(|(r, ..)| *r == limit.resource)
        .map_or(("", ""), |(_, unit, help)| (*unit, *help));
    let label = ui.label(limit.resource.to_lowercase().replace('_', " "));
    if !help.is_empty() {
        label.on_hover_text(help);
    }
    let function = limit.resource == VERIFY_FUNCTION;
    ui.horizontal(|ui| {
        let height = ui.spacing().interact_size.y;
        ui.add_sized(
            [if function { 240.0 } else { 120.0 }, height],
            egui::TextEdit::singleline(&mut limit.value).font(egui::TextStyle::Monospace),
        );
        let items = if function { verify } else { choices };
        if let Some(v) = picker(ui, ("limit", &limit.resource), items) {
            limit.value = v;
        }
    });
    let mut note = unit.to_owned();
    if !limit.inherited.is_empty() && same(&limit.value, "default") {
        note = format!("{note}  (default: {})", limit.inherited);
    }
    ui.weak(note);
    ui.end_row();
}

pub fn ui(ui: &mut Ui, def: &mut ProfileDef, existing: bool, lists: &Lists) {
    if let Some(missing) = missing_privilege(existing, lists) {
        ui.colored_label(ui.visuals().warn_fg_color, missing);
        ui.add_space(4.0);
    }
    let default_profile = existing && def.name == "DEFAULT";
    let mut choices: Vec<String> = Vec::new();
    if !default_profile {
        choices.push("default".into());
    }
    choices.push("unlimited".into());
    let mut verify = choices[..choices.len() - 1].to_vec();
    verify.push("null".into());
    verify.extend(lists.verify_functions.iter().cloned());
    egui::Grid::new("profile")
        .num_columns(3)
        .spacing([16.0, 6.0])
        .min_col_width(110.0)
        .show(ui, |ui| {
            name_field(ui, &mut def.name, existing);
            for (password, heading) in [(true, "Password"), (false, "Resources")] {
                ui.vertical(|ui| {
                    ui.add_space(8.0);
                    ui.label(RichText::new(heading).strong());
                });
                ui.end_row();
                for limit in def.limits.iter_mut().filter(|l| l.password == password) {
                    limit_row(ui, limit, &choices, &verify);
                }
            }
        });
    ui.add_space(6.0);
    if existing {
        ui.label(match def.users.len() {
            0 => "No user has this profile.".to_owned(),
            n if n <= 8 => format!("Users: {}", def.users.join(", ").to_lowercase()),
            n => format!(
                "Users: {}, … ({n} in all)",
                def.users[..8].join(", ").to_lowercase()
            ),
        });
    }
    ui.weak(
        "Resource limits apply while the RESOURCE_LIMIT parameter is TRUE (the default since 12.1). \
         Default means the DEFAULT profile's value.",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limit(resource: &str, password: bool, value: &str) -> Limit {
        Limit {
            resource: resource.into(),
            password,
            value: value.into(),
            inherited: String::new(),
        }
    }

    fn def(name: &str, values: &[(&str, &str)]) -> ProfileDef {
        ProfileDef {
            name: name.into(),
            limits: values
                .iter()
                .map(|(r, v)| limit(r, !r.starts_with("SESSIONS"), v))
                .collect(),
            users: Vec::new(),
        }
    }

    #[test]
    fn creates_profiles() {
        let new = def(
            "app_users",
            &[
                ("FAILED_LOGIN_ATTEMPTS", "5"),
                ("PASSWORD_LOCK_TIME", "1/24"),
                ("PASSWORD_LIFE_TIME", "Unlimited"),
                ("PASSWORD_VERIFY_FUNCTION", "ora12c_verify_function"),
                ("SESSIONS_PER_USER", "default"),
            ],
        );
        assert_eq!(
            statements(None, &new).unwrap(),
            [
                "create profile APP_USERS limit\n  failed_login_attempts 5\n  \
              password_lock_time 1/24\n  password_life_time unlimited\n  \
              password_verify_function ORA12C_VERIFY_FUNCTION"
            ]
        );
        // Everything DEFAULT: CREATE PROFILE still needs a limit.
        let plain = def("plain", &[("SESSIONS_PER_USER", "default")]);
        assert_eq!(
            statements(None, &plain).unwrap(),
            ["create profile PLAIN limit\n  sessions_per_user default"]
        );
    }

    #[test]
    fn alters_changed_limits() {
        let original = def(
            "APP_USERS",
            &[
                ("FAILED_LOGIN_ATTEMPTS", "5"),
                ("PASSWORD_VERIFY_FUNCTION", "null"),
                ("SESSIONS_PER_USER", "default"),
            ],
        );
        assert!(statements(Some(&original), &original).unwrap().is_empty());
        let mut edited = original.clone();
        edited.limits[0].value = "default".into();
        edited.limits[1].value = "NULL".into();
        edited.limits[2].value = "3".into();
        assert_eq!(
            statements(Some(&original), &edited).unwrap(),
            [
                "alter profile APP_USERS limit\n  failed_login_attempts default\n  sessions_per_user 3"
            ]
        );
    }

    #[test]
    fn checks_values() {
        let bad = |resource: &str, value: &str| {
            statements(None, &def("p", &[(resource, value)])).unwrap_err()
        };
        assert_eq!(
            bad("SESSIONS_PER_USER", "1.5"),
            "sessions per user: default, unlimited or a whole number"
        );
        assert_eq!(
            bad("PASSWORD_LIFE_TIME", "a week"),
            "password life time: default, unlimited or days (like 7 or 1/24)"
        );
        assert_eq!(
            bad("PASSWORD_VERIFY_FUNCTION", "unlimited"),
            "password verify function: a function's name, null or default"
        );
        assert_eq!(
            bad("PRIVATE_SGA", "lots"),
            "private sga: default, unlimited or a size (like 64K or 1M)"
        );
        let sga = def("p", &[("PRIVATE_SGA", "64k")]);
        assert_eq!(
            statements(None, &sga).unwrap(),
            ["create profile P limit\n  private_sga 64K"]
        );
        let default = def("DEFAULT", &[("SESSIONS_PER_USER", "default")]);
        assert_eq!(
            statements(
                Some(&def("DEFAULT", &[("SESSIONS_PER_USER", "1")])),
                &default
            )
            .unwrap_err(),
            "The DEFAULT profile's limits can't be DEFAULT"
        );
        assert_eq!(
            statements(None, &def(" ", &[])).unwrap_err(),
            "Enter a name"
        );
    }

    #[test]
    fn orders_dictionary_limits() {
        let rows = vec![
            ("SESSIONS_PER_USER".into(), "KERNEL".into(), "10".into()),
            (
                "PASSWORD_LOCK_TIME".into(),
                "PASSWORD".into(),
                ".0104".into(),
            ),
            (
                "FAILED_LOGIN_ATTEMPTS".into(),
                "PASSWORD".into(),
                "DEFAULT".into(),
            ),
            (
                "PASSWORD_VERIFY_FUNCTION".into(),
                "PASSWORD".into(),
                "NULL".into(),
            ),
        ];
        let inherited = vec![("FAILED_LOGIN_ATTEMPTS".to_owned(), "10".to_owned())];
        let limits = limits(rows, &inherited);
        let shown: Vec<(&str, bool, &str, &str)> = limits
            .iter()
            .map(|l| {
                (
                    l.resource.as_str(),
                    l.password,
                    l.value.as_str(),
                    l.inherited.as_str(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("FAILED_LOGIN_ATTEMPTS", true, "default", "10"),
                ("PASSWORD_LOCK_TIME", true, "0.0104", ""),
                ("PASSWORD_VERIFY_FUNCTION", true, "null", ""),
                ("SESSIONS_PER_USER", false, "10", ""),
            ]
        );
    }
}
