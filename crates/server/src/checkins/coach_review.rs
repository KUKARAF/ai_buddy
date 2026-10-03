//! Best-effort "coach review" LLM pass, run once per goal check-in.
//!
//! After a check-in's note has been classified and its milestone todos processed,
//! this pass takes a wider view: it gathers lightweight progress context for the
//! goal (finish line, deadline + days remaining, current milestone, milestone and
//! todo completion counts, the few most recent prior notes, and this note) and
//! asks the model for a single warm, encouraging [`CoachReview`] — a status, a
//! headline, a short message, and optional questions/suggestions/risks.
//!
//! The review is ALWAYS returned inline on the check-in response (when produced).
//! Separately — and gated to avoid spam — it may also be posted as an in-app
//! notification via [`crate::notify::Notifier::post_now`] under `kind = "coach"`.
//!
//! This NEVER fails or blocks the check-in: a missing/short note, an unresolved
//! model, a missing key, an empty wallet, a network/parse error, or any DB error
//! degrades to `None` and posts nothing. Pure helpers ([`normalize_status`],
//! [`should_post`]) are kept DB-free for unit testing.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use time::format_description::well_known::Rfc3339;
use time::{Date, Duration, Month, OffsetDateTime};

use ai_buddy_core::domain::StepStatus;

use crate::config::Config;
use crate::error::{AppError, AppResult};
use crate::llm::LlmClient;
use crate::notify::Notifier;

/// Notes shorter than this (after trimming) are routine and skip the LLM entirely
/// — matching the check-in classifier's own trivial-note gate.
const MIN_NOTE_LEN: usize = 6;

/// How many prior check-in notes to feed the model for continuity.
const RECENT_NOTES: usize = 3;

/// Hard cap on each of the optional `questions`/`suggestions`/`risks` lists.
const MAX_LIST: usize = 3;

/// Minimum age of a same-status prior coach notification before we re-notify.
const CADENCE_HOURS: i64 = 24;

/// The `kind` recorded on coach notifications (also the cadence-query filter).
const COACH_KIND: &str = "coach";

/// System prompt for the coach-review pass. Warm, encouraging accountability coach
/// who assesses progress against the finish line and returns strict JSON.
const COACH_SYSTEM: &str = "You are a warm, encouraging accountability coach reviewing a user's progress on \
a personal goal after their latest check-in. Assess how they are tracking against their finish line given \
the time remaining, then choose a status: \"on_track\" (steady, roughly where they should be), \"ahead\" \
(moving faster than needed), \"at_risk\" (slipping; the deadline is in some doubt) or \"off_track\" \
(clearly behind). Write a short, warm \"message\" of 1-3 sentences in an upbeat, personal coaching tone \
that names the finish line and encourages them (e.g. \"You're on track to reach Olsztyn — great work, keep \
going!\"). Add a brief \"headline\" of a few words. You MAY include up to 2 gentle \"questions\" to prompt \
reflection and up to 2 concrete \"suggestions\". Only populate \"risks\" when the user is genuinely at risk \
of missing the goal; otherwise leave it empty. ACKNOWLEDGE any todos the check-in already ticked off or \
added — celebrate them, do NOT re-propose them. Be honest but kind; never nag. Return ONLY JSON \
{\"status\":\"...\",\"headline\":\"...\",\"message\":\"...\",\"questions\":[],\"suggestions\":[],\"risks\":[]}.";

/// The coach's structured verdict. **Frozen schema — shared with the frontend; do
/// not rename fields.** `status` is normalized to one of `on_track`/`ahead`/
/// `at_risk`/`off_track`; the three lists default to empty so a partial object
/// still deserializes.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CoachReview {
    pub status: String,
    pub headline: String,
    pub message: String,
    #[serde(default)]
    pub questions: Vec<String>,
    #[serde(default)]
    pub suggestions: Vec<String>,
    #[serde(default)]
    pub risks: Vec<String>,
}

/// Goal row loaded in the single ownership-checked context query.
#[derive(Debug, sqlx::FromRow)]
struct GoalCtx {
    title: String,
    success_criterion: Option<String>,
    deadline: Option<String>,
    category: Option<String>,
}

/// Normalize a raw model status to the frozen vocabulary, case-insensitively.
/// Anything unknown, empty or absent falls back to `on_track`.
fn normalize_status(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "on_track" => "on_track".to_string(),
        "ahead" => "ahead".to_string(),
        "at_risk" => "at_risk".to_string(),
        "off_track" => "off_track".to_string(),
        _ => "on_track".to_string(),
    }
}

