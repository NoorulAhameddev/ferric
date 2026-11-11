use thiserror::Error;

#[derive(Error, Debug, Clone)]
pub enum ProviderError {
    #[error("Authentication failed: {0}")]
    AuthenticationFailed(String),

    #[error("Rate limit exceeded: {0}")]
    RateLimitExceeded(String),

    #[error("Context window exceeded: {0}")]
    ContextWindowExceeded(String),

    #[error("Model overloaded: {0}")]
    ModelOverloaded(String),

    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    #[error("Network or connection error: {0}")]
    NetworkError(String),

    #[error("Streaming error: {0}")]
    StreamError(String),

    #[error("Tool execution failed: {0}")]
    ToolError(String),

    #[error("Malformed response from provider: {0}")]
    MalformedResponse(String),

    #[error("Unsupported capability: {0}")]
    UnsupportedCapability(String),

    #[error("Request cancelled")]
    Cancelled,
}

#[derive(Error, Debug)]
pub enum CoreError {
    #[error("Provider error: {0}")]
    Provider(#[from] ProviderError),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Accumulator error: {0}")]
    Accumulator(String),

    #[error("Tool validation error: {0}")]
    ToolValidation(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("Configuration error: {0}")]
    Config(String),
}
