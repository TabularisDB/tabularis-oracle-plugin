use super::*;

#[test]
fn discarding_a_preview_is_idempotent_and_prevents_start() {
    let id = "unused-preview";
    registry().lock().unwrap().previews.insert(
        id.into(),
        (
            Instant::now(),
            metadata::Preview {
                params: ConnectionParams::default(),
                target: Value::Null,
                arguments: json!([]),
                parameters: vec![],
                source: Value::Null,
                source_id: "source".into(),
            },
        ),
    );
    let command = json!({"version":1,"command":{"action":"stop","session_id":id}});
    for _ in 0..2 {
        let result = execute(&json!("discard"), &command).unwrap();
        assert_eq!(result["session"]["session_id"], id);
        assert_eq!(result["session"]["execution"], "cancelled");
        assert_eq!(result["session"]["resources"], "released");
    }
    assert!(!registry().lock().unwrap().previews.contains_key(id));
    assert!(execute(&json!("start-discarded"), &json!({"version":1,"command":{"action":"start","preview_id":id,"session_id":"discarded-session"}})).unwrap_err().message.contains("preview expired or not found"));
}

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

fn dummy_preview() -> metadata::Preview {
    metadata::Preview {
        params: ConnectionParams::default(),
        target: Value::Null,
        arguments: json!([]),
        parameters: vec![],
        source: Value::Null,
        source_id: "source".into(),
    }
}

fn insert_preview(id: &str) {
    registry()
        .lock()
        .unwrap()
        .previews
        .insert(id.into(), (Instant::now(), dummy_preview()));
}

#[test]
fn stop_against_a_reserved_start_is_recorded_instead_of_not_found() {
    let id = "reserved-start";
    {
        let mut registry = registry().lock().unwrap();
        registry.started.insert(id.into());
        registry
            .pending_starts
            .insert(id.into(), Arc::new(AtomicBool::new(false)));
    }
    let result = execute(
        &json!("stop-reserved"),
        &json!({"version":1,"command":{"action":"stop","session_id":id}}),
    )
    .unwrap();
    assert_eq!(result["session"]["execution"], "cancelled");
    assert_eq!(result["session"]["resources"], "released");
    assert!(registry()
        .lock()
        .unwrap()
        .pending_starts
        .get(id)
        .unwrap()
        .load(Ordering::Acquire));
    let continue_err = execute(
        &json!("continue-reserved"),
        &json!({"version":1,"command":{"action":"continue","session_id":id}}),
    )
    .unwrap_err();
    assert!(continue_err
        .message
        .contains("Native debug session not found"));
}

#[test]
fn stop_during_prepare_prevents_activation_and_releases_resources() {
    let preview_id = "held-preview";
    let session_id = "held-prepare";
    insert_preview(preview_id);
    let (entered, release) = hold_prepare(session_id);
    let started = std::thread::spawn(move || {
        execute(
            &json!("start-held"),
            &json!({"version":1,"command":{"action":"start","preview_id":preview_id,"session_id":session_id}}),
        )
    });
    entered
        .recv_timeout(Duration::from_secs(2))
        .expect("start must reach preparation");
    let stopped = execute(
        &json!("stop-held"),
        &json!({"version":1,"command":{"action":"stop","session_id":session_id}}),
    )
    .unwrap();
    assert_eq!(stopped["session"]["execution"], "cancelled");
    assert_eq!(stopped["session"]["resources"], "released");
    release.send(()).unwrap();
    let started = started.join().unwrap().unwrap();
    assert_eq!(started["session"]["execution"], "cancelled");
    assert_eq!(started["session"]["resources"], "released");
    let registry = registry().lock().unwrap();
    assert!(!registry.sessions.contains_key(session_id));
    assert!(registry.started.contains(session_id));
    assert!(!registry.pending_starts.contains_key(session_id));
    drop(registry);
    let replay = execute(
        &json!("replay-held"),
        &json!({"version":1,"command":{"action":"start","preview_id":preview_id,"session_id":session_id}}),
    )
    .unwrap_err();
    assert!(replay
        .message
        .contains("Session identity was already started"));
}

#[test]
fn cancel_request_marks_a_pending_start() {
    let request_id = json!(4242);
    let session_id = "cancel-pending";
    {
        let mut registry = registry().lock().unwrap();
        registry.started.insert(session_id.into());
        registry
            .pending_starts
            .insert(session_id.into(), Arc::new(AtomicBool::new(false)));
        registry.requests.insert(
            request_id.to_string(),
            Request {
                cancelled: Arc::new(AtomicBool::new(false)),
                session: Some(session_id.into()),
            },
        );
    }
    cancel_request(&request_id);
    let registry = registry().lock().unwrap();
    assert!(registry
        .requests
        .get(&request_id.to_string())
        .unwrap()
        .cancelled
        .load(Ordering::Acquire));
    assert!(registry
        .pending_starts
        .get(session_id)
        .unwrap()
        .load(Ordering::Acquire));
}
