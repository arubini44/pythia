//! "Test" scripts: an anonymous block that calls a procedure or function.
//!
//! IN parameters become bind variables (prompted, values remembered per
//! window), OUT/IN OUT parameters and function results are declared as
//! locals and printed with DBMS_OUTPUT.

use pythia_lsp_core::{CatalogMember, MemberKind, ParamMode};

/// Builds the test block for `member` called as `qualified` (e.g.
/// `hr.emp_pkg.raise_salary` or `hr.my_proc`).
pub fn test_block(qualified: &str, member: &CatalogMember) -> String {
    let local_type = |data_type: &str| -> String {
        let t = data_type.to_ascii_uppercase();
        match t.as_str() {
            "VARCHAR2" | "NVARCHAR2" | "CHAR" | "NCHAR" | "RAW" => {
                format!("{}(32767)", t.to_lowercase())
            }
            "PL/SQL BOOLEAN" | "BOOLEAN" => "boolean".into(),
            "PL/SQL RECORD" | "PL/SQL TABLE" | "TABLE" | "OBJECT" | "REF CURSOR" => {
                format!("{} -- TODO: declare", t.to_lowercase())
            }
            _ => t.to_lowercase(),
        }
    };
    let display = |name: &str, data_type: &str| -> String {
        match data_type.to_ascii_uppercase().as_str() {
            "PL/SQL BOOLEAN" | "BOOLEAN" => {
                format!("case when {name} then 'TRUE' when not {name} then 'FALSE' else 'NULL' end")
            }
            "DATE" => format!("to_char({name}, 'YYYY-MM-DD HH24:MI:SS')"),
            "CLOB" | "NCLOB" => format!("dbms_lob.substr({name}, 4000)"),
            _ => name.to_owned(),
        }
    };

    let mut declare = Vec::new();
    let mut args = Vec::new();
    // IN OUT locals start with the prompted bind value.
    let mut init = Vec::new();
    for p in &member.params {
        let name = p.name.to_lowercase();
        match p.mode {
            ParamMode::In => args.push(format!("{name} => :{name}")),
            ParamMode::Out | ParamMode::InOut => {
                let local = format!("l_{}", name.trim_start_matches("p_"));
                declare.push(format!("  {local} {};", local_type(&p.data_type)));
                if p.mode == ParamMode::InOut {
                    init.push(format!("  {local} := :{name};"));
                }
                args.push(format!("{name} => {local}"));
            }
        }
    }
    let mut after = Vec::new();
    for p in member.params.iter().filter(|p| p.mode != ParamMode::In) {
        let local = format!("l_{}", p.name.to_lowercase().trim_start_matches("p_"));
        after.push(format!(
            "  dbms_output.put_line('{} = ' || {});",
            p.name.to_lowercase(),
            display(&local, &p.data_type)
        ));
    }
    let call_args = if args.is_empty() {
        String::new()
    } else {
        format!("(\n    {}\n  )", args.join(",\n    "))
    };
    let call = match (&member.kind, &member.return_type) {
        (MemberKind::Function, Some(ret)) => {
            declare.insert(0, format!("  l_result {};", local_type(ret)));
            after.insert(
                0,
                format!(
                    "  dbms_output.put_line('result = ' || {});",
                    display("l_result", ret)
                ),
            );
            format!("  l_result := {qualified}{call_args};")
        }
        _ => format!("  {qualified}{call_args};"),
    };

    let mut out = format!("-- Test {qualified}\n");
    if !declare.is_empty() {
        out.push_str("declare\n");
        out.push_str(&declare.join("\n"));
        out.push('\n');
    }
    out.push_str("begin\n");
    for line in &init {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&call);
    out.push('\n');
    for line in &after {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("end;\n");
    out
}

#[cfg(test)]
mod tests {
    use pythia_lsp_core::CatalogParam;

    use super::*;

    fn param(name: &str, mode: ParamMode, t: &str) -> CatalogParam {
        CatalogParam {
            name: name.into(),
            mode,
            data_type: t.into(),
            has_default: false,
        }
    }

    #[test]
    fn function_with_out_param() {
        let member = CatalogMember {
            name: "F".into(),
            kind: MemberKind::Function,
            params: vec![
                param("P_ID", ParamMode::In, "NUMBER"),
                param("P_NAME", ParamMode::Out, "VARCHAR2"),
            ],
            return_type: Some("DATE".into()),
            overload: None,
        };
        assert_eq!(
            test_block("hr.pkg.f", &member),
            "-- Test hr.pkg.f\n\
             declare\n  l_result date;\n  l_name varchar2(32767);\n\
             begin\n  l_result := hr.pkg.f(\n    p_id => :p_id,\n    p_name => l_name\n  );\n\
             \x20 dbms_output.put_line('result = ' || to_char(l_result, 'YYYY-MM-DD HH24:MI:SS'));\n\
             \x20 dbms_output.put_line('p_name = ' || l_name);\n\
             end;\n"
        );
    }

    #[test]
    fn procedure_without_params() {
        let member = CatalogMember {
            name: "P".into(),
            kind: MemberKind::Procedure,
            params: vec![],
            return_type: None,
            overload: None,
        };
        assert_eq!(test_block("p", &member), "-- Test p\nbegin\n  p;\nend;\n");
    }
}