/// Trim, drop empties, and cap a list of strings to [`MAX_LIST`].
fn clean_list(items: Vec<String>) -> Vec<String> {
    items
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .take(MAX_LIST)
        .collect()
}

/// Parse a `YYYY-MM-DD` prefix into a [`Date`], or `None` if malformed. Tolerates
/// a trailing time component (only the first 10 chars are read).
fn parse_ymd(s: &str) -> Option<Date> {
    let year: i32 = s.get(0..4)?.parse().ok()?;
    let month: u8 = s.get(5..7)?.parse().ok()?;
    let day: u8 = s.get(8..10)?.parse().ok()?;
    let month = Month::try_from(month).ok()?;
    Date::from_calendar_date(year, month, day).ok()
}

/// Whole days from `today` until `deadline` (negative if the deadline has passed),
/// or `None` when the deadline is absent/unparseable.
fn days_remaining(deadline: Option<&str>, today: Date) -> Option<i64> {
    let d = parse_ymd(deadline?.trim())?;
    Some((d - today).whole_days())
}

/// Pure cadence decision: post a coach notification ONLY IF there is no prior one,
/// the new status differs from the prior one, or the prior one is older than
/// [`CADENCE_HOURS`]. When a prior same-status notification exists but its
/// timestamp cannot be parsed we err toward delivering (we cannot confirm it is
/// recent), so suppression requires a positively-confirmed recent same-status post.
fn should_post(
    prev_status: Option<&str>,
    prev_created_at: Option<&str>,
    new_status: &str,
    now: OffsetDateTime,
) -> bool {
    let prev_status = match prev_status {
        None => return true, // (a) no prior coach notification
        Some(s) => s,
    };
    if prev_status != new_status {
        return true; // (b) status changed
    }
    // (c) same status: post only if the prior one is older than the cadence window.
    match prev_created_at.and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok()) {
        Some(prev) => now - prev > Duration::hours(CADENCE_HOURS),
        None => true,
    }
}

/// Build the user prompt from the assembled context.
#[allow(clippy::too_many_arguments)]
fn build_prompt(
    goal: &GoalCtx,
    days_left: Option<i64>,
    current_milestone: Option<&(String, Option<String>)>,
    milestones_done: i64,
    milestones_total: i64,
    todos_done: i64,
    todos_total: i64,
    recent_notes: &[String],
    note: &str,
    completed_titles: &[String],
    added_titles: &[String],
) -> String {
    let mut out = format!("Goal: {}\n", goal.title);
    if let Some(c) = goal.category.as_deref() {
        if !c.trim().is_empty() {
            out.push_str(&format!("Category: {c}\n"));
        }
    }
    if let Some(s) = goal.success_criterion.as_deref() {
        if !s.trim().is_empty() {
            out.push_str(&format!("Finish line (definition of success): {s}\n"));
        }
    }
    if let Some(d) = goal.deadline.as_deref() {
        if !d.trim().is_empty() {
            out.push_str(&format!("Deadline: {d}\n"));
        }
    }
    if let Some(days) = days_left {
        if days >= 0 {
            out.push_str(&format!("Days remaining until the deadline: {days}\n"));
        } else {
            out.push_str(&format!("The deadline passed {} day(s) ago.\n", -days));
        }
    }

    if let Some((title, due)) = current_milestone {
        out.push_str(&format!("\nCurrent milestone: {title}\n"));
        if let Some(due) = due.as_deref() {
            if !due.trim().is_empty() {
                out.push_str(&format!("Milestone due date: {due}\n"));
            }
        }
    }
    out.push_str(&format!(
        "Milestones completed: {milestones_done} of {milestones_total}\n"
    ));
    out.push_str(&format!(
        "Current milestone todos completed: {todos_done} of {todos_total}\n"
    ));

    if !recent_notes.is_empty() {
        out.push_str("\nRecent prior check-in notes (most recent first):\n");
        for n in recent_notes {
            out.push_str(&format!("- {n}\n"));
        }
    }

    if !completed_titles.is_empty() {
        out.push_str("\nTodos this check-in just ticked off (acknowledge, don't re-propose):\n");
        for t in completed_titles {
            out.push_str(&format!("- {t}\n"));
        }
    }
    if !added_titles.is_empty() {
        out.push_str("\nTodos this check-in just added (acknowledge, don't re-propose):\n");
        for t in added_titles {
            out.push_str(&format!("- {t}\n"));
        }
    }

    out.push_str(&format!("\nThis check-in note: {note}\n"));
    out.push_str("\nAssess their progress and respond now as the coach.");
    out
}

