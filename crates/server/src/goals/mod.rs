//! Goals + roadmap: CRUD and LLM-driven roadmap generation.
//!
//! Routes (all under `RequireAuth`):
//!   - `GET  /api/goals`               list the caller's goals
//!   - `POST /api/goals`               create a goal (status `draft`)
//!   - `GET  /api/goals/{id}`          fetch one goal (ownership enforced)
//!   - `PATCH /api/goals/{id}`         update mutable fields
//!   - `POST /api/goals/{id}/roadmap`  generate a roadmap via the LLM
//!   - `GET  /api/goals/{id}/roadmap`  fetch the roadmap + ordered steps + breaks
//!   - `POST /api/goals/{id}/adjust`   agentic plan-adjustment (breaks + reschedule)
//!   - `PATCH /api/steps/{id}`         update a step's status
//!
//! Owns migrations `0003_goals.sql` and `0012_goal_breaks.sql`.

use axum::extract::{Path, State};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use axum_extra::extract::WithRejection;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use ai_buddy_core::domain::{GoalStatus, PledgeStatus, ReminderKind, StepStatus};

use crate::auth::session::RequireAuth;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

// --- DTOs ---------------------------------------------------------------------

/// A goal as stored and returned to the client.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct GoalView {
    pub id: String,
    pub user_id: String,
    pub title: String,
    pub description: Option<String>,
    pub category: Option<String>,
    pub deadline: Option<String>,
    pub status: String,
    pub progress_note: Option<String>,
    pub location: Option<String>,
    pub skill_level: Option<String>,
    pub success_criterion: Option<String>,
    pub time_per_session_min: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

/// A single roadmap step as stored and returned to the client.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct StepView {
    pub id: String,
    pub roadmap_id: String,
    pub ord: i64,
    pub title: String,
    pub detail: Option<String>,
    pub due_date: Option<String>,
    pub effort: Option<String>,
    pub status: String,
    pub created_at: String,
}

/// A planned unavailable period on a goal's timeline (e.g. Christmas, a holiday).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct GoalBreak {
    pub id: String,
    pub label: String,
    pub start_date: String,
    pub end_date: String,
}

/// A roadmap plus its ordered steps and any planned breaks. When a goal has no
/// roadmap row yet, [`get_roadmap`] still returns this shape with empty `steps`
/// (and `id`/`created_at` empty, `model` null) so breaks are always visible.
#[derive(Debug, Clone, Serialize)]
pub struct RoadmapView {
    pub id: String,
    pub goal_id: String,
    pub model: Option<String>,
    pub created_at: String,
    pub steps: Vec<StepView>,
    pub breaks: Vec<GoalBreak>,
}

#[derive(Debug, Deserialize)]
struct CreateGoal {
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    deadline: Option<String>,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    skill_level: Option<String>,
    #[serde(default)]
    success_criterion: Option<String>,
    #[serde(default)]
    time_per_session_min: Option<i64>,
}

#[derive(Debug, Default, Deserialize)]
struct PatchGoal {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    deadline: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    progress_note: Option<String>,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    skill_level: Option<String>,
    #[serde(default)]
    success_criterion: Option<String>,
    #[serde(default)]
    time_per_session_min: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct PatchStep {
    status: String,
}

/// The structured roadmap the LLM returns via JSON-mode completion.
#[derive(Debug, Deserialize)]
struct RoadmapDraft {
    #[serde(default)]
    steps: Vec<StepDraft>,
}

#[derive(Debug, Deserialize)]
struct StepDraft {
    title: String,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    due_date: Option<String>,
    #[serde(default)]
    effort: Option<String>,
}

/// A complete goal + milestone plan, as produced by the agentic coach's
/// `save_goal` tool call. Deserialized straight from the model's tool arguments
/// (then sanitized by the caller before it reaches [`create_goal_with_milestones`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewGoalWithPlan {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub deadline: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub skill_level: Option<String>,
    #[serde(default)]
    pub time_per_session_min: Option<i64>,
    #[serde(default)]
    pub success_criterion: Option<String>,
    #[serde(default)]
    pub milestones: Vec<NewMilestone>,
    #[serde(default)]
    pub pledge_cents: Option<i64>,
}

/// One milestone within a [`NewGoalWithPlan`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewMilestone {
    pub title: String,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub due_date: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
}

// --- Router -------------------------------------------------------------------

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/goals", get(list_goals).post(create_goal))
        .route("/api/goals/{id}", get(get_goal).patch(patch_goal))
        .route(
            "/api/goals/{id}/roadmap",
            post(generate_roadmap).get(get_roadmap),
        )
        .route("/api/goals/{id}/adjust", post(adjust_plan))
        .route("/api/steps/{id}", patch(patch_step))
}

// --- Helpers ------------------------------------------------------------------

fn now_rfc3339() -> AppResult<String> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|e| AppError::Internal(e.into()))
}

/// Is `s` a well-formed `YYYY-MM-DD` calendar date (with a plausible month/day)?
/// Used to reject dates the model hallucinated in a bad format before persisting.
fn valid_ymd(s: &str) -> bool {
    let s = s.trim();
    let parts: Vec<&str> = s.split('-').collect();
    let [y, m, d] = parts.as_slice() else {
        return false;
    };
    if y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return false;
    }
    if !s.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
        return false;
    }
    let (Ok(month), Ok(day)) = (m.parse::<u8>(), d.parse::<u8>()) else {
        return false;
    };
    (1..=12).contains(&month) && (1..=31).contains(&day)
}

/// Coerce a possibly date-only string (`YYYY-MM-DD`) into an Rfc3339 timestamp by
/// defaulting the time to 09:00 UTC. Already-timestamped values pass through.
fn to_rfc3339_due(raw: &str) -> String {
    let s = raw.trim();
    let bytes = s.as_bytes();
    let is_date_only = s.len() == 10
        && bytes.get(4) == Some(&b'-')
        && bytes.get(7) == Some(&b'-')
        && bytes.iter().all(|b| b.is_ascii_digit() || *b == b'-');
    if is_date_only {
        format!("{s}T09:00:00Z")
    } else {
        s.to_string()
    }
}

/// Best-effort embed + upsert into the vector store. RAG failures must never fail
/// the caller's request, so all errors are logged and swallowed.
async fn embed_upsert(state: &AppState, user_id: &str, kind: &str, source_id: &str, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    match state.llm.embed(text).await {
        Ok((embedding, _cost)) => {
            if let Err(e) = crate::vector::upsert(
                &state.db,
                user_id,
                kind,
                source_id,
                &state.config.embedding_model,
                text,
                &embedding,
            )
            .await
            {
                tracing::warn!(error = ?e, kind, source_id, "vector upsert failed (ignored)");
            }
        }
        Err(e) => {
            tracing::warn!(error = ?e, kind, source_id, "embedding failed (ignored)");
        }
    }
}

