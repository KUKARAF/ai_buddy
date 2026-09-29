//! Application configuration, loaded from environment variables.
//!
//! All variables are prefixed `AIBUDDY_`. Every field has a dev-friendly
//! default so the server starts locally with zero configuration; production
//! deployments override these via the environment.

#[derive(Debug, Clone)]
pub struct Config {
    pub authentik_issuer_url: String,
    pub oidc_client_id: String,
    /// `None` (unset/empty) means a Public/PKCE client (no shared secret).
    pub oidc_client_secret: Option<String>,
    pub oidc_redirect_uri: String,
    pub cookie_signing_key: String,
    pub base_url: String,
    pub sqlite_path: String,
    pub bind_addr: String,
    pub cookie_secure: Option<bool>,
    /// If true, authentication is bypassed and every request is user "admin".
    /// Enabled by `AIBUDDY_ENV=dev` or `AIBUDDY_DEV_MODE=true`. Never enable in
    /// a deployment reachable by anyone else.
    pub dev_mode: bool,
    pub cors_allowed_origins: Vec<String>,
    /// Path to the built SvelteKit static assets (`web/build`). When set, the
    /// server serves the SPA (fallback `200.html`) so frontend + backend are
    /// one origin. Unset in local dev (Vite serves the frontend).
    pub static_dir: Option<String>,

    // --- LLM / RAG ---
    /// Which provider serves CHAT/roadmap completions: `"litellm"` (default) or
    /// `"openrouter"`. This is the revert switch for CHAT ONLY — embeddings are
    /// always served by OpenRouter regardless of this value.
    pub llm_provider: String,
    /// Deployment-wide OpenRouter API key. `None` when unset/empty. Used for
    /// embeddings always, and for chat when `llm_provider == "openrouter"`.
    pub openrouter_api_key: Option<String>,
    /// LiteLLM proxy API key. `None` when unset/empty. Used for chat when
    /// `llm_provider == "litellm"`.
    pub litellm_api_key: Option<String>,
    /// LiteLLM OpenAI-compatible base URL (no trailing `/chat/completions`).
    pub litellm_base_url: String,
    /// Default chat model id used for the coach + roadmap generation when the
    /// user has not chosen one (or chose one no longer allowed).
    pub chat_model: String,
    /// Chat model ids a user is allowed to select from.
    pub allowed_chat_models: Vec<String>,
    /// Embedding model id used for RAG memory (always via OpenRouter).
    pub embedding_model: String,
    /// Embedding vector dimension (must match `embedding_model`).
    pub embedding_dim: usize,

    // --- Payments / wallet ---
    /// Stripe secret key. `None` → the MockPaymentProvider is used (dev).
    pub stripe_secret_key: Option<String>,
    /// Stripe webhook signing secret (verifies `/api/webhooks/stripe`).
    pub stripe_webhook_secret: Option<String>,
    /// Minimum wallet top-up in cents (default €4.00).
    pub min_topup_cents: i64,
    /// Maximum usable wallet balance in cents (default €100.00).
    pub max_balance_cents: i64,

    // --- MCP ---
    pub mcp_resource_uri: String,
    pub mcp_audience: String,
}

/// User id used for every request when [`Config::dev_mode`] is enabled.
pub const DEV_MODE_USER_ID: &str = "admin";

/// Committed, INSECURE default cookie signing key so local dev works with zero
/// setup. It is not valid standard base64 (`-` chars), so `signing_key_bytes`
/// falls back to its raw UTF-8 bytes (>= 32). [`Config::validate`] refuses to
/// boot on this value unless dev mode is on.
pub const DEV_DEFAULT_COOKIE_SIGNING_KEY: &str =
    "ai-buddy-dev-insecure-signing-key-change-me-please-0123456789";

/// Dev-friendly default base URL. A real deployment must set `AIBUDDY_BASE_URL`.
pub const DEV_DEFAULT_BASE_URL: &str = "http://localhost:8080";

/// Minimum length (bytes, after decoding) of the cookie signing key.
pub const MIN_COOKIE_SIGNING_KEY_BYTES: usize = 32;

