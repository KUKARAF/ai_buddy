//! Conversations + messages: the goal-setting chat, with RAG + streaming.
//!
//! STUB — implemented by the `goals` feature agent. Must expose
//! `pub fn router() -> Router<AppState>` wiring the conversation/message routes
//! (SSE streaming for assistant tokens) from docs/ARCHITECTURE.md. Owns
//! migration `0004_chat.sql`. Debits the wallet for LLM cost and blocks with
//! `AppError::PaymentRequired` when the balance is exhausted.

use axum::Router;

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
}
