//! LLM (OpenRouter) chat + embeddings client.
//!
//! STUB — implemented by the `llm` feature agent. Must keep:
//!   - `pub struct LlmClient` deriving `Clone` (Arc interior),
//!   - `pub fn new(config: &Config) -> Self`.
//! Then add (per docs/ARCHITECTURE.md): `chat`, `chat_stream`, `chat_json`,
//! `embed`, plus the `ChatMsg` type and a token-cost estimator (cents).

use crate::config::Config;

#[derive(Clone)]
pub struct LlmClient {
    #[allow(dead_code)]
    inner: std::sync::Arc<Inner>,
}

struct Inner {
    #[allow(dead_code)]
    config: Config,
}

impl LlmClient {
    pub fn new(config: &Config) -> Self {
        Self {
            inner: std::sync::Arc::new(Inner {
                config: config.clone(),
            }),
        }
    }
}
