//! Notifications + reminder/tip scheduler.
//!
//! STUB — implemented by the `notify` feature agent. Must keep:
//!   - `pub struct Notifier` deriving `Clone`,
//!   - `pub fn new(pool: SqlitePool) -> Self`,
//!   - `pub fn spawn_scheduler(self, state: AppState)` (background tokio loop),
//!   - `pub fn router() -> Router<AppState>`.
//! Then add (per docs/ARCHITECTURE.md): `enqueue`, delivery abstraction,
//! notifications + push-token routes. Owns migration `0005_notify.sql`.

use axum::Router;
use sqlx::SqlitePool;

use crate::state::AppState;

#[derive(Clone)]
pub struct Notifier {
    #[allow(dead_code)]
    pool: SqlitePool,
}

impl Notifier {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Spawn the background reminder/tip scheduler. No-op in the stub.
    pub fn spawn_scheduler(self, _state: AppState) {}
}

pub fn router() -> Router<AppState> {
    Router::new()
}
