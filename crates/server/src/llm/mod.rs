//! LLM chat + embeddings client.
//!
//! Chat/roadmap completions AND embeddings are served by a configurable,
//! OpenAI-compatible provider — the LiteLLM proxy by default (chat via the
//! selected model, embeddings via `bge-m3`), or OpenRouter when
//! `AIBUDDY_LLM_PROVIDER=openrouter` (the revert switch; note OpenRouter has no
//! native embeddings — it proxies to OpenAI via a BYOK key).
//!
//! Exposes:
//!   - non-streaming chat ([`LlmClient::chat`]),
//!   - streaming chat ([`LlmClient::chat_stream`]) yielding [`ChatDelta`]s,
//!   - JSON-mode structured completion ([`LlmClient::chat_json`]),
//!   - single-text embeddings ([`LlmClient::embed`]).
//!
//! The chat methods take an explicit `model` so callers can honour a user's
//! per-user model choice (see [`crate::settings`]). Use [`LlmClient::resolve_model`]
//! to validate a requested id against the allowed list before calling.
//!
//! Every call returns an approximate token cost in euro cents (see
//! [`cost_cents`]); this is the value the wallet debits. The rate table is
//! approximate and overridable — keep it roughly in line with the provider's
//! published prices.

use std::time::Duration;

use anyhow::anyhow;
use futures::stream::StreamExt;
use serde::de::DeserializeOwned;

use crate::config::Config;
use crate::error::{AppError, AppResult};

/// OpenRouter chat completions endpoint. Retained so switching back to the
/// OpenRouter chat path (`AIBUDDY_LLM_PROVIDER=openrouter`) is trivial.
const OPENROUTER_CHAT_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const EMBED_URL: &str = "https://openrouter.ai/api/v1/embeddings";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Approximate USD → EUR conversion used to turn provider prices (quoted in
/// USD) into the euro cents the wallet is denominated in. Approximate/overridable.
const USD_TO_EUR: f64 = 0.92;

/// One chat message in a conversation. `role` is one of `user`, `assistant`,
/// `system`.
#[derive(Debug, Clone)]
pub struct ChatMsg {
    pub role: String,
    pub content: String,
}

/// Result of a non-streaming chat completion.
#[derive(Debug, Clone)]
pub struct ChatOutcome {
    pub text: String,
    pub cost_cents: i64,
}

/// An item in a streaming chat response.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatDelta {
    /// A chunk of assistant text.
    Token(String),
    /// The stream finished normally; carries the total cost estimate.
    Done { cost_cents: i64 },
    /// A transport/parse error occurred mid-stream.
    Error(String),
}

#[derive(Clone)]
pub struct LlmClient {
    inner: std::sync::Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    /// Full chat completions URL (provider-dependent).
    chat_url: String,
    /// API key for the chat provider (LiteLLM or OpenRouter).
    chat_api_key: Option<String>,
    /// Default chat model when the caller passes an unknown/empty model.
    default_chat_model: String,
    /// Chat model ids a user may select.
    allowed_chat_models: Vec<String>,
    /// Full embeddings URL (provider-dependent).
    embed_url: String,
    /// API key for the embeddings provider.
    embed_api_key: Option<String>,
    embedding_model: String,
    #[allow(dead_code)]
    embedding_dim: usize,
}

