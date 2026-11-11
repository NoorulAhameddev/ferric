use crate::error::ProviderError;
use crate::events::StreamEvent;
use crate::models::{CompletionRequest, CompletionResponse, ModelCapabilities};
use async_trait::async_trait;
use futures::Stream;
use std::pin::Pin;

pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>;

#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Unique provider identifier (e.g., "anthropic", "openai", "gemini", "openai-compatible")
    fn id(&self) -> &str;

    /// Provider human-readable display name
    fn name(&self) -> &str;

    /// Retrieve model capabilities for a specified model
    fn capabilities(&self, model: &str) -> ModelCapabilities;

    /// Execute a non-streaming completion request
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, ProviderError>;

    /// Execute a streaming completion request returning normalized StreamEvents
    async fn stream(&self, req: CompletionRequest) -> Result<EventStream, ProviderError>;
}
