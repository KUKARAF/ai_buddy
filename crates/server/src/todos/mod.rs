//! Per-milestone (roadmap step) TODO checklist items.
//!
//! Each `step_todos` row is a concrete, checkable action item belonging to a
//! roadmap step (a milestone). The LLM generates a handful of them for a step;
//! they can then be ticked off manually or auto-ticked from a check-in note.
//! Ownership is always derived through the step -> roadmap -> goal chain
//! (`goals.user_id`) — there is no direct user_id column on `step_todos`.
//!
//! Owns migration `0015_step_todos.sql`.
//!
//! Routes (all require auth; ownership enforced via the step/goal chain):
//!   - `GET   /api/steps/{step_id}/todos`           ordered items for a step
//!   - `POST  /api/steps/{step_id}/todos/generate`  LLM-generate items (idempotent)
//!   - `PATCH /api/todos/{id}`                       toggle an item's `done` flag

use axum::extract::{Path, State};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use axum_extra::extract::WithRejection;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use ai_buddy_core::domain::StepStatus;

use crate::auth::session::RequireAuth;
use crate::config::Config;
use crate::error::{AppError, AppResult};
use crate::llm::LlmClient;
use crate::state::AppState;

/// Hard cap on how many generated items we keep for a single milestone.
const MAX_TODOS: usize = 8;

/// Check-in notes shorter than this (after trimming) are treated as routine and
/// never trigger the auto-tick LLM call.
const MIN_NOTE_LEN: usize = 6;

// --- DTOs ---------------------------------------------------------------------

/// One checklist item as stored and returned to the client. The integer `done`
/// column is decoded straight to `bool` (sqlx 0.8 maps 0/1 → bool via `FromRow`).
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct TodoView {
    pub id: String,
    pub step_id: String,
    pub ord: i64,
    pub title: String,
    pub done: bool,
    pub created_at: String,
    pub done_at: Option<String>,
}

/// The structured list of item titles the LLM returns via JSON-mode completion.
#[derive(Debug, Deserialize)]
struct GeneratedTodos {
    #[serde(default)]
    todos: Vec<String>,
}

/// The LLM's verdict on which pending items a check-in note completed. Values may
/// be 1-based item numbers or exact item ids; both are tolerated (see
/// [`select_todo_ids`]). Defaults so a partial/absent object still deserializes.
#[derive(Debug, Default, Deserialize)]
struct NoteMatch {
    #[serde(default)]
    done: Vec<serde_json::Value>,
}

/// Step + goal context for prompt-building, loaded in one ownership-checked query.
#[derive(Debug, sqlx::FromRow)]
struct StepCtx {
    user_id: String,
    goal_title: String,
    success_criterion: Option<String>,
    deadline: Option<String>,
    step_title: String,
    step_detail: Option<String>,
    step_effort: Option<String>,
    step_due: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PatchTodo {
    done: bool,
}

// --- Router -------------------------------------------------------------------

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/steps/{step_id}/todos", get(list_todos_handler))
        .route(
            "/api/steps/{step_id}/todos/generate",
            post(generate_handler),
        )
        .route("/api/todos/{id}", patch(patch_todo_handler))
}

// --- Helpers ------------------------------------------------------------------

fn now_rfc3339() -> AppResult<String> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|e| AppError::Internal(e.into()))
}

/// Verify the caller owns the step via step -> roadmap -> goal. `NotFound` when
/// the step does not exist, `Forbidden` when it belongs to another user
/// (consistent with `goals::set_step_status`).
async fn owned_step(pool: &SqlitePool, user_id: &str, step_id: &str) -> AppResult<()> {
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
        None => Err(AppError::NotFound),
        Some((o,)) if o != user_id => Err(AppError::Forbidden),
        Some(_) => Ok(()),
    }
}

