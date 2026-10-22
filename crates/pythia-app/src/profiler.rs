//! PL/SQL hierarchical profiler (DBMS_HPROF): runs a block with profiling
//! and reads where the time went, per subprogram and SQL statement.

use egui::{RichText, Ui};
use egui_extras::{Column as TableColumn, TableBuilder};
use pythia_db::oracle::Connection;
use pythia_db::oracle::sql_type::OracleType;
use pythia_db::{DbError, ObjectType};

use crate::documents::ObjectRef;

#[derive(Debug, Clone, Default)]
pub struct Profile {
    pub run_id: i64,
    /// Elapsed time of the profiled run in microseconds.
    pub total_us: i64,
    pub rows: Vec<ProfileRow>,
    /// The block failed; the profile covers the time until the error.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ProfileRow {
    pub owner: Option<String>,
    pub module: Option<String>,
    pub module_type: Option<String>,
    pub function: String,
    pub line: i64,
    pub sql: bool,
    pub calls: i64,
    pub subtree_us: i64,
    pub self_us: i64,
    pub sql_text: Option<String>,
}

impl ProfileRow {
    /// `owner.module.function`, or the SQL text for statements.
    pub fn title(&self) -> String {
        if self.sql
            && let Some(text) = &self.sql_text
        {
            return text.split_whitespace().collect::<Vec<_>>().join(" ");
        }
        match (&self.owner, &self.module) {
            (Some(o), Some(m)) => format!("{o}.{m}.{}", self.function),
            _ => format!("anonymous block · {}", self.function),
        }
    }

    /// The stored unit to open at `line`, if any.
    pub fn object(&self) -> Option<ObjectRef> {
        let t = ObjectType::from_dictionary(self.module_type.as_deref()?)?;
        if !t.has_source() {
            return None;
        }
        Some(ObjectRef {
            owner: self.owner.clone()?,
            name: self.module.clone()?,
            object_type: t,
        })
    }
}

/// What the user lacks, explained.
pub fn explain(e: &DbError) -> String {
    if e.message.contains("DBMS_HPROF") && (e.message.contains("PLS-00201") || e.code == Some(6550))
    {
        format!(
            "{}\nThe profiler needs EXECUTE on DBMS_HPROF. Ask a DBA to run:\n  grant execute on sys.dbms_hprof to <user>;",
            e.message
        )
    } else {
        e.message.clone()
    }
}

/// Profiles a PL/SQL block. The profiler commits: DBMS_HPROF writes its
/// trace data in the session's transaction.
pub fn run(conn: &Connection, block: &str) -> Result<Profile, DbError> {
    // Profiling into tables, without a directory object, came with 18c.
    let (major, minor) = pythia_db::compat::release(conn)?;
    if major < 18 {
        return Err(DbError::new(format!(
            "The profiler needs Oracle 18c or later; this database is {major}.{minor}."
        )));
    }
    let tables: i64 = conn.query_row_as(
        "select count(*) from user_tables where table_name = 'DBMSHP_RUNS'",
        &[],
    )?;
    if tables == 0 {
        conn.execute(
            "begin dbms_hprof.create_tables(force_it => false); end;",
            &[],
        )?;
    }
    let block = block.trim().trim_end_matches('/').trim_end();
    let wrapper = format!(
        "declare
  l_trace number;
  l_error varchar2(4000);
begin
  l_trace := dbms_hprof.start_profiling;
  begin
{block}
  exception
    when others then
      l_error := substr(dbms_utility.format_error_stack, 1, 4000);
  end;
  dbms_hprof.stop_profiling;
  :1 := dbms_hprof.analyze(trace_id => l_trace, run_comment => 'pythia');
  :2 := l_error;
end;"
    );
    let mut stmt = conn.statement(&wrapper).build()?;
    stmt.execute(&[&OracleType::Number(0, 0), &OracleType::Varchar2(4000)])?;
    let run_id: i64 = stmt.bind_value(1)?;
    let error: Option<String> = stmt.bind_value(2)?;
    let total_us: i64 = conn.query_row_as(
        "select nvl(total_elapsed_time, 0) from dbmshp_runs where runid = :1",
        &[&run_id],
    )?;
    let mut rows = Vec::new();
    for row in conn.query(
        "select owner, module, type, function, line#, namespace, calls,
                subtree_elapsed_time, function_elapsed_time, substr(sql_text, 1, 400)
           from dbmshp_function_info
          where runid = :1
            and not (nvl(owner, '-') = 'SYS' and nvl(module, '-') = 'DBMS_HPROF')
          order by function_elapsed_time desc, subtree_elapsed_time desc",
        &[&run_id],
    )? {
        let row = row?;
        rows.push(ProfileRow {
            owner: row.get(0)?,
            module: row.get(1)?,
            module_type: row.get(2)?,
            function: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            line: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
            sql: row.get::<_, Option<String>>(5)?.as_deref() == Some("SQL"),
            calls: row.get::<_, Option<i64>>(6)?.unwrap_or(0),
            subtree_us: row.get::<_, Option<i64>>(7)?.unwrap_or(0),
            self_us: row.get::<_, Option<i64>>(8)?.unwrap_or(0),
            sql_text: row.get(9)?,
        });
    }
    Ok(Profile {
        run_id,
        total_us,
        rows,
        error,
    })
}