impl LlmClient {
    pub fn new(config: &Config) -> Self {
        // Harden the outbound client like the OIDC one: no redirects (SSRF
        // defence) and a bounded timeout so a slow provider can't pin a request
        // open. Fall back to a default client if the builder fails (panic-safe).
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        // Pick the chat endpoint + key by provider (embeddings mirror it below).
        let (chat_url, chat_api_key) = if config.llm_provider == "openrouter" {
            (
                OPENROUTER_CHAT_URL.to_string(),
                config.openrouter_api_key.clone(),
            )
        } else {
            // Default / "litellm": OpenAI-compatible base + /chat/completions.
            (
                format!(
                    "{}/chat/completions",
                    config.litellm_base_url.trim_end_matches('/')
                ),
                config.litellm_api_key.clone(),
            )
        };

        // Embeddings follow the same provider. LiteLLM serves `bge-m3` etc.;
        // OpenRouter's /embeddings proxies to OpenAI (BYOK) and is only used when
        // llm_provider=openrouter. The OpenRouter URLs/keys stay wired for revert.
        let (embed_url, embed_api_key) = if config.llm_provider == "openrouter" {
            (EMBED_URL.to_string(), config.openrouter_api_key.clone())
        } else {
            (
                format!(
                    "{}/embeddings",
                    config.litellm_base_url.trim_end_matches('/')
                ),
                config.litellm_api_key.clone(),
            )
        };

        Self {
            inner: std::sync::Arc::new(Inner {
                http,
                chat_url,
                chat_api_key,
                default_chat_model: config.chat_model.clone(),
                allowed_chat_models: config.allowed_chat_models.clone(),
                embed_url,
                embed_api_key,
                embedding_model: config.embedding_model.clone(),
                embedding_dim: config.embedding_dim,
            }),
        }
    }

    /// The configured default chat model.
    pub fn default_chat_model(&self) -> &str {
        &self.inner.default_chat_model
    }

    /// Resolve a requested model to a usable one: return `requested` when it is
    /// in the allowed list, otherwise fall back to the default.
    pub fn resolve_model(&self, requested: Option<&str>) -> String {
        match requested {
            Some(m) if self.inner.allowed_chat_models.iter().any(|a| a == m) => m.to_string(),
            _ => self.inner.default_chat_model.clone(),
        }
    }

    /// The chat provider's API key, or a `BadRequest` when unconfigured.
    fn require_chat_key(&self) -> AppResult<&str> {
        self.inner
            .chat_api_key
            .as_deref()
            .ok_or_else(|| AppError::BadRequest("LLM API key not configured".to_string()))
    }

    /// The embeddings provider's API key, or a `BadRequest` when unset.
    fn require_embed_key(&self) -> AppResult<&str> {
        self.inner
            .embed_api_key
            .as_deref()
            .ok_or_else(|| AppError::BadRequest("embeddings API key not configured".to_string()))
    }

    /// Non-streaming chat completion with an explicit `model`. `system` is
    /// prepended as a `system` message (when non-empty). Returns the assistant
    /// text + cost estimate.
    pub async fn chat(
        &self,
        model: &str,
        system: &str,
        messages: &[ChatMsg],
    ) -> AppResult<ChatOutcome> {
        let key = self.require_chat_key()?;
        let body = serde_json::json!({
            "model": model,
            "messages": build_messages(system, messages),
        });

        let resp = self
            .inner
            .http
            .post(&self.inner.chat_url)
            .bearer_auth(key)
            .json(&body)
            .send()
            .await
            .map_err(|e| AppError::Internal(anyhow!("LLM request failed: {e}")))?;

        let parsed: ChatResponse = read_json(resp).await?;
        let text = parsed
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .ok_or_else(|| AppError::Internal(anyhow!("LLM returned no content")))?;

        let (pt, ct) = parsed.usage.unwrap_or_default().tokens();
        Ok(ChatOutcome {
            text,
            cost_cents: cost_cents(model, pt, ct),
        })
    }

