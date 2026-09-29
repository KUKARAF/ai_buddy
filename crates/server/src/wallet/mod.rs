//! Wallet / payments / pledges.
//!
//! Public surface (stable — other modules depend on it):
//! - [`PaymentProvider`]: Clone handle over a [`PaymentBackend`] trait object.
//! - Ledger/balance/pledge free functions (see [`ledger`]).
//! - [`router`]: the wallet + pledge + Stripe-webhook HTTP routes.
//!
//! The balance model (see `docs/ARCHITECTURE.md`): min €4 top-up, balance capped
//! at €100, and the balance covers both LLM token usage and goal pledges.

mod backend;
mod ledger;

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::WithRejection;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::auth::session::RequireAuth;
use crate::config::Config;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub use backend::{MockPaymentBackend, StripePaymentBackend};
pub use ledger::{
    balance_cents, credit_topup, debit_tokens, ensure_wallet, pledge_forfeit, pledge_hold,
    pledge_refund,
};

/// Outcome of starting a top-up. For a hosted-checkout backend (Stripe),
/// `checkout_url` is `Some` and the client redirects; the wallet is credited
/// later by the webhook. For the mock backend `checkout_url` is `None` and the
/// caller credits immediately.
pub struct TopupSession {
    pub checkout_url: Option<String>,
    /// Provider-side identifier (Stripe session id, or a mock ref).
    pub external_ref: String,
    /// Human-readable provider status (e.g. "pending", "succeeded").
    pub status: String,
}

/// A verified payment webhook event, normalized across providers.
pub struct WebhookEvent {
    pub user_id: String,
    pub amount_cents: i64,
    pub external_ref: String,
    pub succeeded: bool,
}

/// Payment backend abstraction. Implemented by Stripe (real) and a dev mock.
#[async_trait::async_trait]
pub trait PaymentBackend: Send + Sync {
    /// Begin a top-up of `amount_cents` for `user_id`.
    async fn create_topup(&self, user_id: &str, amount_cents: i64) -> anyhow::Result<TopupSession>;
    /// Verify a webhook `body` against its `sig` header, returning the event.
    async fn verify_webhook(&self, sig: &str, body: &[u8]) -> anyhow::Result<WebhookEvent>;
}

/// Clone-able handle to the configured payment backend (Arc interior).
#[derive(Clone)]
pub struct PaymentProvider(Arc<dyn PaymentBackend>);

impl PaymentProvider {
    /// Build the provider from config: Stripe when a secret key is present,
    /// otherwise the instant-credit mock (dev default).
    pub fn new(config: &Config) -> Self {
        let backend: Arc<dyn PaymentBackend> = match &config.stripe_secret_key {
            Some(key) => Arc::new(StripePaymentBackend::new(
                key.clone(),
                config.stripe_webhook_secret.clone(),
                config.base_url.clone(),
            )),
            None => Arc::new(MockPaymentBackend),
        };
        Self(backend)
    }

    pub async fn create_topup(
        &self,
        user_id: &str,
        amount_cents: i64,
    ) -> anyhow::Result<TopupSession> {
        self.0.create_topup(user_id, amount_cents).await
    }

    pub async fn verify_webhook(&self, sig: &str, body: &[u8]) -> anyhow::Result<WebhookEvent> {
        self.0.verify_webhook(sig, body).await
    }
}

// --- HTTP layer ---------------------------------------------------------------

#[derive(Serialize)]
struct WalletView {
    balance_cents: i64,
    currency: String,
    token_spend_cents: i64,
}

#[derive(Deserialize)]
struct TopupRequest {
    amount_cents: i64,
}

#[derive(Serialize)]
struct TopupResponse {
    /// Set when the client must redirect to a hosted checkout (Stripe).
    checkout_url: Option<String>,
    /// Balance after crediting (unchanged when a redirect is required).
    balance_cents: i64,
    status: String,
}

#[derive(Serialize)]
struct LedgerEntryView {
    id: String,
    kind: String,
    amount_cents: i64,
    goal_id: Option<String>,
    external_ref: Option<String>,
    memo: Option<String>,
    created_at: String,
}

