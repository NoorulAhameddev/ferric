mod common;

use agent_core::{
    CompletionRequest, ContentBlock, LlmProvider, Message, ProviderError, ReasoningConfig,
    ReasoningEffort, Role, ToolCallAccumulator, ToolDefinition,
};
use agent_providers::OpenAiProvider;
use common::{MockResponse, MockServer};
use futures::StreamExt;
use serde_json::json;

#[tokio::test]
async fn test_openai_stream_fragmented_function_arguments_across_7_chunks() {
    // 7 chunks delivering function arguments incrementally
    let sse_events = vec![
        // Chunk 0: Tool call definition with empty arguments
        "data: {\"id\":\"chatcmpl-frag-1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"tool_calls\":[{\"index\":0,\"id\":\"call_exec_001\",\"type\":\"function\",\"function\":{\"name\":\"ShellExecute\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n".to_string(),
        // Chunk 1: arg fragment 1
        "data: {\"id\":\"chatcmpl-frag-1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"command\\\":\"}}]},\"finish_reason\":null}]}\n\n".to_string(),
        // Chunk 2: arg fragment 2
        "data: {\"id\":\"chatcmpl-frag-1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"cargo test --\"}}]},\"finish_reason\":null}]}\n\n".to_string(),
        // Chunk 3: arg fragment 3
        "data: {\"id\":\"chatcmpl-frag-1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"workspace\\\",\\\"timeout_sec\\\":\"}}]},\"finish_reason\":null}]}\n\n".to_string(),
        // Chunk 4: arg fragment 4
        "data: {\"id\":\"chatcmpl-frag-1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"60,\\\"background\\\":\"}}]},\"finish_reason\":null}]}\n\n".to_string(),
        // Chunk 5: arg fragment 5
        "data: {\"id\":\"chatcmpl-frag-1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"false}\"}}]},\"finish_reason\":null}]}\n\n".to_string(),
        // Chunk 6: finish_reason: "tool_calls"
        "data: {\"id\":\"chatcmpl-frag-1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n".to_string(),
        // Chunk 7: stream usage
        "data: {\"id\":\"chatcmpl-frag-1\",\"choices\":[],\"usage\":{\"prompt_tokens\":120,\"completion_tokens\":45,\"total_tokens\":165}}\n\n".to_string(),
        // Done marker
        "data: [DONE]\n\n".to_string(),
    ];

    let server = MockServer::start(vec![MockResponse::sse_chunks(sse_events)]).await;
    let provider = OpenAiProvider::new("sk-test-key").with_base_url(server.base_url());

    let req = CompletionRequest {
        model: "gpt-4o".to_string(),
        messages: vec![Message::user_text("Run workspace tests")],
        tools: vec![ToolDefinition {
            name: "ShellExecute".into(),
            description: "Execute a shell command".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "timeout_sec": {"type": "number"},
                    "background": {"type": "boolean"}
                },
                "required": ["command"]
            }),
        }],
        system_prompt: Some("You are a CLI agent.".to_string()),
        generation: Default::default(),
        reasoning: None,
        cache_policy: None,
        extensions: Default::default(),
    };

    let mut stream = provider.stream(req).await.unwrap();
    let mut acc = ToolCallAccumulator::default();

    while let Some(ev) = stream.next().await {
        acc.process_event(&ev.unwrap()).unwrap();
    }

    let (msg, usage) = acc.finalize().unwrap();
    assert_eq!(msg.role, Role::Assistant);
    assert_eq!(msg.content.len(), 1);

    match &msg.content[0] {
        ContentBlock::ToolUse { id, name, input } => {
            assert_eq!(id, "call_exec_001");
            assert_eq!(name, "ShellExecute");
            assert_eq!(input["command"], "cargo test --workspace");
            assert_eq!(input["timeout_sec"], 60);
            assert_eq!(input["background"], false);
        }
        _ => panic!("Expected ContentBlock::ToolUse"),
    }

    assert_eq!(usage.unwrap().output_tokens, 45);

    // Verify request payload on wire
    let reqs = server.recorded_requests();
    assert_eq!(reqs.len(), 1);
    let r = &reqs[0];
    assert_eq!(r.method, "POST");
    assert_eq!(r.path, "/chat/completions");
    assert_eq!(r.header("authorization").unwrap(), "Bearer sk-test-key");

    let body = r.json_body();
    assert_eq!(body["model"], "gpt-4o");
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
}