impl Config {
    pub fn from_env() -> Self {
        let base_url =
            std::env::var("AIBUDDY_BASE_URL").unwrap_or_else(|_| DEV_DEFAULT_BASE_URL.to_string());
        let mcp_resource_uri = std::env::var("AIBUDDY_MCP_RESOURCE_URI")
            .unwrap_or_else(|_| format!("{}/mcp", base_url.trim_end_matches('/')));
        let mcp_audience =
            std::env::var("AIBUDDY_MCP_AUDIENCE").unwrap_or_else(|_| mcp_resource_uri.clone());
        Self {
            authentik_issuer_url: std::env::var("AIBUDDY_AUTHENTIK_ISSUER_URL")
                .unwrap_or_else(|_| "http://localhost:9000/application/o/ai-buddy/".to_string()),
            oidc_client_id: std::env::var("AIBUDDY_OIDC_CLIENT_ID")
                .unwrap_or_else(|_| "ai-buddy-dev".to_string()),
            oidc_client_secret: std::env::var("AIBUDDY_OIDC_CLIENT_SECRET")
                .ok()
                .filter(|s| !s.is_empty()),
            oidc_redirect_uri: std::env::var("AIBUDDY_OIDC_REDIRECT_URI")
                .unwrap_or_else(|_| "http://localhost:8080/auth/callback".to_string()),
            cookie_signing_key: std::env::var("AIBUDDY_COOKIE_SIGNING_KEY")
                .unwrap_or_else(|_| DEV_DEFAULT_COOKIE_SIGNING_KEY.to_string()),
            base_url,
            sqlite_path: std::env::var("AIBUDDY_SQLITE_PATH")
                .unwrap_or_else(|_| "./data/ai_buddy.db".to_string()),
            bind_addr: std::env::var("AIBUDDY_BIND_ADDR")
                .unwrap_or_else(|_| "127.0.0.1:8080".to_string()),
            cookie_secure: std::env::var("AIBUDDY_COOKIE_SECURE").ok().and_then(|v| {
                match v.trim().to_ascii_lowercase().as_str() {
                    "true" | "1" | "yes" => Some(true),
                    "false" | "0" | "no" => Some(false),
                    _ => None,
                }
            }),
            dev_mode: std::env::var("AIBUDDY_ENV").as_deref() == Ok("dev")
                || std::env::var("AIBUDDY_DEV_MODE").as_deref() == Ok("true"),
            cors_allowed_origins: std::env::var("AIBUDDY_CORS_ORIGINS")
                .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_else(|_| {
                    [
                        "http://localhost:5173",
                        "http://127.0.0.1:5173",
                        "http://localhost:1420",
                        "http://127.0.0.1:1420",
                        // The Tauri Android app's fixed webview origin.
                        "http://tauri.localhost",
                    ]
                    .into_iter()
                    .map(String::from)
                    .collect()
                }),
            static_dir: std::env::var("AIBUDDY_STATIC_DIR").ok(),

            llm_provider: std::env::var("AIBUDDY_LLM_PROVIDER")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "litellm".to_string()),
            openrouter_api_key: std::env::var("AIBUDDY_OPENROUTER_API_KEY")
                .ok()
                .filter(|s| !s.is_empty()),
            litellm_api_key: std::env::var("AIBUDDY_LITELLM_API_KEY")
                .ok()
                .filter(|s| !s.is_empty()),
            litellm_base_url: std::env::var("AIBUDDY_LITELLM_BASE_URL")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "https://litellm.osmosis.page/v1".to_string()),
            chat_model: std::env::var("AIBUDDY_CHAT_MODEL")
                // Overridable; the LiteLLM default routes to Claude Haiku.
                .unwrap_or_else(|_| "openrouter/~anthropic/claude-haiku-latest".to_string()),
            allowed_chat_models: std::env::var("AIBUDDY_ALLOWED_CHAT_MODELS")
                .ok()
                .map(|v| {
                    v.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<String>>()
                })
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| {
                    vec![
                        "openrouter/~anthropic/claude-haiku-latest".to_string(),
                        "gemma4-26b".to_string(),
                    ]
                }),
            embedding_model: std::env::var("AIBUDDY_EMBEDDING_MODEL")
                .unwrap_or_else(|_| "openai/text-embedding-3-small".to_string()),
            embedding_dim: std::env::var("AIBUDDY_EMBEDDING_DIM")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1536),

            stripe_secret_key: std::env::var("AIBUDDY_STRIPE_SECRET_KEY")
                .ok()
                .filter(|s| !s.is_empty()),
            stripe_webhook_secret: std::env::var("AIBUDDY_STRIPE_WEBHOOK_SECRET")
                .ok()
                .filter(|s| !s.is_empty()),
            min_topup_cents: std::env::var("AIBUDDY_MIN_TOPUP_CENTS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(400),
            max_balance_cents: std::env::var("AIBUDDY_MAX_BALANCE_CENTS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10_000),

            mcp_resource_uri,
            mcp_audience,
        }
    }

    /// Effective `Secure` flag for the session cookie.
    pub fn cookie_secure(&self) -> bool {
        self.cookie_secure
            .unwrap_or_else(|| self.base_url.starts_with("https://"))
    }

    /// Whether [`Self::bind_addr`]'s host is a loopback interface.
    pub fn bind_is_loopback(&self) -> bool {
        use std::net::{IpAddr, SocketAddr};

        if let Ok(addr) = self.bind_addr.parse::<SocketAddr>() {
            return addr.ip().is_loopback();
        }
        let host = match self.bind_addr.strip_prefix('[') {
            Some(rest) => rest.split(']').next().unwrap_or(rest),
            None => self
                .bind_addr
                .rsplit_once(':')
                .map(|(h, _)| h)
                .unwrap_or(self.bind_addr.as_str()),
        };
        if let Ok(ip) = host.parse::<IpAddr>() {
            return ip.is_loopback();
        }
        host.eq_ignore_ascii_case("localhost")
    }

    /// Fail-fast validation of cross-field invariants before boot.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.cookie_signing_key.trim() == DEV_DEFAULT_COOKIE_SIGNING_KEY {
            if self.dev_mode {
                tracing::warn!(
                    "using the built-in INSECURE dev cookie signing key; set \
                     AIBUDDY_COOKIE_SIGNING_KEY to a real secret before exposing this server"
                );
            } else {
                anyhow::bail!(
                    "refusing to boot: AIBUDDY_COOKIE_SIGNING_KEY is unset or left at the \
                     built-in insecure dev default. Set it to a real secret (>= {} bytes).",
                    MIN_COOKIE_SIGNING_KEY_BYTES
                );
            }
        }

        if self.dev_mode && !self.bind_is_loopback() {
            anyhow::bail!(
                "refusing to boot: dev mode bypasses authentication and must not bind a \
                 non-loopback address (bind_addr = {}).",
                self.bind_addr
            );
        }

        if !self.dev_mode && self.base_url == DEV_DEFAULT_BASE_URL {
            anyhow::bail!(
                "refusing to boot: AIBUDDY_BASE_URL is unset or left at the dev default ({}). \
                 Set it to this deployment's real public URL.",
                DEV_DEFAULT_BASE_URL
            );
        }

        Ok(())
    }

    /// Decode [`Self::cookie_signing_key`] to raw bytes, validating length.
    pub fn signing_key_bytes(&self) -> anyhow::Result<Vec<u8>> {
        use base64::Engine;

        let bytes = base64::engine::general_purpose::STANDARD
            .decode(self.cookie_signing_key.trim())
            .unwrap_or_else(|_| self.cookie_signing_key.as_bytes().to_vec());

        anyhow::ensure!(
            bytes.len() >= MIN_COOKIE_SIGNING_KEY_BYTES,
            "AIBUDDY_COOKIE_SIGNING_KEY must decode (or be) at least {} bytes, got {}",
            MIN_COOKIE_SIGNING_KEY_BYTES,
            bytes.len()
        );

        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_default_signing_key_is_valid() {
        let config = Config::from_env();
        let bytes = config
            .signing_key_bytes()
            .expect("dev default should be valid");
        assert!(bytes.len() >= MIN_COOKIE_SIGNING_KEY_BYTES);
    }

    #[test]
    fn short_signing_key_is_rejected() {
        let mut config = Config::from_env();
        config.cookie_signing_key = "too-short".to_string();
        assert!(config.signing_key_bytes().is_err());
    }

    #[test]
    fn dev_default_key_refuses_to_boot_outside_dev_mode() {
        let mut config = Config::from_env();
        config.cookie_signing_key = DEV_DEFAULT_COOKIE_SIGNING_KEY.to_string();
        config.dev_mode = false;
        config.bind_addr = "0.0.0.0:8080".to_string();
        config.base_url = "https://aibuddy.example.com".to_string();
        assert!(config.validate().is_err());

        config.cookie_signing_key = "x".repeat(64);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn llm_defaults_are_litellm() {
        let config = Config::from_env();
        assert_eq!(config.llm_provider, "litellm");
        assert_eq!(
            config.chat_model,
            "openrouter/~anthropic/claude-haiku-latest"
        );
        assert_eq!(
            config.allowed_chat_models,
            vec![
                "openrouter/~anthropic/claude-haiku-latest".to_string(),
                "gemma4-26b".to_string(),
            ]
        );
        assert_eq!(config.litellm_base_url, "https://litellm.osmosis.page/v1");
    }

    #[test]
    fn dev_mode_refuses_non_loopback_bind() {
        let mut config = Config::from_env();
        config.cookie_signing_key = "x".repeat(64);
        config.dev_mode = true;
        config.bind_addr = "0.0.0.0:8080".to_string();
        assert!(config.validate().is_err());
        for ok in ["127.0.0.1:8080", "localhost:8080", "[::1]:8080"] {
            config.bind_addr = ok.to_string();
            assert!(config.validate().is_ok(), "dev mode + {ok} must be allowed");
        }
    }
}