async fn fetch_goal(pool: &SqlitePool, id: &str) -> AppResult<Option<GoalView>> {
    sqlx::query_as::<_, GoalView>(
        "SELECT id, user_id, title, description, category, deadline, status, progress_note, \
                location, skill_level, success_criterion, time_per_session_min, \
                created_at, updated_at \
         FROM goals WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))
}

/// Fetch a goal, returning `NotFound` when missing and `Forbidden` when owned by
/// another user.
async fn owned_goal(pool: &SqlitePool, user_id: &str, id: &str) -> AppResult<GoalView> {
    let goal = fetch_goal(pool, id).await?.ok_or(AppError::NotFound)?;
    if goal.user_id != user_id {
        return Err(AppError::Forbidden);
    }
    Ok(goal)
}

#[allow(clippy::too_many_arguments)]
async fn insert_goal(
    pool: &SqlitePool,
    user_id: &str,
    title: &str,
    description: Option<&str>,
    category: Option<&str>,
    deadline: Option<&str>,
    location: Option<&str>,
    skill_level: Option<&str>,
    success_criterion: Option<&str>,
    time_per_session_min: Option<i64>,
) -> AppResult<GoalView> {
    let title = title.trim();
    if title.is_empty() {
        return Err(AppError::BadRequest("title is required".to_string()));
    }
    let id = Uuid::new_v4().to_string();
    let now = now_rfc3339()?;
    sqlx::query(
        "INSERT INTO goals \
             (id, user_id, title, description, category, deadline, status, progress_note, \
              location, skill_level, success_criterion, time_per_session_min, \
              created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(title)
    .bind(description)
    .bind(category)
    .bind(deadline)
    .bind(GoalStatus::Draft.as_str())
    .bind(location)
    .bind(skill_level)
    .bind(success_criterion)
    .bind(time_per_session_min)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    fetch_goal(pool, &id).await?.ok_or(AppError::NotFound)
}

async fn list_goals_for(pool: &SqlitePool, user_id: &str) -> AppResult<Vec<GoalView>> {
    sqlx::query_as::<_, GoalView>(
        "SELECT id, user_id, title, description, category, deadline, status, progress_note, \
                location, skill_level, success_criterion, time_per_session_min, \
                created_at, updated_at \
         FROM goals WHERE user_id = ? ORDER BY created_at DESC, id DESC",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))
}

async fn apply_goal_patch(
    pool: &SqlitePool,
    user_id: &str,
    id: &str,
    patch: PatchGoal,
) -> AppResult<GoalView> {
    let mut goal = owned_goal(pool, user_id, id).await?;
    let prev_status = goal.status.clone();

    if let Some(title) = patch.title {
        let title = title.trim().to_string();
        if title.is_empty() {
            return Err(AppError::BadRequest("title cannot be empty".to_string()));
        }
        goal.title = title;
    }
    if let Some(description) = patch.description {
        goal.description = Some(description);
    }
    if let Some(deadline) = patch.deadline {
        goal.deadline = Some(deadline);
    }
    if let Some(status) = patch.status {
        if GoalStatus::parse(&status).is_none() {
            return Err(AppError::BadRequest(format!(
                "invalid goal status: {status}"
            )));
        }
        goal.status = status;
    }
    if let Some(note) = patch.progress_note {
        goal.progress_note = Some(note);
    }
    if let Some(location) = patch.location {
        goal.location = Some(location);
    }
    if let Some(skill_level) = patch.skill_level {
        goal.skill_level = Some(skill_level);
    }
    if let Some(success_criterion) = patch.success_criterion {
        goal.success_criterion = Some(success_criterion);
    }
    if let Some(time_per_session_min) = patch.time_per_session_min {
        goal.time_per_session_min = Some(time_per_session_min);
    }
    goal.updated_at = now_rfc3339()?;

    sqlx::query(
        "UPDATE goals SET title = ?, description = ?, category = ?, deadline = ?, status = ?, \
                progress_note = ?, location = ?, skill_level = ?, success_criterion = ?, \
                time_per_session_min = ?, updated_at = ? \
         WHERE id = ?",
    )
    .bind(&goal.title)
    .bind(&goal.description)
    .bind(&goal.category)
    .bind(&goal.deadline)
    .bind(&goal.status)
    .bind(&goal.progress_note)
    .bind(&goal.location)
    .bind(&goal.skill_level)
    .bind(&goal.success_criterion)
    .bind(goal.time_per_session_min)
    .bind(&goal.updated_at)
    .bind(&goal.id)
    .execute(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    // Resolve any pledge when the goal reaches a terminal state (best-effort:
    // a goal may carry no pledge, in which case these are no-ops). Success
    // refunds the held stake; failure forfeits it.
    if goal.status != prev_status {
        let outcome = match GoalStatus::parse(&goal.status) {
            Some(GoalStatus::Succeeded) => Some(crate::wallet::pledge_refund(pool, &goal.id).await),
            Some(GoalStatus::Failed) => Some(crate::wallet::pledge_forfeit(pool, &goal.id).await),
            _ => None,
        };
        if let Some(Err(e)) = outcome {
            tracing::warn!(error = ?e, goal_id = %goal.id, "pledge resolution failed (ignored)");
        }
    }

    Ok(goal)
}

/// Load a goal's planned breaks, ordered by start date. Breaks live independently
/// of the roadmap, so this is callable whether or not a roadmap row exists.
async fn fetch_breaks(pool: &SqlitePool, goal_id: &str) -> AppResult<Vec<GoalBreak>> {
    sqlx::query_as::<_, GoalBreak>(
        "SELECT id, label, start_date, end_date FROM goal_breaks \
         WHERE goal_id = ? ORDER BY start_date ASC, id ASC",
    )
    .bind(goal_id)
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))
}

