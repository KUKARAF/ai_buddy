//! Wallet balance + ledger + pledge operations (raw SQL).
//!
//! These are free `pub async fn`s (not methods) because other modules call them
//! directly — the chat handler debits token spend after each LLM call, and the
//! goals module resolves pledges. **Keep these signatures stable.**
//!
//! Accounting model:
//! - `wallets.balance_cents` is the authoritative spendable balance.
//! - `ledger_entries` is an append-only audit log; `amount_cents` is signed
//!   (credits positive, debits negative).
//! - A top-up credits the balance. A token debit reduces it (clamped at zero).
//! - A pledge hold moves funds *out* of the spendable balance into escrow
//!   (tracked by the pledge row's `held` status). A refund returns them; a
//!   forfeit realizes the loss (the funds already left the balance at hold time,
//!   so forfeit records the event without touching the balance again).

use ai_buddy_core::domain::{LedgerKind, PledgeStatus};
use sqlx::{SqliteConnection, SqlitePool};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::error::{AppError, AppResult};

/// Current UTC time formatted as an RFC 3339 string.
pub(super) fn now_rfc3339() -> AppResult<String> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|e| AppError::Internal(e.into()))
}

/// Insert one audit-log row. Runs on whatever connection/transaction is passed.
#[allow(clippy::too_many_arguments)]
async fn insert_ledger(
    conn: &mut SqliteConnection,
    user_id: &str,
    kind: LedgerKind,
    amount_cents: i64,
    goal_id: Option<&str>,
    external_ref: Option<&str>,
    memo: Option<&str>,
    now: &str,
) -> AppResult<()> {
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO ledger_entries \
         (id, user_id, kind, amount_cents, goal_id, external_ref, memo, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(user_id)
    .bind(kind.as_str())
    .bind(amount_cents)
    .bind(goal_id)
    .bind(external_ref)
    .bind(memo)
    .bind(now)
    .execute(&mut *conn)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    Ok(())
}

/// Create the user's wallet row if it does not already exist. Idempotent.
pub async fn ensure_wallet(pool: &SqlitePool, user_id: &str) -> AppResult<()> {
    let now = now_rfc3339()?;
    sqlx::query(
        "INSERT OR IGNORE INTO wallets \
         (user_id, balance_cents, currency, token_spend_cents, updated_at) \
         VALUES (?, 0, 'EUR', 0, ?)",
    )
    .bind(user_id)
    .bind(&now)
    .execute(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    Ok(())
}

/// The user's current spendable balance in cents (0 if no wallet row yet).
pub async fn balance_cents(pool: &SqlitePool, user_id: &str) -> AppResult<i64> {
    let row: Option<(i64,)> = sqlx::query_as("SELECT balance_cents FROM wallets WHERE user_id = ?")
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(row.map(|r| r.0).unwrap_or(0))
}

/// Debit LLM token spend from the wallet. Called by the chat handler after each
/// LLM call. `cents` must be non-negative. The debit is **clamped** to the
/// available balance — the balance is allowed to reach 0 but never goes below —
/// so this never fails on an empty wallet (the 402 gate is enforced *before*
/// the LLM call). Both `balance_cents` and `token_spend_cents` move by the
/// actually-debited amount, and a signed `token_debit` ledger row is appended.
pub async fn debit_tokens(
    pool: &SqlitePool,
    user_id: &str,
    cents: i64,
    memo: &str,
) -> AppResult<()> {
    if cents < 0 {
        return Err(AppError::BadRequest(
            "debit amount must be non-negative".to_string(),
        ));
    }
    ensure_wallet(pool, user_id).await?;

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    let (balance,): (i64,) = sqlx::query_as("SELECT balance_cents FROM wallets WHERE user_id = ?")
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    let debit = cents.min(balance).max(0);
    if debit == 0 {
        // Nothing to debit (empty balance or zero cost); no ledger noise.
        tx.commit()
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        return Ok(());
    }

    let now = now_rfc3339()?;
    sqlx::query(
        "UPDATE wallets SET balance_cents = balance_cents - ?, \
         token_spend_cents = token_spend_cents + ?, updated_at = ? WHERE user_id = ?",
    )
    .bind(debit)
    .bind(debit)
    .bind(&now)
    .bind(user_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    insert_ledger(
        &mut tx,
        user_id,
        LedgerKind::TokenDebit,
        -debit,
        None,
        None,
        Some(memo),
        &now,
    )
    .await?;

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(())
}

/// Credit a completed top-up to the wallet. `cents` must be positive. The
/// maximum-balance cap is enforced by the HTTP handler (this op takes only the
/// pool, per the contract). Idempotent per `external_ref`: a second call with an
/// `external_ref` already recorded as a top-up is a no-op (webhook redelivery).
pub async fn credit_topup(
    pool: &SqlitePool,
    user_id: &str,
    cents: i64,
    external_ref: &str,
) -> AppResult<()> {
    if cents <= 0 {
        return Err(AppError::BadRequest(
            "top-up amount must be positive".to_string(),
        ));
    }
    ensure_wallet(pool, user_id).await?;

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    let already: Option<(String,)> =
        sqlx::query_as("SELECT id FROM ledger_entries WHERE kind = ? AND external_ref = ?")
            .bind(LedgerKind::Topup.as_str())
            .bind(external_ref)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
    if already.is_some() {
        tx.commit()
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        return Ok(());
    }

    let now = now_rfc3339()?;
    sqlx::query(
        "UPDATE wallets SET balance_cents = balance_cents + ?, updated_at = ? WHERE user_id = ?",
    )
    .bind(cents)
    .bind(&now)
    .bind(user_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    insert_ledger(
        &mut tx,
        user_id,
        LedgerKind::Topup,
        cents,
        None,
        Some(external_ref),
        None,
        &now,
    )
    .await?;

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(())
}

/// Move `amount_cents` from the spendable balance into escrow for `goal_id`,
/// marking the pledge `held`. Errors with [`AppError::PaymentRequired`] if the
/// balance is insufficient. Upserts the pledge row (a prior `proposed` pledge on
/// the same goal is transitioned to `held`).
pub async fn pledge_hold(
    pool: &SqlitePool,
    user_id: &str,
    goal_id: &str,
    amount_cents: i64,
) -> AppResult<()> {
    if amount_cents <= 0 {
        return Err(AppError::BadRequest(
            "pledge amount must be positive".to_string(),
        ));
    }
    ensure_wallet(pool, user_id).await?;

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    let (balance,): (i64,) = sqlx::query_as("SELECT balance_cents FROM wallets WHERE user_id = ?")
        .bind(user_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    if balance < amount_cents {
        return Err(AppError::PaymentRequired(
            "insufficient wallet balance to hold this pledge".to_string(),
        ));
    }

    let now = now_rfc3339()?;
    sqlx::query(
        "UPDATE wallets SET balance_cents = balance_cents - ?, updated_at = ? WHERE user_id = ?",
    )
    .bind(amount_cents)
    .bind(&now)
    .bind(user_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    let pledge_id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO pledges (id, goal_id, user_id, amount_cents, status, created_at, resolved_at) \
         VALUES (?, ?, ?, ?, ?, ?, NULL) \
         ON CONFLICT(goal_id) DO UPDATE SET \
             amount_cents = excluded.amount_cents, \
             user_id = excluded.user_id, \
             status = excluded.status, \
             resolved_at = NULL",
    )
    .bind(pledge_id)
    .bind(goal_id)
    .bind(user_id)
    .bind(amount_cents)
    .bind(PledgeStatus::Held.as_str())
    .bind(&now)
    .execute(&mut *tx)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    insert_ledger(
        &mut tx,
        user_id,
        LedgerKind::PledgeHold,
        -amount_cents,
        Some(goal_id),
        None,
        None,
        &now,
    )
    .await?;

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(())
}

/// Return a held pledge's funds to the user's spendable balance and mark it
/// `refunded`. Errors if there is no pledge for `goal_id` or it is not `held`.
pub async fn pledge_refund(pool: &SqlitePool, goal_id: &str) -> AppResult<()> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    let row: Option<(String, i64, String)> =
        sqlx::query_as("SELECT user_id, amount_cents, status FROM pledges WHERE goal_id = ?")
            .bind(goal_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;

    let Some((user_id, amount, status)) = row else {
        return Err(AppError::NotFound);
    };
    if status != PledgeStatus::Held.as_str() {
        return Err(AppError::Conflict(
            "pledge is not held; cannot refund".to_string(),
        ));
    }

    let now = now_rfc3339()?;
    sqlx::query(
        "UPDATE wallets SET balance_cents = balance_cents + ?, updated_at = ? WHERE user_id = ?",
    )
    .bind(amount)
    .bind(&now)
    .bind(&user_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    sqlx::query("UPDATE pledges SET status = ?, resolved_at = ? WHERE goal_id = ?")
        .bind(PledgeStatus::Refunded.as_str())
        .bind(&now)
        .bind(goal_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    insert_ledger(
        &mut tx,
        &user_id,
        LedgerKind::PledgeRefund,
        amount,
        Some(goal_id),
        None,
        None,
        &now,
    )
    .await?;

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(())
}

/// Forfeit a held pledge: the escrowed funds are lost (they already left the
/// spendable balance at hold time, so the balance is not touched again). Marks
/// the pledge `forfeited` and records a `pledge_forfeit` ledger row. Errors if
/// there is no pledge for `goal_id` or it is not `held`.
pub async fn pledge_forfeit(pool: &SqlitePool, goal_id: &str) -> AppResult<()> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    let row: Option<(String, i64, String)> =
        sqlx::query_as("SELECT user_id, amount_cents, status FROM pledges WHERE goal_id = ?")
            .bind(goal_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;

    let Some((user_id, amount, status)) = row else {
        return Err(AppError::NotFound);
    };
    if status != PledgeStatus::Held.as_str() {
        return Err(AppError::Conflict(
            "pledge is not held; cannot forfeit".to_string(),
        ));
    }

    let now = now_rfc3339()?;
    sqlx::query("UPDATE pledges SET status = ?, resolved_at = ? WHERE goal_id = ?")
        .bind(PledgeStatus::Forfeited.as_str())
        .bind(&now)
        .bind(goal_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    insert_ledger(
        &mut tx,
        &user_id,
        LedgerKind::PledgeForfeit,
        -amount,
        Some(goal_id),
        None,
        None,
        &now,
    )
    .await?;

    tx.commit()
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let path = db_path.to_str().expect("utf8 path");
        let pool = crate::db::init_pool(path).await.expect("init pool");
        sqlx::query(
            "INSERT INTO users (id, email, display_name, created_at) \
             VALUES ('u1', NULL, NULL, '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .expect("insert user");
        (dir, pool)
    }

    #[tokio::test]
    async fn ensure_wallet_is_idempotent() {
        let (_dir, pool) = test_pool().await;
        ensure_wallet(&pool, "u1").await.expect("first");
        ensure_wallet(&pool, "u1").await.expect("second");
        assert_eq!(balance_cents(&pool, "u1").await.expect("balance"), 0);
    }

    #[tokio::test]
    async fn topup_credits_balance() {
        let (_dir, pool) = test_pool().await;
        credit_topup(&pool, "u1", 1000, "ext-1")
            .await
            .expect("credit");
        assert_eq!(balance_cents(&pool, "u1").await.expect("balance"), 1000);
    }

    #[tokio::test]
    async fn topup_is_idempotent_per_external_ref() {
        let (_dir, pool) = test_pool().await;
        credit_topup(&pool, "u1", 1000, "ext-1")
            .await
            .expect("credit 1");
        credit_topup(&pool, "u1", 1000, "ext-1")
            .await
            .expect("credit 2");
        assert_eq!(balance_cents(&pool, "u1").await.expect("balance"), 1000);
    }

    #[tokio::test]
    async fn debit_reduces_balance() {
        let (_dir, pool) = test_pool().await;
        credit_topup(&pool, "u1", 1000, "ext-1")
            .await
            .expect("credit");
        debit_tokens(&pool, "u1", 300, "chat").await.expect("debit");
        assert_eq!(balance_cents(&pool, "u1").await.expect("balance"), 700);

        let (spend,): (i64,) =
            sqlx::query_as("SELECT token_spend_cents FROM wallets WHERE user_id = 'u1'")
                .fetch_one(&pool)
                .await
                .expect("spend");
        assert_eq!(spend, 300);
    }

    #[tokio::test]
    async fn debit_never_goes_below_zero() {
        let (_dir, pool) = test_pool().await;
        credit_topup(&pool, "u1", 100, "ext-1")
            .await
            .expect("credit");
        // Debit more than the balance: clamps to the available 100.
        debit_tokens(&pool, "u1", 500, "chat").await.expect("debit");
        assert_eq!(balance_cents(&pool, "u1").await.expect("balance"), 0);
        // Further debit on an empty wallet is a harmless no-op.
        debit_tokens(&pool, "u1", 50, "chat")
            .await
            .expect("debit empty");
        assert_eq!(balance_cents(&pool, "u1").await.expect("balance"), 0);
    }

    #[tokio::test]
    async fn pledge_hold_moves_funds() {
        let (_dir, pool) = test_pool().await;
        credit_topup(&pool, "u1", 1000, "ext-1")
            .await
            .expect("credit");
        pledge_hold(&pool, "u1", "goal-1", 400).await.expect("hold");
        assert_eq!(balance_cents(&pool, "u1").await.expect("balance"), 600);

        let (status,): (String,) =
            sqlx::query_as("SELECT status FROM pledges WHERE goal_id = 'goal-1'")
                .fetch_one(&pool)
                .await
                .expect("status");
        assert_eq!(status, PledgeStatus::Held.as_str());
    }

    #[tokio::test]
    async fn pledge_hold_insufficient_balance_errors() {
        let (_dir, pool) = test_pool().await;
        credit_topup(&pool, "u1", 100, "ext-1")
            .await
            .expect("credit");
        let err = pledge_hold(&pool, "u1", "goal-1", 400)
            .await
            .expect_err("should fail");
        assert!(matches!(err, AppError::PaymentRequired(_)));
        // Balance untouched on failure.
        assert_eq!(balance_cents(&pool, "u1").await.expect("balance"), 100);
    }

    #[tokio::test]
    async fn pledge_refund_returns_funds() {
        let (_dir, pool) = test_pool().await;
        credit_topup(&pool, "u1", 1000, "ext-1")
            .await
            .expect("credit");
        pledge_hold(&pool, "u1", "goal-1", 400).await.expect("hold");
        pledge_refund(&pool, "goal-1").await.expect("refund");
        assert_eq!(balance_cents(&pool, "u1").await.expect("balance"), 1000);

        let (status,): (String,) =
            sqlx::query_as("SELECT status FROM pledges WHERE goal_id = 'goal-1'")
                .fetch_one(&pool)
                .await
                .expect("status");
        assert_eq!(status, PledgeStatus::Refunded.as_str());
    }

    #[tokio::test]
    async fn pledge_forfeit_keeps_funds_removed() {
        let (_dir, pool) = test_pool().await;
        credit_topup(&pool, "u1", 1000, "ext-1")
            .await
            .expect("credit");
        pledge_hold(&pool, "u1", "goal-1", 400).await.expect("hold");
        pledge_forfeit(&pool, "goal-1").await.expect("forfeit");
        // Funds stay gone (already removed at hold time).
        assert_eq!(balance_cents(&pool, "u1").await.expect("balance"), 600);

        let (status,): (String,) =
            sqlx::query_as("SELECT status FROM pledges WHERE goal_id = 'goal-1'")
                .fetch_one(&pool)
                .await
                .expect("status");
        assert_eq!(status, PledgeStatus::Forfeited.as_str());
    }
}
