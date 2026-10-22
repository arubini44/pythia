//! Explores what Oracle's JDWP agent exposes for a PL/SQL call.
//! `PYTHIA_TEST_DB=pythia/pythia@localhost:1521/FREEPDB1 cargo run -p pythia-debugger --example explore`

use std::net::TcpListener;
use std::time::Duration;

use pythia_db::oracle::Connection;
use pythia_debugger::jdwp::{self, Event, Jdwp, Value};

fn connect() -> Connection {
    let spec = std::env::var("PYTHIA_TEST_DB").expect("PYTHIA_TEST_DB");
    let (cred, db) = spec.rsplit_once('@').unwrap();
    let (user, pw) = cred.split_once('/').unwrap();
    Connection::connect(user, pw, db).unwrap()
}

const PROC: &str = "create or replace procedure dbg_demo(p_n in number) is
  l_total number := 0;
  l_name  varchar2(30) := 'start';
  l_flag  boolean := true;
  l_date  date := date '2024-02-29';
begin
  for i in 1 .. p_n loop
    l_total := l_total + i;
    l_name := 'step ' || i;
  end loop;
  dbg_helper(l_total);
end;";

const HELPER: &str = "create or replace procedure dbg_helper(p_v in out number) is
  l_x number := p_v * 2;
begin
  p_v := l_x + 1;
end;";

