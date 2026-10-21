//! Built-in reports: curated queries that open (and run) in a SQL window,
//! so they can be adjusted, exported or edited like any other query.

pub struct Report {
    pub category: &'static str,
    pub name: &'static str,
    pub sql: &'static str,
}

pub const REPORTS: &[Report] = &[
    Report {
        category: "Objects",
        name: "Invalid objects",
        sql: "-- Invalid objects in schemas you own or can see (Oracle schemas excluded)
select o.owner, o.object_type, o.object_name, o.last_ddl_time,
       (select count(*) from all_errors e
         where e.owner = o.owner and e.name = o.object_name and e.type = o.object_type) as errors
  from all_objects o
 where o.status = 'INVALID'
   and o.owner not in (select username from all_users where oracle_maintained = 'Y')
 order by o.owner, o.object_type, o.object_name",
    },
    Report {
        category: "Objects",
        name: "Recently changed objects",
        sql: "-- Objects changed in the last 7 days
select owner, object_type, object_name, status, last_ddl_time, created
  from all_objects
 where last_ddl_time > sysdate - 7
   and owner not in (select username from all_users where oracle_maintained = 'Y')
 order by last_ddl_time desc",
    },
    Report {
        category: "Objects",
        name: "Disabled constraints and triggers",
        sql: "select owner, 'CONSTRAINT' as kind, table_name, constraint_name as name, status
  from all_constraints
 where status = 'DISABLED'
   and owner not in (select username from all_users where oracle_maintained = 'Y')
union all
select owner, 'TRIGGER', table_name, trigger_name, status
  from all_triggers
 where status = 'DISABLED'
   and owner not in (select username from all_users where oracle_maintained = 'Y')
 order by 1, 2, 3",
    },
    Report {
        category: "Objects",
        name: "Compile errors",
        sql: "select owner, type, name, line, position, attribute, text
  from all_errors
 where owner not in (select username from all_users where oracle_maintained = 'Y')
 order by owner, type, name, sequence",
    },
    Report {
        category: "Design",
        name: "Unindexed foreign keys",
        sql: "-- Foreign keys without an index starting with their columns (locking and join risk)
select c.owner, c.table_name, c.constraint_name,
       listagg(cc.column_name, ', ') within group (order by cc.position) as columns,
       r.table_name as references_table
  from all_constraints c
  join all_cons_columns cc on cc.owner = c.owner and cc.constraint_name = c.constraint_name
  join all_constraints r on r.owner = c.r_owner and r.constraint_name = c.r_constraint_name
 where c.constraint_type = 'R'
   and c.owner = sys_context('USERENV', 'CURRENT_SCHEMA')
   and not exists (
         select 1 from all_ind_columns ic
          where ic.table_owner = c.owner and ic.table_name = c.table_name
            and ic.column_name = cc.column_name and ic.column_position = cc.position)
 group by c.owner, c.table_name, c.constraint_name, r.table_name
 order by c.table_name",
    },
    Report {
        category: "Design",
        name: "Tables without primary key",
        sql: "select t.owner, t.table_name, t.num_rows, t.last_analyzed
  from all_tables t
 where t.owner = sys_context('USERENV', 'CURRENT_SCHEMA')
   and t.temporary = 'N'
   and not exists (select 1 from all_constraints c
                    where c.owner = t.owner and c.table_name = t.table_name and c.constraint_type = 'P')
 order by t.table_name",
    },
    Report {
        category: "Storage",
        name: "Largest segments",
        sql: "-- Largest segments of the current schema
select segment_name, segment_type, tablespace_name,
       round(bytes / 1024 / 1024, 1) as mb, blocks, extents
  from user_segments
 order by bytes desc
 fetch first 50 rows only",
    },
    Report {
        category: "Storage",
        name: "Table sizes and statistics",
        sql: "-- Row counts from optimizer statistics, with staleness
select t.table_name, t.num_rows, t.blocks, t.avg_row_len, t.last_analyzed,
       s.stale_stats,
       round(nvl(seg.bytes, 0) / 1024 / 1024, 1) as mb
  from user_tables t
  left join user_tab_statistics s on s.table_name = t.table_name and s.object_type = 'TABLE'
  left join (select segment_name, sum(bytes) bytes from user_segments group by segment_name) seg
         on seg.segment_name = t.table_name
 order by nvl(seg.bytes, 0) desc",
    },
    Report {
        category: "Security",
        name: "Grants received",
        sql: "select 'OBJECT' as kind, grantor, table_schema || '.' || table_name as object, privilege, grantable
  from all_tab_privs
 where grantee in (user, 'PUBLIC') and table_schema <> user
union all
select 'SYSTEM', null, null, privilege, admin_option from user_sys_privs
union all
select 'ROLE', null, granted_role, null, admin_option from user_role_privs
 order by 1, 3, 4",
    },
    Report {
        category: "Security",
        name: "Grants given",
        sql: "select grantee, table_name, privilege, grantable, hierarchy
  from user_tab_privs_made
 order by table_name, grantee, privilege",
    },
    Report {
        category: "Activity",
        name: "Locked objects",
        sql: "-- Needs SELECT_CATALOG_ROLE
select o.owner, o.object_name, o.object_type, l.session_id as sid, s.serial#,
       s.username, s.program, s.status,
       decode(l.locked_mode, 1, 'NULL', 2, 'ROW SHARE', 3, 'ROW EXCLUSIVE', 4, 'SHARE',
              5, 'SHARE ROW EXCLUSIVE', 6, 'EXCLUSIVE', l.locked_mode) as mode_held
  from v$locked_object l
  join all_objects o on o.object_id = l.object_id
  join v$session s on s.sid = l.session_id
 order by o.owner, o.object_name",
    },
    Report {
        category: "Activity",
        name: "Top SQL by elapsed time",
        sql: "-- Needs SELECT_CATALOG_ROLE
select sql_id, executions, round(elapsed_time / 1e6, 2) as elapsed_s,
       round(elapsed_time / greatest(executions, 1) / 1e3, 2) as ms_per_exec,
       buffer_gets, disk_reads, rows_processed,
       substr(sql_text, 1, 200) as sql_text
  from v$sqlstats
 order by elapsed_time desc
 fetch first 25 rows only",
    },
    Report {
        category: "Activity",
        name: "Long running operations",
        sql: "-- Needs SELECT_CATALOG_ROLE
select sid, serial#, opname, target, sofar, totalwork,
       round(sofar / nullif(totalwork, 0) * 100, 1) as pct, elapsed_seconds, time_remaining, message
  from v$session_longops
 where sofar < totalwork
 order by start_time",
    },
    Report {
        category: "Jobs",
        name: "Scheduler jobs",
        sql: "-- Times are converted to text: TIMESTAMP WITH TIME ZONE values with region names
-- need the full Instant Client time zone file
select owner, job_name, enabled, state, job_type, repeat_interval,
       to_char(last_start_date, 'YYYY-MM-DD HH24:MI:SS TZR') as last_start_date, last_run_duration,
       to_char(next_run_date, 'YYYY-MM-DD HH24:MI:SS TZR') as next_run_date, run_count, failure_count
  from all_scheduler_jobs
 where owner not in (select username from all_users where oracle_maintained = 'Y')
 order by owner, job_name",
    },
    Report {
        category: "Jobs",
        name: "Recent job runs",
        sql: "select owner, job_name, status,
       to_char(actual_start_date, 'YYYY-MM-DD HH24:MI:SS TZR') as actual_start_date,
       run_duration, error#, additional_info
  from all_scheduler_job_run_details
 where actual_start_date > systimestamp - interval '7' day
   and owner not in (select username from all_users where oracle_maintained = 'Y')
 order by actual_start_date desc",
    },
];

/// Categories in display order.
pub fn categories() -> Vec<&'static str> {
    let mut out: Vec<&str> = Vec::new();
    for r in REPORTS {
        if !out.contains(&r.category) {
            out.push(r.category);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_are_single_statements() {
        for r in REPORTS {
            let statements = orui_plsql::split_statements(r.sql);
            assert_eq!(statements.len(), 1, "{}", r.name);
        }
        assert!(categories().len() >= 5);
    }

    #[test]
    fn reports_run_on_live_database() {
        let Ok(spec) = std::env::var("ORUI_TEST_DB") else {
            return;
        };
        let (cred, db) = spec.rsplit_once('@').unwrap();
        let (user, pw) = cred.split_once('/').unwrap();
        let conn = orui_db::oracle::Connection::connect(user, pw, db).unwrap();
        let mut failed = Vec::new();
        for r in REPORTS {
            if let Err(e) = conn
                .query(r.sql.trim_end_matches(';'), &[])
                .and_then(|mut rows| rows.next().transpose())
            {
                failed.push(format!("{}: {e}", r.name));
            }
        }
        assert!(failed.is_empty(), "{failed:#?}");
    }
}
