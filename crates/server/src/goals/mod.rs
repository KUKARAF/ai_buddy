//! Goals + roadmap: CRUD and LLM-driven roadmap generation.
//!
//! Routes (all under `RequireAuth`):
//!   - `GET  /api/goals`               list the caller's goals
//!   - `POST /api/goals`               create a goal (status `draft`)
//!   - `GET  /api/goals/{id}`          fetch one goal (ownership enforced)
//!   - `PATCH /api/goals/{id}`         update mutable fields
//!   - `POST /api/goals/{id}/roadmap`  generate a roadmap via the LLM
//!   - `GET  /api/goals/{id}/roadmap`  fetch the roadmap + ordered steps
//!   - `PATCH /api/steps/{id}`         update a step's status
//!
//! Owns migration `0003_goals.sql`.

use axum::extract::{Path, State};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use axum_extra::extract::WithRejection;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use ai_buddy_core::domain::{GoalStatus, ReminderKind, StepStatus};

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
    pub status: String,
    pub created_at: String,
}

/// A roadmap plus its ordered steps.
#[derive(Debug, Clone, Serialize)]
pub struct RoadmapView {
    pub id: String,
    pub goal_id: String,
    pub model: Option<String>,
    pub created_at: String,
    pub steps: Vec<StepView>,
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
        .route("/api/steps/{id}", patch(patch_step))
}

// --- Helpers ------------------------------------------------------------------

fn now_rfc3339() -> AppResult<String> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|e| AppError::Internal(e.into()))
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

async fn fetch_roadmap(pool: &SqlitePool, goal_id: &str) -> AppResult<Option<RoadmapView>> {
    let row: Option<(String, Option<String>, String)> =
        sqlx::query_as("SELECT id, model, created_at FROM roadmaps WHERE goal_id = ?")
            .bind(goal_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;

    let Some((id, model, created_at)) = row else {
        return Ok(None);
    };

    let steps = sqlx::query_as::<_, StepView>(
        "SELECT id, roadmap_id, ord, title, detail, due_date, status, created_at \
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
    }))
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
        "SELECT id, roadmap_id, ord, title, detail, due_date, status, created_at \
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
{\"steps\":[{\"title\":\"...\",\"detail\":\"...\" or null,\"due_date\":\"YYYY-MM-DD\" or null}]}. \
Order steps chronologically and space their due dates sensibly before the goal's deadline. \
Do not include any prose outside the JSON object.";

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
        .bind(&state.config.chat_model)
        .bind(&now)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    for (i, step) in draft.steps.iter().enumerate() {
        let step_id = Uuid::new_v4().to_string();
        let due = step
            .due_date
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(to_rfc3339_due);

        sqlx::query(
            "INSERT INTO roadmap_steps \
                 (id, roadmap_id, ord, title, detail, due_date, status, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&step_id)
        .bind(&roadmap_id)
        .bind(i as i64)
        .bind(&step.title)
        .bind(&step.detail)
        .bind(&due)
        .bind(StepStatus::Pending.as_str())
        .bind(&now)
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

    Ok(roadmap_id)
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

    let user_prompt = roadmap_user_prompt(&goal);
    let (draft, cost_cents) = state
        .llm
        .chat_json::<RoadmapDraft>(ROADMAP_SYSTEM_PROMPT, &user_prompt)
        .await?;

    if draft.steps.is_empty() {
        return Err(AppError::Internal(anyhow::anyhow!(
            "roadmap generation returned no steps"
        )));
    }

    persist_roadmap(&state, &user_id, &goal.id, &draft).await?;

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
    let roadmap = fetch_roadmap(&state.db, &id)
        .await?
        .ok_or(AppError::NotFound)?;
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
}
