use agent_core::{ContentBlock, CoreError, StreamEvent, ToolCallAccumulator};

#[test]
fn test_accumulator_buffer_overflow_limit() {
    // 1. Create an accumulator with a small 64-byte max buffer limit
    let mut acc = ToolCallAccumulator::new(64);

    acc.process_event(&StreamEvent::ToolCallStarted {
        index: 0,
        id: Some("call_overflow_1".into()),
        name: Some("ShellExecute".into()),
    })
    .expect("Tool call start should succeed");

    // First chunk of 40 bytes: fits within 64 bytes
    let chunk_1 = "{\"command\":\"echo 12345678901234567890123\"";
    assert!(chunk_1.len() <= 64);
    acc.process_event(&StreamEvent::ToolCallDelta {
        index: 0,
        arguments_chunk: chunk_1.to_string(),
    })
    .expect("First chunk within limit should succeed");

    // Second chunk: adding 35 bytes brings total to 41 + 35 = 76 > 64 bytes
    let chunk_2 = ",\"flag\":\"overflow_trigger_payload\"}";
    let res = acc.process_event(&StreamEvent::ToolCallDelta {
        index: 0,
        arguments_chunk: chunk_2.to_string(),
    });

    assert!(res.is_err(), "Exceeding buffer limit must return an error");
    match res.unwrap_err() {
        CoreError::Accumulator(msg) => {
            assert!(msg.contains("exceeded maximum argument buffer limit"));
            assert!(msg.contains("64 bytes"));
        }
        other => panic!("Expected CoreError::Accumulator, got {other:?}"),
    }
}

#[test]
fn test_partial_json_chunks_not_prematurely_parsed() {
    let mut acc = ToolCallAccumulator::default();

    acc.process_event(&StreamEvent::ToolCallStarted {
        index: 0,
        id: Some("call_valid_step".into()),
        name: Some("FileWrite".into()),
    })
    .unwrap();

    // Partial JSON fragment 1 (unclosed string and object)
    acc.process_event(&StreamEvent::ToolCallDelta {
        index: 0,
        arguments_chunk: "{\"path\": \"src/main".into(),
    })
    .expect("Partial JSON must NOT fail during streaming accumulation");

    // Partial JSON fragment 2 (unclosed object)
    acc.process_event(&StreamEvent::ToolCallDelta {
        index: 0,
        arguments_chunk: ".rs\", \"content\": \"fn main() {}\"".into(),
    })
    .expect("Partial JSON must NOT fail during streaming accumulation");

    // Partial JSON fragment 3 (closing brace)
    acc.process_event(&StreamEvent::ToolCallDelta {
        index: 0,
        arguments_chunk: "}".into(),
    })
    .expect("Finalizing bracket must succeed");

    acc.process_event(&StreamEvent::ToolCallCompleted { index: 0 })
        .unwrap();

    // Only upon finalize is the full accumulated JSON parsed and validated
    let (msg, _) = acc
        .finalize()
        .expect("Final valid JSON must parse successfully");
    assert_eq!(msg.content.len(), 1);
    match &msg.content[0] {
        ContentBlock::ToolUse { name, input, .. } => {
            assert_eq!(name, "FileWrite");
            assert_eq!(input["path"], "src/main.rs");
            assert_eq!(input["content"], "fn main() {}");
        }
        _ => panic!("Expected ToolUse"),
    }
}

