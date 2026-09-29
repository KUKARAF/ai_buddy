//! Remote MCP (Model Context Protocol) server.
//!
//! Exposes a subset of the goals / roadmaps / progress / wallet surface to LLM
//! clients (e.g. litellm) over the MCP **Streamable HTTP** transport, mounted at
//! `/mcp` as a Tower service on the main axum router (see [`crate::routes`]).
//!
//! Auth: AI Buddy acts as an OAuth 2.1 **Resource Server**. Authentik (the
//! app's existing OIDC provider) is the Authorization Server and issues the
//! access tokens; this server only validates them (see [`auth`]) and maps the
//! token subject to the same `user_id` the REST `RequireAuth` extractor yields,
//! so every tool enforces identical per-user ACLs.

use std::sync::Arc;

use axum::routing::get;
use axum::Router;
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};

use crate::state::AppState;

mod auth;
mod server;

use server::McpServer;

/// Build the MCP Streamable-HTTP Tower service.
///
/// A fresh [`McpServer`] is produced per session by the factory closure, each
/// capturing process-wide [`AppState`]. Request-scoped identity is injected
/// separately by the auth middleware (see [`auth`]).
fn service(state: AppState) -> StreamableHttpService<McpServer, LocalSessionManager> {
    // rmcp's DNS-rebinding guard defaults to loopback-only; behind a reverse
    // proxy the Host header is the deployment's public authority, so allow that
    // (plus loopback for local/dev use) or every request 403s.
    let config = StreamableHttpServerConfig::default()
        .with_allowed_hosts(allowed_hosts(&state.config.base_url));
    StreamableHttpService::new(
        move || Ok(McpServer::new(state.clone())),
        Arc::new(LocalSessionManager::default()),
        config,
    )
}

/// The Host-header allowlist for the MCP endpoint: the public authority derived
/// from `base_url` (with and without an explicit port) plus loopback forms.
fn allowed_hosts(base_url: &str) -> Vec<String> {
    let authority = base_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(base_url)
        .split('/')
        .next()
        .unwrap_or(base_url)
        .to_string();
    let host_only = authority
        .rsplit_once(':')
        .map(|(h, _)| h.to_string())
        .unwrap_or_else(|| authority.clone());
    let mut hosts = vec![
        authority,
        host_only,
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "[::1]".to_string(),
    ];
    hosts.dedup();
    hosts
}

/// Mount the MCP endpoint (`/mcp`, auth-gated) and its unauthenticated OAuth
/// Protected Resource Metadata (`/.well-known/oauth-protected-resource`) onto
/// the application router.
pub fn mount(app: Router, state: AppState) -> Router {
    let mcp_service = service(state.clone());
    let mcp_auth = auth::McpAuth::from_config(&state.config);

    // The MCP endpoint, wrapped in the Resource-Server auth middleware.
    let auth_for_mw = mcp_auth.clone();
    let mcp = Router::new()
        .nest_service("/mcp", mcp_service)
        .layer(axum::middleware::from_fn(move |req, next| {
            let auth = auth_for_mw.clone();
            async move { auth.require(req, next).await }
        }));

    // RFC 9728 Protected Resource Metadata — unauthenticated by design.
    let auth_for_prm = mcp_auth.clone();
    let well_known = Router::new().route(
        "/.well-known/oauth-protected-resource",
        get(move || {
            let auth = auth_for_prm.clone();
            async move { auth.protected_resource_metadata() }
        }),
    );

    app.merge(mcp).merge(well_known)
}

#[cfg(test)]
mod tests {
    use super::allowed_hosts;

    #[test]
    fn allowed_hosts_from_https_base_url() {
        let hosts = allowed_hosts("https://aibuddy.example.com");
        assert!(hosts.contains(&"aibuddy.example.com".to_string()));
        // Loopback is always allowed too (local/dev use).
        assert!(hosts.contains(&"localhost".to_string()));
        assert!(hosts.contains(&"127.0.0.1".to_string()));
    }

    #[test]
    fn allowed_hosts_keeps_explicit_port() {
        let hosts = allowed_hosts("http://localhost:8080");
        // Both the authority-with-port and the bare host are allowed, since the
        // Host header may or may not carry the port.
        assert!(hosts.contains(&"localhost:8080".to_string()));
        assert!(hosts.contains(&"localhost".to_string()));
    }
}
