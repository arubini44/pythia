//! Runs one query and prints the rows: `cargo run -p orui-db --example sql -- user/pw@db "select ..."`.

use orui_db::oracle::Connection;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let spec = args.next().ok_or("usage: sql user/pw@db \"select …\"")?;
    let sql = args.next().ok_or("missing SQL")?;
    let (cred, db) = spec.rsplit_once('@').ok_or("bad connect spec")?;
    let (user, pw) = cred.split_once('/').ok_or("bad connect spec")?;
    let conn = Connection::connect(user, pw, db)?;
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
