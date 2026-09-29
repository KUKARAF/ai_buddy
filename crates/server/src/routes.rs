//! Top-level route table.

use std::time::Duration;

use axum::extract::DefaultBodyLimit;
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use tower::limit::GlobalConcurrencyLimitLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::timeout::TimeoutLayer;

use crate::auth;
use crate::chat;
use crate::checkins;
use crate::circles;
use crate::goals;
use crate::notify;
use crate::settings;
use crate::state::AppState;
use crate::wallet;

/// Max accepted request body. Chat messages / roadmaps are small; 1 MiB is
/// generous and cheaply rejects abusive payloads with a 413.
const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Per-request wall-clock timeout for the REST surface. Note: the SSE chat
/// stream and MCP endpoint are mounted OUTSIDE this (long-lived).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Ceiling on concurrently-processed REST requests.
const MAX_CONCURRENT_REQUESTS: usize = 256;

/// Build the full application router. The session + CORS layers are applied by
/// the caller in `main.rs`.
pub fn build(state: AppState) -> Router {
    let api = Router::new()
        .route("/health", get(|| async { "ok" }))
        .merge(auth::oidc::router())
        .merge(goals::router())
        .merge(chat::router())
        .merge(checkins::router())
        .merge(circles::router())
        .merge(settings::router())
        .merge(wallet::router())
        .merge(notify::router())
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(GlobalConcurrencyLimitLayer::new(MAX_CONCURRENT_REQUESTS));

    let app = api.with_state(state.clone());

    // Mount the MCP Streamable-HTTP server (`/mcp`) + its OAuth metadata,
    // outside the REST timeout/concurrency/body layers (long-lived streams).
    let app = crate::mcp::mount(app, state.clone());

    // Optionally serve the built SvelteKit static assets so frontend + backend
    // run as one origin. Any non-API path falls through to ServeDir, which
    // falls back to `200.html` (the SPA shell) for client-side routing.
    match state.config.static_dir.as_deref() {
        Some(dir) => {
            let spa_fallback = ServeFile::new(format!("{dir}/200.html"));
            let serve_dir = ServeDir::new(dir).not_found_service(spa_fallback);
            app.fallback_service(serve_dir)
        }
        None => app,
    }
}
