mod common;

use agent_core::{
    CompletionRequest, ContentBlock, LlmProvider, Message, ProviderError, Role,
    ToolCallAccumulator, ToolDefinition,
};
use agent_providers::GeminiProvider;
use common::{MockResponse, MockServer};
use futures::StreamExt;
use serde_json::json;

#[tokio::test]
async fn test_gemini_streaming_function_call_part_handling() {
    let sse_events = vec![
        "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"I will compute the result for you.\\n\"}]},\"finishReason\":null}]}\n\n".to_string(),
        "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"functionCall\":{\"name\":\"MathEvaluate\",\"args\":{\"expression\":\"6 * 7\",\"precision\":2}}}]},\"finishReason\":\"STOP\"}],\"usageMetadata\":{\"promptTokenCount\":80,\"candidatesTokenCount\":35}}\n\n".to_string(),
    ];

    let server = MockServer::start(vec![MockResponse::sse_chunks(sse_events)]).await;
    let provider = GeminiProvider::new("gemini-test-key").with_base_url(server.base_url());

    let req = CompletionRequest {
        model: "gemini-2.0-flash".to_string(),
        messages: vec![Message::user_text("What is 6 * 7?")],
        tools: vec![ToolDefinition {
            name: "MathEvaluate".into(),
            description: "Evaluates mathematical expressions".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "expression": {"type": "string"},
                    "precision": {"type": "integer"}
                },
                "required": ["expression"]
            }),
        }],
        system_prompt: Some("You are a helpful math agent.".to_string()),
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
    assert_eq!(msg.content.len(), 2);

    match &msg.content[0] {
        ContentBlock::Text { text } => {
            assert!(text.contains("I will compute the result for you."));
        }
        _ => panic!("Expected ContentBlock::Text as first block"),
    }

    match &msg.content[1] {
        ContentBlock::ToolUse { id, name, input } => {
            assert!(id.contains("MathEvaluate"));
            assert_eq!(name, "MathEvaluate");
            assert_eq!(input["expression"], "6 * 7");
            assert_eq!(input["precision"], 2);
        }
        _ => panic!("Expected ContentBlock::ToolUse as second block"),
    }

    let u = usage.expect("Usage metadata should be extracted");
    assert_eq!(u.input_tokens, 80);
    assert_eq!(u.output_tokens, 35);

    // Verify request wire format
    let reqs = server.recorded_requests();
    assert_eq!(reqs.len(), 1);
    let r = &reqs[0];
    assert!(r.path.contains("gemini-2.0-flash:streamGenerateContent"));
    assert!(r.path.contains("key=gemini-test-key"));

    let body = r.json_body();
    assert_eq!(
        body["systemInstruction"]["parts"][0]["text"],
        "You are a helpful math agent."
    );
    assert_eq!(
        body["tools"][0]["functionDeclarations"][0]["name"],
        "MathEvaluate"
    );
}

#[tokio::test]
async fn test_gemini_non_streaming_function_call_handling() {
    let resp_body = json!({
        "candidates": [
            {
                "content": {
                    "role": "model",
                    "parts": [
                        {
                            "functionCall": {
                                "name": "FileRead",
                                "args": {
                                    "path": "README.md"
                                }
                            }
                        }
                    ]
                },
                "finishReason": "STOP"
            }
        ],
        "usageMetadata": {
            "promptTokenCount": 50,
            "candidatesTokenCount": 20
        }
    });

    let server = MockServer::start(vec![MockResponse::json(200, "OK", resp_body)]).await;
    let provider = GeminiProvider::new("gemini-test-key").with_base_url(server.base_url());

    let req = CompletionRequest {
        model: "gemini-1.5-pro".to_string(),
        messages: vec![Message::user_text("Read README.md")],
        tools: vec![ToolDefinition {
            name: "FileRead".into(),
            description: "Read a file".into(),
            input_schema: json!({"type": "object"}),
        }],
        system_prompt: None,
        generation: Default::default(),
        reasoning: None,
        cache_policy: None,
        extensions: Default::default(),
    };

    let resp = provider.complete(req).await.unwrap();
    assert_eq!(resp.message.content.len(), 1);
    match &resp.message.content[0] {
        ContentBlock::ToolUse { id, name, input } => {
            assert!(id.contains("FileRead"));
            assert_eq!(name, "FileRead");
            assert_eq!(input["path"], "README.md");
        }
        _ => panic!("Expected ContentBlock::ToolUse"),
    }
}

#[tokio::test]
async fn test_gemini_http_429_rate_limit_with_retry_after() {
    let server = MockServer::start(vec![
        MockResponse::error_with_retry_after(
            429,
            30,
            json!({
                "error": {
                    "code": 429,
                    "message": "Resource has been exhausted (e.g. check quota)",
                    "status": "RESOURCE_EXHAUSTED"
                }
            }),
        ),
        MockResponse::error_with_retry_after(
            429,
            30,
            json!({
                "error": {
                    "code": 429,
                    "message": "Resource has been exhausted (e.g. check quota)",
                    "status": "RESOURCE_EXHAUSTED"
                }
            }),
        ),
    ])
    .await;

    let provider = GeminiProvider::new("gemini-test-key").with_base_url(server.base_url());
    let req = CompletionRequest {
        model: "gemini-2.0-flash".to_string(),
        messages: vec![Message::user_text("hello")],
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
            assert!(msg.contains("retry-after: 30"));
            assert!(msg.contains("RESOURCE_EXHAUSTED"));
        }
        _ => panic!("Expected RateLimitExceeded"),
    }

    let comp_err = provider.complete(req).await.unwrap_err();
    match comp_err {
        ProviderError::RateLimitExceeded(msg) => {
            assert!(msg.contains("retry-after: 30"));
        }
        other => panic!("Expected RateLimitExceeded, got {other:?}"),
    }
}

#[tokio::test]
async fn test_gemini_http_503_service_unavailable() {
    let server = MockServer::start(vec![MockResponse::status_only(
        503,
        "Service Unavailable",
        "Google backend unavailable",
    )])
    .await;

    let provider = GeminiProvider::new("gemini-test-key").with_base_url(server.base_url());
    let req = CompletionRequest {
        model: "gemini-2.0-flash".to_string(),
        messages: vec![Message::user_text("hello")],
        tools: vec![],
        system_prompt: None,
        generation: Default::default(),
        reasoning: None,
        cache_policy: None,
        extensions: Default::default(),
    };

    let err_503 = match provider.stream(req).await {
        Err(e) => e,
        Ok(_) => panic!("Expected error"),
    };
    assert!(matches!(err_503, ProviderError::NetworkError(_)));
}