    /// JSON-mode structured completion. Sends `response_format:json_object` and
    /// `temperature:0`, then extracts the first `{ … }` object from the model's
    /// output (tolerating prose / code fences) and deserializes it into `T`.
    /// Returns `(T, cost_cents)`.
    pub async fn chat_json<T: DeserializeOwned>(
        &self,
        model: &str,
        system: &str,
        user: &str,
    ) -> AppResult<(T, i64)> {
        let key = self.require_chat_key()?;
        let body = serde_json::json!({
            "model": model,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user },
            ],
            "response_format": { "type": "json_object" },
            "temperature": 0,
        });

        let resp = self
            .inner
            .http
            .post(&self.inner.chat_url)
            .bearer_auth(key)
            .json(&body)
            .send()
            .await
            .map_err(|e| AppError::Internal(anyhow!("LLM request failed: {e}")))?;

        let parsed: ChatResponse = read_json(resp).await?;
        let content = parsed
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .ok_or_else(|| AppError::Internal(anyhow!("LLM returned no content")))?;

        let json = extract_json(&content)
            .ok_or_else(|| AppError::Internal(anyhow!("no JSON object in model output")))?;
        let value: T = serde_json::from_str(json)
            .map_err(|e| AppError::Internal(anyhow!("could not parse model JSON: {e}")))?;

        let (pt, ct) = parsed.usage.unwrap_or_default().tokens();
        Ok((value, cost_cents(model, pt, ct)))
    }

    /// Streaming chat completion. Yields a [`ChatDelta::Token`] per delta, then a
    /// final [`ChatDelta::Done`] carrying the cost (from the usage event emitted
    /// because we send `stream_options.include_usage`). Robust to SSE lines that
    /// are split across network chunks.
    pub async fn chat_stream(
        &self,
        model: &str,
        system: &str,
        messages: &[ChatMsg],
    ) -> AppResult<impl futures::Stream<Item = ChatDelta> + Send> {
        let key = self.require_chat_key()?;
        let body = serde_json::json!({
            "model": model,
            "messages": build_messages(system, messages),
            "stream": true,
            "stream_options": { "include_usage": true },
        });

        let resp = self
            .inner
            .http
            .post(&self.inner.chat_url)
            .bearer_auth(key)
            .json(&body)
            .send()
            .await
            .map_err(|e| AppError::Internal(anyhow!("LLM request failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let raw = resp.text().await.unwrap_or_default();
            if status.as_u16() == 401 {
                return Err(AppError::BadRequest(
                    "LLM provider rejected the API key".to_string(),
                ));
            }
            return Err(AppError::Internal(anyhow!("LLM returned {status}: {raw}")));
        }

        let (tx, rx) = futures::channel::mpsc::unbounded::<ChatDelta>();
        let model = model.to_string();

        tokio::spawn(async move {
            let mut bytes = resp.bytes_stream();
            let mut buf: Vec<u8> = Vec::new();
            let mut cost: i64 = 0;
            let mut done = false;

            'outer: while let Some(item) = bytes.next().await {
                let chunk = match item {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = tx.unbounded_send(ChatDelta::Error(e.to_string()));
                        break;
                    }
                };
                buf.extend_from_slice(&chunk);

                while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = buf.drain(..=nl).collect();
                    let line = String::from_utf8_lossy(&line);
                    if let Some(parsed) = parse_sse_line(&line) {
                        if let Some(token) = parsed.token {
                            if !token.is_empty() {
                                let _ = tx.unbounded_send(ChatDelta::Token(token));
                            }
                        }
                        if let Some((pt, ct)) = parsed.usage {
                            cost = cost_cents(&model, pt, ct);
                        }
                        if parsed.done {
                            done = true;
                            break 'outer;
                        }
                    }
                }
            }

            // Process any trailing line that arrived without a newline.
            if !done && !buf.is_empty() {
                let line = String::from_utf8_lossy(&buf);
                if let Some(parsed) = parse_sse_line(&line) {
                    if let Some(token) = parsed.token {
                        if !token.is_empty() {
                            let _ = tx.unbounded_send(ChatDelta::Token(token));
                        }
                    }
                    if let Some((pt, ct)) = parsed.usage {
                        cost = cost_cents(&model, pt, ct);
                    }
                }
            }

            let _ = tx.unbounded_send(ChatDelta::Done { cost_cents: cost });
        });

        Ok(rx)
    }

    /// Embed a single text into a `Vec<f32>` (length `embedding_dim`), plus a
    /// cost estimate. Embeddings are cheap, so the cost floors at 0. On any
    /// endpoint error the [`AppError`] is returned so callers can treat RAG as
    /// best-effort.
    pub async fn embed(&self, text: &str) -> AppResult<(Vec<f32>, i64)> {
        let key = self.require_embed_key()?;
        let body = serde_json::json!({
            "model": self.inner.embedding_model,
            "input": text,
        });

        let resp = self
            .inner
            .http
            .post(&self.inner.embed_url)
            .bearer_auth(key)
            .json(&body)
            .send()
            .await
            .map_err(|e| AppError::Internal(anyhow!("embeddings request failed: {e}")))?;

        let parsed: EmbedResponse = read_json(resp).await?;
        let embedding = parsed
            .data
            .into_iter()
            .next()
            .map(|d| d.embedding)
            .ok_or_else(|| {
                AppError::Internal(anyhow!("embeddings provider returned no embedding"))
            })?;

        // Fall back to a rough token estimate (~4 chars/token) if usage is absent.
        let tokens = parsed
            .usage
            .and_then(|u| u.prompt_tokens.or(u.total_tokens))
            .unwrap_or((text.len() / 4) as u32);

        Ok((
            embedding,
            embed_cost_cents(&self.inner.embedding_model, tokens),
        ))
    }
}