fn main() {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let conn = connect();
    conn.execute(HELPER, &[]).unwrap();
    conn.execute(PROC, &[]).unwrap();
    conn.execute("alter procedure dbg_helper compile debug", &[])
        .unwrap();
    conn.execute("alter procedure dbg_demo compile debug", &[])
        .unwrap();

    let listener = TcpListener::bind("0.0.0.0:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    println!("listening on {port}");

    let target = std::thread::spawn(move || {
        let conn = connect();
        let r = conn.execute(
            &format!("begin dbms_debug_jdwp.connect_tcp('host.containers.internal', {port}); end;"),
            &[],
        );
        println!("target: connect_tcp -> {r:?}");
        let r = conn.execute("begin dbg_demo(3); end;", &[]);
        println!("target: call -> {r:?}");
        let r = conn.execute("begin dbms_debug_jdwp.disconnect; end;", &[]);
        println!("target: disconnect -> {r:?}");
    });

    let (stream, addr) = listener.accept().unwrap();
    println!("accepted {addr}");
    let mut j = Jdwp::handshake(stream).unwrap();
    println!("sizes {:?}", j.sizes);
    println!("version raw {:?}", j.command(1, 1, jdwp::Out::default()));
    let threads = j.all_threads().unwrap();
    for t in &threads {
        println!("thread {t}: {:?}", j.thread_name(*t));
    }
    let classes = j.all_classes().unwrap();
    println!("{} classes loaded", classes.len());
    for (tag, id, sig) in classes.iter().take(40) {
        println!("  class {tag} {id} {sig}");
    }
    let req = j
        .set_event(jdwp::kind::CLASS_PREPARE, jdwp::suspend::ALL, vec![])
        .unwrap();
    println!("class prepare request {req}");

    let mut steps = 0;
    loop {
        let batch = match j.next_events(Duration::from_secs(20)) {
            Ok(Some(b)) => b,
            Ok(None) => {
                println!("no events for 20s");
                break;
            }
            Err(e) => {
                println!("events error {e}");
                break;
            }
        };
        println!("events {batch:?}");
        let mut done = false;
        for event in &batch.1 {
            match event {
                Event::ClassPrepare {
                    class,
                    signature,
                    thread,
                    ..
                } => {
                    println!("== prepared {signature}");
                    let methods = j.methods(*class).unwrap_or_default();
                    for m in &methods {
                        println!(
                            "   method {} {} {} mods={}",
                            m.id, m.name, m.signature, m.modifiers
                        );
                        match j.line_table(*class, m.id) {
                            Ok(lt) => println!("     lines {:?}", lt.lines),
                            Err(e) => println!("     lines err {e}"),
                        }
                        match j.variable_table(*class, m.id) {
                            Ok((args, vars)) => {
                                println!("     args {args}");
                                for v in vars {
                                    println!(
                                        "       var {} {} slot {} at {} len {}",
                                        v.name, v.signature, v.slot, v.code_index, v.length
                                    );
                                }
                            }
                            Err(e) => println!("     vars err {e}"),
                        }
                    }
                    for f in j.fields(*class).unwrap_or_default() {
                        println!("   field {} {} {}", f.id, f.name, f.signature);
                    }
                    if signature.contains("DBG_DEMO") {
                        // Break on the first line of the loop body.
                        for m in &methods {
                            if let Ok(lt) = j.line_table(*class, m.id)
                                && let Some((idx, _)) = lt.lines.iter().find(|(_, l)| *l == 8)
                            {
                                let loc = jdwp::Location {
                                    type_tag: 1,
                                    class: *class,
                                    method: m.id,
                                    index: *idx,
                                };
                                let m = j.modifier_location(&loc);
                                let r = j.set_event(
                                    jdwp::kind::BREAKPOINT,
                                    jdwp::suspend::ALL,
                                    vec![m],
                                );
                                println!("   breakpoint at line 8 -> {r:?}");
                            }
                        }
                    }
                    let _ = thread;
                }
                Event::Breakpoint {
                    thread, location, ..
                }
                | Event::SingleStep {
                    thread, location, ..
                } => {
                    println!("== stopped at {location:?}");
                    let frames = j.frames(*thread).unwrap();
                    for (frame, loc) in &frames {
                        let sig = j.signature(loc.class).unwrap_or_default();
                        let line = j.line_table(loc.class, loc.method).ok().and_then(|lt| {
                            lt.lines
                                .iter()
                                .filter(|(i, _)| *i <= loc.index)
                                .max_by_key(|(i, _)| *i)
                                .map(|(_, l)| *l)
                        });
                        println!("   frame {frame} {sig} index {} line {line:?}", loc.index);
                    }
                    if let Some((frame, loc)) = frames.first() {
                        let (_, vars) = j.variable_table(loc.class, loc.method).unwrap();
                        let slots: Vec<(i32, u8)> = vars
                            .iter()
                            .map(|v| (v.slot, v.signature.as_bytes()[0]))
                            .collect();
                        match j.frame_values(*thread, *frame, &slots) {
                            Ok(values) => {
                                for (v, val) in vars.iter().zip(values) {
                                    println!(
                                        "   {} ({}) = {val:?} {}",
                                        v.name,
                                        v.signature,
                                        describe(&mut j, &val, 0)
                                    );
                                }
                            }
                            Err(e) => println!("   values err {e}"),
                        }
                    }
                    steps += 1;
                    if steps < 4 {
                        let m = j.modifier_step(*thread, jdwp::step::OVER);
                        let r = j.set_event(
                            jdwp::kind::SINGLE_STEP,
                            jdwp::suspend::ALL,
                            vec![m, Jdwp::modifier_count(1)],
                        );
                        println!("   step over -> {r:?}");
                    } else if steps == 4 {
                        let m = j.modifier_step(*thread, jdwp::step::INTO);
                        let r = j.set_event(
                            jdwp::kind::SINGLE_STEP,
                            jdwp::suspend::ALL,
                            vec![m, Jdwp::modifier_count(1)],
                        );
                        println!("   step into -> {r:?}");
                    }
                }
                Event::VmDeath => done = true,
                _ => {}
            }
        }
        if done {
            break;
        }
        if batch.0 != jdwp::suspend::NONE
            && let Err(e) = j.resume_vm()
        {
            println!("resume err {e}");
            break;
        }
    }
    target.join().unwrap();
}

fn describe(j: &mut Jdwp, v: &Value, depth: usize) -> String {
    match v {
        Value::String(id) if *id != 0 => format!("{:?}", j.string_value(*id).unwrap_or_default()),
        Value::Object { id, .. } if *id != 0 && depth < 2 => {
            let Ok((_, class)) = j.object_type(*id) else {
                return "?".into();
            };
            let sig = j.signature(class).unwrap_or_default();
            let fields = j.fields(class).unwrap_or_default();
            let ids: Vec<u64> = fields.iter().map(|f| f.id).collect();
            let vals = j.object_values(*id, &ids).unwrap_or_default();
            let parts: Vec<String> = fields
                .iter()
                .zip(vals)
                .map(|(f, v)| format!("{}:{}={}", f.name, f.signature, describe(j, &v, depth + 1)))
                .collect();
            format!("{sig}{{{}}}", parts.join(", "))
        }
        other => format!("{other:?}"),
    }
}