#[tokio::test]
async fn test_openai_reasoning_models_o1_o3_with_reasoning_effort() {
    let sse_events = vec![
        "data: {\"id\":\"chatcmpl-o1-1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Let's think through this step by step. Solution: 42.\"},\"finish_reason\":\"stop\"}]}\n\n".to_string(),
        "data: [DONE]\n\n".to_string(),
    ];

    let server = MockServer::start(vec![MockResponse::sse_chunks(sse_events)]).await;
    let provider = OpenAiProvider::new("sk-test-key").with_base_url(server.base_url());

    // 1. Verify capability reporting for o1 and o3 models
    let cap_o1 = provider.capabilities("o1");
    assert_eq!(cap_o1.max_context_tokens, 200_000);
    assert_eq!(cap_o1.max_output_tokens, 64_000);

    let cap_o3 = provider.capabilities("o3-mini");
    assert_eq!(cap_o3.max_context_tokens, 200_000);
    assert_eq!(cap_o3.max_output_tokens, 64_000);

    let cap_gpt4 = provider.capabilities("gpt-4o");
    assert_eq!(cap_gpt4.max_context_tokens, 128_000);
    assert_eq!(cap_gpt4.max_output_tokens, 16_384);

    // 2. Stream with reasoning_effort: High
    let req = CompletionRequest {
        model: "o3-mini".to_string(),
        messages: vec![Message::user_text("Solve complex problem")],
        tools: vec![],
        system_prompt: None,
        generation: Default::default(),
        reasoning: Some(ReasoningConfig {
            effort: Some(ReasoningEffort::High),
            token_budget: None,
        }),
        cache_policy: None,
        extensions: Default::default(),
    };

    let mut stream = provider.stream(req).await.unwrap();
    let mut acc = ToolCallAccumulator::default();

    while let Some(ev) = stream.next().await {
        acc.process_event(&ev.unwrap()).unwrap();
    }

    let (msg, _) = acc.finalize().unwrap();
    assert_eq!(
        msg.content,
        vec![ContentBlock::Text {
            text: "Let's think through this step by step. Solution: 42.".into()
        }]
    );

    // Verify wire payload has reasoning_effort: "high"
    let reqs = server.recorded_requests();
    assert_eq!(reqs.len(), 1);
    let body = reqs[0].json_body();
    assert_eq!(body["model"], "o3-mini");
    assert_eq!(body["reasoning_effort"], "high");
}

#[tokio::test]
async fn test_openai_http_429_rate_limit_with_retry_after() {
    let server = MockServer::start(vec![
        MockResponse::error_with_retry_after(
            429,
            20,
            json!({
                "error": {
                    "message": "Rate limit reached for requests",
                    "type": "requests",
                    "code": "rate_limit_exceeded"
                }
            }),
        ),
        MockResponse::error_with_retry_after(
            429,
            20,
            json!({
                "error": {
                    "message": "Rate limit reached for requests",
                    "type": "requests",
                    "code": "rate_limit_exceeded"
                }
            }),
        ),
    ])
    .await;

    let provider = OpenAiProvider::new("sk-test").with_base_url(server.base_url());
    let req = CompletionRequest {
        model: "gpt-4o".to_string(),
        messages: vec![Message::user_text("test")],
        tools: vec![],
        system_prompt: None,
        generation: Default::default(),
        reasoning: None,
        cache_policy: None,
        extensions: Default::default(),
    };

    let stream_res = provider.stream(req.clone()).await;
    match stream_res {
        Err(ProviderError::RateLimitExceeded(msg)) => {
            assert!(msg.contains("retry-after: 20"));
            assert!(msg.contains("rate_limit_exceeded"));
        }
        _ => panic!("Expected RateLimitExceeded"),
    }

    let comp_err = provider.complete(req).await.unwrap_err();
    match comp_err {
        ProviderError::RateLimitExceeded(msg) => {
            assert!(msg.contains("retry-after: 20"));
        }
        other => panic!("Expected RateLimitExceeded, got {other:?}"),
    }
}