/// Verify the caller owns the todo via todo -> step -> roadmap -> goal. `NotFound`
/// when the todo does not exist, `Forbidden` when owned by another user.
async fn owned_todo(pool: &SqlitePool, user_id: &str, todo_id: &str) -> AppResult<()> {
    let owner: Option<(String,)> = sqlx::query_as(
        "SELECT g.user_id FROM step_todos t \
             JOIN roadmap_steps s ON t.step_id = s.id \
             JOIN roadmaps r ON s.roadmap_id = r.id \
             JOIN goals g ON r.goal_id = g.id \
         WHERE t.id = ?",
    )
    .bind(todo_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    match owner {
        None => Err(AppError::NotFound),
        Some((o,)) if o != user_id => Err(AppError::Forbidden),
        Some(_) => Ok(()),
    }
}

/// All items for a step, ordered by `ord`. Empty vec when none.
async fn fetch_todos(pool: &SqlitePool, step_id: &str) -> AppResult<Vec<TodoView>> {
    sqlx::query_as::<_, TodoView>(
        "SELECT id, step_id, ord, title, done, created_at, done_at FROM step_todos \
         WHERE step_id = ? ORDER BY ord ASC, created_at ASC, id ASC",
    )
    .bind(step_id)
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))
}

/// Fetch a single item by id.
async fn fetch_todo(pool: &SqlitePool, id: &str) -> AppResult<Option<TodoView>> {
    sqlx::query_as::<_, TodoView>(
        "SELECT id, step_id, ord, title, done, created_at, done_at FROM step_todos WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))
}

/// Set an item's `done` flag (and `done_at`: now when marking done, NULL when
/// un-marking). Ownership is verified first.
async fn set_todo_done(
    pool: &SqlitePool,
    user_id: &str,
    todo_id: &str,
    done: bool,
) -> AppResult<TodoView> {
    owned_todo(pool, user_id, todo_id).await?;
    let done_at = if done { Some(now_rfc3339()?) } else { None };
    sqlx::query("UPDATE step_todos SET done = ?, done_at = ? WHERE id = ?")
        .bind(i64::from(done))
        .bind(&done_at)
        .bind(todo_id)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    fetch_todo(pool, todo_id).await?.ok_or(AppError::NotFound)
}

// --- Generation ---------------------------------------------------------------

const TODO_SYSTEM_PROMPT: &str = "You generate a short checklist of concrete, checkable action items \
for ONE milestone of a larger goal. Each item is a short imperative action the user can tick off when \
done (e.g. \"Complete a 60 km long ride\", \"Do 3 interval sessions this week\", \"Draft the intro \
section\"). Produce between 3 and 6 items, specific to THIS milestone — no numbering, no prose, no \
commentary, no sub-tasks that merely restate the milestone title. Respond ONLY with a single JSON \
object of the exact form: {\"todos\":[\"...\",\"...\"]}.";

/// Build the per-milestone user prompt from the step + goal context.
fn todo_user_prompt(ctx: &StepCtx) -> String {
    let mut out = format!("Goal: {}\n", ctx.goal_title);
    if let Some(success) = ctx.success_criterion.as_deref() {
        if !success.trim().is_empty() {
            out.push_str(&format!(
                "Definition of success (the finish line): {success}\n"
            ));
        }
    }
    if let Some(deadline) = ctx.deadline.as_deref() {
        if !deadline.trim().is_empty() {
            out.push_str(&format!("Goal deadline: {deadline}\n"));
        }
    }
    out.push_str(&format!("\nThis milestone: {}\n", ctx.step_title));
    if let Some(detail) = ctx.step_detail.as_deref() {
        if !detail.trim().is_empty() {
            out.push_str(&format!("Milestone detail: {detail}\n"));
        }
    }
    if let Some(effort) = ctx.step_effort.as_deref() {
        if !effort.trim().is_empty() {
            out.push_str(&format!("Milestone effort: {effort}\n"));
        }
    }
    if let Some(due) = ctx.step_due.as_deref() {
        if !due.trim().is_empty() {
            out.push_str(&format!("Milestone due date: {due}\n"));
        }
    }
    out.push_str("\nList the concrete action items for THIS milestone now.");
    out
}

/// Core of `POST /api/steps/{step_id}/todos/generate`, split out so the
/// idempotency guard is testable without an `AppState`.
///
/// Verifies ownership, then — if items already exist for the step — returns them
/// WITHOUT any LLM call (idempotent, no charge). Otherwise resolves the user's
/// model, enforces the wallet gate (402 when balance <= 0), calls the LLM, trims
/// and clamps the result, persists each item, debits the token cost best-effort,
/// and returns the stored rows.
async fn generate_todos(
    db: &SqlitePool,
    llm: &LlmClient,
    config: &Config,
    user_id: &str,
    step_id: &str,
) -> AppResult<Vec<TodoView>> {
    // Ownership + prompt context in one query.
    let ctx: Option<StepCtx> = sqlx::query_as::<_, StepCtx>(
        "SELECT g.user_id AS user_id, g.title AS goal_title, \
                g.success_criterion AS success_criterion, g.deadline AS deadline, \
                s.title AS step_title, s.detail AS step_detail, \
                s.effort AS step_effort, s.due_date AS step_due \
         FROM roadmap_steps s \
             JOIN roadmaps r ON s.roadmap_id = r.id \
             JOIN goals g ON r.goal_id = g.id \
         WHERE s.id = ?",
    )
    .bind(step_id)
    .fetch_optional(db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    let ctx = match ctx {
        None => return Err(AppError::NotFound),
        Some(c) if c.user_id != user_id => return Err(AppError::Forbidden),
        Some(c) => c,
    };

    // Idempotent: if items already exist, return them with no LLM call / charge.
    let existing = fetch_todos(db, step_id).await?;
    if !existing.is_empty() {
        return Ok(existing);
    }

    let model = crate::settings::user_chat_model(db, config, user_id).await?;

    // Wallet gate: refuse before spending on the LLM when the balance is empty.
    if crate::wallet::balance_cents(db, user_id).await? <= 0 {
        return Err(AppError::PaymentRequired(
            "insufficient balance to generate milestone todos".to_string(),
        ));
    }

    let user_prompt = todo_user_prompt(&ctx);
    let (gen, cost_cents) = llm
        .chat_json::<GeneratedTodos>(&model, TODO_SYSTEM_PROMPT, &user_prompt)
        .await?;

    let titles: Vec<String> = gen
        .todos
        .into_iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .take(MAX_TODOS)
        .collect();
    if titles.is_empty() {
        return Err(AppError::Internal(anyhow::anyhow!(
            "todo generation returned no items"
        )));
    }

    let now = now_rfc3339()?;
    for (i, title) in titles.iter().enumerate() {
        let id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO step_todos (id, step_id, ord, title, done, created_at, done_at) \
             VALUES (?, ?, ?, ?, 0, ?, NULL)",
        )
        .bind(&id)
        .bind(step_id)
        .bind(i as i64)
        .bind(title)
        .bind(&now)
        .execute(db)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    }

    // Best-effort debit: a charging failure must not lose the generated items.
    if cost_cents > 0 {
        if let Err(e) = crate::wallet::debit_tokens(db, user_id, cost_cents, "todos").await {
            tracing::warn!(error = ?e, step_id, "todo generation debit failed (ignored)");
        }
    }

    fetch_todos(db, step_id).await
}

// --- Check-in auto-tick -------------------------------------------------------

const NOTE_MATCH_SYSTEM: &str = "You decide which of a user's pending milestone action items their \
check-in note clearly says they have COMPLETED. Be CONSERVATIVE: include an item ONLY when the note \
unambiguously indicates that item is done — never guess, and never include an item the note merely \
relates to. The items are numbered; return the NUMBERS of the completed items. Respond ONLY as JSON: \
{\"done\":[<numbers>]}. Return {\"done\":[]} when nothing clearly matches.";

/// Build the numbered auto-tick prompt from the note and the pending items.
fn note_match_prompt(note: &str, pending: &[TodoView]) -> String {
    let mut out = format!("Check-in note: {note}\n\nPending action items:\n");
    for (i, todo) in pending.iter().enumerate() {
        out.push_str(&format!("{}. {}\n", i + 1, todo.title));
    }
    out.push_str("\nWhich items does the note clearly say are completed?");
    out
}

/// Pure selection: map the LLM's `done` values (1-based item numbers or exact
/// item ids) to the ids of the matching pending items. Unknown/out-of-range
/// values are dropped and ids are de-duplicated, preserving first-seen order.
fn select_todo_ids(pending: &[TodoView], done: &[serde_json::Value]) -> Vec<String> {
    fn nth(pending: &[TodoView], one_based: i64) -> Option<&TodoView> {
        if one_based < 1 {
            return None;
        }
        let idx = usize::try_from(one_based - 1).ok()?;
        pending.get(idx)
    }

    let mut ids: Vec<String> = Vec::new();
    for value in done {
        let picked: Option<&TodoView> = match value {
            serde_json::Value::Number(n) => n.as_i64().and_then(|i| nth(pending, i)),
            serde_json::Value::String(s) => {
                let s = s.trim();
                pending
                    .iter()
                    .find(|t| t.id == s)
                    .or_else(|| s.parse::<i64>().ok().and_then(|i| nth(pending, i)))
            }
            _ => None,
        };
        if let Some(todo) = picked {
            if !ids.iter().any(|existing| existing == &todo.id) {
                ids.push(todo.id.clone());
            }
        }
    }
    ids
}

/// Best-effort: auto-tick any pending items of a goal's CURRENT milestone that a
/// check-in note clearly reports completed. Returns the newly-ticked items (empty
/// in the common case). This NEVER fails the caller: any error degrades to `[]`.
///
/// The "current milestone" is the first roadmap step with status != `done` by
/// `ord`. Only its pending (`done = 0`) items are considered, and only when the
/// note is non-trivial. Exactly one LLM call is made; its cost is debited
/// best-effort. Called from the check-in handler.
pub async fn complete_todos_from_note(
    llm: &LlmClient,
    pool: &SqlitePool,
    config: &Config,
    user_id: &str,
    goal_id: &str,
    note: Option<&str>,
) -> Vec<TodoView> {
    match try_complete_todos_from_note(llm, pool, config, user_id, goal_id, note).await {
        Ok(completed) => completed,
        Err(e) => {
            tracing::warn!(error = ?e, goal_id, "auto-tick todos from note failed (ignored)");
            Vec::new()
        }
    }
}

/// Fallible inner body of [`complete_todos_from_note`]; all errors are swallowed
/// by the public wrapper.
async fn try_complete_todos_from_note(
    llm: &LlmClient,
    pool: &SqlitePool,
    config: &Config,
    user_id: &str,
    goal_id: &str,
    note: Option<&str>,
) -> AppResult<Vec<TodoView>> {
    let note = match note.map(str::trim) {
        Some(n) if n.len() >= MIN_NOTE_LEN => n,
        _ => return Ok(Vec::new()),
    };

    // Current milestone: first not-yet-done step by order.
    let step: Option<(String,)> = sqlx::query_as(
        "SELECT s.id FROM roadmap_steps s \
             JOIN roadmaps r ON s.roadmap_id = r.id \
         WHERE r.goal_id = ? AND s.status != ? \
         ORDER BY s.ord ASC, s.created_at ASC LIMIT 1",
    )
    .bind(goal_id)
    .bind(StepStatus::Done.as_str())
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    let step_id = match step {
        Some((id,)) => id,
        None => return Ok(Vec::new()),
    };

    // Pending items of that milestone.
    let pending: Vec<TodoView> = sqlx::query_as::<_, TodoView>(
        "SELECT id, step_id, ord, title, done, created_at, done_at FROM step_todos \
         WHERE step_id = ? AND done = 0 ORDER BY ord ASC, created_at ASC, id ASC",
    )
    .bind(&step_id)
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    if pending.is_empty() {
        return Ok(Vec::new());
    }

    let model = crate::settings::user_chat_model(pool, config, user_id).await?;
    let user_prompt = note_match_prompt(note, &pending);
    let (matched, cost_cents) = llm
        .chat_json::<NoteMatch>(&model, NOTE_MATCH_SYSTEM, &user_prompt)
        .await?;

    // Charge best-effort; a debit failure must not lose the tick.
    if cost_cents > 0 {
        if let Err(e) =
            crate::wallet::debit_tokens(pool, user_id, cost_cents, "todos-autotick").await
        {
            tracing::warn!(error = ?e, goal_id, "auto-tick debit failed (ignored)");
        }
    }

    let ids = select_todo_ids(&pending, &matched.done);
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let now = now_rfc3339()?;
    let mut completed: Vec<TodoView> = Vec::new();
    for id in &ids {
        sqlx::query("UPDATE step_todos SET done = 1, done_at = ? WHERE id = ? AND done = 0")
            .bind(&now)
            .bind(id)
            .execute(pool)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        if let Some(todo) = fetch_todo(pool, id).await? {
            completed.push(todo);
        }
    }
    Ok(completed)
}

// --- Handlers -----------------------------------------------------------------

async fn list_todos_handler(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(step_id): Path<String>,
) -> AppResult<Json<Vec<TodoView>>> {
    owned_step(&state.db, &user_id, &step_id).await?;
    Ok(Json(fetch_todos(&state.db, &step_id).await?))
}

async fn generate_handler(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(step_id): Path<String>,
) -> AppResult<Json<Vec<TodoView>>> {
    Ok(Json(
        generate_todos(&state.db, &state.llm, &state.config, &user_id, &step_id).await?,
    ))
}

async fn patch_todo_handler(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(id): Path<String>,
    WithRejection(Json(body), _): WithRejection<Json<PatchTodo>, AppError>,
) -> AppResult<Json<TodoView>> {
    Ok(Json(
        set_todo_done(&state.db, &user_id, &id, body.done).await?,
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
        sqlx::query(
            "INSERT INTO users (id, email, display_name, created_at) \
             VALUES ('u1', NULL, NULL, '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .expect("insert user");
        (dir, pool)
    }

    async fn insert_user(pool: &SqlitePool, id: &str) {
        sqlx::query(
            "INSERT INTO users (id, email, display_name, created_at) \
             VALUES (?, NULL, NULL, '2026-01-01T00:00:00Z')",
        )
        .bind(id)
        .execute(pool)
        .await
        .expect("insert user");
    }

    /// Seed a goal + roadmap + one step owned by `user_id`. Returns nothing; the
    /// ids passed in are used verbatim.
    async fn seed_step(
        pool: &SqlitePool,
        user_id: &str,
        goal_id: &str,
        roadmap_id: &str,
        step_id: &str,
        status: &str,
        ord: i64,
    ) {
        sqlx::query(
            "INSERT INTO goals (id, user_id, title, success_criterion, deadline, status, created_at, updated_at) \
             VALUES (?, ?, 'Ride 100km', 'Finish a 100 km ride', '2026-06-01', 'active', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z') \
             ON CONFLICT(id) DO NOTHING",
        )
        .bind(goal_id)
        .bind(user_id)
        .execute(pool)
        .await
        .expect("insert goal");
        sqlx::query(
            "INSERT INTO roadmaps (id, goal_id, model, created_at) \
             VALUES (?, ?, 'test', '2026-01-01T00:00:00Z') ON CONFLICT(id) DO NOTHING",
        )
        .bind(roadmap_id)
        .bind(goal_id)
        .execute(pool)
        .await
        .expect("insert roadmap");
        sqlx::query(
            "INSERT INTO roadmap_steps (id, roadmap_id, ord, title, detail, due_date, effort, status, created_at) \
             VALUES (?, ?, ?, 'Base week', 'Build base fitness', '2026-02-01', '3 rides', ?, '2026-01-01T00:00:00Z')",
        )
        .bind(step_id)
        .bind(roadmap_id)
        .bind(ord)
        .bind(status)
        .execute(pool)
        .await
        .expect("insert step");
    }

    async fn seed_todo(pool: &SqlitePool, id: &str, step_id: &str, ord: i64, title: &str) {
        sqlx::query(
            "INSERT INTO step_todos (id, step_id, ord, title, done, created_at, done_at) \
             VALUES (?, ?, ?, ?, 0, '2026-01-01T00:00:00Z', NULL)",
        )
        .bind(id)
        .bind(step_id)
        .bind(ord)
        .bind(title)
        .execute(pool)
        .await
        .expect("insert todo");
    }

    fn tv(id: &str) -> TodoView {
        TodoView {
            id: id.to_string(),
            step_id: "s1".to_string(),
            ord: 0,
            title: format!("item {id}"),
            done: false,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            done_at: None,
        }
    }

    #[tokio::test]
    async fn crud_roundtrip_orders_and_toggles() {
        let (_dir, pool) = test_pool().await;
        seed_step(&pool, "u1", "g1", "r1", "s1", "pending", 0).await;
        // Seed out of order; fetch must return them by `ord`.
        seed_todo(&pool, "t2", "s1", 1, "Second").await;
        seed_todo(&pool, "t1", "s1", 0, "First").await;

        let todos = fetch_todos(&pool, "s1").await.expect("fetch");
        let titles: Vec<&str> = todos.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["First", "Second"]);
        assert!(todos.iter().all(|t| !t.done));
        assert!(todos.iter().all(|t| t.done_at.is_none()));

        // Mark done -> done flag set, done_at populated.
        let updated = set_todo_done(&pool, "u1", "t1", true)
            .await
            .expect("mark done");
        assert!(updated.done);
        assert!(updated.done_at.is_some());

        // Un-mark -> done cleared, done_at nulled.
        let reverted = set_todo_done(&pool, "u1", "t1", false)
            .await
            .expect("unmark");
        assert!(!reverted.done);
        assert!(reverted.done_at.is_none());
    }

    #[tokio::test]
    async fn ownership_rejections_for_step_and_todo() {
        let (_dir, pool) = test_pool().await;
        insert_user(&pool, "owner").await;
        seed_step(&pool, "owner", "g1", "r1", "s1", "pending", 0).await;
        seed_todo(&pool, "t1", "s1", 0, "First").await;

        // Another user is Forbidden on the step and the todo...
        assert!(matches!(
            owned_step(&pool, "u1", "s1").await,
            Err(AppError::Forbidden)
        ));
        assert!(matches!(
            owned_todo(&pool, "u1", "t1").await,
            Err(AppError::Forbidden)
        ));
        // ...and a patch attempt by them is rejected before any write.
        assert!(matches!(
            set_todo_done(&pool, "u1", "t1", true).await,
            Err(AppError::Forbidden)
        ));

        // Missing rows are NotFound.
        assert!(matches!(
            owned_step(&pool, "owner", "missing").await,
            Err(AppError::NotFound)
        ));
        assert!(matches!(
            owned_todo(&pool, "owner", "missing").await,
            Err(AppError::NotFound)
        ));
    }

    #[tokio::test]
    async fn generate_is_idempotent_when_todos_exist() {
        let (_dir, pool) = test_pool().await;
        seed_step(&pool, "u1", "g1", "r1", "s1", "pending", 0).await;
        seed_todo(&pool, "t1", "s1", 0, "First").await;
        seed_todo(&pool, "t2", "s1", 1, "Second").await;

        // No LLM key is configured in tests; the idempotency guard must short-circuit
        // and return the seeded rows WITHOUT any network call.
        let config = Config::from_env();
        let llm = LlmClient::new(&config);
        let todos = generate_todos(&pool, &llm, &config, "u1", "s1")
            .await
            .expect("idempotent generate returns existing");
        let titles: Vec<&str> = todos.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["First", "Second"]);
    }

    #[tokio::test]
    async fn generate_rejects_unauthorized_step() {
        let (_dir, pool) = test_pool().await;
        insert_user(&pool, "owner").await;
        seed_step(&pool, "owner", "g1", "r1", "s1", "pending", 0).await;

        let config = Config::from_env();
        let llm = LlmClient::new(&config);
        // Another user cannot generate for a step they do not own.
        assert!(matches!(
            generate_todos(&pool, &llm, &config, "u1", "s1").await,
            Err(AppError::Forbidden)
        ));
        // Missing step is NotFound.
        assert!(matches!(
            generate_todos(&pool, &llm, &config, "owner", "missing").await,
            Err(AppError::NotFound)
        ));
    }

    #[test]
    fn select_matches_numbers_ids_and_dedupes() {
        let pending = vec![tv("a"), tv("b"), tv("c")];

        // 1-based numbers.
        let by_num = select_todo_ids(&pending, &[serde_json::json!(1), serde_json::json!(3)]);
        assert_eq!(by_num, vec!["a".to_string(), "c".to_string()]);

        // Exact id strings.
        let by_id = select_todo_ids(&pending, &[serde_json::json!("b")]);
        assert_eq!(by_id, vec!["b".to_string()]);

        // Numeric strings are treated as 1-based indices too.
        let by_numstr = select_todo_ids(&pending, &[serde_json::json!("2")]);
        assert_eq!(by_numstr, vec!["b".to_string()]);

        // Out-of-range / unknown dropped; duplicates collapsed.
        let messy = select_todo_ids(
            &pending,
            &[
                serde_json::json!(1),
                serde_json::json!(1),
                serde_json::json!(0),
                serde_json::json!(99),
                serde_json::json!("zzz"),
            ],
        );
        assert_eq!(messy, vec!["a".to_string()]);

        // Empty input -> empty output.
        assert!(select_todo_ids(&pending, &[]).is_empty());
    }

    #[test]
    fn note_match_prompt_numbers_items_from_one() {
        let pending = vec![tv("a"), tv("b")];
        let prompt = note_match_prompt("did my long ride", &pending);
        assert!(prompt.contains("Check-in note: did my long ride"));
        assert!(prompt.contains("1. item a"));
        assert!(prompt.contains("2. item b"));
    }

    #[tokio::test]
    async fn auto_tick_degrades_to_empty_without_llm() {
        let (_dir, pool) = test_pool().await;
        seed_step(&pool, "u1", "g1", "r1", "s1", "pending", 0).await;
        seed_todo(&pool, "t1", "s1", 0, "Long ride").await;

        // No LLM key in tests, so the classification errors and the helper must
        // degrade to an empty result — never fail — leaving the item pending.
        let config = Config::from_env();
        let llm = LlmClient::new(&config);
        let completed =
            complete_todos_from_note(&llm, &pool, &config, "u1", "g1", Some("did my long ride"))
                .await;
        assert!(completed.is_empty());

        let todos = fetch_todos(&pool, "s1").await.expect("fetch");
        assert!(
            todos.iter().all(|t| !t.done),
            "nothing auto-ticked on error"
        );
    }

    #[tokio::test]
    async fn auto_tick_skips_trivial_notes() {
        let (_dir, pool) = test_pool().await;
        seed_step(&pool, "u1", "g1", "r1", "s1", "pending", 0).await;
        seed_todo(&pool, "t1", "s1", 0, "Long ride").await;

        let config = Config::from_env();
        let llm = LlmClient::new(&config);
        // A short note is routine and never reaches the LLM path.
        let completed =
            complete_todos_from_note(&llm, &pool, &config, "u1", "g1", Some("ok")).await;
        assert!(completed.is_empty());
    }
}