fn ms(us: i64) -> String {
    let ms = us as f64 / 1000.0;
    if ms >= 100.0 {
        format!("{ms:.0} ms")
    } else {
        format!("{ms:.2} ms")
    }
}

/// The profile as a table; returns a unit to open (with the line).
pub fn ui(ui: &mut Ui, profile: &Profile) -> Option<(ObjectRef, usize)> {
    let mut open = None;
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("Total {}", ms(profile.total_us))).strong());
        ui.weak(format!(
            "· {} entries · run {}",
            profile.rows.len(),
            profile.run_id
        ));
        ui.weak("· double-click a subprogram to open it");
    });
    if let Some(e) = &profile.error {
        ui.colored_label(
            ui.visuals().error_fg_color,
            format!("The block failed: {}", e.trim()),
        );
    }
    let total = profile.total_us.max(1) as f32;
    let bar_color = ui.visuals().hyperlink_color;
    let row_height = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
    TableBuilder::new(ui)
        .id_salt("profile")
        .striped(true)
        .resizable(true)
        .auto_shrink(false)
        .sense(egui::Sense::click())
        .column(TableColumn::initial(160.0).at_least(80.0))
        .column(TableColumn::initial(90.0))
        .column(TableColumn::initial(60.0))
        .column(TableColumn::initial(60.0))
        .column(TableColumn::remainder().at_least(160.0).clip(true))
        .header(row_height, |mut h| {
            for title in [
                "Self time",
                "Total",
                "Calls",
                "Line",
                "Subprogram / statement",
            ] {
                h.col(|ui| {
                    ui.strong(title);
                });
            }
        })
        .body(|body| {
            body.rows(row_height, profile.rows.len(), |mut row| {
                let r = &profile.rows[row.index()];
                row.col(|ui| {
                    // A slim bar under the number for the share of the total.
                    let rect = ui.max_rect();
                    let share = (r.self_us as f32 / total).clamp(0.0, 1.0);
                    let bar = egui::Rect::from_min_size(
                        egui::pos2(rect.left(), rect.bottom() - 4.0),
                        egui::vec2((rect.width() - 8.0) * share, 3.0),
                    );
                    ui.painter().rect_filled(bar, 1.5, bar_color);
                    ui.label(format!("{} · {:.0}%", ms(r.self_us), share * 100.0));
                });
                row.col(|ui| {
                    ui.label(ms(r.subtree_us));
                });
                row.col(|ui| {
                    ui.label(r.calls.to_string());
                });
                row.col(|ui| {
                    ui.label(r.line.to_string());
                });
                row.col(|ui| {
                    let text = if r.sql {
                        RichText::new(r.title()).monospace()
                    } else {
                        RichText::new(r.title()).monospace().strong()
                    };
                    ui.add(egui::Label::new(text).truncate())
                        .on_hover_text(r.sql_text.as_deref().unwrap_or(&r.title()));
                });
                if row.response().double_clicked()
                    && let Some(object) = r.object()
                {
                    open = Some((object, r.line.max(1) as usize));
                }
            });
        });
    open
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_a_block() {
        let Ok(spec) = std::env::var("PYTHIA_TEST_DB") else {
            return;
        };
        let (cred, db) = spec.rsplit_once('@').unwrap();
        let (user, pw) = cred.split_once('/').unwrap();
        let conn = Connection::connect(user, pw, db).unwrap();
        let profile = match run(
            &conn,
            "declare n number := 0;
begin
  for r in (select level l from dual connect by level <= 2000) loop
    n := n + r.l;
  end loop;
end;",
        ) {
            Ok(p) => p,
            // No EXECUTE on DBMS_HPROF in this database, or before 18c.
            Err(e) if explain(&e).contains("grant execute") => return,
            Err(e) if e.message.contains("needs Oracle 18c") => return,
            Err(e) => panic!("{e}"),
        };
        assert!(profile.error.is_none());
        assert!(profile.total_us > 0);
        let fetch = profile
            .rows
            .iter()
            .find(|r| {
                r.sql
                    && r.sql_text
                        .as_deref()
                        .is_some_and(|t| t.contains("CONNECT BY"))
            })
            .unwrap_or_else(|| panic!("{:#?}", profile.rows));
        assert!(fetch.calls >= 1);

        let failed = run(&conn, "begin raise_application_error(-20001, 'boom'); end;").unwrap();
        assert!(failed.error.unwrap().contains("ORA-20001: boom"));
    }
}