/// Build the OpenRouter `messages` array: a leading `system` message (when
/// non-empty) followed by the conversation.
fn build_messages(system: &str, messages: &[ChatMsg]) -> serde_json::Value {
    let mut out = Vec::with_capacity(messages.len() + 1);
    if !system.is_empty() {
        out.push(serde_json::json!({ "role": "system", "content": system }));
    }
    for m in messages {
        out.push(serde_json::json!({ "role": m.role, "content": m.content }));
    }
    serde_json::Value::Array(out)
}

/// Read a JSON response, mapping non-2xx statuses to `AppError` (401 → a
/// `BadRequest` naming the key, other statuses → `Internal` with the body).
async fn read_json<T: DeserializeOwned>(resp: reqwest::Response) -> AppResult<T> {
    let status = resp.status();
    let raw = resp
        .text()
        .await
        .map_err(|e| AppError::Internal(anyhow!("reading OpenRouter response failed: {e}")))?;

    if !status.is_success() {
        if status.as_u16() == 401 {
            return Err(AppError::BadRequest(
                "OpenRouter rejected the API key".to_string(),
            ));
        }
        return Err(AppError::Internal(anyhow!(
            "OpenRouter returned {status}: {raw}"
        )));
    }

    serde_json::from_str(&raw)
        .map_err(|e| AppError::Internal(anyhow!("unexpected OpenRouter response: {e}")))
}

// --- OpenRouter response shapes ---

#[derive(Debug, serde::Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, serde::Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Debug, serde::Deserialize)]
struct ChatChoiceMessage {
    content: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct Usage {
    #[serde(default)]
    prompt_tokens: Option<u32>,
    #[serde(default)]
    completion_tokens: Option<u32>,
    #[serde(default)]
    total_tokens: Option<u32>,
}

impl Usage {
    /// `(prompt_tokens, completion_tokens)`, defaulting missing fields to 0.
    fn tokens(&self) -> (u32, u32) {
        (
            self.prompt_tokens.unwrap_or(0),
            self.completion_tokens.unwrap_or(0),
        )
    }
}

#[derive(Debug, serde::Deserialize)]
struct EmbedResponse {
    data: Vec<EmbedData>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Debug, serde::Deserialize)]
struct EmbedData {
    embedding: Vec<f32>,
}

// --- Cost estimation ---

