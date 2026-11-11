use crate::openai::OpenAiProvider;
use agent_core::{
    CompletionRequest, CompletionResponse, EventStream, LlmProvider, ModelCapabilities,
    ProviderError,
};
use async_trait::async_trait;

/// Generic OpenAI-compatible adapter supporting Ollama, vLLM, OpenRouter, LiteLLM,
/// and self-hosted inference servers with configurable base URLs and authorization headers.
pub struct GenericOpenAiCompatibleProvider {
    inner: OpenAiProvider,
    custom_id: String,
    custom_name: String,
}

impl GenericOpenAiCompatibleProvider {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        let provider = OpenAiProvider::new(api_key).with_base_url(base_url);
        Self {
            inner: provider,
            custom_id: id.into(),
            custom_name: name.into(),
        }
    }

    pub fn ollama(base_url: Option<&str>) -> Self {
        let url = base_url.unwrap_or("http://localhost:11434/v1");
        Self::new("ollama", "Ollama (Local)", url, "ollama")
    }

    pub fn openrouter(api_key: impl Into<String>) -> Self {
        Self::new(
            "openrouter",
            "OpenRouter",
            "https://openrouter.ai/api/v1",
            api_key,
        )
    }
}

#[async_trait]
impl LlmProvider for GenericOpenAiCompatibleProvider {
    fn id(&self) -> &str {
        &self.custom_id
    }

    fn name(&self) -> &str {
        &self.custom_name
    }

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        self.inner.capabilities(model)
    }

    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, ProviderError> {
        self.inner.complete(req).await
    }

    async fn stream(&self, req: CompletionRequest) -> Result<EventStream, ProviderError> {
        self.inner.stream(req).await
    }
}
