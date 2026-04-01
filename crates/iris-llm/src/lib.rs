pub mod anthropic;
pub mod google;
pub mod mcp;
pub mod oauth;
pub mod openai;
pub mod retry;
pub mod sse;
pub mod types;

pub use anthropic::{AnthropicProvider, AuthSource};
pub use google::GoogleProvider;
pub use mcp::{McpClient, McpServerConfig, McpTransport};
pub use oauth::{clear_credentials, load_credentials, login, save_credentials, OAuthTokenSet};
pub use openai::{detect_provider, get_provider, OpenAiCompatProvider, ProviderInfo, PROVIDERS};
pub use retry::RetryPolicy;
pub use types::{
    ContentBlock, Message, ModelConfig, Role, StreamEvent, ToolDefinition, TokenUsage,
};