#[tokio::test]
async fn test_openai_http_500_and_503_server_errors() {
    let server = MockServer::start(vec![
        MockResponse::status_only(500, "Internal Server Error", "OpenAI internal error"),
        MockResponse::status_only(503, "Service Unavailable", "Capacity temporarily degraded"),
    ])
    .await;

    let provider = OpenAiProvider::new("sk-test").with_base_url(server.base_url());
    let req = CompletionRequest {
        model: "gpt-4o".to_string(),
        messages: vec![Message::user_text("test")],
        tools: vec![],
        system_prompt: None,
        generation: Default::default(),
        reasoning: None,
        cache_policy: None,
        extensions: Default::default(),
    };

    let err_500 = match provider.stream(req.clone()).await {
        Err(e) => e,
        Ok(_) => panic!("Expected error"),
    };
    assert!(matches!(err_500, ProviderError::NetworkError(_)));

    let err_503 = match provider.stream(req).await {
        Err(e) => e,
        Ok(_) => panic!("Expected error"),
    };
    assert!(matches!(err_503, ProviderError::NetworkError(_)));
}

#[tokio::test]
async fn test_openai_multi_turn_tool_roundtrip() {
    let sse_events = vec![
        "data: {\"id\":\"chatcmpl-multi-1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"I have inspected the test output and everything passed.\"},\"finish_reason\":\"stop\"}]}\n\n".to_string(),
        "data: [DONE]\n\n".to_string(),
    ];

    let server = MockServer::start(vec![MockResponse::sse_chunks(sse_events)]).await;
    let provider = OpenAiProvider::new("sk-test").with_base_url(server.base_url());

    let req = CompletionRequest {
        model: "gpt-4o".to_string(),
        messages: vec![
            Message::user_text("Run the tests"),
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "call_cmd_1".into(),
                    name: "ShellExecute".into(),
                    input: json!({"command": "cargo test"}),
                }],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_cmd_1".into(),
                    content: "test result: ok. 45 passed".into(),
                    is_error: false,
                }],
            },
        ],
        tools: vec![],
        system_prompt: None,
        generation: Default::default(),
        reasoning: None,
        cache_policy: None,
        extensions: Default::default(),
    };

    let mut stream = provider.stream(req).await.unwrap();
    let mut acc = ToolCallAccumulator::default();

    while let Some(ev) = stream.next().await {
        acc.process_event(&ev.unwrap()).unwrap();
    }

    let (msg, _) = acc.finalize().unwrap();
    assert_eq!(
        msg.content,
        vec![ContentBlock::Text {
            text: "I have inspected the test output and everything passed.".into()
        }]
    );

    // Verify wire messages transformation
    let reqs = server.recorded_requests();
    let body = reqs[0].json_body();
    let wire_msgs = body["messages"].as_array().unwrap();
    assert_eq!(wire_msgs.len(), 3);
    assert_eq!(wire_msgs[0]["role"], "user");
    assert_eq!(wire_msgs[1]["role"], "assistant");
    assert!(wire_msgs[1]["tool_calls"].is_array());
    assert_eq!(wire_msgs[2]["role"], "tool");
    assert_eq!(wire_msgs[2]["tool_call_id"], "call_cmd_1");
}
