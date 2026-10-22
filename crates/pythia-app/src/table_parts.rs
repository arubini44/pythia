//! Indexes, constraints and triggers of a table, for the object browser.

use pythia_db::DbError;
use pythia_db::oracle::Connection;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartKind {
    Index {
        unique: bool,
    },
    /// P, U, R or C.
    Constraint {
        kind: char,
    },
    Trigger,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub kind: PartKind,
    pub name: String,
    /// Columns, the referenced table or the trigger timing.
    pub detail: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableParts {
    pub indexes: Vec<Part>,
    pub constraints: Vec<Part>,
    pub triggers: Vec<Part>,
    /// Foreign keys of other tables pointing at this one.
    pub referenced_by: Vec<Part>,
    /// Columns that are part of a foreign key.
    pub fk_columns: Vec<String>,
}

pub fn load(conn: &Connection, owner: &str, table: &str) -> Result<TableParts, DbError> {
    let mut parts = TableParts::default();
    let sql = "
select i.index_name, i.uniqueness, i.status,
       listagg(c.column_name, ', ') within group (order by c.column_position)
  from all_indexes i
  join all_ind_columns c on c.index_owner = i.owner and c.index_name = i.index_name
 where i.table_owner = :1 and i.table_name = :2
 group by i.index_name, i.uniqueness, i.status
 order by i.index_name";
    for row in conn.query_as::<(String, String, String, Option<String>)>(sql, &[&owner, &table])? {
        let (name, uniqueness, status, columns) = row?;
        parts.indexes.push(Part {
            kind: PartKind::Index {
                unique: uniqueness == "UNIQUE",
            },
            name,
            detail: columns.unwrap_or_default().to_lowercase(),
            enabled: status != "UNUSABLE",
        });
    }
    // Leave out the NOT NULL checks Oracle generates for columns.
    let sql = "
select c.constraint_name, c.constraint_type, c.status,
       (select listagg(cc.column_name, ', ') within group (order by cc.position)
          from all_cons_columns cc
         where cc.owner = c.owner and cc.constraint_name = c.constraint_name),
       r.table_name,
       case when c.constraint_type = 'C' then substr(c.search_condition_vc, 1, 200) end
  from all_constraints c
  left join all_constraints r on r.owner = c.r_owner and r.constraint_name = c.r_constraint_name
 where c.owner = :1 and c.table_name = :2
   and c.constraint_type in ('P', 'U', 'R', 'C')
   and not (c.constraint_type = 'C' and c.generated = 'GENERATED NAME'
            and c.search_condition_vc like '%IS NOT NULL')
 order by decode(c.constraint_type, 'P', 1, 'U', 2, 'R', 3, 4), c.constraint_name";
    for row in conn.query_as::<(
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    )>(sql, &[&owner, &table])?
    {
        let (name, kind, status, columns, references, condition) = row?;
        let kind = kind.chars().next().unwrap_or('C');
        let columns = columns.unwrap_or_default().to_lowercase();
        if kind == 'R' {
            parts
                .fk_columns
                .extend(columns.split(", ").map(str::to_uppercase));
        }
        let detail = match (kind, references, condition) {
            ('R', Some(table), _) => format!("{columns} → {}", table.to_lowercase()),
            ('C', _, Some(condition)) => condition,
            _ => columns,
        };
        parts.constraints.push(Part {
            kind: PartKind::Constraint { kind },
            name,
            detail,
            enabled: status == "ENABLED",
        });
    }
    let sql = "
select c.constraint_name, c.owner, c.table_name, c.status,
       (select listagg(cc.column_name, ', ') within group (order by cc.position)
          from all_cons_columns cc
         where cc.owner = c.owner and cc.constraint_name = c.constraint_name)
  from all_constraints r
  join all_constraints c on c.r_owner = r.owner and c.r_constraint_name = r.constraint_name
 where r.owner = :1 and r.table_name = :2 and r.constraint_type in ('P', 'U')
   and c.constraint_type = 'R'
 order by c.table_name, c.constraint_name";
    for row in
        conn.query_as::<(String, String, String, String, Option<String>)>(sql, &[&owner, &table])?
    {
        let (name, child_owner, child, status, columns) = row?;
        let child = if child_owner == owner {
            child.to_lowercase()
        } else {
            format!("{}.{}", child_owner.to_lowercase(), child.to_lowercase())
        };
        parts.referenced_by.push(Part {
            kind: PartKind::Constraint { kind: 'R' },
            name,
            detail: format!("{child} ({})", columns.unwrap_or_default().to_lowercase()),
            enabled: status == "ENABLED",
        });
    }
    let sql = "
select trigger_name, lower(trigger_type || ' ' || triggering_event), status
  from all_triggers
 where table_owner = :1 and table_name = :2
 order by trigger_name";
    for row in conn.query_as::<(String, String, String)>(sql, &[&owner, &table])? {
        let (name, detail, status) = row?;
        parts.triggers.push(Part {
            kind: PartKind::Trigger,
            name,
            detail,
            enabled: status == "ENABLED",
        });
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_from_live_database() {
        let Ok(spec) = std::env::var("PYTHIA_TEST_DB") else {
            return;
        };
        let (cred, db) = spec.rsplit_once('@').unwrap();
        let (user, pw) = cred.split_once('/').unwrap();
        let conn = Connection::connect(user, pw, db).unwrap();
        conn.execute(
            "begin
               for t in (select table_name from user_tables
                          where table_name in ('PYTHIA_PARTS_C', 'PYTHIA_PARTS_P')) loop
                 execute immediate 'drop table ' || t.table_name || ' cascade constraints purge';
               end loop;
             end;",
            &[],
        )
        .unwrap();
        for sql in [
            "create table pythia_parts_p (id number primary key)",
            "create table pythia_parts_c (id number constraint pythia_parts_pk primary key,
               p_id number not null constraint pythia_parts_fk references pythia_parts_p,
               qty number constraint pythia_parts_qty check (qty > 0))",
            "create index pythia_parts_ix on pythia_parts_c (p_id, qty)",
            "create trigger pythia_parts_trg before insert on pythia_parts_c for each row begin null; end;",
        ] {
            conn.execute(sql, &[]).unwrap();
        }
        let user = conn
            .query_row_as::<String>("select user from dual", &[])
            .unwrap();
        let parts = load(&conn, &user, "PYTHIA_PARTS_C").unwrap();
        let names = |p: &[Part]| p.iter().map(|p| p.name.clone()).collect::<Vec<_>>();
        assert_eq!(
            names(&parts.indexes),
            ["PYTHIA_PARTS_IX", "PYTHIA_PARTS_PK"]
        );
        assert_eq!(parts.indexes[0].detail, "p_id, qty");
        assert_eq!(
            names(&parts.constraints),
            ["PYTHIA_PARTS_PK", "PYTHIA_PARTS_FK", "PYTHIA_PARTS_QTY"]
        );
        assert_eq!(parts.constraints[1].detail, "p_id → pythia_parts_p");
        assert_eq!(parts.constraints[2].detail, "qty > 0");
        assert_eq!(parts.fk_columns, ["P_ID"]);
        assert_eq!(names(&parts.triggers), ["PYTHIA_PARTS_TRG"]);
        assert_eq!(parts.triggers[0].detail, "before each row insert");
        let parent = load(&conn, &user, "PYTHIA_PARTS_P").unwrap();
        assert_eq!(names(&parent.referenced_by), ["PYTHIA_PARTS_FK"]);
        assert_eq!(parent.referenced_by[0].detail, "pythia_parts_c (p_id)");
        conn.execute("drop table pythia_parts_c purge", &[])
            .unwrap();
        conn.execute("drop table pythia_parts_p purge", &[])
            .unwrap();
    }
}
