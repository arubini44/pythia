use super::*;

fn corpus() -> Vec<(String, String)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/testdata");
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    files
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "sql"))
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read_to_string(&p).unwrap(),
            )
        })
        .collect()
}

fn variants() -> Vec<FormatOptions> {
    let d = FormatOptions::default();
    vec![
        d.clone(),
        FormatOptions {
            keyword_case: Case::Upper,
            identifier_case: Case::Upper,
            indent: 4,
            ..d.clone()
        },
        FormatOptions {
            comma: CommaStyle::Leading,
            max_width: 60,
            ..d.clone()
        },
        FormatOptions {
            keyword_case: Case::Preserve,
            align_aliases: false,
            align_assignments: false,
            align_declarations: false,
            on_new_line: true,
            blank_lines: 0,
            max_width: 40,
            ..d
        },
    ]
}

/// Checks the guarantees on one input under one option set.
fn check(src: &str, o: &FormatOptions, what: &str) -> String {
    let once = format(src, o).text;
    assert!(
        equivalent(src, &once),
        "{what}: tokens changed\n--- input\n{src}\n--- output\n{once}"
    );
    let twice = format(&once, o).text;
    assert_eq!(once, twice, "{what}: not idempotent\n--- input\n{src}");
    once
}

#[test]
fn corpus_is_lossless_and_idempotent() {
    for (name, src) in corpus() {
        for (i, o) in variants().iter().enumerate() {
            check(&src, o, &format!("{name} (options #{i})"));
        }
    }
}

/// Every statement of the corpus is actually formatted, not silently kept
/// by the safety net (except SQL*Plus commands and compound triggers).
#[test]
fn corpus_statements_are_formatted() {
    for (name, src) in corpus() {
        for (i, o) in variants().iter().enumerate() {
            for stmt in split_statements(&src) {
                let text = stmt.text(&src);
                let unsupported = text.to_ascii_lowercase().contains("compound trigger");
                if stmt.kind == StatementKind::SqlPlus || unsupported {
                    continue;
                }
                assert!(
                    try_format_statement(text, stmt.kind, o).is_some(),
                    "{name} (options #{i}): fell back for\n{text}"
                );
            }
        }
    }
}

#[test]
fn snapshots() {
    for (name, src) in corpus() {
        let out = format(&src, &FormatOptions::default()).text;
        insta::assert_snapshot!(name.clone(), out);
    }
    let (_, src) = corpus()
        .into_iter()
        .find(|(n, _)| n == "queries.sql")
        .unwrap();
    let upper = FormatOptions {
        keyword_case: Case::Upper,
        comma: CommaStyle::Leading,
        indent: 4,
        ..Default::default()
    };
    insta::assert_snapshot!("queries_upper_leading", format(&src, &upper).text);
}

#[test]
fn prefixes_never_break() {
    // Every third prefix of every corpus file: no panic, lossless, idempotent.
    let o = FormatOptions::default();
    for (name, src) in corpus() {
        let cuts: Vec<usize> = src
            .char_indices()
            .map(|(i, _)| i)
            .step_by(3)
            .chain([src.len()])
            .collect();
        for cut in cuts {
            check(&src[..cut], &o, &format!("{name}[..{cut}]"));
        }
    }
}

/// Tiny deterministic PRNG for the token soups.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as usize
    }
}

#[test]
fn token_soup_never_breaks() {
    const VOCAB: &[&str] = &[
        "select",
        "from",
        "where",
        "and",
        "or",
        "(",
        ")",
        ",",
        ";",
        "begin",
        "end",
        "if",
        "then",
        "else",
        "elsif",
        "loop",
        "for",
        "in",
        "case",
        "when",
        "declare",
        "is",
        "as",
        "create",
        "procedure",
        "function",
        "package",
        "body",
        "x",
        "y",
        "1",
        "'s'",
        ":=",
        "=",
        "+",
        "-",
        "*",
        "||",
        "--c\n",
        "/*b*/",
        "\n",
        "\n\n",
        "/",
        "insert",
        "into",
        "values",
        "update",
        "set",
        "delete",
        "merge",
        "using",
        "on",
        "join",
        "left",
        "group",
        "by",
        "order",
        "union",
        "all",
        "exception",
        "others",
        "return",
        "type",
        "record",
        "cursor",
        "<<",
        ">>",
        "%",
        "rowtype",
        ".",
        "q'[a]'",
        "\"Q\"",
        ":b",
        "&s",
        "between",
        "exit",
        "null",
        "with",
        "trigger",
        "before",
        "each",
        "row",
        "table",
        "view",
        "fetch",
        "first",
        "rows",
        "only",
    ];
    let options = variants();
    let mut rng = Lcg(42);
    for round in 0..600 {
        let len = 1 + rng.next() % 60;
        let mut src = String::new();
        for _ in 0..len {
            src.push_str(VOCAB[rng.next() % VOCAB.len()]);
            src.push(' ');
        }
        let o = &options[round % options.len()];
        check(&src, o, &format!("soup #{round}"));
    }
}

