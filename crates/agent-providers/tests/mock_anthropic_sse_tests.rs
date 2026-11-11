mod common;

use agent_core::{
    CachePolicy, CompletionRequest, ContentBlock, CoreError, LlmProvider, Message, ProviderError,
    ReasoningConfig, StreamEvent, ToolCallAccumulator, ToolDefinition,
};
use agent_providers::AnthropicProvider;
use common::{MockResponse, MockServer};
use futures::StreamExt;
use serde_json::json;

#[tokio::test]
async fn test_anthropic_multi_turn_prompt_caching_and_thinking_blocks() {
    // 1. Set up mock SSE response with thinking and text blocks
    let sse_events = vec![
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_cache_think_1\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-3-7-sonnet-20250219\"}}\n\n".to_string(),
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n".to_string(),
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Let me analyze the multi-turn context and cached data... \"}}\n\n".to_string(),
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Context verified.\\n\"}}\n\n".to_string(),
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n".to_string(),
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n".to_string(),
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello! I have loaded your cached project context.\"}}\n\n".to_string(),
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":1}\n\n".to_string(),
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":42}}\n\n".to_string(),
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string(),
    ];

    let server = MockServer::start(vec![MockResponse::sse_chunks(sse_events)]).await;
    let provider = AnthropicProvider::new("test-key").with_base_url(server.base_url());

    // 2. Multi-turn conversation with prompt caching and reasoning enabled
    let req = CompletionRequest {
        model: "claude-3-7-sonnet-20250219".to_string(),
        messages: vec![
            Message::user_text("Turn 1: Initialize context"),
            Message::assistant_text("Initialized context successfully."),
            Message::user_text("Turn 2: Continue with cached thinking"),
        ],
        tools: vec![],
        system_prompt: Some("You are an expert engineer.".to_string()),
        generation: Default::default(),
        reasoning: Some(ReasoningConfig {
            effort: None,
            token_budget: Some(4096),
        }),
        cache_policy: Some(CachePolicy::Prefer),
        extensions: Default::default(),
    };

    let mut stream = provider.stream(req).await.expect("Stream should start");
    let mut acc = ToolCallAccumulator::default();

    while let Some(ev_res) = stream.next().await {
        let ev = ev_res.expect("Event should parse");
        acc.process_event(&ev)
            .expect("Accumulator should process event");
    }

    let (msg, usage) = acc.finalize().expect("Finalize should succeed");
    assert_eq!(msg.content.len(), 1);
    if let ContentBlock::Text { text } = &msg.content[0] {
        assert!(text.contains("cached data"));
        assert!(text.contains("Hello! I have loaded your cached project context."));
    } else {
        panic!("Expected Text content block");
    }

    assert_eq!(usage.unwrap().output_tokens, 42);

    // 3. Verify wire headers & payload on server
    let reqs = server.recorded_requests();
    assert_eq!(reqs.len(), 1);
    let r = &reqs[0];
    assert_eq!(r.method, "POST");
    assert_eq!(r.path, "/v1/messages");
    assert_eq!(r.header("x-api-key").unwrap(), "test-key");

    let beta_header = r
        .header("anthropic-beta")
        .expect("Beta header must be present");
    assert!(beta_header.contains("prompt-caching-2024-07-25"));
    assert!(beta_header.contains("interleaved-thinking-2024-11-20"));

    let body = r.json_body();
    assert_eq!(body["thinking"]["type"], "enabled");
    assert_eq!(body["thinking"]["budget_tokens"], 4096);
    assert_eq!(body["messages"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn test_anthropic_fragmented_tool_use_deltas_and_clean_completion() {
    let sse_events = vec![
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_tool_frag_1\",\"type\":\"message\",\"role\":\"assistant\"}}\n\n".to_string(),
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_edit_789\",\"name\":\"FileEdit\",\"input\":{}}}\n\n".to_string(),
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"file_path\\\":\\\"\"}}\n\n".to_string(),
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"crates/agent-core/src/\"}}\n\n".to_string(),
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"lib.rs\\\",\\\"old_str\\\":\\\"fn main()\"}}\n\n".to_string(),
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\",\\\"new_str\\\":\\\"fn main() { init(); }\\\"}\"}}\n\n".to_string(),
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n".to_string(),
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":75}}\n\n".to_string(),
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n".to_string(),
    ];

    let server = MockServer::start(vec![MockResponse::sse_chunks(sse_events)]).await;
    let provider = AnthropicProvider::new("test-key").with_base_url(server.base_url());

    let req = CompletionRequest {
        model: "claude-3-5-sonnet-20241022".to_string(),
        messages: vec![Message::user_text("Edit lib.rs")],
        tools: vec![ToolDefinition {
            name: "FileEdit".into(),
            description: "Edit file".into(),
            input_schema: json!({"type": "object"}),
        }],
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

    let (msg, usage) = acc.finalize().unwrap();
    assert_eq!(msg.content.len(), 1);
    match &msg.content[0] {
        ContentBlock::ToolUse { id, name, input } => {
            assert_eq!(id, "toolu_edit_789");
            assert_eq!(name, "FileEdit");
            assert_eq!(input["file_path"], "crates/agent-core/src/lib.rs");
            assert_eq!(input["old_str"], "fn main()");
            assert_eq!(input["new_str"], "fn main() { init(); }");
        }
        _ => panic!("Expected ToolUse content block"),
    }
    assert_eq!(usage.unwrap().output_tokens, 75);
}

#[tokio::test]
async fn test_anthropic_stream_interruption_disconnect() {
    let sse_events = vec![
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_drop_1\"}}\n\n".to_string(),
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n".to_string(),
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Partial chunk before failure\"}}\n\n".to_string(),
    ];

    // Server abruptly drops connection after sending chunk 1
    let server = MockServer::start(vec![MockResponse::sse_with_disconnect(sse_events, 1)]).await;
    let provider = AnthropicProvider::new("test-key").with_base_url(server.base_url());

    let req = CompletionRequest {
        model: "claude-3-5-sonnet-20241022".to_string(),
        messages: vec![Message::user_text("Hello")],
        tools: vec![],
        system_prompt: None,
        generation: Default::default(),
        reasoning: None,
        cache_policy: None,
        extensions: Default::default(),
    };

    let mut stream = provider.stream(req).await.unwrap();

    let mut received_events = Vec::new();
    let mut got_stream_error = false;

    while let Some(res) = stream.next().await {
        match res {
            Ok(ev) => received_events.push(ev),
            Err(e) => {
                assert!(matches!(e, ProviderError::StreamError(_)));
                got_stream_error = true;
                break;
            }
        }
    }

    assert!(
        got_stream_error,
        "Expected stream error upon abrupt socket drop"
    );
    assert!(
        !received_events.is_empty(),
        "Should have received initial events before socket drop"
    );
}

#[tokio::test]
async fn test_anthropic_http_429_rate_limit_with_retry_after() {
    let server = MockServer::start(vec![
        MockResponse::error_with_retry_after(
            429,
            45,
            json!({
                "type": "error",
                "error": {
                    "type": "rate_limit_error",
                    "message": "Number of request tokens has exceeded your rate limit"
                }
            }),
        ),
        MockResponse::error_with_retry_after(
            429,
            45,
            json!({
                "type": "error",
                "error": {
                    "type": "rate_limit_error",
                    "message": "Number of request tokens has exceeded your rate limit"
                }
            }),
        ),
    ])
    .await;

    let provider = AnthropicProvider::new("test-key").with_base_url(server.base_url());
    let req = CompletionRequest {
        model: "claude-3-5-sonnet-20241022".to_string(),
        messages: vec![Message::user_text("Hello")],
        tools: vec![],
        system_prompt: None,
        generation: Default::default(),
        reasoning: None,
        cache_policy: None,
        extensions: Default::default(),
    };

    let res = provider.stream(req.clone()).await;
    match res {
        Err(ProviderError::RateLimitExceeded(msg)) => {
            assert!(msg.contains("retry-after: 45"));
            assert!(msg.contains("rate_limit_error"));
        }
        _ => panic!("Expected RateLimitExceeded"),
    }

    // Also test non-streaming complete() handles 429 Retry-After identically
    let comp_res = provider.complete(req).await;
    assert!(comp_res.is_err());
    match comp_res.unwrap_err() {
        ProviderError::RateLimitExceeded(msg) => {
            assert!(msg.contains("retry-after: 45"));
        }
        other => panic!("Expected RateLimitExceeded, got {other:?}"),
    }
}

#[tokio::test]
async fn test_anthropic_http_529_overloaded_error() {
    let server = MockServer::start(vec![
        MockResponse::json(
            529,
            "Site Overloaded",
            json!({
                "type": "error",
                "error": {
                    "type": "overloaded_error",
                    "message": "Anthropic API is temporarily overloaded"
                }
            }),
        ),
        MockResponse::json(
            529,
            "Site Overloaded",
            json!({
                "type": "error",
                "error": {
                    "type": "overloaded_error",
                    "message": "Anthropic API is temporarily overloaded"
                }
            }),
        ),
    ])
    .await;

    let provider = AnthropicProvider::new("test-key").with_base_url(server.base_url());
    let req = CompletionRequest {
        model: "claude-3-5-sonnet-20241022".to_string(),
        messages: vec![Message::user_text("Hello")],
        tools: vec![],
        system_prompt: None,
        generation: Default::default(),
        reasoning: None,
        cache_policy: None,
        extensions: Default::default(),
    };

    let stream_res = provider.stream(req.clone()).await;
    match stream_res {
        Err(ProviderError::ModelOverloaded(_)) => {}
        _ => panic!("Expected ModelOverloaded"),
    }

    let comp_res = provider.complete(req).await;
    assert!(matches!(
        comp_res.unwrap_err(),
        ProviderError::ModelOverloaded(_)
    ));
}

#[tokio::test]
async fn test_anthropic_http_500_and_503_server_errors() {
    let server = MockServer::start(vec![
        MockResponse::status_only(500, "Internal Server Error", "server crash"),
        MockResponse::status_only(503, "Service Unavailable", "maintenance"),
    ])
    .await;

    let provider = AnthropicProvider::new("test-key").with_base_url(server.base_url());
    let req = CompletionRequest {
        model: "claude-3-5-sonnet-20241022".to_string(),
        messages: vec![Message::user_text("Hello")],
        tools: vec![],
        system_prompt: None,
        generation: Default::default(),
        reasoning: None,
        cache_policy: None,
        extensions: Default::default(),
    };

    // First request -> 500 NetworkError
    let err_500 = match provider.stream(req.clone()).await {
        Err(e) => e,
        Ok(_) => panic!("Expected error"),
    };
    assert!(matches!(err_500, ProviderError::NetworkError(_)));

    // Second request -> 503 NetworkError
    let err_503 = match provider.stream(req).await {
        Err(e) => e,
        Ok(_) => panic!("Expected error"),
    };
    assert!(matches!(err_503, ProviderError::NetworkError(_)));
}

#[tokio::test]
async fn test_anthropic_inband_sse_error_event() {
    let sse_events = vec![
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_err_1\"}}\n\n".to_string(),
        "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Server busy mid-stream\"}}\n\n".to_string(),
    ];

    let server = MockServer::start(vec![MockResponse::sse_chunks(sse_events)]).await;
    let provider = AnthropicProvider::new("test-key").with_base_url(server.base_url());

    let req = CompletionRequest {
        model: "claude-3-5-sonnet-20241022".to_string(),
        messages: vec![Message::user_text("Hello")],
        tools: vec![],
        system_prompt: None,
        generation: Default::default(),
        reasoning: None,
        cache_policy: None,
        extensions: Default::default(),
    };

    let mut stream = provider.stream(req).await.unwrap();
    let mut acc = ToolCallAccumulator::default();

    // message_start event
    let ev1 = stream.next().await.unwrap().unwrap();
    acc.process_event(&ev1).unwrap();

    // error event
    let ev2 = stream.next().await.unwrap().unwrap();
    assert!(matches!(ev2, StreamEvent::StreamError { .. }));

    let acc_err = acc.process_event(&ev2).unwrap_err();
    assert!(matches!(acc_err, CoreError::Accumulator(_)));
}
