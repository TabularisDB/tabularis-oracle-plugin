//! Workaround for fetching Oracle `JSON` columns.
//!
//! rust-oracle (up to 0.6.x) cannot create fetch buffers for the native JSON
//! type introduced in Oracle 21c: any SELECT touching a JSON column fails at
//! define time with "unsupported Oracle type JSON", before a single row (or
//! even the column list) is available.
//!
//! The recovery path implemented here:
//!
//! 1. catch that specific error,
//! 2. describe the statement server-side with `DBMS_SQL.PARSE` +
//!    `DESCRIBE_COLUMNS2` (parsing never creates fetch buffers, and describing
//!    needs no bind values),
//! 3. re-run the query wrapped as
//!    `SELECT ..., JSON_SERIALIZE("COL" RETURNING CLOB) AS "COL", ... FROM (original)`
//!    so JSON columns come back as CLOB text, which fetches fine.
//!
//! `JSON_SERIALIZE` exists on every server that has the JSON type (21c+), so
//! the rewrite never runs against a server that cannot handle it.

use crate::utils::identifiers::quote;

/// Oracle's internal datatype code for the native JSON type, as reported in
/// `DBMS_SQL.DESC_TAB2.col_type`.
pub const JSON_TYPE_CODE: u32 = 119;

/// Anonymous block that describes an arbitrary statement without executing
/// it. Bind 1 (IN): the SQL text. Bind 2 (OUT): one `<col_type>:<col_name>`
/// entry per column, terminated by CHR(1).
pub const DESCRIBE_COLUMNS_BLOCK: &str = "\
DECLARE
  cur PLS_INTEGER;
  col_count PLS_INTEGER;
  cols DBMS_SQL.DESC_TAB2;
  buf VARCHAR2(32767);
BEGIN
  cur := DBMS_SQL.OPEN_CURSOR;
  BEGIN
    DBMS_SQL.PARSE(cur, :1, DBMS_SQL.NATIVE);
    DBMS_SQL.DESCRIBE_COLUMNS2(cur, col_count, cols);
    FOR i IN 1 .. col_count LOOP
      buf := buf || cols(i).col_type || ':' || cols(i).col_name || CHR(1);
    END LOOP;
    DBMS_SQL.CLOSE_CURSOR(cur);
  EXCEPTION WHEN OTHERS THEN
    DBMS_SQL.CLOSE_CURSOR(cur);
    RAISE;
  END;
  :2 := buf;
END;";

/// Is this the define-time failure rust-oracle raises for JSON columns?
pub fn is_unsupported_json_error(message: &str) -> bool {
    message.contains("unsupported Oracle type JSON")
}

/// Parse the OUT buffer produced by [`DESCRIBE_COLUMNS_BLOCK`] into
/// `(type_code, column_name)` pairs. Malformed entries are skipped.
pub fn parse_describe(buf: &str) -> Vec<(u32, String)> {
    buf.split('\u{1}')
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| {
            let (type_code, name) = entry.split_once(':')?;
            Some((type_code.parse().ok()?, name.to_string()))
        })
        .collect()
}

/// Build the select list that serializes JSON columns and passes everything
/// else through. Returns `None` when no column is JSON — the caller should
/// then keep the original error rather than retry pointlessly.
pub fn json_safe_projection(cols: &[(u32, String)]) -> Option<String> {
    if !cols.iter().any(|(t, _)| *t == JSON_TYPE_CODE) {
        return None;
    }
    let list = cols
        .iter()
        .map(|(t, name)| {
            let quoted = quote(name);
            if *t == JSON_TYPE_CODE {
                format!("JSON_SERIALIZE({quoted} RETURNING CLOB) AS {quoted}")
            } else {
                quoted
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    Some(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_the_json_define_error() {
        assert!(is_unsupported_json_error(
            "oracle error: unsupported Oracle type JSON"
        ));
        assert!(!is_unsupported_json_error(
            "ORA-00942: table or view does not exist"
        ));
        assert!(!is_unsupported_json_error("unsupported Oracle type VECTOR"));
    }

    #[test]
    fn parses_describe_entries() {
        let buf = "2:ID\u{1}1:NAME\u{1}119:META\u{1}";
        assert_eq!(
            parse_describe(buf),
            vec![
                (2, "ID".to_string()),
                (1, "NAME".to_string()),
                (119, "META".to_string()),
            ],
        );
    }

    #[test]
    fn keeps_colons_inside_column_names() {
        // Only the first ':' separates type from name.
        assert_eq!(parse_describe("1:A:B\u{1}"), vec![(1, "A:B".to_string())]);
    }

    #[test]
    fn skips_malformed_entries_and_empty_buffers() {
        assert_eq!(parse_describe(""), vec![]);
        assert_eq!(parse_describe("noseparator\u{1}x:NAME\u{1}"), vec![]);
    }

    #[test]
    fn no_projection_without_json_columns() {
        assert_eq!(
            json_safe_projection(&[(2, "ID".into()), (1, "NAME".into())]),
            None
        );
    }

    #[test]
    fn serializes_only_json_columns() {
        let cols = vec![
            (2, "ID".to_string()),
            (119, "META".to_string()),
            (1, "Name".to_string()),
        ];
        assert_eq!(
            json_safe_projection(&cols).unwrap(),
            "\"ID\", JSON_SERIALIZE(\"META\" RETURNING CLOB) AS \"META\", \"Name\"",
        );
    }

    #[test]
    fn quotes_hostile_column_names() {
        let cols = vec![(119, "we\"ird".to_string())];
        assert_eq!(
            json_safe_projection(&cols).unwrap(),
            "JSON_SERIALIZE(\"we\"\"ird\" RETURNING CLOB) AS \"we\"\"ird\"",
        );
    }
}
