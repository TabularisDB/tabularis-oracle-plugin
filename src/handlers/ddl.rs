//! DDL generation and execution.
//!
//! The `get_*_sql` methods generate SQL from the definitions the host sends
//! (its `ColumnDefinition` model) and return a **list** of statements the host
//! may show before running them through `execute_query`. Oracle supports the
//! full set — MODIFY column, ADD CONSTRAINT ... FOREIGN KEY, DROP CONSTRAINT —
//! with one caveat: foreign keys have no ON UPDATE action.

use serde_json::{json, Value};

use crate::error::PluginError;
use crate::handlers::{connect, opt_str, req_str, req_str_any, respond};
use crate::utils::identifiers::{qualify, quote, quote_literal};

pub fn get_create_table_sql(id: Value, params: &Value) -> Value {
    respond(id, {
        let schema = opt_str(params, "schema");
        req_str_any(params, &["table_name", "table"]).and_then(|table| {
            let columns = params
                .get("columns")
                .and_then(Value::as_array)
                .ok_or_else(|| PluginError::invalid_params("missing 'columns' array"))?;
            let sql = build_create_table_sql(schema.as_deref(), &table, columns)?;
            Ok(json!([sql]))
        })
    })
}

pub fn get_add_column_sql(id: Value, params: &Value) -> Value {
    respond(id, {
        let schema = opt_str(params, "schema");
        req_str(params, "table").and_then(|table| {
            let column = params
                .get("column")
                .ok_or_else(|| PluginError::invalid_params("missing 'column' definition"))?;
            let sql = format!(
                "ALTER TABLE {} ADD ({})",
                qualify(schema.as_deref(), &table),
                column_spec(column)?
            );
            Ok(json!([sql]))
        })
    })
}

pub fn get_alter_column_sql(id: Value, params: &Value) -> Value {
    respond(id, {
        let schema = opt_str(params, "schema");
        req_str(params, "table").and_then(|table| {
            let old_column = params.get("old_column");
            let new_column = params
                .get("new_column")
                .or_else(|| params.get("column"))
                .ok_or_else(|| PluginError::invalid_params("missing 'new_column' definition"))?;
            let statements =
                build_alter_column_sql(schema.as_deref(), &table, old_column, new_column)?;
            Ok(json!(statements))
        })
    })
}

pub fn get_create_index_sql(id: Value, params: &Value) -> Value {
    respond(id, {
        let schema = opt_str(params, "schema");
        req_str(params, "table").and_then(|table| {
            let index_name = req_str_any(params, &["index_name", "name"])?;
            let columns: Vec<String> = params
                .get("columns")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(quote)
                        .collect()
                })
                .unwrap_or_default();
            if columns.is_empty() {
                return Err(PluginError::invalid_params(
                    "index definition needs at least one column",
                ));
            }
            let unique = params
                .get("is_unique")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            // The index lives in the same schema as the table.
            let sql = format!(
                "CREATE {}INDEX {} ON {} ({})",
                if unique { "UNIQUE " } else { "" },
                qualify(schema.as_deref(), &index_name),
                qualify(schema.as_deref(), &table),
                columns.join(", "),
            );
            Ok(json!([sql]))
        })
    })
}

pub fn get_create_foreign_key_sql(id: Value, params: &Value) -> Value {
    respond(id, {
        let schema = opt_str(params, "schema");
        req_str(params, "table").and_then(|table| {
            let column = req_str_any(params, &["column", "column_name"])?;
            let ref_table = req_str_any(params, &["ref_table", "referenced_table"])?;
            let ref_column = req_str_any(params, &["ref_column", "referenced_column"])?;
            let fk_name = opt_str(params, "fk_name")
                .or_else(|| opt_str(params, "constraint_name"))
                .unwrap_or_else(|| format!("FK_{}_{}", table, column));

            let mut sql = format!(
                "ALTER TABLE {} ADD CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({})",
                qualify(schema.as_deref(), &table),
                quote(&fk_name),
                quote(&column),
                qualify(schema.as_deref(), &ref_table),
                quote(&ref_column),
            );
            // Oracle supports ON DELETE CASCADE / SET NULL only (no ON UPDATE;
            // NO ACTION / RESTRICT is the default and takes no clause).
            if let Some(on_delete) = opt_str(params, "on_delete") {
                match on_delete.to_ascii_uppercase().as_str() {
                    "CASCADE" => sql.push_str(" ON DELETE CASCADE"),
                    "SET NULL" => sql.push_str(" ON DELETE SET NULL"),
                    _ => {}
                }
            }
            Ok(json!([sql]))
        })
    })
}