async fn fetch_roadmap(pool: &SqlitePool, goal_id: &str) -> AppResult<Option<RoadmapView>> {
    let row: Option<(String, Option<String>, String)> =
        sqlx::query_as("SELECT id, model, created_at FROM roadmaps WHERE goal_id = ?")
            .bind(goal_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;

    let breaks = fetch_breaks(pool, goal_id).await?;

    let Some((id, model, created_at)) = row else {
        return Ok(None);
    };

    let steps = sqlx::query_as::<_, StepView>(
        "SELECT id, roadmap_id, ord, title, detail, due_date, effort, status, created_at \
         FROM roadmap_steps WHERE roadmap_id = ? ORDER BY ord ASC, created_at ASC",
    )
    .bind(&id)
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    Ok(Some(RoadmapView {
        id,
        goal_id: goal_id.to_string(),
        model,
        created_at,
        steps,
        breaks,
    }))
}

/// Fetch the roadmap for `goal_id`, or — when no roadmap row exists yet — an empty
/// roadmap view that still carries the goal's breaks. This lets
/// `GET /api/goals/{id}/roadmap` always return `breaks`, even before planning.
async fn fetch_roadmap_or_empty(pool: &SqlitePool, goal_id: &str) -> AppResult<RoadmapView> {
    if let Some(roadmap) = fetch_roadmap(pool, goal_id).await? {
        return Ok(roadmap);
    }
    let breaks = fetch_breaks(pool, goal_id).await?;
    Ok(RoadmapView {
        id: String::new(),
        goal_id: goal_id.to_string(),
        model: None,
        created_at: String::new(),
        steps: Vec::new(),
        breaks,
    })
}

async fn set_step_status(
    pool: &SqlitePool,
    user_id: &str,
    step_id: &str,
    status: &str,
) -> AppResult<StepView> {
    if StepStatus::parse(status).is_none() {
        return Err(AppError::BadRequest(format!(
            "invalid step status: {status}"
        )));
    }

    let owner: Option<(String,)> = sqlx::query_as(
        "SELECT g.user_id FROM roadmap_steps s \
             JOIN roadmaps r ON s.roadmap_id = r.id \
             JOIN goals g ON r.goal_id = g.id \
         WHERE s.id = ?",
    )
    .bind(step_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    match owner {
        None => return Err(AppError::NotFound),
        Some((owner_id,)) if owner_id != user_id => return Err(AppError::Forbidden),
        Some(_) => {}
    }

    sqlx::query("UPDATE roadmap_steps SET status = ? WHERE id = ?")
        .bind(status)
        .bind(step_id)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    sqlx::query_as::<_, StepView>(
        "SELECT id, roadmap_id, ord, title, detail, due_date, effort, status, created_at \
         FROM roadmap_steps WHERE id = ?",
    )
    .bind(step_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?
    .ok_or(AppError::NotFound)
}

// --- Roadmap generation -------------------------------------------------------

const ROADMAP_SYSTEM_PROMPT: &str = "You are an accountability coach and expert planner. \
Given a user's goal, design a concrete, realistic, motivating step-by-step roadmap of between \
3 and 8 steps that build toward the goal by its deadline. Each step must be a small, \
actionable milestone. Respond ONLY with a single JSON object of the exact form: \
{\"steps\":[{\"title\":\"...\",\"detail\":\"...\" or null,\"due_date\":\"YYYY-MM-DD\" or null,\"effort\":\"...\" or null}]}. \
Give EACH step its OWN realistic per-step effort (e.g. '4-5 h ride', '3 x 45 min interval sessions', \
'20 min/day') — never one goal-wide number repeated across steps. Order steps chronologically and \
space their due dates sensibly before the goal's deadline. For endurance or skill goals, apply \
sound planning: progressively increase the key stressor, include a recovery/rest cadence, and taper \
before the event. Do not include any prose outside the JSON object.";

fn roadmap_user_prompt(goal: &GoalView) -> String {
    let mut out = format!("Goal: {}\n", goal.title);
    if let Some(desc) = goal.description.as_deref() {
        if !desc.trim().is_empty() {
            out.push_str(&format!("Description: {desc}\n"));
        }
    }
    if let Some(deadline) = goal.deadline.as_deref() {
        if !deadline.trim().is_empty() {
            out.push_str(&format!("Deadline: {deadline}\n"));
        }
    }
    if let Some(location) = goal.location.as_deref() {
        if !location.trim().is_empty() {
            out.push_str(&format!("Location / setting: {location}\n"));
        }
    }
    if let Some(skill_level) = goal.skill_level.as_deref() {
        if !skill_level.trim().is_empty() {
            out.push_str(&format!("Starting point / skill level: {skill_level}\n"));
        }
    }
    if let Some(success) = goal.success_criterion.as_deref() {
        if !success.trim().is_empty() {
            out.push_str(&format!(
                "Definition of success (the finish line): {success}\n"
            ));
        }
    }
    if let Some(minutes) = goal.time_per_session_min {
        if minutes > 0 {
            out.push_str(&format!("Time available per session: {minutes} minutes\n"));
        }
    }
    if let Some(note) = goal.progress_note.as_deref() {
        if !note.trim().is_empty() {
            out.push_str(&format!("Current progress: {note}\n"));
        }
    }
    out.push_str("Design the roadmap now.");
    out
}

/// Persist a freshly generated roadmap: replaces any existing roadmap for the
/// goal, inserts steps (`ord` = index), seeds reminders for dated steps, and
/// best-effort embeds each step. Returns the roadmap id.
async fn persist_roadmap(
    state: &AppState,
    user_id: &str,
    goal_id: &str,
    model: &str,
    draft: &RoadmapDraft,
) -> AppResult<String> {
    let now = now_rfc3339()?;

    // Replace any prior roadmap for this goal (regeneration is idempotent).
    sqlx::query(
        "DELETE FROM roadmap_steps WHERE roadmap_id IN (SELECT id FROM roadmaps WHERE goal_id = ?)",
    )
    .bind(goal_id)
    .execute(&state.db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    sqlx::query("DELETE FROM roadmaps WHERE goal_id = ?")
        .bind(goal_id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    let roadmap_id = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO roadmaps (id, goal_id, model, created_at) VALUES (?, ?, ?, ?)")
        .bind(&roadmap_id)
        .bind(goal_id)
        .bind(model)
        .bind(&now)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    insert_roadmap_steps(state, user_id, goal_id, &roadmap_id, &draft.steps, &now).await?;

    Ok(roadmap_id)
}

/// Insert an ordered list of steps into an existing roadmap (`ord` = index), seed
/// a reminder for each dated step, and best-effort embed each step. Shared by the
/// roadmap-generation path ([`persist_roadmap`]) and the plan-adjustment path
/// ([`replace_milestones`]). The caller is responsible for having cleared any
/// prior steps for the roadmap.
async fn insert_roadmap_steps(
    state: &AppState,
    user_id: &str,
    goal_id: &str,
    roadmap_id: &str,
    steps: &[StepDraft],
    now: &str,
) -> AppResult<()> {
    for (i, step) in steps.iter().enumerate() {
        let step_id = Uuid::new_v4().to_string();
        let due = step
            .due_date
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(to_rfc3339_due);
        let effort = step
            .effort
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        sqlx::query(
            "INSERT INTO roadmap_steps \
                 (id, roadmap_id, ord, title, detail, due_date, effort, status, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&step_id)
        .bind(roadmap_id)
        .bind(i as i64)
        .bind(&step.title)
        .bind(&step.detail)
        .bind(&due)
        .bind(&effort)
        .bind(StepStatus::Pending.as_str())
        .bind(now)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

        // Seed a reminder for dated steps (best-effort: a scheduling failure must
        // not lose the persisted roadmap).
        if let Some(due_ts) = due.as_deref() {
            if let Err(e) = state
                .notifier
                .enqueue(
                    user_id,
                    Some(goal_id),
                    Some(&step_id),
                    ReminderKind::Reminder.as_str(),
                    due_ts,
                    "inapp",
                    serde_json::json!({ "title": step.title }),
                )
                .await
            {
                tracing::warn!(error = ?e, step_id, "reminder enqueue failed (ignored)");
            }
        }

        // Best-effort RAG memory of the step.
        let step_text = match step.detail.as_deref() {
            Some(detail) if !detail.trim().is_empty() => format!("{}: {}", step.title, detail),
            _ => step.title.clone(),
        };
        embed_upsert(state, user_id, "step", &step_id, &step_text).await;
    }

    Ok(())
}

/// Create a goal plus its milestone roadmap from an agent-supplied plan (no LLM
/// call — the milestones come from the caller's tool arguments). Inserts the goal
/// as `active`, persists a roadmap (`model = "agent"`) when milestones are
/// present (which also seeds reminders for dated steps and embeds each step),
/// best-effort embeds the goal for RAG, and — when `pledge_cents > 0` — records a
/// `proposed` pledge (no funds are held; the user confirms holds separately).
/// Returns the new goal id.
pub async fn create_goal_with_milestones(
    state: &AppState,
    user_id: &str,
    draft: NewGoalWithPlan,
) -> AppResult<String> {
    // Insert the goal (starts as `draft`), then activate it now that it has a plan.
    let goal = insert_goal(
        &state.db,
        user_id,
        &draft.title,
        draft.description.as_deref(),
        draft.category.as_deref(),
        draft.deadline.as_deref(),
        draft.location.as_deref(),
        draft.skill_level.as_deref(),
        draft.success_criterion.as_deref(),
        draft.time_per_session_min,
    )
    .await?;

    let now = now_rfc3339()?;
    sqlx::query("UPDATE goals SET status = ?, updated_at = ? WHERE id = ?")
        .bind(GoalStatus::Active.as_str())
        .bind(&now)
        .bind(&goal.id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    // Persist the milestone roadmap (reuses the LLM roadmap path's writer).
    if !draft.milestones.is_empty() {
        let roadmap = RoadmapDraft {
            steps: draft
                .milestones
                .iter()
                .map(|m| StepDraft {
                    title: m.title.clone(),
                    detail: m.detail.clone(),
                    due_date: m.due_date.clone(),
                    effort: m.effort.clone(),
                })
                .collect(),
        };
        persist_roadmap(state, user_id, &goal.id, "agent", &roadmap).await?;
    }

    // Best-effort RAG memory of the goal.
    let goal_text = match goal.description.as_deref() {
        Some(desc) if !desc.trim().is_empty() => format!("{}: {}", goal.title, desc),
        _ => goal.title.clone(),
    };
    embed_upsert(state, user_id, "goal", &goal.id, &goal_text).await;

    // Optional symbolic pledge: record it as `proposed` only (funds are held only
    // when the user confirms via the wallet's confirm path). Best-effort.
    if let Some(cents) = draft.pledge_cents {
        if cents > 0 {
            let pledge_id = Uuid::new_v4().to_string();
            if let Err(e) = sqlx::query(
                "INSERT INTO pledges \
                     (id, goal_id, user_id, amount_cents, status, created_at, resolved_at) \
                 VALUES (?, ?, ?, ?, ?, ?, NULL) \
                 ON CONFLICT(goal_id) DO UPDATE SET \
                     amount_cents = excluded.amount_cents, \
                     user_id = excluded.user_id, \
                     status = excluded.status, \
                     resolved_at = NULL",
            )
            .bind(&pledge_id)
            .bind(&goal.id)
            .bind(user_id)
            .bind(cents)
            .bind(PledgeStatus::Proposed.as_str())
            .bind(&now)
            .execute(&state.db)
            .await
            {
                tracing::warn!(error = ?e, goal_id = %goal.id, "proposed pledge insert failed (ignored)");
            }
        }
    }

    Ok(goal.id)
}

// --- Break + milestone mutations ---------------------------------------------

/// Add a planned break to a goal's timeline. Verifies ownership, validates the
/// `YYYY-MM-DD` dates and a non-empty label, and returns the new break id.
async fn add_break(
    pool: &SqlitePool,
    user_id: &str,
    goal_id: &str,
    label: &str,
    start_date: &str,
    end_date: &str,
) -> AppResult<String> {
    owned_goal(pool, user_id, goal_id).await?;

    let label = label.trim();
    if label.is_empty() {
        return Err(AppError::BadRequest("break label is required".to_string()));
    }
    let start = start_date.trim();
    let end = end_date.trim();
    if !valid_ymd(start) || !valid_ymd(end) {
        return Err(AppError::BadRequest(
            "break dates must be YYYY-MM-DD".to_string(),
        ));
    }
    if end < start {
        return Err(AppError::BadRequest(
            "break end_date must not precede start_date".to_string(),
        ));
    }

    let id = Uuid::new_v4().to_string();
    let now = now_rfc3339()?;
    sqlx::query(
        "INSERT INTO goal_breaks (id, goal_id, user_id, label, start_date, end_date, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(goal_id)
    .bind(user_id)
    .bind(label)
    .bind(start)
    .bind(end)
    .bind(&now)
    .execute(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    Ok(id)
}

/// Remove a break from a goal, scoped to the owner (a break belonging to another
/// user or goal is simply not deleted).
async fn remove_break(
    pool: &SqlitePool,
    user_id: &str,
    goal_id: &str,
    break_id: &str,
) -> AppResult<()> {
    owned_goal(pool, user_id, goal_id).await?;
    sqlx::query("DELETE FROM goal_breaks WHERE id = ? AND goal_id = ? AND user_id = ?")
        .bind(break_id)
        .bind(goal_id)
        .bind(user_id)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(())
}

/// Replace the goal's roadmap steps with a new set of milestones. Verifies
/// ownership, ensures a roadmap row exists (creating one with `model = "agent"`
/// when missing), deletes the roadmap's existing steps, then inserts the new
/// milestones (which re-seeds reminders for dated steps and re-embeds each step).
async fn replace_milestones(
    state: &AppState,
    user_id: &str,
    goal_id: &str,
    milestones: Vec<NewMilestone>,
) -> AppResult<()> {
    owned_goal(&state.db, user_id, goal_id).await?;
    let now = now_rfc3339()?;

    // Ensure a roadmap row exists for this goal.
    let existing: Option<(String,)> = sqlx::query_as("SELECT id FROM roadmaps WHERE goal_id = ?")
        .bind(goal_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    let roadmap_id = match existing {
        Some((id,)) => id,
        None => {
            let id = Uuid::new_v4().to_string();
            sqlx::query(
                "INSERT INTO roadmaps (id, goal_id, model, created_at) VALUES (?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(goal_id)
            .bind("agent")
            .bind(&now)
            .execute(&state.db)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
            id
        }
    };

    // Replace the roadmap's steps.
    sqlx::query("DELETE FROM roadmap_steps WHERE roadmap_id = ?")
        .bind(&roadmap_id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    let steps: Vec<StepDraft> = milestones
        .into_iter()
        .map(|m| StepDraft {
            title: m.title,
            detail: m.detail,
            due_date: m.due_date,
            effort: m.effort,
        })
        .collect();
    insert_roadmap_steps(state, user_id, goal_id, &roadmap_id, &steps, &now).await?;

    Ok(())
}

// --- Handlers -----------------------------------------------------------------

async fn list_goals(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
) -> AppResult<Json<Vec<GoalView>>> {
    Ok(Json(list_goals_for(&state.db, &user_id).await?))
}

async fn create_goal(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    WithRejection(Json(body), _): WithRejection<Json<CreateGoal>, AppError>,
) -> AppResult<Json<GoalView>> {
    let goal = insert_goal(
        &state.db,
        &user_id,
        &body.title,
        body.description.as_deref(),
        body.category.as_deref(),
        body.deadline.as_deref(),
        body.location.as_deref(),
        body.skill_level.as_deref(),
        body.success_criterion.as_deref(),
        body.time_per_session_min,
    )
    .await?;

    // Best-effort RAG memory of the goal.
    let goal_text = match goal.description.as_deref() {
        Some(desc) if !desc.trim().is_empty() => format!("{}: {}", goal.title, desc),
        _ => goal.title.clone(),
    };
    embed_upsert(&state, &user_id, "goal", &goal.id, &goal_text).await;

    Ok(Json(goal))
}

async fn get_goal(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(id): Path<String>,
) -> AppResult<Json<GoalView>> {
    Ok(Json(owned_goal(&state.db, &user_id, &id).await?))
}

async fn patch_goal(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(id): Path<String>,
    WithRejection(Json(body), _): WithRejection<Json<PatchGoal>, AppError>,
) -> AppResult<Json<GoalView>> {
    Ok(Json(
        apply_goal_patch(&state.db, &user_id, &id, body).await?,
    ))
}

async fn generate_roadmap(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(id): Path<String>,
) -> AppResult<Json<RoadmapView>> {
    let goal = owned_goal(&state.db, &user_id, &id).await?;

    // Resolve the user's chosen chat model (falls back to the config default).
    let model = crate::settings::user_chat_model(&state.db, &state.config, &user_id).await?;

    let user_prompt = roadmap_user_prompt(&goal);
    let (draft, cost_cents) = state
        .llm
        .chat_json::<RoadmapDraft>(&model, ROADMAP_SYSTEM_PROMPT, &user_prompt)
        .await?;

    if draft.steps.is_empty() {
        return Err(AppError::Internal(anyhow::anyhow!(
            "roadmap generation returned no steps"
        )));
    }

    persist_roadmap(&state, &user_id, &goal.id, &model, &draft).await?;

    if cost_cents > 0 {
        crate::wallet::debit_tokens(&state.db, &user_id, cost_cents, "roadmap").await?;
    }

    // Activate the goal now that it has a plan.
    let now = now_rfc3339()?;
    sqlx::query("UPDATE goals SET status = ?, updated_at = ? WHERE id = ?")
        .bind(GoalStatus::Active.as_str())
        .bind(&now)
        .bind(&goal.id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    let roadmap = fetch_roadmap(&state.db, &goal.id)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(roadmap))
}

async fn get_roadmap(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(id): Path<String>,
) -> AppResult<Json<RoadmapView>> {
    // Enforce ownership of the parent goal first.
    owned_goal(&state.db, &user_id, &id).await?;
    // Always return a roadmap-or-empty view so breaks surface even before a
    // roadmap has been generated.
    let roadmap = fetch_roadmap_or_empty(&state.db, &id).await?;
    Ok(Json(roadmap))
}

async fn patch_step(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(id): Path<String>,
    WithRejection(Json(body), _): WithRejection<Json<PatchStep>, AppError>,
) -> AppResult<Json<StepView>> {
    Ok(Json(
        set_step_status(&state.db, &user_id, &id, &body.status).await?,
    ))
}

// --- Agentic plan adjustment -------------------------------------------------

#[derive(Debug, Deserialize)]
struct AdjustRequest {
    content: String,
}

#[derive(Debug, Serialize)]
struct AdjustReply {
    reply: String,
    changed: bool,
}

/// Title of the goal-scoped conversation that stores plan-adjustment history.
const ADJUST_CONVERSATION_TITLE: &str = "Plan adjustments";

/// The plan-adjustment coach persona. The concrete date + goal/plan/breaks
/// context are injected at call time (see [`adjust_system_prompt`]). It is a
/// PROPOSE-THEN-CONFIRM agent: it must describe changes in prose and only call a
/// tool after the user explicitly confirms.
const ADJUST_SYSTEM_PROMPT_BODY: &str = "You are AI Buddy, a warm, concise accountability coach \
helping the user adjust the timeline of an EXISTING goal. When the user reports a disruption \
(travel, illness, falling behind) or a planned unavailable period (e.g. \"away for Christmas \
Dec 20-27\", \"on vacation next week\"), PROPOSE concrete changes in PROSE and ask the user to \
confirm before doing anything. A good proposal: add a break for any unavailable period, and \
reschedule or restructure the milestones so that no milestone due-date falls inside a break and \
the plan still reaches the goal by its deadline. Do NOT invent breaks the user did not mention. \
Call the tools (add_break, remove_break, update_milestones) ONLY AFTER the user has explicitly \
confirmed your proposal in the conversation — never before, and never in the same reply as the \
proposal. When the user asks you to AVOID scheduling during a period or holiday (e.g. \"take the \
holidays into account\", \"don't set any deadlines around Christmas\", \"keep December clear\"), \
treat that period as off-limits: propose adding a break (add_break) covering it AND rescheduling \
the milestones (update_milestones) so that NO milestone due_date falls inside ANY break — existing \
or newly added — while the plan still reaches the goal by its deadline. You know the common NAMED \
holidays even when the user gives no exact dates: resolve them to concrete YYYY-MM-DD dates using \
the year implied by the plan's timeline (the goal deadline's year, or the next occurrence after \
today) — e.g. Christmas ≈ Dec 24-26, New Year's ≈ Dec 31-Jan 1, Easter in spring, a named exam or \
vacation period to its usual span — and state the exact dates you will use in your proposal so the \
user can correct them. When you do call update_milestones, send the COMPLETE new ordered list of \
milestones (it REPLACES all existing milestones), each with a realistic per-milestone effort and a \
YYYY-MM-DD due_date between today and the deadline, and verify every due_date falls OUTSIDE every \
break period before sending. After a tool succeeds, briefly confirm what changed. Keep every reply \
short and encouraging, and keep this advice goal-agnostic.";

/// Build the plan-adjustment system prompt with today's date and the goal's
/// current plan (milestones) and breaks injected as grounding context.
fn adjust_system_prompt(
    today: &str,
    goal: &GoalView,
    steps: &[StepView],
    breaks: &[GoalBreak],
) -> String {
    let mut ctx = format!("Today is {today}.\n{ADJUST_SYSTEM_PROMPT_BODY}\n\n");
    ctx.push_str(&format!("GOAL: {}\n", goal.title));
    if let Some(deadline) = goal.deadline.as_deref() {
        if !deadline.trim().is_empty() {
            ctx.push_str(&format!("DEADLINE: {deadline}\n"));
        }
    }
    if let Some(success) = goal.success_criterion.as_deref() {
        if !success.trim().is_empty() {
            ctx.push_str(&format!("DEFINITION OF SUCCESS: {success}\n"));
        }
    }
    ctx.push_str("CURRENT MILESTONES:\n");
    if steps.is_empty() {
        ctx.push_str("  (none yet)\n");
    } else {
        for s in steps {
            let due = s.due_date.as_deref().unwrap_or("no date");
            let effort = s.effort.as_deref().unwrap_or("no effort set");
            ctx.push_str(&format!("  - {} (due {due}; effort {effort})\n", s.title));
        }
    }
    ctx.push_str("CURRENT BREAKS:\n");
    if breaks.is_empty() {
        ctx.push_str("  (none)\n");
    } else {
        for b in breaks {
            ctx.push_str(&format!(
                "  - id={} \"{}\" {} to {}\n",
                b.id, b.label, b.start_date, b.end_date
            ));
        }
    }
    ctx
}

/// The three plan-adjustment tools exposed to the agent, as an OpenAI `tools`
/// array. All are called only AFTER the user confirms (enforced by the prompt).
fn adjust_tools() -> serde_json::Value {
    serde_json::json!([
        {
            "type": "function",
            "function": {
                "name": "add_break",
                "description": "Add a planned unavailable period to the goal timeline. ONLY call \
    after the user has confirmed.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "label": { "type": "string", "description": "Short label, e.g. 'Christmas'." },
                        "start_date": { "type": "string", "description": "First unavailable day, YYYY-MM-DD." },
                        "end_date": { "type": "string", "description": "Last unavailable day, YYYY-MM-DD." }
                    },
                    "required": ["label", "start_date", "end_date"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "remove_break",
                "description": "Remove a previously added break by its id. ONLY call after the user confirms.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "break_id": { "type": "string", "description": "The id of the break to remove." }
                    },
                    "required": ["break_id"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "update_milestones",
                "description": "REPLACE all of the goal's milestones with a new ordered list to \
    reschedule/restructure the plan. ONLY call after the user confirms. Send the COMPLETE list.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "milestones": {
                            "type": "array",
                            "description": "The complete new ordered list of milestones.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "title": { "type": "string", "description": "Short milestone title." },
                                    "detail": { "type": "string", "description": "Optional milestone detail." },
                                    "due_date": { "type": "string", "description": "Target date for THIS milestone, YYYY-MM-DD, between today and the deadline and outside every break." },
                                    "effort": { "type": "string", "description": "Realistic effort for THIS milestone, e.g. '4-5 h ride'." }
                                },
                                "required": ["title"]
                            }
                        }
                    },
                    "required": ["milestones"]
                }
            }
        }
    ])
}

#[derive(Debug, Deserialize)]
struct AddBreakArgs {
    label: String,
    start_date: String,
    end_date: String,
}

#[derive(Debug, Deserialize)]
struct RemoveBreakArgs {
    break_id: String,
}

#[derive(Debug, Deserialize)]
struct UpdateMilestonesArgs {
    #[serde(default)]
    milestones: Vec<NewMilestone>,
}

/// Sanitize milestones from a tool call: trim titles/efforts, drop malformed
/// due dates and title-less milestones. Pure.
fn sanitize_milestones(milestones: Vec<NewMilestone>) -> Vec<NewMilestone> {
    milestones
        .into_iter()
        .filter_map(|mut m| {
            m.title = m.title.trim().to_string();
            if m.title.is_empty() {
                return None;
            }
            if let Some(due) = &m.due_date {
                if !valid_ymd(due) {
                    m.due_date = None;
                }
            }
            m.effort = m
                .effort
                .take()
                .map(|e| e.trim().to_string())
                .filter(|e| !e.is_empty());
            Some(m)
        })
        .collect()
}

/// Find the goal-scoped "Plan adjustments" conversation for this user, creating
/// one if it does not exist. Returns the conversation id.
async fn find_or_create_adjust_conversation(
    pool: &SqlitePool,
    user_id: &str,
    goal_id: &str,
) -> AppResult<String> {
    let existing: Option<(String,)> = sqlx::query_as(
        "SELECT id FROM conversations WHERE user_id = ? AND goal_id = ? AND title = ? \
         ORDER BY created_at ASC LIMIT 1",
    )
    .bind(user_id)
    .bind(goal_id)
    .bind(ADJUST_CONVERSATION_TITLE)
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    if let Some((id,)) = existing {
        return Ok(id);
    }

    let id = Uuid::new_v4().to_string();
    let now = now_rfc3339()?;
    sqlx::query(
        "INSERT INTO conversations (id, user_id, goal_id, title, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(goal_id)
    .bind(ADJUST_CONVERSATION_TITLE)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    Ok(id)
}

/// Persist a message to a conversation and bump the conversation's recency.
async fn insert_adjust_message(
    pool: &SqlitePool,
    conversation_id: &str,
    role: &str,
    content: &str,
    cost_cents: i64,
) -> AppResult<()> {
    let id = Uuid::new_v4().to_string();
    let now = now_rfc3339()?;
    sqlx::query(
        "INSERT INTO messages (id, conversation_id, role, content, token_cost_cents, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(conversation_id)
    .bind(role)
    .bind(content)
    .bind(cost_cents)
    .bind(&now)
    .execute(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    sqlx::query("UPDATE conversations SET updated_at = ? WHERE id = ?")
        .bind(&now)
        .bind(conversation_id)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(())
}

/// Prior `user`/`assistant` messages of a conversation, oldest first, as
/// `(role, content)` pairs.
async fn adjust_history(
    pool: &SqlitePool,
    conversation_id: &str,
) -> AppResult<Vec<(String, String)>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT role, content FROM messages WHERE conversation_id = ? \
         ORDER BY created_at ASC, id ASC",
    )
    .bind(conversation_id)
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    Ok(rows)
}

/// One agentic plan-adjustment turn. The model proposes timeline changes in prose
/// and, only after the user confirms, calls tools to add/remove breaks and
/// reschedule milestones. Returns the assistant's reply and whether any tool
/// modified the plan this turn.
async fn adjust_plan(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(id): Path<String>,
    WithRejection(Json(body), _): WithRejection<Json<AdjustRequest>, AppError>,
) -> AppResult<Json<AdjustReply>> {
    let content = body.content.trim().to_string();
    if content.is_empty() {
        return Err(AppError::BadRequest(
            "message content is required".to_string(),
        ));
    }

    // Ownership (wrong owner or missing -> Forbidden/NotFound) + load the goal.
    let goal = owned_goal(&state.db, &user_id, &id).await?;

    // Wallet gate.
    if crate::wallet::balance_cents(&state.db, &user_id).await? <= 0 {
        return Err(AppError::PaymentRequired(
            "Top up your wallet to keep chatting".to_string(),
        ));
    }

    // Goal-scoped conversation for adjustment history.
    let conv_id = find_or_create_adjust_conversation(&state.db, &user_id, &id).await?;
    insert_adjust_message(&state.db, &conv_id, "user", &content, 0).await?;

    // Current plan + breaks for grounding the system prompt.
    let roadmap = fetch_roadmap_or_empty(&state.db, &id).await?;
    let today = now_rfc3339()?
        .get(..10)
        .map(str::to_string)
        .unwrap_or_default();
    let system = adjust_system_prompt(&today, &goal, &roadmap.steps, &roadmap.breaks);

    // Build the OpenAI messages array: system prompt + prior history (which
    // already includes the message just inserted).
    let mut messages: Vec<serde_json::Value> =
        vec![serde_json::json!({ "role": "system", "content": system })];
    for (role, text) in adjust_history(&state.db, &conv_id).await? {
        if role == "user" || role == "assistant" {
            messages.push(serde_json::json!({ "role": role, "content": text }));
        }
    }

    let tools = adjust_tools();
    let model = crate::settings::user_chat_model(&state.db, &state.config, &user_id).await?;

    let mut total_cost: i64 = 0;
    let mut changed = false;
    let mut reply: Option<String> = None;

    for _ in 0..4 {
        let turn = state
            .llm
            .chat_tools(&model, messages.clone(), &tools)
            .await?;
        total_cost = total_cost.saturating_add(turn.cost_cents);

        if let Some(text) = &turn.content {
            if !text.trim().is_empty() {
                reply = Some(text.clone());
            }
        }

        if turn.tool_calls.is_empty() {
            break;
        }

        let tool_calls_json: Vec<serde_json::Value> = turn
            .tool_calls
            .iter()
            .map(|tc| {
                serde_json::json!({
                    "id": tc.id,
                    "type": "function",
                    "function": { "name": tc.name, "arguments": tc.arguments },
                })
            })
            .collect();
        messages.push(serde_json::json!({
            "role": "assistant",
            "content": turn.content.clone().unwrap_or_default(),
            "tool_calls": tool_calls_json,
        }));

        for tc in &turn.tool_calls {
            let result = match tc.name.as_str() {
                "add_break" => match serde_json::from_str::<AddBreakArgs>(&tc.arguments) {
                    Ok(args) => {
                        match add_break(
                            &state.db,
                            &user_id,
                            &id,
                            &args.label,
                            &args.start_date,
                            &args.end_date,
                        )
                        .await
                        {
                            Ok(break_id) => {
                                changed = true;
                                serde_json::json!({ "ok": true, "break_id": break_id })
                            }
                            Err(e) => {
                                tracing::warn!(error = ?e, "add_break tool failed");
                                serde_json::json!({ "ok": false, "error": "could not add break" })
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = ?e, "add_break arguments invalid");
                        serde_json::json!({ "ok": false, "error": "invalid arguments" })
                    }
                },
                "remove_break" => match serde_json::from_str::<RemoveBreakArgs>(&tc.arguments) {
                    Ok(args) => {
                        match remove_break(&state.db, &user_id, &id, &args.break_id).await {
                            Ok(()) => {
                                changed = true;
                                serde_json::json!({ "ok": true })
                            }
                            Err(e) => {
                                tracing::warn!(error = ?e, "remove_break tool failed");
                                serde_json::json!({ "ok": false, "error": "could not remove break" })
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = ?e, "remove_break arguments invalid");
                        serde_json::json!({ "ok": false, "error": "invalid arguments" })
                    }
                },
                "update_milestones" => {
                    match serde_json::from_str::<UpdateMilestonesArgs>(&tc.arguments) {
                        Ok(args) => {
                            let milestones = sanitize_milestones(args.milestones);
                            if milestones.is_empty() {
                                serde_json::json!({ "ok": false, "error": "no valid milestones" })
                            } else {
                                match replace_milestones(&state, &user_id, &id, milestones).await {
                                    Ok(()) => {
                                        changed = true;
                                        serde_json::json!({ "ok": true })
                                    }
                                    Err(e) => {
                                        tracing::warn!(error = ?e, "update_milestones tool failed");
                                        serde_json::json!({ "ok": false, "error": "could not update milestones" })
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, "update_milestones arguments invalid");
                            serde_json::json!({ "ok": false, "error": "invalid arguments" })
                        }
                    }
                }
                _ => serde_json::json!({ "ok": false, "error": "unknown tool" }),
            };
            messages.push(serde_json::json!({
                "role": "tool",
                "tool_call_id": tc.id,
                "content": result.to_string(),
            }));
        }
    }

    let reply = reply.unwrap_or_else(|| "Done — your plan is updated.".to_string());

    // Debit the wallet (best-effort), persist the assistant reply.
    if total_cost > 0 {
        if let Err(e) = crate::wallet::debit_tokens(&state.db, &user_id, total_cost, "chat").await {
            tracing::error!(error = ?e, "wallet debit failed");
        }
    }
    insert_adjust_message(&state.db, &conv_id, "assistant", &reply, total_cost).await?;

    Ok(Json(AdjustReply { reply, changed }))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> (tempfile::TempDir, SqlitePool) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("test.db");
        let path = db_path.to_str().expect("utf8 path");
        let pool = crate::db::init_pool(path).await.expect("init pool");
        (dir, pool)
    }

    /// Build an [`AppState`] backed by `pool` for helpers that need it
    /// (e.g. [`replace_milestones`]). LLM/embedding calls degrade to no-ops in
    /// tests because no API key is configured, so nothing hits the network.
    fn test_state(pool: SqlitePool) -> AppState {
        use axum_extra::extract::cookie::Key;
        let config = std::sync::Arc::new(crate::config::Config::from_env());
        let llm = crate::llm::LlmClient::new(&config);
        let payments = crate::wallet::PaymentProvider::new(&config);
        let notifier = crate::notify::Notifier::new(pool.clone());
        AppState {
            db: pool,
            config,
            oidc: None,
            cookie_key: Key::generate(),
            llm,
            payments,
            notifier,
        }
    }

    #[test]
    fn valid_ymd_accepts_and_rejects() {
        assert!(valid_ymd("2026-09-29"));
        assert!(valid_ymd("  2026-12-27  "));
        assert!(!valid_ymd("next spring"));
        assert!(!valid_ymd("2026/09/29"));
        assert!(!valid_ymd("2026-13-01")); // bad month
        assert!(!valid_ymd("2026-12-40")); // bad day
        assert!(!valid_ymd("26-9-1")); // wrong widths
    }

    #[test]
    fn date_only_gets_default_time() {
        assert_eq!(to_rfc3339_due("2026-10-01"), "2026-10-01T09:00:00Z");
        assert_eq!(
            to_rfc3339_due("2026-10-01T12:30:00Z"),
            "2026-10-01T12:30:00Z"
        );
        assert_eq!(to_rfc3339_due("  2026-10-01  "), "2026-10-01T09:00:00Z");
    }

    #[tokio::test]
    async fn create_list_get_roundtrip() {
        let (_dir, pool) = test_pool().await;

        let goal = insert_goal(
            &pool,
            "u1",
            "Run a 5k",
            Some("from couch"),
            None,
            Some("2026-12-01"),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("insert");
        assert_eq!(goal.status, "draft");
        assert_eq!(goal.title, "Run a 5k");

        let listed = list_goals_for(&pool, "u1").await.expect("list");
        assert_eq!(listed.len(), 1);

        let fetched = owned_goal(&pool, "u1", &goal.id).await.expect("get");
        assert_eq!(fetched.id, goal.id);
    }

    #[tokio::test]
    async fn create_with_new_fields_roundtrips() {
        let (_dir, pool) = test_pool().await;

        let goal = insert_goal(
            &pool,
            "u1",
            "Learn to surf",
            Some("catch a green wave"),
            Some("sport"),
            Some("2026-12-01"),
            Some("Ericeira, Portugal"),
            Some("total beginner"),
            Some("ride an unbroken wave for 5 seconds"),
            Some(90),
        )
        .await
        .expect("insert");

        assert_eq!(goal.location.as_deref(), Some("Ericeira, Portugal"));
        assert_eq!(goal.skill_level.as_deref(), Some("total beginner"));
        assert_eq!(
            goal.success_criterion.as_deref(),
            Some("ride an unbroken wave for 5 seconds")
        );
        assert_eq!(goal.time_per_session_min, Some(90));

        // Read back through fetch to confirm the SELECT column list maps correctly.
        let fetched = owned_goal(&pool, "u1", &goal.id).await.expect("get");
        assert_eq!(fetched.location.as_deref(), Some("Ericeira, Portugal"));
        assert_eq!(fetched.skill_level.as_deref(), Some("total beginner"));
        assert_eq!(
            fetched.success_criterion.as_deref(),
            Some("ride an unbroken wave for 5 seconds")
        );
        assert_eq!(fetched.time_per_session_min, Some(90));

        // And through the list query (separate SELECT column list).
        let listed = list_goals_for(&pool, "u1").await.expect("list");
        assert_eq!(listed.len(), 1);
        let first = listed.first().expect("one goal");
        assert_eq!(first.time_per_session_min, Some(90));

        // Patch a subset of the new fields and confirm they persist.
        let patched = apply_goal_patch(
            &pool,
            "u1",
            &goal.id,
            PatchGoal {
                skill_level: Some("can stand on the board".to_string()),
                time_per_session_min: Some(60),
                ..Default::default()
            },
        )
        .await
        .expect("patch");
        assert_eq!(
            patched.skill_level.as_deref(),
            Some("can stand on the board")
        );
        assert_eq!(patched.time_per_session_min, Some(60));
        // Untouched new field is preserved.
        assert_eq!(patched.location.as_deref(), Some("Ericeira, Portugal"));
    }

    #[tokio::test]
    async fn empty_title_is_rejected() {
        let (_dir, pool) = test_pool().await;
        let err = insert_goal(&pool, "u1", "   ", None, None, None, None, None, None, None)
            .await
            .expect_err("should reject");
        assert!(matches!(err, AppError::BadRequest(_)));
    }

    #[tokio::test]
    async fn ownership_is_enforced() {
        let (_dir, pool) = test_pool().await;
        let goal = insert_goal(
            &pool, "owner", "Mine", None, None, None, None, None, None, None,
        )
        .await
        .expect("insert");

        // Wrong owner -> Forbidden.
        let err = owned_goal(&pool, "intruder", &goal.id)
            .await
            .expect_err("forbidden");
        assert!(matches!(err, AppError::Forbidden));

        // Missing -> NotFound.
        let err = owned_goal(&pool, "owner", "does-not-exist")
            .await
            .expect_err("not found");
        assert!(matches!(err, AppError::NotFound));
    }

    #[tokio::test]
    async fn patch_updates_fields_and_validates_status() {
        let (_dir, pool) = test_pool().await;
        let goal = insert_goal(
            &pool, "u1", "Original", None, None, None, None, None, None, None,
        )
        .await
        .expect("insert");

        let patched = apply_goal_patch(
            &pool,
            "u1",
            &goal.id,
            PatchGoal {
                title: Some("Updated".to_string()),
                status: Some("active".to_string()),
                progress_note: Some("started".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("patch");
        assert_eq!(patched.title, "Updated");
        assert_eq!(patched.status, "active");
        assert_eq!(patched.progress_note.as_deref(), Some("started"));

        let bad = apply_goal_patch(
            &pool,
            "u1",
            &goal.id,
            PatchGoal {
                status: Some("bogus".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect_err("invalid status");
        assert!(matches!(bad, AppError::BadRequest(_)));

        // Patch by non-owner is forbidden.
        let forbidden = apply_goal_patch(&pool, "someone-else", &goal.id, PatchGoal::default())
            .await
            .expect_err("forbidden");
        assert!(matches!(forbidden, AppError::Forbidden));
    }

    #[tokio::test]
    async fn step_status_update_checks_ownership() {
        let (_dir, pool) = test_pool().await;
        let goal = insert_goal(
            &pool, "u1", "Goal", None, None, None, None, None, None, None,
        )
        .await
        .expect("insert");

        // Hand-roll a roadmap + one step for the goal.
        let now = now_rfc3339().expect("now");
        let roadmap_id = "rm1".to_string();
        sqlx::query("INSERT INTO roadmaps (id, goal_id, model, created_at) VALUES (?, ?, ?, ?)")
            .bind(&roadmap_id)
            .bind(&goal.id)
            .bind("test-model")
            .bind(&now)
            .execute(&pool)
            .await
            .expect("insert roadmap");
        let step_id = "st1".to_string();
        sqlx::query(
            "INSERT INTO roadmap_steps (id, roadmap_id, ord, title, detail, due_date, status, created_at) \
             VALUES (?, ?, 0, 'Step one', NULL, NULL, 'pending', ?)",
        )
        .bind(&step_id)
        .bind(&roadmap_id)
        .bind(&now)
        .execute(&pool)
        .await
        .expect("insert step");

        // Owner can mark done.
        let updated = set_step_status(&pool, "u1", &step_id, "done")
            .await
            .expect("update");
        assert_eq!(updated.status, "done");

        // Invalid status rejected.
        let bad = set_step_status(&pool, "u1", &step_id, "nope")
            .await
            .expect_err("bad status");
        assert!(matches!(bad, AppError::BadRequest(_)));

        // Non-owner forbidden.
        let forbidden = set_step_status(&pool, "intruder", &step_id, "done")
            .await
            .expect_err("forbidden");
        assert!(matches!(forbidden, AppError::Forbidden));

        // Unknown step not found.
        let missing = set_step_status(&pool, "u1", "ghost", "done")
            .await
            .expect_err("not found");
        assert!(matches!(missing, AppError::NotFound));
    }

    #[tokio::test]
    async fn fetch_roadmap_returns_ordered_steps() {
        let (_dir, pool) = test_pool().await;
        let goal = insert_goal(
            &pool, "u1", "Goal", None, None, None, None, None, None, None,
        )
        .await
        .expect("insert");

        let now = now_rfc3339().expect("now");
        sqlx::query(
            "INSERT INTO roadmaps (id, goal_id, model, created_at) VALUES ('rm', ?, 'm', ?)",
        )
        .bind(&goal.id)
        .bind(&now)
        .execute(&pool)
        .await
        .expect("roadmap");
        for (id, ord) in [("b", 1i64), ("a", 0i64)] {
            sqlx::query(
                "INSERT INTO roadmap_steps (id, roadmap_id, ord, title, detail, due_date, status, created_at) \
                 VALUES (?, 'rm', ?, ?, NULL, NULL, 'pending', ?)",
            )
            .bind(id)
            .bind(ord)
            .bind(format!("step {ord}"))
            .bind(&now)
            .execute(&pool)
            .await
            .expect("step");
        }

        let roadmap = fetch_roadmap(&pool, &goal.id)
            .await
            .expect("fetch")
            .expect("present");
        let ords: Vec<i64> = roadmap.steps.iter().map(|s| s.ord).collect();
        assert_eq!(ords, vec![0, 1]);
    }

    #[tokio::test]
    async fn fetch_roadmap_returns_per_step_effort() {
        let (_dir, pool) = test_pool().await;
        let goal = insert_goal(
            &pool, "u1", "Goal", None, None, None, None, None, None, None,
        )
        .await
        .expect("insert");

        let now = now_rfc3339().expect("now");
        sqlx::query(
            "INSERT INTO roadmaps (id, goal_id, model, created_at) VALUES ('rm', ?, 'm', ?)",
        )
        .bind(&goal.id)
        .bind(&now)
        .execute(&pool)
        .await
        .expect("roadmap");
        // Migration 0011 adds the `effort` column; confirm it round-trips.
        sqlx::query(
            "INSERT INTO roadmap_steps \
                 (id, roadmap_id, ord, title, detail, due_date, effort, status, created_at) \
             VALUES ('s1', 'rm', 0, 'Long ride', NULL, NULL, '4-5 h ride', 'pending', ?)",
        )
        .bind(&now)
        .execute(&pool)
        .await
        .expect("step");

        let roadmap = fetch_roadmap(&pool, &goal.id)
            .await
            .expect("fetch")
            .expect("present");
        assert_eq!(
            roadmap.steps.first().and_then(|s| s.effort.as_deref()),
            Some("4-5 h ride")
        );
    }

    #[tokio::test]
    async fn add_and_remove_break_roundtrips() {
        let (_dir, pool) = test_pool().await;
        let goal = insert_goal(
            &pool, "u1", "Goal", None, None, None, None, None, None, None,
        )
        .await
        .expect("insert");

        // Add two breaks.
        let xmas = add_break(
            &pool,
            "u1",
            &goal.id,
            "Christmas",
            "2026-12-20",
            "2026-12-27",
        )
        .await
        .expect("add xmas");
        add_break(
            &pool,
            "u1",
            &goal.id,
            "New Year",
            "2026-12-31",
            "2027-01-02",
        )
        .await
        .expect("add ny");

        // They come back ordered by start_date.
        let breaks = fetch_breaks(&pool, &goal.id).await.expect("fetch");
        let labels: Vec<&str> = breaks.iter().map(|b| b.label.as_str()).collect();
        assert_eq!(labels, vec!["Christmas", "New Year"]);

        // Remove one.
        remove_break(&pool, "u1", &goal.id, &xmas)
            .await
            .expect("remove");
        let breaks = fetch_breaks(&pool, &goal.id).await.expect("fetch");
        assert_eq!(breaks.len(), 1);
        assert_eq!(breaks.first().map(|b| b.label.as_str()), Some("New Year"));
    }

    #[tokio::test]
    async fn add_break_validates_ownership_and_dates() {
        let (_dir, pool) = test_pool().await;
        let goal = insert_goal(
            &pool, "owner", "Goal", None, None, None, None, None, None, None,
        )
        .await
        .expect("insert");

        // Wrong owner is forbidden.
        let forbidden = add_break(&pool, "intruder", &goal.id, "X", "2026-12-20", "2026-12-27")
            .await
            .expect_err("forbidden");
        assert!(matches!(forbidden, AppError::Forbidden));

        // Empty label rejected.
        let bad_label = add_break(&pool, "owner", &goal.id, "  ", "2026-12-20", "2026-12-27")
            .await
            .expect_err("empty label");
        assert!(matches!(bad_label, AppError::BadRequest(_)));

        // Malformed date rejected.
        let bad_date = add_break(&pool, "owner", &goal.id, "X", "Dec 20", "2026-12-27")
            .await
            .expect_err("bad date");
        assert!(matches!(bad_date, AppError::BadRequest(_)));

        // End before start rejected.
        let bad_range = add_break(&pool, "owner", &goal.id, "X", "2026-12-27", "2026-12-20")
            .await
            .expect_err("bad range");
        assert!(matches!(bad_range, AppError::BadRequest(_)));
    }

    #[test]
    fn sanitize_milestones_drops_bad_entries() {
        let milestones = vec![
            NewMilestone {
                title: "  Base ride  ".to_string(),
                detail: None,
                due_date: Some("2026-11-01".to_string()),
                effort: Some("  3 h ride  ".to_string()),
            },
            NewMilestone {
                title: "Bad date".to_string(),
                detail: None,
                due_date: Some("not a date".to_string()),
                effort: Some("   ".to_string()),
            },
            NewMilestone {
                title: "   ".to_string(),
                detail: None,
                due_date: None,
                effort: None,
            },
        ];
        let clean = sanitize_milestones(milestones);
        assert_eq!(clean.len(), 2);
        assert_eq!(clean.first().map(|m| m.title.as_str()), Some("Base ride"));
        assert_eq!(
            clean.first().and_then(|m| m.effort.as_deref()),
            Some("3 h ride")
        );
        // Bad date dropped to None, whitespace effort dropped to None.
        assert_eq!(clean.get(1).and_then(|m| m.due_date.as_deref()), None);
        assert_eq!(clean.get(1).and_then(|m| m.effort.as_deref()), None);
    }

    #[tokio::test]
    async fn replace_milestones_creates_and_replaces_steps() {
        let (_dir, pool) = test_pool().await;
        let state = test_state(pool.clone());
        let goal = insert_goal(
            &pool, "u1", "Goal", None, None, None, None, None, None, None,
        )
        .await
        .expect("insert");

        // No roadmap yet: replace_milestones must create one.
        replace_milestones(
            &state,
            "u1",
            &goal.id,
            vec![
                NewMilestone {
                    title: "Step A".to_string(),
                    detail: None,
                    due_date: Some("2026-11-01".to_string()),
                    effort: Some("2 h".to_string()),
                },
                NewMilestone {
                    title: "Step B".to_string(),
                    detail: None,
                    due_date: None,
                    effort: None,
                },
            ],
        )
        .await
        .expect("replace 1");

        let roadmap = fetch_roadmap(&pool, &goal.id)
            .await
            .expect("fetch")
            .expect("present");
        let titles: Vec<&str> = roadmap.steps.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, vec!["Step A", "Step B"]);
        let roadmap_id = roadmap.id.clone();

        // Replace again: steps are swapped, roadmap row reused.
        replace_milestones(
            &state,
            "u1",
            &goal.id,
            vec![NewMilestone {
                title: "Step C".to_string(),
                detail: None,
                due_date: None,
                effort: None,
            }],
        )
        .await
        .expect("replace 2");

        let roadmap = fetch_roadmap(&pool, &goal.id)
            .await
            .expect("fetch")
            .expect("present");
        assert_eq!(roadmap.id, roadmap_id, "roadmap row reused");
        let titles: Vec<&str> = roadmap.steps.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, vec!["Step C"]);

        // Non-owner cannot replace.
        let forbidden = replace_milestones(&state, "intruder", &goal.id, vec![])
            .await
            .expect_err("forbidden");
        assert!(matches!(forbidden, AppError::Forbidden));
    }

    #[tokio::test]
    async fn roadmap_or_empty_returns_breaks_without_roadmap() {
        let (_dir, pool) = test_pool().await;
        let goal = insert_goal(
            &pool, "u1", "Goal", None, None, None, None, None, None, None,
        )
        .await
        .expect("insert");

        add_break(
            &pool,
            "u1",
            &goal.id,
            "Christmas",
            "2026-12-20",
            "2026-12-27",
        )
        .await
        .expect("add break");

        // No roadmap row exists, but breaks must still surface.
        assert!(fetch_roadmap(&pool, &goal.id)
            .await
            .expect("fetch")
            .is_none());
        let view = fetch_roadmap_or_empty(&pool, &goal.id).await.expect("view");
        assert!(view.steps.is_empty());
        assert_eq!(view.breaks.len(), 1);
        assert_eq!(
            view.breaks.first().map(|b| b.label.as_str()),
            Some("Christmas")
        );
    }
}
