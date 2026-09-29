//! Check-ins + streaks.
//!
//! A `check_ins` row records that a user "showed up" on a given day, optionally
//! attached to a goal and carrying a free-text note and/or mood. From the set of
//! distinct UTC calendar dates on which a user has checked in we derive their
//! current streak, longest streak, and a rolling 7-day window (see
//! [`compute_streak`], a pure helper kept free of the DB for easy testing).
//!
//! Owns migration `0008_checkins.sql` (`check_ins`).
//!
//! Routes (all require auth):
//!   - `POST /api/goals/{id}/check-ins`  check in against a specific goal
//!   - `GET  /api/goals/{id}/check-ins`  that goal's check-ins, newest first
//!   - `POST /api/check-ins`             a general (optionally goal-less) check-in
//!   - `GET  /api/check-ins`             the caller's recent check-ins, newest first
//!   - `GET  /api/streak`                current/longest streak + last-7-days window

use std::collections::HashSet;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::WithRejection;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use time::format_description::well_known::Rfc3339;
use time::{Date, Duration, Month, OffsetDateTime};
use uuid::Uuid;

use crate::auth::session::RequireAuth;
use crate::config::Config;
use crate::error::{AppError, AppResult};
use crate::llm::LlmClient;
use crate::state::AppState;

/// Upper bound on rows returned by `GET /api/check-ins`.
const LIST_LIMIT: i64 = 100;

/// Notes shorter than this (after trimming) are treated as routine and skip the
/// LLM classification entirely — nothing to reason about, and it keeps the fast
/// path free of a network round-trip for a bare "done".
const MIN_CLASSIFY_NOTE_LEN: usize = 6;

/// System prompt for the best-effort check-in note classifier. Asks for a strict
/// JSON verdict on whether the plan/timeline should be revisited.
const CLASSIFY_SYSTEM: &str = "You analyze a user's check-in note about a goal. Decide if it implies the plan/timeline should be revisited — e.g. they'll be unavailable for a period (travel, holidays like Christmas, vacation), they're falling behind or ahead, injured/sick, or circumstances changed. Respond ONLY as JSON: {\"adjust\": boolean, \"message\": string}. If adjust is true, message is a SHORT, warm one-sentence offer from the coach to update the plan (e.g. \"Sounds like you'll be away over Christmas — want me to add a break and shift your milestones?\"). If the note is just routine progress with no scheduling impact, return {\"adjust\": false, \"message\": null}. Keep message under 160 characters.";

/// One `check_ins` row as returned to clients.
#[derive(Debug, Serialize, sqlx::FromRow)]
struct CheckIn {
    id: String,
    goal_id: Option<String>,
    note: Option<String>,
    mood: Option<String>,
    created_at: String,
}

/// Coach "offer" attached to a check-in response: whether the plan/timeline
/// should be revisited, plus a short warm message when so (`null` otherwise).
#[derive(Debug, Serialize)]
struct Suggestion {
    adjust: bool,
    message: Option<String>,
}

impl Suggestion {
    /// The neutral default returned whenever classification is skipped or fails:
    /// no adjustment, no message. Never fails a check-in.
    fn none() -> Self {
        Suggestion {
            adjust: false,
            message: None,
        }
    }
}

/// The goal-scoped check-in response: the stored row plus a coach `suggestion`.
#[derive(Debug, Serialize)]
struct GoalCheckInResponse {
    #[serde(flatten)]
    check_in: CheckIn,
    suggestion: Suggestion,
}

/// The model's verdict, parsed from the classifier's JSON output. Fields default
/// so a partial object still deserializes (best-effort classification).
#[derive(Debug, Deserialize)]
struct Classification {
    #[serde(default)]
    adjust: bool,
    #[serde(default)]
    message: Option<String>,
}

/// Body of `POST /api/goals/{id}/check-ins`.
#[derive(Debug, Deserialize)]
struct GoalCheckInReq {
    note: Option<String>,
    mood: Option<String>,
}

/// Body of `POST /api/check-ins` (goal is optional).
#[derive(Debug, Deserialize)]
struct GeneralCheckInReq {
    note: Option<String>,
    mood: Option<String>,
    goal_id: Option<String>,
}

/// One day of the rolling 7-day window in the streak response.
#[derive(Debug, Serialize)]
struct WeekDay {
    date: String,
    checked: bool,
}

/// Response of `GET /api/streak`.
#[derive(Debug, Serialize)]
struct StreakResponse {
    current_streak: i64,
    longest_streak: i64,
    week: Vec<WeekDay>,
}

