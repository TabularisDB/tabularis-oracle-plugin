//! Lightweight SQL statement classification for Oracle.
//!
//! `execute_query` accepts arbitrary SQL, so we need to tell apart statements
//! that return rows (and can be paginated) from DML/DDL, and — Oracle-specific
//! — PL/SQL blocks, which must keep their trailing semicolon while plain SQL
//! statements must lose it (OCI rejects `SELECT 1 FROM DUAL;`).

/// Return the first `n` whitespace-separated keywords, lowercased.
fn keywords(sql: &str, n: usize) -> Vec<String> {
    sql.split(|c: char| c.is_whitespace() || c == '(')
        .filter(|tok| !tok.is_empty())
        .take(n)
        .map(str::to_ascii_lowercase)
        .collect()
}

/// The first SQL keyword in lowercase.
pub fn first_keyword(sql: &str) -> String {
    keywords(sql, 1).into_iter().next().unwrap_or_default()
}

/// Does this statement produce a result set we should read with a row cursor?
pub fn returns_rows(sql: &str) -> bool {
    matches!(first_keyword(sql).as_str(), "select" | "with")
}

/// Can we safely wrap this statement in `SELECT * FROM (...)` for
/// OFFSET/FETCH pagination and `SELECT COUNT(*) FROM (...)` counting?
pub fn is_wrappable(sql: &str) -> bool {
    matches!(first_keyword(sql).as_str(), "select" | "with")
}

/// Is this a PL/SQL block or stored-code definition? Those keep their trailing
/// semicolon (it terminates the block), unlike plain SQL.
pub fn is_plsql(sql: &str) -> bool {
    let kws = keywords(sql, 4);
    match kws.first().map(String::as_str) {
        Some("begin") | Some("declare") => true,
        Some("create") => {
            let unit_kinds = [
                "procedure", "function", "package", "trigger", "type", "body",
            ];
            kws.iter()
                .skip(1)
                .any(|k| unit_kinds.contains(&k.as_str()))
        }
        _ => false,
    }
}

/// Prepare a user statement for OCI: PL/SQL passes through untouched, plain
/// SQL loses trailing semicolons and whitespace.
pub fn prepare_statement(sql: &str) -> &str {
    let trimmed = sql.trim();
    if is_plsql(trimmed) {
        trimmed
    } else {
        trimmed.trim_end_matches(';').trim_end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_first_keyword_case_insensitively() {
        assert_eq!(first_keyword("  SELECT 1 FROM DUAL"), "select");
        assert_eq!(first_keyword("\nWITH x AS (...)"), "with");
        assert_eq!(first_keyword("(select 1)"), "select");
    }

    #[test]
    fn classifies_row_returning_statements() {
        assert!(returns_rows("select * from t"));
        assert!(returns_rows("WITH a AS (select 1 from dual) select * from a"));
        assert!(!returns_rows("insert into t values (1)"));
        assert!(!returns_rows("update t set a = 1"));
        assert!(!returns_rows("create table t (id number)"));
        assert!(!returns_rows("begin null; end;"));
    }

    #[test]
    fn detects_plsql_blocks() {
        assert!(is_plsql("BEGIN null; END;"));
        assert!(is_plsql("declare x number; begin null; end;"));
        assert!(is_plsql("CREATE OR REPLACE PROCEDURE p AS BEGIN NULL; END;"));
        assert!(is_plsql("create or replace package body pkg as end;"));
        assert!(is_plsql("CREATE TRIGGER trg BEFORE INSERT ON t BEGIN NULL; END;"));
        assert!(!is_plsql("create table t (id number)"));
        assert!(!is_plsql("select 1 from dual"));
    }

    #[test]
    fn strips_semicolons_from_sql_but_not_plsql() {
        assert_eq!(prepare_statement("select 1 from dual; "), "select 1 from dual");
        assert_eq!(prepare_statement("select 1 from dual ;;"), "select 1 from dual");
        assert_eq!(
            prepare_statement("BEGIN null; END;"),
            "BEGIN null; END;"
        );
    }
}
