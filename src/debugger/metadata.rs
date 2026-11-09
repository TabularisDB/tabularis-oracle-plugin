use oracle::Connection;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use super::{invalid, text, unavailable, Result};
use crate::models::ConnectionParams;

#[derive(Clone, Debug, PartialEq)]
pub struct Parameter {
    pub position: i64,
    pub name: String,
    pub data_type: String,
    pub mode: String,
}

#[derive(Clone)]
pub struct Preview {
    pub params: ConnectionParams,
    pub target: Value,
    pub arguments: Value,
    pub parameters: Vec<Parameter>,
    pub source: Value,
    pub source_id: String,
}

pub struct Invocation {
    pub sql: String,
    pub inputs: Vec<(String, Option<String>)>,
    pub outputs: Vec<(String, Parameter)>,
}

pub fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

pub fn numeric(data_type: &str) -> bool {
    matches!(
        data_type,
        "NUMBER"
            | "BINARY_FLOAT"
            | "BINARY_DOUBLE"
            | "PLS_INTEGER"
            | "BINARY_INTEGER"
            | "PL/SQL BINARY INTEGER"
    )
}

pub fn scalar(value: &Value, parameter: &Parameter) -> Result<Option<String>> {
    if value.is_null() {
        return Ok(None);
    }
    let value = match value {
        Value::String(value) => value.clone(),
        Value::Number(value) if numeric(&parameter.data_type) => value.to_string(),
        _ => {
            return Err(invalid(
                "Only scalar text/numeric inputs and NULL are supported",
            ))
        }
    };
    if value.len() > 32767 {
        return Err(invalid(
            "Input exceeds the supported 32767-byte scalar size",
        ));
    }
    if numeric(&parameter.data_type)
        && (!value
            .chars()
            .all(|c| c.is_ascii_digit() || "+-.eE".contains(c))
            || !value.parse::<f64>().is_ok_and(f64::is_finite))
    {
        return Err(invalid(format!(
            "Invalid numeric input for {}",
            parameter.name
        )));
    }
    Ok(Some(value))
}

pub fn read(conn: &Connection, params: ConnectionParams, command: &Value) -> Result<Preview> {
    let target = command
        .get("target")
        .ok_or_else(|| invalid("Missing routine target"))?
        .clone();
    let owner = text(&target, "schema")?;
    let name = text(&target, "name")?;
    let kind = text(&target, "routine_type")?;
    if !matches!(kind, "PROCEDURE" | "FUNCTION") || name.contains('.') {
        return Err(invalid("Debugging supports standalone procedures/functions; packaged routines are not supported"));
    }
    if text(&target, "database")? != params.database.as_deref().unwrap_or("") {
        return Err(invalid(
            "Routine target belongs to a different database service",
        ));
    }
    let row = conn.query_row("SELECT o.object_id, TO_CHAR(o.last_ddl_time,'YYYYMMDDHH24MISS'), s.plsql_debug \
        FROM all_objects o JOIN all_plsql_object_settings s ON s.owner=o.owner AND s.name=o.object_name AND s.type=o.object_type \
        WHERE o.owner=:1 AND o.object_name=:2 AND o.object_type=:3 AND o.status='VALID'", &[&owner, &name, &kind])?;
    if row.get::<_, String>(2)? != "TRUE" {
        return Err(invalid("The routine has no debug information. Compile it with DEBUG explicitly outside the debugger; no recompilation is performed automatically"));
    }
    let object_id: i64 = row.get(0)?;
    let modified: String = row.get(1)?;
    let mut parameters = Vec::new();
    let mut subprogram = None;
    for row in conn.query("SELECT position, NVL(argument_name,'RETURN'), data_type, in_out, subprogram_id FROM all_arguments \
        WHERE owner=:1 AND object_name=:2 AND package_name IS NULL AND data_level=0 ORDER BY sequence", &[&owner, &name])? {
        let row = row?;
        let id: i64 = row.get(4)?;
        if subprogram.is_some_and(|previous| previous != id) { return Err(invalid("Ambiguous routine overload")); }
        subprogram = Some(id);
        let parameter = Parameter { position: row.get(0)?, name: row.get(1)?, data_type: row.get(2)?, mode: row.get(3)? };
        if !numeric(&parameter.data_type) && !matches!(parameter.data_type.as_str(), "VARCHAR2" | "NVARCHAR2" | "CHAR" | "NCHAR") {
            return Err(invalid(format!("Unsupported debugger parameter type: {}", parameter.data_type)));
        }
        parameters.push(parameter);
    }
    let identity = target
        .get("identity_args")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("Missing overload identity"))?;
    if !identity.is_empty() && Some(identity.to_string()) != subprogram.map(|id| id.to_string()) {
        return Err(invalid("The selected routine overload no longer exists"));
    }
    let arguments = command
        .get("arguments")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("Missing bound arguments"))?;
    if arguments.len() > 128 {
        return Err(invalid("Too many input arguments"));
    }
    let mut inputs = HashMap::new();
    for argument in arguments {
        let position = argument
            .get("position")
            .and_then(Value::as_i64)
            .ok_or_else(|| invalid("Invalid argument position"))?;
        let value = argument
            .get("value")
            .ok_or_else(|| invalid("Missing argument value"))?;
        if inputs.insert(position, value).is_some() {
            return Err(invalid("Duplicate argument position"));
        }
        let parameter = parameters
            .iter()
            .find(|p| p.position == position && p.mode != "OUT")
            .ok_or_else(|| invalid("Input does not match a routine IN/INOUT parameter"))?;
        scalar(value, parameter)?;
    }
    if parameters
        .iter()
        .any(|p| p.position > 0 && p.mode != "OUT" && !inputs.contains_key(&p.position))
    {
        return Err(invalid(
            "All IN/INOUT parameters require an explicit value or NULL",
        ));
    }
    let mut source = String::new();
    for row in conn.query(
        "SELECT text FROM all_source WHERE owner=:1 AND name=:2 AND type=:3 ORDER BY line",
        &[&owner, &name, &kind],
    )? {
        source.push_str(&row?.get::<_, String>(0)?);
        if source.len() > 1_048_576 {
            return Err(invalid("Routine source exceeds debugger size limit"));
        }
    }
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hash);
    let source_id = format!("{object_id}:{modified}:{:x}", hash.finish());
    let source = if source.is_empty() {
        unavailable("Routine source is not visible")
    } else {
        json!({"state":"available", "value":{"source_id":source_id,"text":source}})
    };
    Ok(Preview {
        params,
        target,
        arguments: Value::Array(arguments.clone()),
        parameters,
        source,
        source_id,
    })
}

