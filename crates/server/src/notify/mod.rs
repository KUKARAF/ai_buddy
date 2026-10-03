//! Notifications + reminder/tip/check-in scheduler.
//!
//! A `reminders` row is both a scheduled job and (once delivered) an in-app
//! notification. [`Notifier::enqueue`] seeds rows (called by the goals module
//! when a roadmap is generated); [`Notifier::spawn_scheduler`] runs a background
//! tokio loop that every 60s polls rows that have come due
//! (`sent_at IS NULL AND scheduled_at <= now`), "delivers" each (MVP: a tracing
//! log line + stamping `sent_at`; push/LLM-tip generation hook later), and moves
//! on. The loop is resilient: per-tick and per-row errors are logged and the
//! loop continues — it never panics and never exits.
//!
//! Owns migration `0005_notify.sql` (`reminders`, `push_tokens`).

use std::time::Duration as StdDuration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use axum_extra::extract::WithRejection;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::session::RequireAuth;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// How often the background scheduler polls for due reminders.
const TICK_INTERVAL: StdDuration = StdDuration::from_secs(60);

/// Upper bound on reminders delivered per tick (keeps a tick bounded/non-blocking).
const POLL_BATCH: i64 = 200;

/// Notification store + enqueue handle. Cheap to clone (Arc-backed pool).
#[derive(Clone)]
pub struct Notifier {
    pool: SqlitePool,
}

/// One `reminders` row.
#[derive(Debug, sqlx::FromRow)]
struct ReminderRow {
    id: String,
    user_id: String,
    goal_id: Option<String>,
    step_id: Option<String>,
    kind: String,
    scheduled_at: String,
    sent_at: Option<String>,
    channel: String,
    payload: String,
    created_at: String,
    read_at: Option<String>,
}

/// A notification as returned by `GET /api/notifications` (payload parsed to JSON).
#[derive(Debug, Serialize)]
struct NotificationDto {
    id: String,
    goal_id: Option<String>,
    step_id: Option<String>,
    kind: String,
    scheduled_at: String,
    sent_at: Option<String>,
    channel: String,
    payload: serde_json::Value,
    created_at: String,
    read_at: Option<String>,
}

impl From<ReminderRow> for NotificationDto {
    fn from(r: ReminderRow) -> Self {
        // Stored payload is JSON TEXT; fall back to `{}` if it is somehow invalid
        // rather than failing the whole listing.
        let payload = serde_json::from_str(&r.payload).unwrap_or_else(|_| serde_json::json!({}));
        NotificationDto {
            id: r.id,
            goal_id: r.goal_id,
            step_id: r.step_id,
            kind: r.kind,
            scheduled_at: r.scheduled_at,
            sent_at: r.sent_at,
            channel: r.channel,
            payload,
            created_at: r.created_at,
            read_at: r.read_at,
        }
    }
}

/// Current UTC time as an Rfc3339 string. Errors bubble up as `AppError::Internal`.
fn now_rfc3339() -> anyhow::Result<String> {
    Ok(OffsetDateTime::now_utc().format(&Rfc3339)?)
}

