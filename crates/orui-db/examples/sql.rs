//! Runs one statement and prints the rows of a query:
//! `cargo run -p orui-db --example sql -- user/pw@db "select ..."`.

use orui_db::oracle::Connection;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let spec = args.next().ok_or("usage: sql user/pw@db \"statement\"")?;
    let sql = args.next().ok_or("missing SQL")?;
    let (cred, db) = spec.rsplit_once('@').ok_or("bad connect spec")?;
    let (user, pw) = cred.split_once('/').ok_or("bad connect spec")?;
    let conn = Connection::connect(user, pw, db)?;
    if !conn.statement(&sql).build()?.is_query() {
        let stmt = conn.execute(&sql, &[])?;
        println!("ok ({} rows)", stmt.row_count().unwrap_or(0));
        conn.commit()?;
        return Ok(());
    }
    let rows = conn.query(&sql, &[])?;
    let names: Vec<String> = rows
        .column_info()
        .iter()
        .map(|c| c.name().to_owned())
        .collect();
    println!("{}", names.join(" | "));
    for row in rows {
        let row = row?;
        let values: Vec<String> = row.sql_values().iter().map(|v| v.to_string()).collect();
        println!("{}", values.join(" | "));
    }
    Ok(())
}