/// Current UTC time as an Rfc3339 string. Errors bubble up as `AppError::Internal`.
fn now_rfc3339() -> anyhow::Result<String> {
    Ok(OffsetDateTime::now_utc().format(&Rfc3339)?)
}

/// Format a date as `YYYY-MM-DD` (matching the `substr(created_at,1,10)` keys).
fn fmt_date(d: Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}

/// Parse a `YYYY-MM-DD` prefix into a [`Date`], or `None` if malformed.
fn parse_ymd(s: &str) -> Option<Date> {
    let year: i32 = s.get(0..4)?.parse().ok()?;
    let month: u8 = s.get(5..7)?.parse().ok()?;
    let day: u8 = s.get(8..10)?.parse().ok()?;
    let month = Month::try_from(month).ok()?;
    Date::from_calendar_date(year, month, day).ok()
}

/// Pure streak math over a user's set of distinct check-in dates.
///
/// `dates` are distinct `YYYY-MM-DD` strings (order not relied upon). Returns
/// `(current_streak, longest_streak, week)` where:
///   - `current_streak` counts consecutive UTC days with a check-in ending
///     today; if today has none yet it counts back from yesterday instead.
///   - `longest_streak` is the longest run of consecutive days over all history.
///   - `week` is the last 7 days including today, oldest→newest, each flagged
///     with whether that date has a check-in.
fn compute_streak(dates: &[String], today: Date) -> (i64, i64, Vec<(String, bool)>) {
    let set: HashSet<&str> = dates.iter().map(String::as_str).collect();
    let has = |d: Date| set.contains(fmt_date(d).as_str());

    // Current streak: start from today (or yesterday if today has none yet),
    // then walk backwards while each prior day has a check-in.
    let mut current: i64 = 0;
    let mut cursor = if has(today) {
        today
    } else {
        today - Duration::days(1)
    };
    while has(cursor) {
        current += 1;
        cursor -= Duration::days(1);
    }

    // Longest streak: longest run of consecutive calendar days over history.
    let mut days: Vec<i64> = dates
        .iter()
        .filter_map(|s| parse_ymd(s))
        .map(|d| d.to_julian_day() as i64)
        .collect();
    days.sort_unstable();
    days.dedup();
    let mut longest: i64 = 0;
    let mut run: i64 = 0;
    let mut prev: Option<i64> = None;
    for d in &days {
        run = match prev {
            Some(p) if *d == p + 1 => run + 1,
            _ => 1,
        };
        if run > longest {
            longest = run;
        }
        prev = Some(*d);
    }

    // Rolling 7-day window, oldest→newest, including today.
    let mut week = Vec::with_capacity(7);
    for i in (0..7i64).rev() {
        let day = today - Duration::days(i);
        let s = fmt_date(day);
        let checked = set.contains(s.as_str());
        week.push((s, checked));
    }

    (current, longest, week)
}

