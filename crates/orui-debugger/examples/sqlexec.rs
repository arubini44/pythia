//! Runs SQL/PLSQL statements (separated by lines containing only `/`):
//! `cargo run -p orui-debugger --example sqlexec -- user/pw@db file.sql`.
//! Queries print their rows. Used for setup and exploration.

use orui_db::oracle::Connection;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let spec = args.next().ok_or("usage: sqlexec user/pw@db file.sql")?;
    let file = args.next().ok_or("missing file")?;
    let (cred, db) = spec.rsplit_once('@').ok_or("bad spec")?;
    let (user, pw) = cred.split_once('/').ok_or("bad spec")?;
    let conn = Connection::connect(user, pw, db)?;
    let text = std::fs::read_to_string(file)?;
    for stmt in text.split("\n/\n").map(str::trim).filter(|s| !s.is_empty()) {
        let stmt = stmt.trim_end_matches('/').trim();
        println!("-- {}", stmt.lines().next().unwrap_or_default());
        let lower = stmt.to_lowercase();
        if lower.starts_with("select") || lower.starts_with("with") {
            match conn.query(stmt, &[]) {
                Ok(rows) => {
                    for row in rows {
                        let row = row?;
                        let v: Vec<String> =
                            row.sql_values().iter().map(|v| v.to_string()).collect();
                        println!("{}", v.join(" | "));
                    }
                }
                Err(e) => println!("ERROR {e}"),
            }
        } else if let Err(e) = conn.execute(stmt, &[]) {
            println!("ERROR {e}");
        }
    }
    conn.commit()?;
    Ok(())
}