/// Reminders that are due at `now` and not yet delivered, oldest first.
async fn poll_due(pool: &SqlitePool, now: &str) -> anyhow::Result<Vec<ReminderRow>> {
    let rows = sqlx::query_as::<_, ReminderRow>(
        "SELECT id, user_id, goal_id, step_id, kind, scheduled_at, sent_at, channel, payload, created_at, read_at \
         FROM reminders \
         WHERE sent_at IS NULL AND scheduled_at <= ? \
         ORDER BY scheduled_at ASC \
         LIMIT ?",
    )
    .bind(now)
    .bind(POLL_BATCH)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Deliver a single reminder. MVP: emit a tracing line and stamp `sent_at` (the
/// row, now with `sent_at` set, is the in-app notification). Push delivery via
/// `push_tokens` (FCM/web-push) would hook in here later.
async fn deliver(pool: &SqlitePool, reminder: &ReminderRow) -> anyhow::Result<()> {
    tracing::info!(
        reminder_id = %reminder.id,
        user_id = %reminder.user_id,
        kind = %reminder.kind,
        channel = %reminder.channel,
        goal_id = ?reminder.goal_id,
        step_id = ?reminder.step_id,
        "delivering notification"
    );

    let now = now_rfc3339()?;
    // Guard on `sent_at IS NULL` so a concurrent tick can't double-deliver.
    sqlx::query("UPDATE reminders SET sent_at = ? WHERE id = ? AND sent_at IS NULL")
        .bind(&now)
        .bind(&reminder.id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Derive a user-facing push title + body from a reminder's `kind` and parsed
/// `payload`. Pure (no I/O) so it is unit-testable. Prefers explicit
/// `title`/`body` fields in the payload, falling back to a `kind`-based title and
/// a `message`/`text` body.
fn reminder_push_text(kind: &str, payload: &serde_json::Value) -> (String, String) {
    let title = payload
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| match kind {
            "tip" => "A tip for you".to_string(),
            "checkin" => "Time to check in".to_string(),
            "reminder" => "Reminder".to_string(),
            "coach" => "Coach update".to_string(),
            other if !other.is_empty() => {
                let mut chars = other.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => "Reminder".to_string(),
                }
            }
            _ => "Reminder".to_string(),
        });

    let body = payload
        .get("body")
        .or_else(|| payload.get("message"))
        .or_else(|| payload.get("text"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    (title, body)
}

/// One scheduler pass: find due reminders and deliver each. A failure delivering
/// one reminder is logged and does not abort the rest of the batch.
async fn run_tick(pool: &SqlitePool, state: &AppState) -> anyhow::Result<()> {
    let now = now_rfc3339()?;
    let due = poll_due(pool, &now).await?;
    if due.is_empty() {
        return Ok(());
    }
    tracing::debug!(count = due.len(), "notify: delivering due reminders");

    for reminder in &due {
        // TODO(llm-tip): for `kind == "tip"` (ReminderKind::Tip) this is where an
        // LLM-generated, personalized tip could be produced via `state.llm` and
        // merged into the payload before delivery. Deliberately NOT called here —
        // the scheduler stays self-contained and non-blocking for the MVP.
        if let Err(e) = deliver(pool, reminder).await {
            tracing::error!(error = ?e, reminder_id = %reminder.id, "notify: delivery failed");
            continue;
        }

        // Best-effort mobile push for the just-delivered reminder (no-op when FCM
        // is unconfigured; never affects delivery/sent_at stamping above).
        let payload: serde_json::Value =
            serde_json::from_str(&reminder.payload).unwrap_or_else(|_| serde_json::json!({}));
        let (title, body) = reminder_push_text(&reminder.kind, &payload);
        crate::fcm::send_to_user(
            pool,
            &state.config,
            &reminder.user_id,
            &title,
            &body,
            Some(payload),
        )
        .await;
    }
    Ok(())
}

impl Notifier {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Enqueue a reminder/tip/check-in. Returns the new reminder id.
    ///
    /// Called by the goals module to seed roadmap reminders — signature is stable.
    /// `payload` is stored as JSON TEXT; `channel` is e.g. `"inapp"`.
    #[allow(clippy::too_many_arguments)]
    pub async fn enqueue(
        &self,
        user_id: &str,
        goal_id: Option<&str>,
        step_id: Option<&str>,
        kind: &str,
        scheduled_at: &str,
        channel: &str,
        payload: serde_json::Value,
    ) -> AppResult<String> {
        let id = Uuid::new_v4().to_string();
        let now = now_rfc3339()?;
        let payload_s =
            serde_json::to_string(&payload).map_err(|e| AppError::Internal(e.into()))?;

        sqlx::query(
            "INSERT INTO reminders \
                 (id, user_id, goal_id, step_id, kind, scheduled_at, sent_at, channel, payload, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, NULL, ?, ?, ?)",
        )
        .bind(&id)
        .bind(user_id)
        .bind(goal_id)
        .bind(step_id)
        .bind(kind)
        .bind(scheduled_at)
        .bind(channel)
        .bind(&payload_s)
        .bind(&now)
        .execute(&self.pool)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

        Ok(id)
    }

    /// Post an in-app notification that is immediately delivered and unread.
    /// Inserts scheduled_at=now, sent_at=now, read_at=NULL, channel="inapp".
    pub async fn post_now(
        &self,
        user_id: &str,
        goal_id: Option<&str>,
        step_id: Option<&str>,
        kind: &str,
        payload: serde_json::Value,
    ) -> AppResult<String> {
        let id = Uuid::new_v4().to_string();
        let now = now_rfc3339()?;
        let payload_s =
            serde_json::to_string(&payload).map_err(|e| AppError::Internal(e.into()))?;

        sqlx::query(
            "INSERT INTO reminders \
                 (id, user_id, goal_id, step_id, kind, scheduled_at, sent_at, channel, payload, created_at, read_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 'inapp', ?, ?, NULL)",
        )
        .bind(&id)
        .bind(user_id)
        .bind(goal_id)
        .bind(step_id)
        .bind(kind)
        .bind(&now)
        .bind(&now)
        .bind(&payload_s)
        .bind(&now)
        .execute(&self.pool)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

        Ok(id)
    }

    /// Spawn the background reminder/tip scheduler: a tokio loop that ticks every
    /// [`TICK_INTERVAL`], delivering due reminders. Resilient — a failed tick is
    /// logged and the loop continues; it never panics or exits.
    pub fn spawn_scheduler(self, state: AppState) {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(TICK_INTERVAL);
            loop {
                interval.tick().await;
                if let Err(e) = run_tick(&self.pool, &state).await {
                    tracing::error!(error = ?e, "notify: scheduler tick failed");
                }
            }
        });
    }
}

#[derive(Debug, Deserialize)]
struct PushTokenReq {
    platform: String,
    token: String,
}

/// `GET /api/notifications` — the caller's notifications, most recent first.
async fn list_notifications(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
) -> AppResult<Json<Vec<NotificationDto>>> {
    let rows = sqlx::query_as::<_, ReminderRow>(
        "SELECT id, user_id, goal_id, step_id, kind, scheduled_at, sent_at, channel, payload, created_at, read_at \
         FROM reminders \
         WHERE user_id = ? \
         ORDER BY scheduled_at DESC, created_at DESC \
         LIMIT ?",
    )
    .bind(&user_id)
    .bind(POLL_BATCH)
    .fetch_all(&state.db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    let out = rows.into_iter().map(NotificationDto::from).collect();
    Ok(Json(out))
}

/// `PATCH /api/notifications/{id}/read` — mark one notification read (no-op if
/// already read). Scoped to the caller via `user_id`.
async fn mark_read(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(id): Path<String>,
) -> AppResult<StatusCode> {
    let now = now_rfc3339()?;
    sqlx::query(
        "UPDATE reminders SET read_at = ? WHERE id = ? AND user_id = ? AND read_at IS NULL",
    )
    .bind(&now)
    .bind(&id)
    .bind(&user_id)
    .execute(&state.db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/notifications/read-all` — mark all the caller's unread
/// notifications read.
async fn mark_all_read(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
) -> AppResult<StatusCode> {
    let now = now_rfc3339()?;
    sqlx::query("UPDATE reminders SET read_at = ? WHERE user_id = ? AND read_at IS NULL")
        .bind(&now)
        .bind(&user_id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/push-tokens` — register/refresh a push token for the caller.
async fn upsert_push_token(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    WithRejection(Json(body), _): WithRejection<Json<PushTokenReq>, AppError>,
) -> AppResult<StatusCode> {
    let platform = body.platform.trim();
    let token = body.token.trim();
    if platform.is_empty() {
        return Err(AppError::BadRequest("platform is required".to_string()));
    }
    if token.is_empty() {
        return Err(AppError::BadRequest("token is required".to_string()));
    }

    let id = Uuid::new_v4().to_string();
    let now = now_rfc3339()?;

    // Upsert on (user_id, token): keep one row per token, refresh its platform.
    sqlx::query(
        "INSERT INTO push_tokens (id, user_id, platform, token, created_at) \
         VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(user_id, token) DO UPDATE SET platform = excluded.platform",
    )
    .bind(&id)
    .bind(&user_id)
    .bind(platform)
    .bind(token)
    .bind(&now)
    .execute(&state.db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/notifications", get(list_notifications))
        .route("/api/notifications/{id}/read", patch(mark_read))
        .route("/api/notifications/read-all", post(mark_all_read))
        .route("/api/push-tokens", post(upsert_push_token))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAST: &str = "2000-01-01T00:00:00Z";
    const FUTURE: &str = "2999-01-01T00:00:00Z";

    #[test]
    fn reminder_push_text_prefers_payload_fields() {
        let payload = serde_json::json!({ "title": "Custom", "body": "Do the thing" });
        let (title, body) = reminder_push_text("reminder", &payload);
        assert_eq!(title, "Custom");
        assert_eq!(body, "Do the thing");
    }

    #[test]
    fn reminder_push_text_falls_back_to_kind_and_message() {
        // No title => kind-based title; body falls back to `message` then `text`.
        let (title, body) = reminder_push_text("tip", &serde_json::json!({ "message": "stretch" }));
        assert_eq!(title, "A tip for you");
        assert_eq!(body, "stretch");

        let (title, body) =
            reminder_push_text("checkin", &serde_json::json!({ "text": "how's it going" }));
        assert_eq!(title, "Time to check in");
        assert_eq!(body, "how's it going");

        // Unknown kind => capitalized kind, empty body when nothing usable.
        let (title, body) = reminder_push_text("weekly", &serde_json::json!({}));
        assert_eq!(title, "Weekly");
        assert_eq!(body, "");
    }

    async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let pool = crate::db::init_pool(db_path.to_str().unwrap())
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO users (id, email, display_name, created_at) \
             VALUES ('u1', NULL, NULL, '2026-01-01T00:00:00Z'), \
                    ('u2', NULL, NULL, '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        (dir, pool)
    }

    /// Run the same SELECT as `list_notifications` for one user and map to DTOs.
    async fn list_for(pool: &SqlitePool, user_id: &str) -> Vec<NotificationDto> {
        let rows = sqlx::query_as::<_, ReminderRow>(
            "SELECT id, user_id, goal_id, step_id, kind, scheduled_at, sent_at, channel, payload, created_at, read_at \
             FROM reminders \
             WHERE user_id = ? \
             ORDER BY scheduled_at DESC, created_at DESC \
             LIMIT ?",
        )
        .bind(user_id)
        .bind(POLL_BATCH)
        .fetch_all(pool)
        .await
        .unwrap();
        rows.into_iter().map(NotificationDto::from).collect()
    }

    /// Read one row's `read_at` directly.
    async fn read_at_of(pool: &SqlitePool, id: &str) -> Option<String> {
        let (read_at,): (Option<String>,) =
            sqlx::query_as("SELECT read_at FROM reminders WHERE id = ?")
                .bind(id)
                .fetch_one(pool)
                .await
                .unwrap();
        read_at
    }

    #[tokio::test]
    async fn enqueue_inserts_a_row() {
        let (_dir, pool) = test_pool().await;
        let notifier = Notifier::new(pool.clone());

        let id = notifier
            .enqueue(
                "u1",
                Some("g1"),
                Some("s1"),
                "reminder",
                PAST,
                "inapp",
                serde_json::json!({ "title": "hi" }),
            )
            .await
            .unwrap();

        let (count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM reminders WHERE id = ? AND user_id = 'u1'")
                .bind(&id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 1);

        let (sent_at,): (Option<String>,) =
            sqlx::query_as("SELECT sent_at FROM reminders WHERE id = ?")
                .bind(&id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(sent_at, None, "newly enqueued reminder is not yet sent");
    }

    #[tokio::test]
    async fn due_reminder_is_polled_and_marked_sent() {
        let (_dir, pool) = test_pool().await;
        let notifier = Notifier::new(pool.clone());

        let id = notifier
            .enqueue(
                "u1",
                None,
                None,
                "tip",
                PAST,
                "inapp",
                serde_json::json!({}),
            )
            .await
            .unwrap();

        let now = now_rfc3339().unwrap();
        let due = poll_due(&pool, &now).await.unwrap();
        assert_eq!(due.len(), 1, "the due reminder is found by the poll query");
        let first = due.first().unwrap();
        assert_eq!(first.id, id);

        deliver(&pool, first).await.unwrap();

        let (sent_at,): (Option<String>,) =
            sqlx::query_as("SELECT sent_at FROM reminders WHERE id = ?")
                .bind(&id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(sent_at.is_some(), "delivered reminder has sent_at set");

        // A second poll no longer returns it (sent_at is set).
        let now2 = now_rfc3339().unwrap();
        let due2 = poll_due(&pool, &now2).await.unwrap();
        assert!(due2.is_empty(), "already-sent reminder is not re-polled");
    }

    #[tokio::test]
    async fn future_reminder_is_not_polled() {
        let (_dir, pool) = test_pool().await;
        let notifier = Notifier::new(pool.clone());

        notifier
            .enqueue(
                "u1",
                None,
                None,
                "checkin",
                FUTURE,
                "inapp",
                serde_json::json!({}),
            )
            .await
            .unwrap();

        let now = now_rfc3339().unwrap();
        let due = poll_due(&pool, &now).await.unwrap();
        assert!(due.is_empty(), "a future reminder is not yet due");
    }

    #[tokio::test]
    async fn post_now_inserts_delivered_unread_row() {
        let (_dir, pool) = test_pool().await;
        let notifier = Notifier::new(pool.clone());

        let id = notifier
            .post_now(
                "u1",
                Some("g1"),
                None,
                "coach",
                serde_json::json!({ "text": "nice work" }),
            )
            .await
            .unwrap();

        // Delivered immediately: sent_at set, read_at NULL, channel inapp.
        let (sent_at, channel): (Option<String>, String) =
            sqlx::query_as("SELECT sent_at, channel FROM reminders WHERE id = ?")
                .bind(&id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            sent_at.is_some(),
            "post_now delivers immediately (sent_at set)"
        );
        assert_eq!(channel, "inapp");
        assert_eq!(
            read_at_of(&pool, &id).await,
            None,
            "new notification is unread"
        );

        // The list query returns it with read_at = null.
        let list = list_for(&pool, "u1").await;
        let item = list.into_iter().find(|n| n.id == id).unwrap();
        assert_eq!(item.read_at, None);
        assert_eq!(item.sent_at, sent_at);
        let json = serde_json::to_value(&item).unwrap();
        assert!(
            json.get("read_at").is_some(),
            "DTO serializes a read_at field"
        );
        assert!(
            json.get("read_at").unwrap().is_null(),
            "read_at is null when unread"
        );
    }

    #[tokio::test]
    async fn mark_read_sets_read_at_and_is_idempotent() {
        let (_dir, pool) = test_pool().await;
        let notifier = Notifier::new(pool.clone());

        let id = notifier
            .post_now("u1", None, None, "coach", serde_json::json!({}))
            .await
            .unwrap();

        let now = now_rfc3339().unwrap();
        // Same statement as the PATCH handler.
        sqlx::query(
            "UPDATE reminders SET read_at = ? WHERE id = ? AND user_id = ? AND read_at IS NULL",
        )
        .bind(&now)
        .bind(&id)
        .bind("u1")
        .execute(&pool)
        .await
        .unwrap();

        let first = read_at_of(&pool, &id).await;
        assert_eq!(first.as_deref(), Some(now.as_str()), "read_at is stamped");

        // Marking read again is a no-op: the `read_at IS NULL` guard means the
        // already-set timestamp is not overwritten.
        let later = "2999-12-31T23:59:59Z";
        sqlx::query(
            "UPDATE reminders SET read_at = ? WHERE id = ? AND user_id = ? AND read_at IS NULL",
        )
        .bind(later)
        .bind(&id)
        .bind("u1")
        .execute(&pool)
        .await
        .unwrap();

        assert_eq!(
            read_at_of(&pool, &id).await,
            first,
            "re-marking an already-read notification does not change read_at"
        );
    }

    #[tokio::test]
    async fn mark_read_rejects_another_users_notification() {
        let (_dir, pool) = test_pool().await;
        let notifier = Notifier::new(pool.clone());

        let id = notifier
            .post_now("u1", None, None, "coach", serde_json::json!({}))
            .await
            .unwrap();

        let now = now_rfc3339().unwrap();
        // u2 attempts to mark u1's notification read.
        sqlx::query(
            "UPDATE reminders SET read_at = ? WHERE id = ? AND user_id = ? AND read_at IS NULL",
        )
        .bind(&now)
        .bind(&id)
        .bind("u2")
        .execute(&pool)
        .await
        .unwrap();

        assert_eq!(
            read_at_of(&pool, &id).await,
            None,
            "another user cannot mark this notification read"
        );
    }

    #[tokio::test]
    async fn read_all_clears_only_callers_unread() {
        let (_dir, pool) = test_pool().await;
        let notifier = Notifier::new(pool.clone());

        let a = notifier
            .post_now("u1", None, None, "coach", serde_json::json!({}))
            .await
            .unwrap();
        let b = notifier
            .post_now("u1", None, None, "coach", serde_json::json!({}))
            .await
            .unwrap();
        let other = notifier
            .post_now("u2", None, None, "coach", serde_json::json!({}))
            .await
            .unwrap();

        let now = now_rfc3339().unwrap();
        // Same statement as the read-all handler.
        sqlx::query("UPDATE reminders SET read_at = ? WHERE user_id = ? AND read_at IS NULL")
            .bind(&now)
            .bind("u1")
            .execute(&pool)
            .await
            .unwrap();

        assert!(read_at_of(&pool, &a).await.is_some(), "u1's first is read");
        assert!(read_at_of(&pool, &b).await.is_some(), "u1's second is read");
        assert_eq!(
            read_at_of(&pool, &other).await,
            None,
            "another user's notification is untouched"
        );

        // u1 now has no unread rows.
        let u1_unread = list_for(&pool, "u1")
            .await
            .into_iter()
            .filter(|n| n.read_at.is_none())
            .count();
        assert_eq!(u1_unread, 0);
    }
}