#[test]
fn deep_nesting_is_left_alone() {
    let src = format!("select {}1{} from dual", "(".repeat(500), ")".repeat(500));
    let out = check(&src, &FormatOptions::default(), "deep");
    assert_eq!(out, src);
    let src = "begin ".repeat(300) + &"end; ".repeat(300);
    check(&src, &FormatOptions::default(), "deep blocks");
}

#[test]
fn keeps_sqlplus_and_terminators() {
    let src =
        "set serveroutput on\nprompt  Two  spaces\nbegin null; end;\n/\nselect 1 from dual\n/\n";
    let out = format(src, &FormatOptions::default()).text;
    assert_eq!(
        out,
        "set serveroutput on\nprompt  Two  spaces\nbegin\n  null;\nend;\n/\nselect 1 from dual\n/\n"
    );
}

#[test]
fn comments_stay_in_place() {
    let src = "select a, -- first\n b /* second */ from t -- trailing\n;";
    let out = check(src, &FormatOptions::default(), "comments");
    assert!(out.contains("-- first"), "{out}");
    assert!(out.contains("/* second */"), "{out}");
    assert!(out.ends_with("-- trailing\n;"), "{out}");
}

#[test]
fn quoted_identifiers_and_literals_are_untouched() {
    let o = FormatOptions {
        keyword_case: Case::Upper,
        identifier_case: Case::Upper,
        ..Default::default()
    };
    let out = format("select \"MixedCase\", 'MiXeD', q'[x y]' from MyTable", &o).text;
    assert_eq!(out, "SELECT \"MixedCase\", 'MiXeD', q'[x y]' FROM MYTABLE");
}

#[test]
fn short_queries_stay_compact() {
    let o = FormatOptions::default();
    assert_eq!(
        format("SELECT  *  FROM   dual", &o).text,
        "select * from dual"
    );
    assert_eq!(
        format("select a,b from t where x=1", &o).text,
        "select a, b\nfrom t\nwhere x = 1"
    );
}

#[test]
fn format_range_touches_only_selected_statements() {
    let o = FormatOptions::default();
    let src = "select   1 from dual;\nselect   2 from dual;\nselect   3 from dual;\n";
    let at = src.find('2').unwrap();
    let out = format_range(src, at..at, &o).text;
    assert_eq!(
        out,
        "select   1 from dual;\nselect 2 from dual;\nselect   3 from dual;\n"
    );
    assert_eq!(
        format_range(src, 0..src.len(), &o).text,
        format(src, &o).text
    );
}

#[test]
fn options_use_readable_names() {
    let o = FormatOptions {
        comma: CommaStyle::Leading,
        keyword_case: Case::Upper,
        ..Default::default()
    };
    let json = serde_json::to_string(&o).unwrap();
    assert!(
        json.contains("\"comma\":\"leading\"") && json.contains("\"keyword_case\":\"upper\""),
        "{json}"
    );
    let partial: FormatOptions = serde_json::from_str("{\"indent\": 4}").unwrap();
    assert_eq!(
        partial,
        FormatOptions {
            indent: 4,
            ..Default::default()
        }
    );
}

#[test]
#[cfg(not(debug_assertions))]
fn large_package_body_is_fast() {
    let (_, src) = corpus()
        .into_iter()
        .find(|(n, _)| n == "hr_pkg.sql")
        .unwrap();
    let body_start = src.find("create or replace package body").unwrap();
    let body = &src[body_start..];
    let (head, rest) = body.split_once(" as\n").unwrap();
    let members = &rest[..rest.rfind("\nbegin\n").unwrap()];
    let mut big = format!("{head} as\n");
    let mut n = 0;
    while big.lines().count() < 20_000 {
        big.push_str(&members.replace("raise_salary", &format!("raise_salary_{n}")));
        big.push('\n');
        n += 1;
    }
    big.push_str("end;\n");
    let lines = big.lines().count();
    let start = std::time::Instant::now();
    let out = format(&big, &FormatOptions::default());
    let elapsed = start.elapsed();
    eprintln!("formatted {lines} lines in {elapsed:?}");
    assert!(out.changed);
    assert!(
        elapsed < std::time::Duration::from_millis(200),
        "{elapsed:?} for {lines} lines"
    );
}

#[test]
#[ignore]
fn debug_one() {
    let src = std::env::var("FMT_SRC").unwrap().replace("\\n", "\n");
    let o = FormatOptions::default();
    let once = format(&src, &o).text;
    let twice = format(&once, &o).text;
    println!("--- once\n{once:?}\n--- twice\n{twice:?}");
}

#[test]
#[ignore]
fn show() {
    let only = std::env::var("FMT_FILE").ok();
    for (name, src) in corpus() {
        if only.as_deref().is_some_and(|o| name != o) {
            continue;
        }
        println!(
            "===== {name}\n{}",
            format(&src, &FormatOptions::default()).text
        );
    }
}
