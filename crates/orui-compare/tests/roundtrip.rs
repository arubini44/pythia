//! Compare → apply sync script → re-compare against a real database.
//!
//! Skipped unless `ORUI_TEST_DB_DBA=system/password@connect_string` is set
//! (a DBA user that may create and drop the ORUI_CMP_A / ORUI_CMP_B schemas).

use orui_compare::data::{self, DataOptions, DataSource};
use orui_compare::schema::{self, Options, Status};
mod common;

use common::{connect, run, setup, teardown};

fn describe(diff: &schema::SchemaDiff) -> String {
    diff.objects
        .iter()
        .filter(|o| o.status != Status::Equal)
        .map(|o| {
            format!(
                "{:?} {:?} {}\n  statements: {:?}\n  source:\n{}\n  target:\n{}\n",
                o.status,
                o.key.object_type,
                o.key.name,
                o.statements.iter().map(|s| &s.sql).collect::<Vec<_>>(),
                o.source_ddl.as_deref().unwrap_or("-"),
                o.target_ddl.as_deref().unwrap_or("-")
            )
        })
        .collect()
}

#[test]
fn schema_and_data_roundtrip() {
    let Some(conn) = connect() else {
        eprintln!("ORUI_TEST_DB_DBA not set, skipping");
        return;
    };
    setup(&conn);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let options = Options {
            include_drops: true,
            ..Options::default()
        };
        let source = schema::collect(&conn, "ORUI_CMP_A", &options).expect("collect source");
        let target = schema::collect(&conn, "ORUI_CMP_B", &options).expect("collect target");
        let diff = schema::compare(&source, &target, &options);
        assert!(diff.errors.is_empty(), "{:?}", diff.errors);

        let status = |name: &str| {
            diff.objects
                .iter()
                .find(|o| o.key.name == name)
                .map(|o| o.status)
        };
        assert_eq!(status("DEPT"), Some(Status::Equal), "{}", describe(&diff));
        assert_eq!(status("EMP"), Some(Status::Different));
        assert_eq!(status("NEW_T"), Some(Status::OnlyInSource));
        assert_eq!(status("OLD_T"), Some(Status::OnlyInTarget));
        assert_eq!(status("S1"), Some(Status::Different));
        assert_eq!(status("F"), Some(Status::Equal));
        assert_eq!(status("P_A"), Some(Status::OnlyInSource));
        assert_eq!(status("P_B"), Some(Status::OnlyInTarget));
        assert_eq!(status("TRG"), Some(Status::OnlyInSource));
        assert_eq!(status("SYN"), Some(Status::OnlyInSource));

        let script = diff.script("ORUI_CMP_A → ORUI_CMP_B");
        eprintln!("{script}");
        run(&conn, &script);
        run(&conn, "alter session set current_schema = system;");

        let target = schema::collect(&conn, "ORUI_CMP_B", &options).expect("collect target again");
        let again = schema::compare(&source, &target, &options);
        assert!(
            again.is_equal(),
            "not equal after sync:\n{}",
            describe(&again)
        );

        // Data: DEPT rows differ (changed, missing, extra).
        let src = DataSource::Table {
            schema: "ORUI_CMP_A".into(),
            table: "DEPT".into(),
        };
        let tgt = DataSource::Table {
            schema: "ORUI_CMP_B".into(),
            table: "DEPT".into(),
        };
        let options = DataOptions {
            chunk_size: 2,
            ..DataOptions::default()
        };
        let mut progress_calls = 0;
        let d = data::compare(&conn, &src, &conn, &tgt, options.clone(), |_, _| {
            progress_calls += 1
        })
        .expect("data compare");
        assert_eq!(
            (d.equal, d.changed, d.only_in_source, d.only_in_target),
            (1, 1, 1, 1),
            "{d:?}"
        );
        assert!(progress_calls > 0);
        let script = d.script("data");
        eprintln!("{script}");
        run(&conn, &script);
        run(&conn, "commit;");
        let d = data::compare(&conn, &src, &conn, &tgt, options, |_, _| {})
            .expect("data compare again");
        assert!(d.is_equal(), "{d:?}");
        assert_eq!(d.equal, 3);
    }));
    teardown(&conn);
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}
