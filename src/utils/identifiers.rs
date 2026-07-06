//! SQL identifier quoting. Oracle uses ANSI double quotes; quoted identifiers
//! are case-sensitive, which matches the exact names the catalog views return.

/// Quote an identifier with `"`, doubling any embedded double quotes to escape
/// them.
///
/// ```
/// use oracle_plugin::utils::identifiers::quote;
/// assert_eq!(quote("EMPLOYEES"), "\"EMPLOYEES\"");
/// assert_eq!(quote("weird\"name"), "\"weird\"\"name\"");
/// ```
pub fn quote(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 2);
    out.push('"');
    for c in name.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Schema-qualify a table/view/index name: `"SCHEMA"."NAME"` when a schema is
/// given, `"NAME"` otherwise.
pub fn qualify(schema: Option<&str>, name: &str) -> String {
    match schema.map(str::trim).filter(|s| !s.is_empty()) {
        Some(schema) => format!("{}.{}", quote(schema), quote(name)),
        None => quote(name),
    }
}

/// Escape a string literal for use inside single quotes in SQL.
pub fn quote_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for c in value.chars() {
        if c == '\'' {
            out.push('\'');
        }
        out.push(c);
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_plain_names() {
        assert_eq!(quote("EMPLOYEES"), "\"EMPLOYEES\"");
    }

    #[test]
    fn escapes_embedded_quotes() {
        assert_eq!(quote("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn qualifies_with_schema() {
        assert_eq!(qualify(Some("HR"), "EMPLOYEES"), "\"HR\".\"EMPLOYEES\"");
        assert_eq!(qualify(None, "EMPLOYEES"), "\"EMPLOYEES\"");
        assert_eq!(qualify(Some("  "), "T"), "\"T\"");
    }

    #[test]
    fn escapes_string_literals() {
        assert_eq!(quote_literal("O'Brien"), "'O''Brien'");
        assert_eq!(quote_literal("plain"), "'plain'");
    }
}
