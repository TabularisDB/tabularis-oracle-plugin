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

#[test]
fn breakpoint_removal_failures_keep_handles_and_do_not_add_duplicates() {
    let bp = |line| json!({"source_id":"source","line":line});
    let mut tracked = vec![(1, bp(1)), (2, bp(2)), (3, bp(3))];
    let mut deleted = vec![];
    let mut added = vec![];
    let result = session::replace_breakpoints(
        &mut tracked,
        &[bp(2), bp(4), bp(4)],
        |id| {
            deleted.push(id);
            match id {
                1 => Ok(()),
                2 => Err(invalid("Connection call failed")),
                _ => Err(invalid("Native delete status rejected")),
            }
        },
        |value| {
            added.push(value.clone());
            Ok(4)
        },
    );
    assert_eq!(deleted, vec![1, 2, 3]);
    assert_eq!(added, vec![bp(4)]);
    assert_eq!(tracked, vec![(2, bp(2)), (3, bp(3)), (4, bp(4))]);
    assert_eq!(result["state"], "partial");
    assert_eq!(result["value"], json!([bp(2), bp(3), bp(4)]));
    let reason = result["reason"].as_str().unwrap();
    assert!(reason.contains("breakpoint 2"));
    assert!(reason.contains("breakpoint 3"));

    let mut retried = vec![];
    let result = session::replace_breakpoints(
        &mut tracked,
        &[],
        |id| {
            retried.push(id);
            Ok(())
        },
        |_| panic!("Clearing breakpoints must not add any"),
    );
    assert_eq!(retried, vec![2, 3, 4]);
    assert!(tracked.is_empty());
    assert_eq!(result, available(json!([])));
}

#[test]
fn breakpoint_addition_failure_reports_successful_changes_in_snapshot() {
    let bp = |line| json!({"source_id":"source","line":line});
    let mut tracked = vec![(1, bp(1))];
    let mut added = vec![];
    let result = session::replace_breakpoints(
        &mut tracked,
        &[bp(2), bp(3), bp(3), bp(4)],
        |_| Ok(()),
        |value| {
            added.push(value.clone());
            if value["line"] == 3 {
                Err(invalid("Native add call failed"))
            } else {
                Ok(value["line"].as_i64().unwrap())
            }
        },
    );
    assert_eq!(added, vec![bp(2), bp(3), bp(4)]);
    assert_eq!(tracked, vec![(2, bp(2)), (4, bp(4))]);
    assert_eq!(result["state"], "partial");
    assert_eq!(result["value"], json!([bp(2), bp(4)]));
    assert!(result["reason"].as_str().unwrap().contains("Line 3"));
}