impl Preview {
    pub fn response(&self, id: &str) -> Value {
        json!({"kind":"preview","version":1,"preview_id":id,"target":self.target,"arguments":self.arguments,"source":self.source,
            "warnings":["Oracle 19c+ standalone scalar routines only. DEBUG CONNECT SESSION and object DEBUG/EXECUTE privileges and existing COMPILE DEBUG information are required.",
                "Execution uses an isolated session with autocommit disabled. Routine COMMIT, DDL, autonomous transactions and external effects cannot be undone by stopping. Remaining uncommitted work is rolled back at cleanup; overall transaction outcome is conservatively unknown.",
                "An abandoned debugger expires after 120 seconds. Numeric values and OUT/return values are represented as text to preserve precision. No grants, compilation or persistent helper objects are installed."]})
    }

    pub fn invocation(&self) -> Result<Invocation> {
        let mut sql = if self.parameters.is_empty() {
            String::new()
        } else {
            String::from("DECLARE\n")
        };
        let mut inputs = Vec::new();
        let mut outputs = Vec::new();
        let mut arguments = Vec::new();
        for parameter in &self.parameters {
            let variable = format!("p{}", parameter.position);
            let data_type = if numeric(&parameter.data_type) {
                "NUMBER"
            } else {
                "VARCHAR2(32767)"
            };
            sql.push_str(&format!("{variable} {data_type}"));
            if parameter.position > 0 && parameter.mode != "OUT" {
                let bind = format!("i{}", parameter.position);
                let value = self
                    .arguments
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|v| v["position"].as_i64() == Some(parameter.position))
                    .ok_or_else(|| invalid("Missing input parameter"))?;
                inputs.push((bind.clone(), scalar(&value["value"], parameter)?));
                sql.push_str(&format!(" := :{bind}"));
            }
            sql.push_str(";\n");
            if parameter.position > 0 {
                arguments.push(variable.clone());
            }
            if parameter.mode != "IN" || parameter.position == 0 {
                outputs.push((format!("o{}", parameter.position), parameter.clone()));
            }
        }
        sql.push_str("BEGIN\n");
        if self.target["routine_type"] == "FUNCTION" {
            sql.push_str("p0 := ");
        }
        sql.push_str(&format!(
            "{}.{}({});\n",
            quote(text(&self.target, "schema")?),
            quote(text(&self.target, "name")?),
            arguments.join(",")
        ));
        for (bind, parameter) in &outputs {
            sql.push_str(&format!(":{bind} := p{};\n", parameter.position));
        }
        sql.push_str("END;");
        Ok(Invocation {
            sql,
            inputs,
            outputs,
        })
    }
}