/// Fetch the most recent prior coach notification's `(payload, created_at)` for
/// this goal, if any.
async fn last_coach_notification(
    pool: &SqlitePool,
    user_id: &str,
    goal_id: &str,
) -> AppResult<Option<(String, String)>> {
    sqlx::query_as::<_, (String, String)>(
        "SELECT payload, created_at FROM reminders \
         WHERE user_id = ? AND goal_id = ? AND kind = ? \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(user_id)
    .bind(goal_id)
    .bind(COACH_KIND)
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))
}

/// Pull the `status` string out of a stored coach-notification payload, if present.
fn status_from_payload(payload: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(payload).ok()?;
    value.get("status")?.as_str().map(|s| s.to_string())
}

/// Run the coach-review pass for one goal check-in. See the module docs. NEVER
/// fails: any error or skip condition yields `None` and posts nothing. When a
/// review is produced it is ALWAYS returned inline, whether or not a notification
/// was posted.
#[allow(clippy::too_many_arguments)]
pub async fn coach_review(
    llm: &LlmClient,
    pool: &SqlitePool,
    config: &Config,
    notifier: &Notifier,
    user_id: &str,
    goal_id: &str,
    note: Option<&str>,
    todos_completed: &[crate::todos::TodoView],
    todos_added: &[crate::todos::TodoView],
) -> Option<CoachReview> {
    match try_coach_review(
        llm,
        pool,
        config,
        notifier,
        user_id,
        goal_id,
        note,
        todos_completed,
        todos_added,
    )
    .await
    {
        Ok(review) => review,
        Err(e) => {
            tracing::warn!(error = ?e, goal_id, "coach review failed (ignored)");
            None
        }
    }
}

