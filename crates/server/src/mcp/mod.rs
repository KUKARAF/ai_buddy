//! Remote MCP (Model Context Protocol) server.
//!
//! STUB — implemented by the `mcp` feature agent by porting rust_note's `mcp/`
//! (mod.rs mount + StreamableHttpService, auth.rs JWT resource-server, server.rs
//! `#[tool_router]` tools). Tools operate on the authenticated user's
//! goals/roadmap/progress. Must keep `pub fn mount(app: Router, state: AppState)
//! -> Router` so `routes::build` can call it.

use axum::Router;

use crate::state::AppState;

pub fn mount(app: Router, _state: AppState) -> Router {
    app
}