pub fn drop_index(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|client| {
            let schema = opt_str(params, "schema");
            let index_name = req_str(params, "index_name")?;
            client.execute(
                &format!("DROP INDEX {}", qualify(schema.as_deref(), &index_name)),
                &[],
            )?;
            Ok(Value::Null)
        })
    })
}

pub fn drop_foreign_key(id: Value, params: &Value) -> Value {
    respond(id, {
        connect(params).and_then(|client| {
            let schema = opt_str(params, "schema");
            let table = req_str(params, "table")?;
            let constraint = req_str_any(params, &["fk_name", "constraint_name"])?;
            client.execute(
                &format!(
                    "ALTER TABLE {} DROP CONSTRAINT {}",
                    qualify(schema.as_deref(), &table),
                    quote(&constraint)
                ),
                &[],
            )?;
            Ok(Value::Null)
        })
    })
}

// ---------------------------------------------------------------------------
// Pure SQL builders (operate on the host's ColumnDefinition JSON shape:
// { name, data_type, is_nullable, is_pk, is_auto_increment, default_value })
// ---------------------------------------------------------------------------

fn def_str(def: &Value, key: &str) -> Option<String> {
    def.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn def_bool(def: &Value, key: &str) -> bool {
    def.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// Render a default value string for DDL: numbers and SQL expressions pass
/// through raw, everything else becomes a quoted literal.
fn render_default(value: &str) -> String {
    let upper = value.trim().to_ascii_uppercase();
    let is_expression = matches!(
        upper.as_str(),
        "NULL" | "SYSDATE" | "SYSTIMESTAMP" | "CURRENT_DATE" | "CURRENT_TIMESTAMP" | "LOCALTIMESTAMP"
    ) || upper.ends_with(".NEXTVAL")
        || upper == "SYS_GUID()";
    if is_expression || value.trim().parse::<f64>().is_ok() {
        value.trim().to_string()
    } else {
        quote_literal(value)
    }
}

/// The `"NAME" TYPE [GENERATED ...] [DEFAULT x] [NOT NULL]` fragment shared by
/// CREATE TABLE, ADD and MODIFY. Oracle requires DEFAULT before NOT NULL, and
/// identity columns cannot also carry a DEFAULT.
fn column_spec(column: &Value) -> Result<String, PluginError> {
    let name = def_str(column, "name")
        .ok_or_else(|| PluginError::invalid_params("column definition needs a 'name'"))?;
    let data_type = def_str(column, "data_type").unwrap_or_else(|| "VARCHAR2(255)".to_string());

    let mut spec = format!("{} {}", quote(&name), data_type);

    let identity = def_bool(column, "is_auto_increment");
    if identity {
        spec.push_str(" GENERATED BY DEFAULT AS IDENTITY");
    } else if let Some(default) = def_str(column, "default_value") {
        spec.push_str(&format!(" DEFAULT {}", render_default(&default)));
    }

    let nullable = column
        .get("is_nullable")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if !nullable {
        spec.push_str(" NOT NULL");
    }

    Ok(spec)
}

fn build_create_table_sql(
    schema: Option<&str>,
    table: &str,
    columns: &[Value],
) -> Result<String, PluginError> {
    if columns.is_empty() {
        return Err(PluginError::invalid_params(
            "table definition needs at least one column",
        ));
    }

    let mut parts = Vec::with_capacity(columns.len() + 1);
    for column in columns {
        parts.push(format!("  {}", column_spec(column)?));
    }

    let pk_columns: Vec<String> = columns
        .iter()
        .filter(|c| def_bool(c, "is_pk"))
        .filter_map(|c| def_str(c, "name"))
        .map(|n| quote(&n))
        .collect();
    if !pk_columns.is_empty() {
        parts.push(format!(
            "  CONSTRAINT {} PRIMARY KEY ({})",
            quote(&format!("PK_{table}")),
            pk_columns.join(", ")
        ));
    }

    Ok(format!(
        "CREATE TABLE {} (\n{}\n)",
        qualify(schema, table),
        parts.join(",\n")
    ))
}

/// MODIFY (and RENAME if the name changed). Nullability is only emitted when
/// it actually changes — Oracle raises ORA-01442/01451 when re-declaring the
/// current nullability.
fn build_alter_column_sql(
    schema: Option<&str>,
    table: &str,
    old_column: Option<&Value>,
    new_column: &Value,
) -> Result<Vec<String>, PluginError> {
    let new_name = def_str(new_column, "name")
        .ok_or_else(|| PluginError::invalid_params("column definition needs a 'name'"))?;
    let qualified_table = qualify(schema, table);

    let mut statements = Vec::new();

    if let Some(old_name) = old_column.and_then(|c| def_str(c, "name")) {
        if old_name != new_name {
            statements.push(format!(
                "ALTER TABLE {} RENAME COLUMN {} TO {}",
                qualified_table,
                quote(&old_name),
                quote(&new_name)
            ));
        }
    }

    let data_type = def_str(new_column, "data_type").unwrap_or_else(|| "VARCHAR2(255)".to_string());
    let mut spec = format!("{} {}", quote(&new_name), data_type);

    if let Some(default) = def_str(new_column, "default_value") {
        spec.push_str(&format!(" DEFAULT {}", render_default(&default)));
    }

    let new_nullable = new_column
        .get("is_nullable")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let old_nullable = old_column
        .and_then(|c| c.get("is_nullable"))
        .and_then(Value::as_bool);
    if old_nullable.map(|o| o != new_nullable).unwrap_or(false) {
        spec.push_str(if new_nullable { " NULL" } else { " NOT NULL" });
    }

    statements.push(format!("ALTER TABLE {qualified_table} MODIFY ({spec})"));
    Ok(statements)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn create_table_with_pk_and_identity() {
        let columns = vec![
            json!({ "name": "ID", "data_type": "NUMBER", "is_nullable": false, "is_pk": true, "is_auto_increment": true, "default_value": null }),
            json!({ "name": "NAME", "data_type": "VARCHAR2(100)", "is_nullable": false, "is_pk": false, "is_auto_increment": false, "default_value": null }),
        ];
        assert_eq!(
            build_create_table_sql(Some("HR"), "PEOPLE", &columns).unwrap(),
            "CREATE TABLE \"HR\".\"PEOPLE\" (\n  \"ID\" NUMBER GENERATED BY DEFAULT AS IDENTITY NOT NULL,\n  \"NAME\" VARCHAR2(100) NOT NULL,\n  CONSTRAINT \"PK_PEOPLE\" PRIMARY KEY (\"ID\")\n)"
        );
    }

    #[test]
    fn create_table_requires_columns() {
        assert!(build_create_table_sql(None, "T", &[]).is_err());
    }

    #[test]
    fn column_spec_with_default_and_not_null() {
        let col = json!({
            "name": "STATUS", "data_type": "VARCHAR2(20)",
            "is_nullable": false, "default_value": "active"
        });
        assert_eq!(
            column_spec(&col).unwrap(),
            "\"STATUS\" VARCHAR2(20) DEFAULT 'active' NOT NULL"
        );
    }

    #[test]
    fn defaults_pass_expressions_and_quote_strings() {
        assert_eq!(render_default("SYSDATE"), "SYSDATE");
        assert_eq!(render_default("42"), "42");
        assert_eq!(render_default("3.5"), "3.5");
        assert_eq!(render_default("O'Brien"), "'O''Brien'");
        assert_eq!(render_default("active"), "'active'");
    }

    #[test]
    fn alter_column_rename_and_retype() {
        let old = json!({ "name": "NAME", "data_type": "VARCHAR2(100)", "is_nullable": true });
        let new = json!({ "name": "FULL_NAME", "data_type": "VARCHAR2(200)", "is_nullable": true });
        let stmts = build_alter_column_sql(Some("HR"), "T", Some(&old), &new).unwrap();
        assert_eq!(
            stmts,
            vec![
                "ALTER TABLE \"HR\".\"T\" RENAME COLUMN \"NAME\" TO \"FULL_NAME\"".to_string(),
                "ALTER TABLE \"HR\".\"T\" MODIFY (\"FULL_NAME\" VARCHAR2(200))".to_string(),
            ]
        );
    }

    #[test]
    fn alter_column_emits_nullability_only_on_change() {
        let old = json!({ "name": "A", "data_type": "NUMBER", "is_nullable": true });
        let new = json!({ "name": "A", "data_type": "NUMBER", "is_nullable": false });
        let stmts = build_alter_column_sql(None, "T", Some(&old), &new).unwrap();
        assert_eq!(stmts, vec!["ALTER TABLE \"T\" MODIFY (\"A\" NUMBER NOT NULL)".to_string()]);

        let unchanged = json!({ "name": "A", "data_type": "NUMBER", "is_nullable": true });
        let stmts =
            build_alter_column_sql(None, "T", Some(&unchanged), &unchanged.clone()).unwrap();
        assert_eq!(stmts, vec!["ALTER TABLE \"T\" MODIFY (\"A\" NUMBER)".to_string()]);
    }
}