#[test]
fn test_malformed_final_json_returns_tool_validation_error() {
    let mut acc = ToolCallAccumulator::default();

    acc.process_event(&StreamEvent::ToolCallStarted {
        index: 0,
        id: Some("call_bad_json".into()),
        name: Some("FileEdit".into()),
    })
    .unwrap();

    // Send broken / unparseable JSON syntax
    acc.process_event(&StreamEvent::ToolCallDelta {
        index: 0,
        arguments_chunk: "{\"path\": \"test.rs\", broken_unquoted_syntax: [{{".into(),
    })
    .expect("Buffering broken JSON chunk must succeed without premature crash");

    acc.process_event(&StreamEvent::ToolCallCompleted { index: 0 })
        .unwrap();

    // Finalize must fail with CoreError::ToolValidation
    let finalize_res = acc.finalize();
    assert!(
        finalize_res.is_err(),
        "Finalizing invalid JSON must return error"
    );
    match finalize_res.unwrap_err() {
        CoreError::ToolValidation(err_msg) => {
            assert!(err_msg.contains("Failed to parse tool call 'FileEdit' arguments JSON"));
            assert!(err_msg.contains("broken_unquoted_syntax"));
        }
        other => panic!("Expected CoreError::ToolValidation, got {other:?}"),
    }
}

#[test]
fn test_multiple_interleaved_tool_calls_sorted_deterministically() {
    let mut acc = ToolCallAccumulator::default();

    // Interleave tool call 1 and tool call 0 in reverse order
    acc.process_event(&StreamEvent::ToolCallStarted {
        index: 1,
        id: Some("call_second".into()),
        name: Some("SecondTool".into()),
    })
    .unwrap();

    acc.process_event(&StreamEvent::ToolCallStarted {
        index: 0,
        id: Some("call_first".into()),
        name: Some("FirstTool".into()),
    })
    .unwrap();

    acc.process_event(&StreamEvent::ToolCallDelta {
        index: 1,
        arguments_chunk: "{\"order\": 2}".into(),
    })
    .unwrap();

    acc.process_event(&StreamEvent::ToolCallDelta {
        index: 0,
        arguments_chunk: "{\"order\": 1}".into(),
    })
    .unwrap();

    acc.process_event(&StreamEvent::ToolCallCompleted { index: 1 })
        .unwrap();
    acc.process_event(&StreamEvent::ToolCallCompleted { index: 0 })
        .unwrap();

    let (msg, _) = acc.finalize().unwrap();
    assert_eq!(msg.content.len(), 2);

    // Final message must preserve ordered indices: index 0 before index 1
    match (&msg.content[0], &msg.content[1]) {
        (
            ContentBlock::ToolUse {
                name: n0,
                input: i0,
                ..
            },
            ContentBlock::ToolUse {
                name: n1,
                input: i1,
                ..
            },
        ) => {
            assert_eq!(n0, "FirstTool");
            assert_eq!(i0["order"], 1);
            assert_eq!(n1, "SecondTool");
            assert_eq!(i1["order"], 2);
        }
        _ => panic!("Expected ordered ToolUse blocks"),
    }
}

#[test]
fn test_empty_arguments_fallbacks_to_empty_json_object() {
    let mut acc = ToolCallAccumulator::default();

    acc.process_event(&StreamEvent::ToolCallStarted {
        index: 0,
        id: Some("call_empty_args".into()),
        name: Some("ListDirectory".into()),
    })
    .unwrap();

    acc.process_event(&StreamEvent::ToolCallCompleted { index: 0 })
        .unwrap();

    let (msg, _) = acc.finalize().unwrap();
    assert_eq!(msg.content.len(), 1);
    match &msg.content[0] {
        ContentBlock::ToolUse { name, input, .. } => {
            assert_eq!(name, "ListDirectory");
            assert_eq!(input, &serde_json::json!({}));
        }
        _ => panic!("Expected ToolUse"),
    }
}

#[test]
fn test_stream_error_aborts_accumulation() {
    let mut acc = ToolCallAccumulator::default();

    acc.process_event(&StreamEvent::TextDelta {
        text: "Partial text ".into(),
    })
    .unwrap();

    let err_res = acc.process_event(&StreamEvent::StreamError {
        error: "Mid-stream connection reset by peer".into(),
    });

    assert!(err_res.is_err());
    match err_res.unwrap_err() {
        CoreError::Accumulator(msg) => {
            assert!(msg.contains("Mid-stream connection reset by peer"));
        }
        other => panic!("Expected CoreError::Accumulator, got {other:?}"),
    }
}
