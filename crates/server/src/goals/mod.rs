//! Goals + roadmap: CRUD and LLM-driven roadmap generation.
//!
//! STUB — implemented by the `goals` feature agent. Must expose
//! `pub fn router() -> Router<AppState>` wiring the goals/roadmap/steps routes
//! from docs/ARCHITECTURE.md. Owns migration `0003_goals.sql`.

use axum::Router;

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
}