/// Per-model price in USD per 1M tokens as `(prompt_rate, completion_rate)`.
/// Approximate and overridable; keep roughly in line with OpenRouter's prices.
/// Unknown models fall back to a conservative default.
fn model_rate(model: &str) -> (f64, f64) {
    match model {
        "anthropic/claude-3.5-sonnet" | "anthropic/claude-3-5-sonnet" => (3.0, 15.0),
        "anthropic/claude-3.5-haiku" | "anthropic/claude-3-5-haiku" => (0.80, 4.0),
        "anthropic/claude-3-haiku" => (0.25, 1.25),
        "anthropic/claude-3-opus" => (15.0, 75.0),
        "openai/gpt-4o" => (2.5, 10.0),
        "openai/gpt-4o-mini" => (0.15, 0.60),
        // LiteLLM-proxied models. Haiku ~ claude-haiku pricing; gemma is a small
        // local/self-hosted model, so treat it as near-free (min 1 cent still
        // applies to any chat call).
        "openrouter/~anthropic/claude-haiku-latest" => (0.80, 4.0),
        "gemma4-26b" => (0.2, 0.2),
        // Conservative default so an unknown model never under-charges the wallet.
        _ => (5.0, 15.0),
    }
}

/// Estimate the euro-cent cost of a chat completion from token usage, rounded
/// **up**, with a **minimum of 1 cent**.
fn cost_cents(model: &str, prompt_tokens: u32, completion_tokens: u32) -> i64 {
    let (pr, cr) = model_rate(model);
    let usd =
        (prompt_tokens as f64 / 1_000_000.0) * pr + (completion_tokens as f64 / 1_000_000.0) * cr;
    // Subtract a tiny epsilon before ceil so f64 representation noise (e.g.
    // 5*0.92*100 == 460.0000000000001) doesn't spuriously round a whole cent up.
    let cents = ((usd * USD_TO_EUR * 100.0) - 1e-9).ceil() as i64;
    cents.max(1)
}

/// Estimate the euro-cent cost of an embedding call. Embeddings are cheap, so
/// this floors at 0 (a call may legitimately cost nothing).
fn embed_cost_cents(model: &str, tokens: u32) -> i64 {
    // Per 1M-token USD rates for common embedding models; conservative default.
    let rate = match model {
        "openai/text-embedding-3-small" => 0.02,
        "openai/text-embedding-3-large" => 0.13,
        "openai/text-embedding-ada-002" => 0.10,
        _ => 0.10,
    };
    let usd = (tokens as f64 / 1_000_000.0) * rate;
    let cents = (usd * USD_TO_EUR * 100.0).round() as i64;
    cents.max(0)
}

// --- SSE + JSON parsing (pure, unit-tested) ---

/// The meaningful content extracted from a single OpenRouter SSE `data:` line.
#[derive(Debug, Default, PartialEq)]
struct SseParsed {
    /// A delta of assistant text (`choices[0].delta.content`), if present.
    token: Option<String>,
    /// `(prompt_tokens, completion_tokens)` from a trailing usage event.
    usage: Option<(u32, u32)>,
    /// `true` for the terminal `data: [DONE]` sentinel.
    done: bool,
}

/// Parse one SSE line. Returns `None` for lines that aren't `data:` payloads
/// (comments/keepalives, `event:` lines, blank lines) or unparsable JSON.
fn parse_sse_line(line: &str) -> Option<SseParsed> {
    let data = line.trim().strip_prefix("data:")?.trim();
    if data == "[DONE]" {
        return Some(SseParsed {
            done: true,
            ..Default::default()
        });
    }

    let v: serde_json::Value = serde_json::from_str(data).ok()?;
    let token = v
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("delta"))
        .and_then(|d| d.get("content"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string());
    let usage = v.get("usage").and_then(|u| {
        let p = u.get("prompt_tokens").and_then(|x| x.as_u64())? as u32;
        let c = u.get("completion_tokens").and_then(|x| x.as_u64())? as u32;
        Some((p, c))
    });

    Some(SseParsed {
        token,
        usage,
        done: false,
    })
}