/// Fallible inner body of [`coach_review`]; all errors are swallowed by the public
/// wrapper. Posting failures are swallowed here so the inline review still returns.
#[allow(clippy::too_many_arguments)]
async fn try_coach_review(
    llm: &LlmClient,
    pool: &SqlitePool,
    config: &Config,
    notifier: &Notifier,
    user_id: &str,
    goal_id: &str,
    note: Option<&str>,
    todos_completed: &[crate::todos::TodoView],
    todos_added: &[crate::todos::TodoView],
) -> AppResult<Option<CoachReview>> {
    // Trivial / absent notes are routine — nothing to review.
    let note = match note.map(str::trim) {
        Some(n) if n.len() >= MIN_NOTE_LEN => n,
        _ => return Ok(None),
    };

    // Ownership-checked goal context (missing / not owned => skip, no error).
    let goal: Option<GoalCtx> = sqlx::query_as::<_, GoalCtx>(
        "SELECT title, success_criterion, deadline, category \
         FROM goals WHERE id = ? AND user_id = ?",
    )
    .bind(goal_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    let goal = match goal {
        Some(g) => g,
        None => return Ok(None),
    };

    // Optional balance gate: skip the pass on an empty wallet. A balance lookup
    // error is non-fatal — the debit is clamped anyway — so we proceed on error.
    if let Ok(balance) = crate::wallet::balance_cents(pool, user_id).await {
        if balance <= 0 {
            return Ok(None);
        }
    }

    let done_status = StepStatus::Done.as_str();

    // Current milestone: first roadmap step not yet done, by order. (id, title, due)
    let current: Option<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT s.id, s.title, s.due_date FROM roadmap_steps s \
             JOIN roadmaps r ON s.roadmap_id = r.id \
         WHERE r.goal_id = ? AND s.status != ? \
         ORDER BY s.ord ASC, s.created_at ASC LIMIT 1",
    )
    .bind(goal_id)
    .bind(done_status)
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    // Milestone counts (total, done) across the goal's roadmap.
    let (milestones_total, milestones_done): (i64, Option<i64>) = sqlx::query_as(
        "SELECT COUNT(*), SUM(CASE WHEN s.status = ? THEN 1 ELSE 0 END) \
         FROM roadmap_steps s JOIN roadmaps r ON s.roadmap_id = r.id \
         WHERE r.goal_id = ?",
    )
    .bind(done_status)
    .bind(goal_id)
    .fetch_one(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    let milestones_done = milestones_done.unwrap_or(0);

    // Todo counts for the current milestone step (total, done).
    let (todos_total, todos_done): (i64, Option<i64>) = match current.as_ref() {
        Some((step_id, _, _)) => sqlx::query_as(
            "SELECT COUNT(*), SUM(CASE WHEN done = 1 THEN 1 ELSE 0 END) \
             FROM step_todos WHERE step_id = ?",
        )
        .bind(step_id)
        .fetch_one(pool)
        .await
        .map_err(|e| AppError::Internal(e.into()))?,
        None => (0, Some(0)),
    };
    let todos_done = todos_done.unwrap_or(0);

    // The few most recent prior notes for continuity, excluding this note. We over-
    // fetch by one and drop the current note, then keep the most recent few.
    let recent_rows: Vec<(String,)> = sqlx::query_as(
        "SELECT note FROM check_ins \
         WHERE goal_id = ? AND note IS NOT NULL AND trim(note) != '' \
         ORDER BY created_at DESC, id DESC LIMIT ?",
    )
    .bind(goal_id)
    .bind((RECENT_NOTES + 1) as i64)
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    let mut skipped_current = false;
    let recent_notes: Vec<String> = recent_rows
        .into_iter()
        .map(|(n,)| n)
        .filter(|n| {
            if !skipped_current && n.trim() == note {
                skipped_current = true;
                false
            } else {
                true
            }
        })
        .take(RECENT_NOTES)
        .collect();

    let completed_titles: Vec<String> = todos_completed.iter().map(|t| t.title.clone()).collect();
    let added_titles: Vec<String> = todos_added.iter().map(|t| t.title.clone()).collect();

    let today = OffsetDateTime::now_utc().date();
    let days_left = days_remaining(goal.deadline.as_deref(), today);
    let current_milestone = current
        .as_ref()
        .map(|(_, title, due)| (title.clone(), due.clone()));

    let model = crate::settings::user_chat_model(pool, config, user_id).await?;
    let prompt = build_prompt(
        &goal,
        days_left,
        current_milestone.as_ref(),
        milestones_done,
        milestones_total,
        todos_done,
        todos_total,
        &recent_notes,
        note,
        &completed_titles,
        &added_titles,
    );

    let (raw, cost_cents) = llm
        .chat_json::<CoachReview>(&model, COACH_SYSTEM, &prompt)
        .await?;

    // Charge the wallet best-effort; a debit failure must not lose the review.
    if cost_cents > 0 {
        if let Err(e) =
            crate::wallet::debit_tokens(pool, user_id, cost_cents, "checkin-coach").await
        {
            tracing::warn!(error = ?e, goal_id, "coach review debit failed (ignored)");
        }
    }

    // Normalize + clean the model output.
    let review = CoachReview {
        status: normalize_status(&raw.status),
        headline: raw.headline.trim().to_string(),
        message: raw.message.trim().to_string(),
        questions: clean_list(raw.questions),
        suggestions: clean_list(raw.suggestions),
        risks: clean_list(raw.risks),
    };
    // An empty message is useless — drop the whole review.
    if review.message.is_empty() {
        return Ok(None);
    }

    // Cadence gate + post — entirely best-effort; never affects the returned review.
    let prev = last_coach_notification(pool, user_id, goal_id)
        .await
        .ok()
        .flatten();
    let prev_status = prev
        .as_ref()
        .and_then(|(payload, _)| status_from_payload(payload));
    let prev_created = prev.as_ref().map(|(_, created)| created.as_str());
    if should_post(
        prev_status.as_deref(),
        prev_created,
        &review.status,
        OffsetDateTime::now_utc(),
    ) {
        let mut payload = serde_json::to_value(&review).unwrap_or_else(|_| serde_json::json!({}));
        if let Some(obj) = payload.as_object_mut() {
            obj.insert(
                "goal_title".to_string(),
                serde_json::Value::String(goal.title.clone()),
            );
        }
        if let Err(e) = notifier
            .post_now(user_id, Some(goal_id), None, COACH_KIND, payload)
            .await
        {
            tracing::warn!(error = ?e, goal_id, "coach notification post failed (ignored)");
        }
    }

    Ok(Some(review))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_status_passes_known_and_defaults_unknown() {
        assert_eq!(normalize_status("on_track"), "on_track");
        assert_eq!(normalize_status("ahead"), "ahead");
        assert_eq!(normalize_status("at_risk"), "at_risk");
        assert_eq!(normalize_status("off_track"), "off_track");
        // Case-insensitive + trimmed.
        assert_eq!(normalize_status("  AT_RISK "), "at_risk");
        // Unknown / empty / garbage all fall back to on_track.
        assert_eq!(normalize_status("sideways"), "on_track");
        assert_eq!(normalize_status(""), "on_track");
        assert_eq!(normalize_status("behind"), "on_track");
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::parse("2026-10-03T12:00:00Z", &Rfc3339).expect("parse now")
    }

    #[test]
    fn should_post_when_no_prior() {
        assert!(should_post(None, None, "on_track", now()));
    }

    #[test]
    fn should_not_post_same_status_within_window() {
        // Prior post 2 hours ago, same status => suppressed.
        let recent = "2026-10-03T10:00:00Z";
        assert!(!should_post(
            Some("on_track"),
            Some(recent),
            "on_track",
            now()
        ));
    }

    #[test]
    fn should_post_on_status_change_even_if_recent() {
        let recent = "2026-10-03T10:00:00Z";
        assert!(should_post(
            Some("on_track"),
            Some(recent),
            "at_risk",
            now()
        ));
    }

    #[test]
    fn should_post_when_prior_is_old() {
        // Same status but more than 24h ago => re-notify.
        let old = "2026-10-01T10:00:00Z";
        assert!(should_post(Some("on_track"), Some(old), "on_track", now()));
    }

    #[test]
    fn should_post_same_status_when_timestamp_unparseable() {
        // Cannot confirm recency => err toward delivering.
        assert!(should_post(
            Some("on_track"),
            Some("not-a-date"),
            "on_track",
            now()
        ));
    }

    #[test]
    fn clean_list_trims_drops_empty_and_caps() {
        let out = clean_list(vec![
            "  keep one ".to_string(),
            "".to_string(),
            "   ".to_string(),
            "keep two".to_string(),
            "keep three".to_string(),
            "dropped by cap".to_string(),
        ]);
        assert_eq!(out, vec!["keep one", "keep two", "keep three"]);
    }

    #[test]
    fn parsed_review_is_normalized_capped_and_message_checked() {
        // Simulate parsing an over-long, unknown-status model object.
        let json = r#"{
            "status": "WOBBLY",
            "headline": "  Nice progress  ",
            "message": "  You're on track to reach Olsztyn — keep going!  ",
            "questions": ["q1", " q2 ", "q3", "q4 over cap"],
            "suggestions": [],
            "risks": ["  ", "real risk"]
        }"#;
        let raw: CoachReview = serde_json::from_str(json).expect("parse review");
        let review = CoachReview {
            status: normalize_status(&raw.status),
            headline: raw.headline.trim().to_string(),
            message: raw.message.trim().to_string(),
            questions: clean_list(raw.questions),
            suggestions: clean_list(raw.suggestions),
            risks: clean_list(raw.risks),
        };
        assert_eq!(review.status, "on_track", "unknown status normalized");
        assert_eq!(review.headline, "Nice progress");
        assert_eq!(
            review.message,
            "You're on track to reach Olsztyn — keep going!"
        );
        assert_eq!(
            review.questions,
            vec!["q1", "q2", "q3"],
            "capped to 3 + trimmed"
        );
        assert!(review.suggestions.is_empty());
        assert_eq!(review.risks, vec!["real risk"], "empty entry dropped");
        assert!(!review.message.is_empty());
    }

    #[test]
    fn empty_message_signals_drop() {
        let json = r#"{"status":"ahead","headline":"Hi","message":"   "}"#;
        let raw: CoachReview = serde_json::from_str(json).expect("parse review");
        let message = raw.message.trim().to_string();
        assert!(
            message.is_empty(),
            "blank message should trigger a None drop"
        );
    }

    #[test]
    fn status_from_payload_reads_status_field() {
        let payload = r#"{"status":"at_risk","message":"hi","goal_title":"Ride"}"#;
        assert_eq!(status_from_payload(payload).as_deref(), Some("at_risk"));
        // Missing field / bad JSON => None.
        assert_eq!(status_from_payload(r#"{"message":"hi"}"#), None);
        assert_eq!(status_from_payload("not json"), None);
    }

    #[test]
    fn days_remaining_computes_and_tolerates_bad_input() {
        let today = parse_ymd("2026-10-03").expect("today");
        assert_eq!(days_remaining(Some("2026-10-13"), today), Some(10));
        assert_eq!(days_remaining(Some("2026-10-01"), today), Some(-2));
        // Tolerates a trailing time component.
        assert_eq!(
            days_remaining(Some("2026-10-13T00:00:00Z"), today),
            Some(10)
        );
        // Absent / malformed => None.
        assert_eq!(days_remaining(None, today), None);
        assert_eq!(days_remaining(Some("whenever"), today), None);
    }
}