#[derive(Deserialize)]
struct PledgeRequest {
    amount_cents: i64,
}

#[derive(Serialize)]
struct PledgeView {
    id: String,
    goal_id: String,
    user_id: String,
    amount_cents: i64,
    status: String,
    created_at: String,
    resolved_at: Option<String>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/wallet", get(get_wallet))
        .route("/api/wallet/topup", post(topup))
        .route("/api/wallet/ledger", get(get_ledger))
        .route(
            "/api/goals/{id}/pledge",
            get(get_pledge).post(propose_pledge),
        )
        .route("/api/goals/{id}/pledge/confirm", post(confirm_pledge))
        .route("/api/webhooks/stripe", post(stripe_webhook))
}

async fn get_wallet(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
) -> AppResult<Json<WalletView>> {
    ensure_wallet(&state.db, &user_id).await?;
    let (balance_cents, currency, token_spend_cents): (i64, String, i64) = sqlx::query_as(
        "SELECT balance_cents, currency, token_spend_cents FROM wallets WHERE user_id = ?",
    )
    .bind(&user_id)
    .fetch_one(&state.db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    Ok(Json(WalletView {
        balance_cents,
        currency,
        token_spend_cents,
    }))
}

async fn topup(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    WithRejection(Json(body), _): WithRejection<Json<TopupRequest>, AppError>,
) -> AppResult<Json<TopupResponse>> {
    ensure_wallet(&state.db, &user_id).await?;

    let amount = body.amount_cents;
    if amount < state.config.min_topup_cents {
        return Err(AppError::BadRequest(format!(
            "minimum top-up is {} cents",
            state.config.min_topup_cents
        )));
    }

    let current = balance_cents(&state.db, &user_id).await?;
    let projected = current
        .checked_add(amount)
        .ok_or_else(|| AppError::BadRequest("top-up amount out of range".to_string()))?;
    if projected > state.config.max_balance_cents {
        return Err(AppError::BadRequest(format!(
            "top-up would exceed the maximum balance of {} cents",
            state.config.max_balance_cents
        )));
    }

    let session = state
        .payments
        .create_topup(&user_id, amount)
        .await
        .map_err(AppError::Internal)?;

    // Hosted checkout: client redirects, webhook credits later.
    if session.checkout_url.is_some() {
        return Ok(Json(TopupResponse {
            checkout_url: session.checkout_url,
            balance_cents: current,
            status: session.status,
        }));
    }

    // Mock/instant backend: credit now.
    credit_topup(&state.db, &user_id, amount, &session.external_ref).await?;
    let new_balance = balance_cents(&state.db, &user_id).await?;
    Ok(Json(TopupResponse {
        checkout_url: None,
        balance_cents: new_balance,
        status: session.status,
    }))
}

#[allow(clippy::type_complexity)]
async fn get_ledger(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
) -> AppResult<Json<Vec<LedgerEntryView>>> {
    ensure_wallet(&state.db, &user_id).await?;
    let rows: Vec<(
        String,
        String,
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
    )> = sqlx::query_as(
        "SELECT id, kind, amount_cents, goal_id, external_ref, memo, created_at \
             FROM ledger_entries WHERE user_id = ? ORDER BY created_at DESC, id DESC LIMIT 100",
    )
    .bind(&user_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    let entries = rows
        .into_iter()
        .map(
            |(id, kind, amount_cents, goal_id, external_ref, memo, created_at)| LedgerEntryView {
                id,
                kind,
                amount_cents,
                goal_id,
                external_ref,
                memo,
                created_at,
            },
        )
        .collect();
    Ok(Json(entries))
}

/// Verify a goal belongs to `user_id`. The `goals` table is owned by another
/// migration; if it is not present yet the ownership check is skipped rather
/// than failing the request (dev resilience).
async fn verify_goal_owner(pool: &SqlitePool, goal_id: &str, user_id: &str) -> AppResult<()> {
    let res: Result<Option<(String,)>, sqlx::Error> =
        sqlx::query_as("SELECT user_id FROM goals WHERE id = ?")
            .bind(goal_id)
            .fetch_optional(pool)
            .await;
    match res {
        Ok(Some((owner,))) => {
            if owner == user_id {
                Ok(())
            } else {
                Err(AppError::Forbidden)
            }
        }
        Ok(None) => Err(AppError::NotFound),
        Err(e) => {
            if e.to_string().contains("no such table") {
                tracing::warn!("goals table not present; skipping pledge ownership check");
                Ok(())
            } else {
                Err(AppError::Internal(e.into()))
            }
        }
    }
}

#[allow(clippy::type_complexity)]
async fn load_pledge(pool: &SqlitePool, goal_id: &str) -> AppResult<PledgeView> {
    let (id, goal_id, user_id, amount_cents, status, created_at, resolved_at): (
        String,
        String,
        String,
        i64,
        String,
        String,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT id, goal_id, user_id, amount_cents, status, created_at, resolved_at \
         FROM pledges WHERE goal_id = ?",
    )
    .bind(goal_id)
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    Ok(PledgeView {
        id,
        goal_id,
        user_id,
        amount_cents,
        status,
        created_at,
        resolved_at,
    })
}

/// Load the caller's pledge for `goal_id`, scoped by ownership (the pledge row's
/// `user_id` must equal `user_id`). Returns `None` when no matching pledge exists
/// — either the goal has no pledge, or it belongs to another user.
#[allow(clippy::type_complexity)]
async fn load_pledge_for_user(
    pool: &SqlitePool,
    goal_id: &str,
    user_id: &str,
) -> AppResult<Option<PledgeView>> {
    let row: Option<(String, String, String, i64, String, String, Option<String>)> =
        sqlx::query_as(
            "SELECT id, goal_id, user_id, amount_cents, status, created_at, resolved_at \
         FROM pledges WHERE goal_id = ? AND user_id = ?",
        )
        .bind(goal_id)
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    Ok(row.map(
        |(id, goal_id, user_id, amount_cents, status, created_at, resolved_at)| PledgeView {
            id,
            goal_id,
            user_id,
            amount_cents,
            status,
            created_at,
            resolved_at,
        },
    ))
}

/// Read the caller's current pledge for a goal. Returns `200` with the pledge
/// (shaped like [`PledgeView`]) when one exists and belongs to the caller,
/// otherwise `204 No Content`. Lets the frontend show held/refunded/forfeited
/// state without proposing a new pledge.
async fn get_pledge(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(goal_id): Path<String>,
) -> AppResult<Response> {
    match load_pledge_for_user(&state.db, &goal_id, &user_id).await? {
        Some(view) => Ok(Json(view).into_response()),
        None => Ok(StatusCode::NO_CONTENT.into_response()),
    }
}

async fn propose_pledge(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(goal_id): Path<String>,
    WithRejection(Json(body), _): WithRejection<Json<PledgeRequest>, AppError>,
) -> AppResult<Json<PledgeView>> {
    if body.amount_cents <= 0 {
        return Err(AppError::BadRequest(
            "pledge amount must be positive".to_string(),
        ));
    }
    verify_goal_owner(&state.db, &goal_id, &user_id).await?;
    ensure_wallet(&state.db, &user_id).await?;

    // Don't clobber a pledge whose funds are already held.
    let existing: Option<(String,)> =
        sqlx::query_as("SELECT status FROM pledges WHERE goal_id = ?")
            .bind(&goal_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
    if let Some((status,)) = existing {
        if status == ai_buddy_core::domain::PledgeStatus::Held.as_str() {
            return Err(AppError::Conflict(
                "a pledge is already held for this goal".to_string(),
            ));
        }
    }

    let id = uuid::Uuid::new_v4().to_string();
    let now = ledger::now_rfc3339()?;
    sqlx::query(
        "INSERT INTO pledges (id, goal_id, user_id, amount_cents, status, created_at, resolved_at) \
         VALUES (?, ?, ?, ?, ?, ?, NULL) \
         ON CONFLICT(goal_id) DO UPDATE SET \
             amount_cents = excluded.amount_cents, \
             user_id = excluded.user_id, \
             status = excluded.status, \
             resolved_at = NULL",
    )
    .bind(id)
    .bind(&goal_id)
    .bind(&user_id)
    .bind(body.amount_cents)
    .bind(ai_buddy_core::domain::PledgeStatus::Proposed.as_str())
    .bind(&now)
    .execute(&state.db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    let view = load_pledge(&state.db, &goal_id).await?;
    Ok(Json(view))
}

async fn confirm_pledge(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(goal_id): Path<String>,
) -> AppResult<Json<PledgeView>> {
    let row: Option<(String, i64, String)> =
        sqlx::query_as("SELECT user_id, amount_cents, status FROM pledges WHERE goal_id = ?")
            .bind(&goal_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;

    let Some((owner, amount, status)) = row else {
        return Err(AppError::NotFound);
    };
    if owner != user_id {
        return Err(AppError::Forbidden);
    }
    if status == ai_buddy_core::domain::PledgeStatus::Held.as_str() {
        return Err(AppError::Conflict("pledge is already held".to_string()));
    }
    if status != ai_buddy_core::domain::PledgeStatus::Proposed.as_str() {
        return Err(AppError::Conflict(
            "pledge is not in a confirmable state".to_string(),
        ));
    }

    pledge_hold(&state.db, &user_id, &goal_id, amount).await?;
    let view = load_pledge(&state.db, &goal_id).await?;
    Ok(Json(view))
}

/// Stripe webhook endpoint — no auth, verified by signature. Reads the raw body
/// (signature is computed over the exact bytes) and the `Stripe-Signature`
/// header, then credits the wallet on a successful payment.
async fn stripe_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<StatusCode> {
    let sig = headers
        .get("Stripe-Signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();

    let event = state
        .payments
        .verify_webhook(sig, &body)
        .await
        .map_err(|e| AppError::BadRequest(format!("webhook verification failed: {e}")))?;

    if event.succeeded && event.amount_cents > 0 && !event.user_id.is_empty() {
        ensure_wallet(&state.db, &event.user_id).await?;
        credit_topup(
            &state.db,
            &event.user_id,
            event.amount_cents,
            &event.external_ref,
        )
        .await?;
    }

    Ok(StatusCode::OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let path = db_path.to_str().expect("utf8 path");
        let pool = crate::db::init_pool(path).await.expect("init pool");
        for id in ["u1", "u2"] {
            sqlx::query(
                "INSERT INTO users (id, email, display_name, created_at) \
                 VALUES (?, NULL, NULL, '2026-01-01T00:00:00Z')",
            )
            .bind(id)
            .execute(&pool)
            .await
            .expect("insert user");
        }
        (dir, pool)
    }

    async fn insert_pledge(pool: &SqlitePool, goal_id: &str, user_id: &str, status: &str) {
        sqlx::query(
            "INSERT INTO pledges (id, goal_id, user_id, amount_cents, status, created_at, resolved_at) \
             VALUES (?, ?, ?, ?, ?, '2026-01-01T00:00:00Z', NULL)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(goal_id)
        .bind(user_id)
        .bind(500_i64)
        .bind(status)
        .execute(pool)
        .await
        .expect("insert pledge");
    }

    #[tokio::test]
    async fn get_pledge_returns_owner_pledge() {
        let (_dir, pool) = test_pool().await;
        insert_pledge(&pool, "goal-1", "u1", "held").await;

        let view = load_pledge_for_user(&pool, "goal-1", "u1")
            .await
            .expect("query")
            .expect("pledge present");
        assert_eq!(view.goal_id, "goal-1");
        assert_eq!(view.user_id, "u1");
        assert_eq!(view.amount_cents, 500);
        assert_eq!(view.status, "held");
    }

    #[tokio::test]
    async fn get_pledge_none_when_absent() {
        let (_dir, pool) = test_pool().await;
        let result = load_pledge_for_user(&pool, "goal-missing", "u1")
            .await
            .expect("query");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn get_pledge_does_not_return_other_users_pledge() {
        let (_dir, pool) = test_pool().await;
        // A pledge on goal-2 owned by u2 must not surface for u1.
        insert_pledge(&pool, "goal-2", "u2", "proposed").await;

        let result = load_pledge_for_user(&pool, "goal-2", "u1")
            .await
            .expect("query");
        assert!(result.is_none());
    }
}