/// Verify `goal_id` exists and belongs to `user_id`; otherwise `NotFound`
/// (a missing goal and another user's goal are indistinguishable to the caller).
async fn verify_goal_owner(pool: &SqlitePool, user_id: &str, goal_id: &str) -> AppResult<()> {
    let owner: Option<(String,)> = sqlx::query_as("SELECT user_id FROM goals WHERE id = ?")
        .bind(goal_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    match owner {
        Some((o,)) if o == user_id => Ok(()),
        _ => Err(AppError::NotFound),
    }
}

/// Fetch one check-in by id (used to return the freshly-inserted row).
async fn fetch_checkin(pool: &SqlitePool, id: &str) -> AppResult<Option<CheckIn>> {
    sqlx::query_as::<_, CheckIn>(
        "SELECT id, goal_id, note, mood, created_at FROM check_ins WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))
}

/// Insert a check-in and return the created row.
async fn insert_checkin(
    pool: &SqlitePool,
    user_id: &str,
    goal_id: Option<&str>,
    note: Option<&str>,
    mood: Option<&str>,
) -> AppResult<CheckIn> {
    let id = Uuid::new_v4().to_string();
    let now = now_rfc3339()?;
    sqlx::query(
        "INSERT INTO check_ins (id, user_id, goal_id, note, mood, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(goal_id)
    .bind(note)
    .bind(mood)
    .bind(&now)
    .execute(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    fetch_checkin(pool, &id).await?.ok_or(AppError::NotFound)
}

/// Normalize an optional free-text field: trim, and treat empty as absent.
fn normalize(s: Option<String>) -> Option<String> {
    s.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// List check-ins for a goal, newest first.
async fn list_goal_checkins(pool: &SqlitePool, goal_id: &str) -> AppResult<Vec<CheckIn>> {
    sqlx::query_as::<_, CheckIn>(
        "SELECT id, goal_id, note, mood, created_at FROM check_ins \
         WHERE goal_id = ? ORDER BY created_at DESC, id DESC",
    )
    .bind(goal_id)
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))
}

/// Best-effort classification of a check-in note into a coach [`Suggestion`].
///
/// This NEVER fails: on a missing/short note, an unresolved model, a missing LLM
/// key, a network/parse error, or any other problem it falls back to
/// [`Suggestion::none`]. The token cost of a successful call is debited best-effort
/// (only when positive; a debit failure is swallowed). Callers must treat the
/// check-in itself as the source of truth — the suggestion is purely advisory.
async fn classify_note(
    llm: &LlmClient,
    pool: &SqlitePool,
    config: &Config,
    user_id: &str,
    note: Option<&str>,
) -> Suggestion {
    // Nothing meaningful to classify: empty or very short notes are routine.
    let note = match note.map(str::trim) {
        Some(n) if n.len() >= MIN_CLASSIFY_NOTE_LEN => n,
        _ => return Suggestion::none(),
    };

    let model = match crate::settings::user_chat_model(pool, config, user_id).await {
        Ok(m) => m,
        Err(_) => return Suggestion::none(),
    };

    let (classification, cost_cents) = match llm
        .chat_json::<Classification>(&model, CLASSIFY_SYSTEM, note)
        .await
    {
        Ok(pair) => pair,
        Err(_) => return Suggestion::none(),
    };

    // Charge the wallet best-effort; a debit failure must not fail the check-in.
    if cost_cents > 0 {
        let _ = crate::wallet::debit_tokens(pool, user_id, cost_cents, "checkin-classify").await;
    }

    if classification.adjust {
        let message = classification
            .message
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty());
        Suggestion {
            adjust: true,
            message,
        }
    } else {
        Suggestion::none()
    }
}

/// `POST /api/goals/{id}/check-ins` — check in against a specific goal.
///
/// Stores the check-in, then runs a best-effort LLM classification of the note to
/// attach a coach `suggestion`. Classification never blocks or fails the check-in:
/// on any error it degrades to `{ adjust: false, message: null }`.
async fn create_goal_checkin(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(goal_id): Path<String>,
    WithRejection(Json(body), _): WithRejection<Json<GoalCheckInReq>, AppError>,
) -> AppResult<Json<GoalCheckInResponse>> {
    verify_goal_owner(&state.db, &user_id, &goal_id).await?;
    let note = normalize(body.note);
    let mood = normalize(body.mood);
    let check_in = insert_checkin(
        &state.db,
        &user_id,
        Some(&goal_id),
        note.as_deref(),
        mood.as_deref(),
    )
    .await?;

    let suggestion = classify_note(
        &state.llm,
        &state.db,
        &state.config,
        &user_id,
        note.as_deref(),
    )
    .await;

    Ok(Json(GoalCheckInResponse {
        check_in,
        suggestion,
    }))
}

/// `GET /api/goals/{id}/check-ins` — a goal's check-ins, newest first.
async fn list_goal_checkins_handler(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(goal_id): Path<String>,
) -> AppResult<Json<Vec<CheckIn>>> {
    verify_goal_owner(&state.db, &user_id, &goal_id).await?;
    let rows = list_goal_checkins(&state.db, &goal_id).await?;
    Ok(Json(rows))
}

/// `POST /api/check-ins` — a general (optionally goal-less) check-in.
async fn create_checkin(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    WithRejection(Json(body), _): WithRejection<Json<GeneralCheckInReq>, AppError>,
) -> AppResult<Json<CheckIn>> {
    let goal_id = normalize(body.goal_id);
    if let Some(gid) = goal_id.as_deref() {
        verify_goal_owner(&state.db, &user_id, gid).await?;
    }
    let note = normalize(body.note);
    let mood = normalize(body.mood);
    let created = insert_checkin(
        &state.db,
        &user_id,
        goal_id.as_deref(),
        note.as_deref(),
        mood.as_deref(),
    )
    .await?;
    Ok(Json(created))
}

/// `GET /api/check-ins` — the caller's recent check-ins, newest first.
async fn list_checkins(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
) -> AppResult<Json<Vec<CheckIn>>> {
    let rows = sqlx::query_as::<_, CheckIn>(
        "SELECT id, goal_id, note, mood, created_at FROM check_ins \
         WHERE user_id = ? ORDER BY created_at DESC, id DESC LIMIT ?",
    )
    .bind(&user_id)
    .bind(LIST_LIMIT)
    .fetch_all(&state.db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    Ok(Json(rows))
}

/// `GET /api/streak` — current/longest streak + last-7-days window.
async fn get_streak(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
) -> AppResult<Json<StreakResponse>> {
    let dates: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT substr(created_at, 1, 10) FROM check_ins \
         WHERE user_id = ? ORDER BY 1",
    )
    .bind(&user_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    let dates: Vec<String> = dates.into_iter().map(|(d,)| d).collect();
    let today = OffsetDateTime::now_utc().date();
    let (current_streak, longest_streak, week) = compute_streak(&dates, today);
    let week = week
        .into_iter()
        .map(|(date, checked)| WeekDay { date, checked })
        .collect();

    Ok(Json(StreakResponse {
        current_streak,
        longest_streak,
        week,
    }))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/goals/{id}/check-ins",
            post(create_goal_checkin).get(list_goal_checkins_handler),
        )
        .route("/api/check-ins", post(create_checkin).get(list_checkins))
        .route("/api/streak", get(get_streak))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- pure compute_streak tests ------------------------------------------

    fn d(s: &str) -> Date {
        parse_ymd(s).expect("valid test date")
    }

    #[test]
    fn empty_history_has_no_streak() {
        let today = d("2026-09-29");
        let (current, longest, week) = compute_streak(&[], today);
        assert_eq!(current, 0);
        assert_eq!(longest, 0);
        assert_eq!(week.len(), 7);
        assert!(week.iter().all(|(_, checked)| !*checked));
        // Oldest -> newest, last entry is today.
        assert_eq!(week.first().map(|(s, _)| s.as_str()), Some("2026-09-23"));
        assert_eq!(week.last().map(|(s, _)| s.as_str()), Some("2026-09-29"));
    }

    #[test]
    fn single_checkin_today() {
        let today = d("2026-09-29");
        let dates = vec!["2026-09-29".to_string()];
        let (current, longest, week) = compute_streak(&dates, today);
        assert_eq!(current, 1);
        assert_eq!(longest, 1);
        assert_eq!(week.last(), Some(&("2026-09-29".to_string(), true)));
    }

    #[test]
    fn streak_counts_from_yesterday_when_no_checkin_today() {
        let today = d("2026-09-29");
        // Checked in yesterday and the day before, but not today.
        let dates = vec!["2026-09-27".to_string(), "2026-09-28".to_string()];
        let (current, longest, _week) = compute_streak(&dates, today);
        assert_eq!(current, 2, "counts back from yesterday");
        assert_eq!(longest, 2);
    }

    #[test]
    fn consecutive_days_build_current_streak() {
        let today = d("2026-09-29");
        let dates = vec![
            "2026-09-27".to_string(),
            "2026-09-28".to_string(),
            "2026-09-29".to_string(),
        ];
        let (current, longest, _week) = compute_streak(&dates, today);
        assert_eq!(current, 3);
        assert_eq!(longest, 3);
    }

    #[test]
    fn gap_breaks_current_streak() {
        let today = d("2026-09-29");
        // Today + a broken older run (gap on the 28th).
        let dates = vec![
            "2026-09-25".to_string(),
            "2026-09-26".to_string(),
            "2026-09-29".to_string(),
        ];
        let (current, longest, _week) = compute_streak(&dates, today);
        assert_eq!(current, 1, "only today counts; the 28th is missing");
        assert_eq!(longest, 2, "the 25th-26th run is the longest");
    }

    #[test]
    fn longest_streak_over_history_independent_of_today() {
        let today = d("2026-09-29");
        // A 4-day run in the past, nothing recent.
        let dates = vec![
            "2026-01-01".to_string(),
            "2026-01-02".to_string(),
            "2026-01-03".to_string(),
            "2026-01-04".to_string(),
        ];
        let (current, longest, _week) = compute_streak(&dates, today);
        assert_eq!(current, 0);
        assert_eq!(longest, 4);
    }

    #[test]
    fn week_window_reflects_checked_days() {
        let today = d("2026-09-29");
        let dates = vec!["2026-09-24".to_string(), "2026-09-29".to_string()];
        let (_current, _longest, week) = compute_streak(&dates, today);
        assert_eq!(week.len(), 7);
        let checked: Vec<&str> = week
            .iter()
            .filter(|(_, c)| *c)
            .map(|(s, _)| s.as_str())
            .collect();
        assert_eq!(checked, vec!["2026-09-24", "2026-09-29"]);
    }

    #[test]
    fn month_boundary_streak() {
        let today = d("2026-10-01");
        let dates = vec!["2026-09-30".to_string(), "2026-10-01".to_string()];
        let (current, longest, _week) = compute_streak(&dates, today);
        assert_eq!(current, 2, "streak spans the month boundary");
        assert_eq!(longest, 2);
    }

    // ---- DB-backed tests ----------------------------------------------------

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

    async fn insert_goal(pool: &SqlitePool, id: &str, user_id: &str) {
        sqlx::query(
            "INSERT INTO goals (id, user_id, title, status, created_at, updated_at) \
             VALUES (?, ?, 'Goal', 'draft', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .bind(id)
        .bind(user_id)
        .execute(pool)
        .await
        .expect("insert goal");
    }

    #[tokio::test]
    async fn posting_a_goal_checkin_creates_a_row() {
        let (_dir, pool) = test_pool().await;
        insert_goal(&pool, "g1", "u1").await;

        let created = insert_checkin(&pool, "u1", Some("g1"), Some("did it"), Some("great"))
            .await
            .expect("insert checkin");
        assert_eq!(created.goal_id.as_deref(), Some("g1"));
        assert_eq!(created.note.as_deref(), Some("did it"));

        let (count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM check_ins WHERE user_id = 'u1' AND goal_id = 'g1'",
        )
        .fetch_one(&pool)
        .await
        .expect("count");
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn checkin_stored_and_suggestion_defaults_when_classification_errors() {
        let (_dir, pool) = test_pool().await;
        insert_goal(&pool, "g1", "u1").await;

        // Mirror the handler: store the check-in, then classify best-effort.
        let note = "Heads up — I'll be away over Christmas for two weeks";
        let created = insert_checkin(&pool, "u1", Some("g1"), Some(note), None)
            .await
            .expect("insert checkin");

        // The check-in row is persisted regardless of classification.
        let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM check_ins WHERE id = ?")
            .bind(&created.id)
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(count, 1);

        // Tests have no LLM key configured, so `chat_json` errors and the
        // classifier falls back to the neutral suggestion — never a failure.
        let config = Config::from_env();
        let llm = LlmClient::new(&config);
        let suggestion = classify_note(&llm, &pool, &config, "u1", Some(note)).await;
        assert!(
            !suggestion.adjust,
            "classification error falls back to false"
        );
        assert!(suggestion.message.is_none());
    }

    #[tokio::test]
    async fn short_or_empty_note_skips_classification() {
        let (_dir, pool) = test_pool().await;
        let config = Config::from_env();
        let llm = LlmClient::new(&config);

        // Empty / absent / trivially short notes never reach the LLM.
        for note in [None, Some(""), Some("  "), Some("ok")] {
            let suggestion = classify_note(&llm, &pool, &config, "u1", note).await;
            assert!(!suggestion.adjust);
            assert!(suggestion.message.is_none());
        }
    }

    #[tokio::test]
    async fn ownership_rejection_for_another_users_goal() {
        let (_dir, pool) = test_pool().await;
        insert_goal(&pool, "g1", "owner").await;

        // A different user cannot see or check in against the goal.
        let err = verify_goal_owner(&pool, "u1", "g1")
            .await
            .expect_err("should reject");
        assert!(matches!(err, AppError::NotFound));

        // A missing goal is likewise NotFound.
        let missing = verify_goal_owner(&pool, "u1", "does-not-exist")
            .await
            .expect_err("should reject");
        assert!(matches!(missing, AppError::NotFound));
    }

    #[tokio::test]
    async fn streak_reflects_a_checkin_for_today() {
        let (_dir, pool) = test_pool().await;

        // A general (goal-less) check-in stamped now (today, UTC).
        insert_checkin(&pool, "u1", None, None, None)
            .await
            .expect("insert checkin");

        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT DISTINCT substr(created_at, 1, 10) FROM check_ins \
             WHERE user_id = 'u1' ORDER BY 1",
        )
        .fetch_all(&pool)
        .await
        .expect("dates");
        let dates: Vec<String> = rows.into_iter().map(|(d,)| d).collect();

        let today = OffsetDateTime::now_utc().date();
        let (current, longest, week) = compute_streak(&dates, today);
        assert!(
            current >= 1,
            "today's check-in yields a current streak >= 1"
        );
        assert!(longest >= 1);
        assert_eq!(
            week.last().map(|(_, c)| *c),
            Some(true),
            "today (last in window) is checked"
        );
    }
}
