//! Database objects as files: one file per object in a folder per type
//! (`packages/billing.pks`), written by "Export schema to folder" and read
//! back to compare a file with the database.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use pythia_db::oracle::Connection;
use pythia_db::{DbError, ObjectType};

/// Folder and extension of each exported type.
const LAYOUT: &[(ObjectType, &str, &str)] = &[
    (ObjectType::Table, "tables", "sql"),
    (ObjectType::View, "views", "vw"),
    (ObjectType::MaterializedView, "materialized_views", "sql"),
    (ObjectType::Sequence, "sequences", "sql"),
    (ObjectType::Synonym, "synonyms", "sql"),
    (ObjectType::Type, "types", "tps"),
    (ObjectType::TypeBody, "types", "tpb"),
    (ObjectType::Package, "packages", "pks"),
    (ObjectType::PackageBody, "packages", "pkb"),
    (ObjectType::Procedure, "procedures", "prc"),
    (ObjectType::Function, "functions", "fnc"),
    (ObjectType::Trigger, "triggers", "trg"),
];

/// Where an object goes, relative to the export folder.
pub fn relative_path(object_type: ObjectType, name: &str) -> Option<PathBuf> {
    let (_, dir, ext) = LAYOUT.iter().find(|(t, ..)| *t == object_type)?;
    Some(Path::new(dir).join(format!("{}.{ext}", name.to_lowercase())))
}

/// The object a file holds: its name and type from the `CREATE` statement,
/// else from the file name and extension (`billing.pkb`).
pub fn object_of_file(path: &Path, text: &str) -> Option<(Option<String>, String, ObjectType)> {
    if let Some(header) = pythia_plsql::unit_header(text)
        && let Some(t) = ObjectType::from_dictionary(&header.object_type)
    {
        return Some((header.owner, header.name, t));
    }
    let stem = path.file_stem()?.to_str()?.to_uppercase();
    let ext = path.extension()?.to_str()?.to_lowercase();
    let dir = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|d| d.to_str())
        .unwrap_or_default()
        .to_lowercase();
    // `.sql` is shared by several types; the folder decides.
    let t = LAYOUT
        .iter()
        .find(|(_, d, e)| *e == ext && (ext != "sql" || *d == dir))
        .map(|(t, ..)| *t)?;
    Some((None, stem, t))
}

/// The file contents for an object: runnable DDL ending with `/` for
/// stored units, without the schema name.
pub fn object_text(
    conn: &Connection,
    owner: &str,
    name: &str,
    object_type: ObjectType,
) -> Result<String, DbError> {
    if object_type.has_source() {
        let source = pythia_db::object_source(conn, owner, name, object_type)?;
        Ok(format!("{}\n/\n", source.trim_end()))
    } else {
        let ddl = pythia_db::object_ddl(conn, owner, name, object_type, false)?;
        Ok(format!("{}\n", ddl.trim_end()))
    }
}

#[derive(Default)]
pub struct ExportProgress {
    pub done: AtomicUsize,
    pub total: AtomicUsize,
    pub stop: AtomicBool,
}

/// Writes every object of `owner` into `dir`. Returns the number of files.
pub fn export_schema(
    conn: &Connection,
    owner: &str,
    dir: &Path,
    progress: &ExportProgress,
) -> Result<usize, DbError> {
    let types: Vec<String> = LAYOUT
        .iter()
        .map(|(t, ..)| format!("'{}'", t.dictionary_name()))
        .collect();
    let sql = format!(
        "select object_name, object_type from all_objects
          where owner = :1 and object_type in ({})
            and generated = 'N' and object_name not like 'BIN$%'
            and not (object_type = 'TABLE' and (object_name like 'SYS\\_%' escape '\\'
                     or object_name in (select mview_name from all_mviews where owner = :1)))
          order by object_type, object_name",
        types.join(", ")
    );
    let objects: Vec<(String, String)> = conn
        .query_as::<(String, String)>(&sql, &[&owner])?
        .collect::<Result<_, _>>()?;
    progress.total.store(objects.len(), Ordering::Relaxed);
    let mut written = 0;
    for (name, type_name) in objects {
        if progress.stop.load(Ordering::Relaxed) {
            break;
        }
        progress.done.fetch_add(1, Ordering::Relaxed);
        let Some(t) = ObjectType::from_dictionary(&type_name) else {
            continue;
        };
        let Some(relative) = relative_path(t, &name) else {
            continue;
        };
        let text = match object_text(conn, owner, &name, t) {
            Ok(text) => text,
            // Dropped since the list was read.
            Err(e) if e.code == Some(31603) || e.message.starts_with("No source found") => {
                continue;
            }
            Err(e) => return Err(e),
        };
        let path = dir.join(relative);
        let io = |e: std::io::Error| DbError::new(format!("{}: {e}", path.display()));
        std::fs::create_dir_all(path.parent().unwrap_or(dir)).map_err(io)?;
        std::fs::write(&path, text).map_err(io)?;
        written += 1;
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_files_to_objects() {
        assert_eq!(
            relative_path(ObjectType::PackageBody, "BILLING"),
            Some(PathBuf::from("packages/billing.pkb"))
        );
        let found = |p: &str, text: &str| object_of_file(Path::new(p), text);
        assert_eq!(
            found("x/packages/billing.pkb", ""),
            Some((None, "BILLING".into(), ObjectType::PackageBody))
        );
        assert_eq!(
            found("x/tables/orders.sql", "create table orders (id number);"),
            Some((None, "ORDERS".into(), ObjectType::Table))
        );
        assert_eq!(
            found(
                "anything.sql",
                "create or replace package body hr.pay as\nend;\n/"
            ),
            Some((Some("HR".into()), "PAY".into(), ObjectType::PackageBody))
        );
        assert_eq!(found("notes.txt", "hello"), None);
    }

    #[test]
    fn exports_a_schema() {
        let Ok(spec) = std::env::var("PYTHIA_TEST_DB") else {
            return;
        };
        let (cred, db) = spec.rsplit_once('@').unwrap();
        let (user, pw) = cred.split_once('/').unwrap();
        let conn = Connection::connect(user, pw, db).unwrap();
        conn.execute(
            "create or replace procedure pythia_export_p is begin null; end;",
            &[],
        )
        .unwrap();
        let owner = conn
            .query_row_as::<String>("select user from dual", &[])
            .unwrap();
        let dir = std::env::temp_dir().join(format!("pythia-export-{}", std::process::id()));
        let progress = ExportProgress::default();
        let n = export_schema(&conn, &owner, &dir, &progress).unwrap();
        assert!(n > 0);
        assert_eq!(
            progress.done.load(Ordering::Relaxed),
            progress.total.load(Ordering::Relaxed)
        );
        let text = std::fs::read_to_string(dir.join("procedures/pythia_export_p.prc")).unwrap();
        assert!(
            text.starts_with("CREATE OR REPLACE procedure pythia_export_p is"),
            "{text}"
        );
        assert!(text.ends_with("\n/\n"));
        // Reading it back finds the object and the same text.
        let (_, name, t) =
            object_of_file(&dir.join("procedures/pythia_export_p.prc"), &text).unwrap();
        assert_eq!(object_text(&conn, &owner, &name, t).unwrap(), text);
        conn.execute("drop procedure pythia_export_p", &[]).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
