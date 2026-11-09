use super::*;

#[test]
fn protocol_rejects_unknown_versions_actions_and_raw_sql() {
    for params in [
        json!({"version":2,"command":{"action":"status","session_id":"s"}}),
        json!({"version":1,"command":{"action":"execute","session_id":"s"}}),
        json!({"version":1,"command":{"action":"start","session_id":"s","preview_id":"p","sql":"DELETE FROM x"}}),
    ] {
        assert!(validate(&params).is_err());
    }
}

#[test]
fn variable_reads_cannot_evaluate_expressions() {
    for name in ["P_VALUE", "local_1", "X$VALUE"] {
        assert!(session::variable_name(name));
    }
    for name in ["", "F()", "X := 1", "PKG.F", "X;COMMIT", "1X", "X Y"] {
        assert!(!session::variable_name(name));
    }
}

#[test]
fn inputs_are_bound_and_identifiers_are_quoted() {
    let preview = metadata::Preview {
        params: ConnectionParams::default(),
        target: json!({"schema":"A\"B","name":"P","routine_type":"PROCEDURE"}),
        arguments: json!([{"position":1,"value":"'); DELETE FROM t; --"}]),
        parameters: vec![
            metadata::Parameter {
                position: 1,
                name: "P_VALUE".into(),
                data_type: "VARCHAR2".into(),
                mode: "IN".into(),
            },
            metadata::Parameter {
                position: 2,
                name: "P_OUT".into(),
                data_type: "NUMBER".into(),
                mode: "OUT".into(),
            },
        ],
        source: Value::Null,
        source_id: "source".into(),
    };
    let metadata::Invocation {
        sql,
        inputs,
        outputs,
    } = preview.invocation().unwrap();
    assert!(sql.contains("\"A\"\"B\".\"P\"(p1,p2)"));
    assert!(!sql.contains("DELETE"));
    assert_eq!(inputs[0].1.as_deref(), Some("'); DELETE FROM t; --"));
    assert_eq!(outputs[0].0, "o2");
    assert!(sql.contains(":o2 := TO_CHAR(p2, 'TM9'"));
}

#[test]
fn invalid_numeric_and_structured_inputs_are_rejected() {
    let parameter = metadata::Parameter {
        position: 1,
        name: "N".into(),
        data_type: "NUMBER".into(),
        mode: "IN".into(),
    };
    for value in [
        json!("1;COMMIT"),
        json!("NaN"),
        json!("Infinity"),
        json!({"sql":"x"}),
        json!(true),
    ] {
        assert!(metadata::scalar(&value, &parameter).is_err());
    }
    for value in [
        Value::Null,
        json!("123456789012345678901234567890"),
        json!(-1.5),
    ] {
        assert!(metadata::scalar(&value, &parameter).is_ok());
    }
}