/// Pull the first `{ … }` JSON object out of `content`, tolerating a model that
/// wrapped it in prose or ```json fences.
fn extract_json(content: &str) -> Option<&str> {
    let start = content.find('{')?;
    let end = content.rfind('}')?;
    if end < start {
        return None;
    }
    content.get(start..=end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_cents_minimum_is_one() {
        assert_eq!(cost_cents("anthropic/claude-3.5-sonnet", 0, 0), 1);
    }

    #[test]
    fn cost_cents_rounds_up() {
        // sonnet completion: 15 USD/1M * 70_000 = 1.05 USD; * 0.92 = 0.966 USD
        // = 96.6 cents → ceil → 97.
        assert_eq!(cost_cents("anthropic/claude-3.5-sonnet", 0, 70_000), 97);
        // sonnet prompt: 3 USD/1M * 1_000_000 = 3 USD; * 0.92 * 100 = 276.
        assert_eq!(cost_cents("anthropic/claude-3.5-sonnet", 1_000_000, 0), 276);
    }

    #[test]
    fn cost_cents_unknown_model_uses_default_rate() {
        // default prompt rate 5 USD/1M: 5 * 0.92 * 100 = 460.
        assert_eq!(cost_cents("some/unknown-model", 1_000_000, 0), 460);
    }

    #[test]
    fn cost_cents_litellm_models() {
        // Haiku completion: 4 USD/1M * 1_000_000 = 4 USD * 0.92 * 100 = 368.
        assert_eq!(
            cost_cents("openrouter/~anthropic/claude-haiku-latest", 0, 1_000_000),
            368
        );
        // gemma is near-free but chat still floors at 1 cent.
        assert_eq!(cost_cents("gemma4-26b", 0, 0), 1);
    }

    #[test]
    fn resolve_model_honours_allowed_list() {
        let client = LlmClient::new(&Config::from_env());
        // The config default is always allowed.
        assert_eq!(
            client.default_chat_model(),
            "openrouter/~anthropic/claude-haiku-latest"
        );
        // An allowed model is returned as-is.
        assert_eq!(client.resolve_model(Some("gemma4-26b")), "gemma4-26b");
        // A disallowed / absent model falls back to the default.
        assert_eq!(
            client.resolve_model(Some("not-allowed")),
            client.default_chat_model()
        );
        assert_eq!(client.resolve_model(None), client.default_chat_model());
    }

    #[test]
    fn embed_cost_floors_at_zero_and_scales() {
        assert_eq!(embed_cost_cents("openai/text-embedding-3-small", 100), 0);
        // large rate 0.13 USD/1M * 100_000_000 tokens = 13 USD * 0.92 * 100 = 1196.
        assert_eq!(
            embed_cost_cents("openai/text-embedding-3-large", 100_000_000),
            1196
        );
    }

    #[test]
    fn parse_sse_token_line() {
        let line = r#"data: {"choices":[{"delta":{"content":"hello"}}]}"#;
        assert_eq!(
            parse_sse_line(line),
            Some(SseParsed {
                token: Some("hello".to_string()),
                usage: None,
                done: false,
            })
        );
    }

    #[test]
    fn parse_sse_done_and_usage() {
        assert_eq!(
            parse_sse_line("data: [DONE]"),
            Some(SseParsed {
                done: true,
                ..Default::default()
            })
        );
        let usage = r#"data: {"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":20}}"#;
        assert_eq!(
            parse_sse_line(usage),
            Some(SseParsed {
                token: None,
                usage: Some((10, 20)),
                done: false,
            })
        );
    }

    #[test]
    fn parse_sse_ignores_non_data_lines() {
        assert_eq!(parse_sse_line(""), None);
        assert_eq!(parse_sse_line(": OPENROUTER PROCESSING"), None);
        assert_eq!(parse_sse_line("event: message"), None);
        assert_eq!(parse_sse_line("data: {not json}"), None);
    }

    #[test]
    fn extract_json_handles_fenced_output() {
        let fenced = "```json\n{\"a\":1}\n```";
        assert_eq!(extract_json(fenced), Some("{\"a\":1}"));
        assert_eq!(
            extract_json("prose {\"x\":true} more"),
            Some("{\"x\":true}")
        );
        assert_eq!(extract_json("no json here"), None);
        assert_eq!(extract_json("} {"), None);
    }
}
