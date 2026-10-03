//! Per-milestone (roadmap step) TODO checklist items.
//!
//! Each `step_todos` row is a concrete, checkable action item. A `category` tags
//! it as `training` (the default; a workout/ride/practice item scoped to a
//! milestone step) or a goal-level concern (`gear`/`logistics`/`prep`/`other`).
//! The LLM generates the initial training items for a step; items can then be
//! ticked off manually, auto-ticked from a check-in note, or ADDED by a check-in
//! note (training items filed under the current milestone, everything else at the
//! goal level via the `goal_id` column). Ownership is derived through the step ->
//! roadmap -> goal chain (`goals.user_id`) — there is no direct user_id column.
//!
//! Owns migrations `0015_step_todos.sql`, `0016_todo_category.sql`.
//!
//! Routes (all require auth; ownership enforced via the step/goal chain):
//!   - `GET   /api/steps/{step_id}/todos`           a step's TRAINING items, ordered
//!   - `GET   /api/goals/{goal_id}/todos`           a goal's NON-training items, ordered
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
/// `category` is always materialized via `COALESCE(category,'training')`, so a
/// legacy NULL row reads back as `"training"`. (`goal_id` is not part of the DTO.)
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct TodoView {
    pub id: String,
    pub step_id: String,
    pub ord: i64,
    pub title: String,
    pub done: bool,
    pub created_at: String,
    pub done_at: Option<String>,
    pub category: String,
}

/// The categories a todo may carry. A NULL column (legacy rows) is treated as
/// `training`; an unknown/missing value on an LLM-added item defaults to `other`.
const VALID_CATEGORIES: [&str; 5] = ["training", "gear", "logistics", "prep", "other"];

/// Column list shared by every `TodoView` query, materializing the category.
const TODO_COLUMNS: &str =
    "id, step_id, ord, title, done, created_at, done_at, COALESCE(category,'training') AS category";

/// The structured list of item titles the LLM returns via JSON-mode completion.
#[derive(Debug, Deserialize)]
struct GeneratedTodos {
    #[serde(default)]
    todos: Vec<String>,
}

/// The LLM's verdict on a check-in note: which pending items it COMPLETED and
/// which new items it implies the user should ADD. `done` values may be 1-based
/// item numbers or exact item ids (see [`select_todo_ids`]); `add` items carry a
/// title + category (see [`select_additions`]). Defaults so a partial/absent
/// object still deserializes.
#[derive(Debug, Default, Deserialize)]
struct NotePass {
    #[serde(default)]
    done: Vec<serde_json::Value>,
    #[serde(default)]
    add: Vec<AddTodo>,
}

/// One new item the LLM proposes adding from a check-in note.
#[derive(Debug, Clone, Deserialize)]
struct AddTodo {
    #[serde(default)]
    title: String,
    #[serde(default)]
    category: Option<String>,
}

/// Outcome of processing a check-in note against a goal's current milestone:
/// items newly marked done and items newly added. Default is two empty lists,
/// returned whenever the pass is skipped or any error is swallowed.
#[derive(Debug, Default)]
pub struct CheckinTodoResult {
    pub completed: Vec<TodoView>,
    pub added: Vec<TodoView>,
}

/// Step + goal context for prompt-building, loaded in one ownership-checked query.
#[derive(Debug, sqlx::FromRow)]
struct StepCtx {
    user_id: String,
    goal_id: String,
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
        .route("/api/goals/{goal_id}/todos", get(list_goal_todos_handler))
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

/// Verify the caller owns the goal directly (`goals.user_id`). `NotFound` when the
/// goal does not exist, `Forbidden` when it belongs to another user (consistent
/// with [`owned_step`] / [`owned_todo`]).
async fn owned_goal(pool: &SqlitePool, user_id: &str, goal_id: &str) -> AppResult<()> {
    let owner: Option<(String,)> = sqlx::query_as("SELECT user_id FROM goals WHERE id = ?")
        .bind(goal_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

    match owner {
        None => Err(AppError::NotFound),
        Some((o,)) if o != user_id => Err(AppError::Forbidden),
        Some(_) => Ok(()),
    }
}

/// A step's TRAINING items, ordered by `ord`. Empty vec when none. Non-training
/// items (gear/logistics/prep/other) are listed at the goal level instead.
async fn fetch_todos(pool: &SqlitePool, step_id: &str) -> AppResult<Vec<TodoView>> {
    sqlx::query_as::<_, TodoView>(&format!(
        "SELECT {TODO_COLUMNS} FROM step_todos \
         WHERE step_id = ? AND COALESCE(category,'training') = 'training' \
         ORDER BY ord ASC, created_at ASC, id ASC"
    ))
    .bind(step_id)
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))
}

/// A goal's NON-training items (gear/logistics/prep/other), ordered by `ord`.
/// These are added by check-ins and shown at the goal level, not under a step.
async fn fetch_goal_todos(pool: &SqlitePool, goal_id: &str) -> AppResult<Vec<TodoView>> {
    sqlx::query_as::<_, TodoView>(&format!(
        "SELECT {TODO_COLUMNS} FROM step_todos \
         WHERE goal_id = ? AND COALESCE(category,'training') <> 'training' \
         ORDER BY ord ASC, created_at ASC, id ASC"
    ))
    .bind(goal_id)
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))
}

/// Fetch a single item by id.
async fn fetch_todo(pool: &SqlitePool, id: &str) -> AppResult<Option<TodoView>> {
    sqlx::query_as::<_, TodoView>(&format!(
        "SELECT {TODO_COLUMNS} FROM step_todos WHERE id = ?"
    ))
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
        "SELECT g.user_id AS user_id, g.id AS goal_id, g.title AS goal_title, \
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

    // Initial milestone items are always TRAINING, scoped to the step's goal.
    let now = now_rfc3339()?;
    for (i, title) in titles.iter().enumerate() {
        let id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO step_todos (id, step_id, ord, title, done, created_at, done_at, goal_id, category) \
             VALUES (?, ?, ?, ?, 0, ?, NULL, ?, 'training')",
        )
        .bind(&id)
        .bind(step_id)
        .bind(i as i64)
        .bind(title)
        .bind(&now)
        .bind(&ctx.goal_id)
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

// --- Check-in pass (complete + add) -------------------------------------------

const NOTE_PASS_SYSTEM: &str = "You process a user's check-in note about ONE milestone of a larger goal \
and do TWO things, returning a single JSON object {\"done\":[...],\"add\":[...]}.\n\
1) done: the NUMBERS of the listed pending action items the note CLEARLY says are COMPLETED. Be \
CONSERVATIVE — include an item only when the note unambiguously indicates it is done; never guess, and \
never include an item the note merely relates to. Use [] when nothing clearly matches.\n\
2) add: NEW actionable items the note implies the user still needs to DO, BUY, MAKE or FIX and that are \
not already in the list. Each has a short imperative \"title\" and a \"category\". Example: note \"I have \
no bottle holder and need spare parts for patching a tire\" -> add [{\"title\":\"Buy a bottle cage/holder\",\
\"category\":\"gear\"},{\"title\":\"Get a tire patch kit + spare tube\",\"category\":\"gear\"}]. Choose \
category \"training\" for workouts/rides/practice, otherwise \"gear\" (equipment to buy/fix), \"logistics\" \
(bookings/travel/registration), \"prep\" (preparation) or \"other\". Be conservative — do NOT invent items \
the note does not imply. Use [] when the note implies no new tasks.\n\
Respond ONLY with the JSON object, e.g. {\"done\":[1],\"add\":[{\"title\":\"...\",\"category\":\"gear\"}]}.";

/// Build the check-in pass prompt from the note and the pending training items.
fn note_pass_prompt(note: &str, pending: &[TodoView]) -> String {
    let mut out = format!(
        "Check-in note: {note}\n\nPending training action items for the current milestone:\n"
    );
    if pending.is_empty() {
        out.push_str("(none)\n");
    } else {
        for (i, todo) in pending.iter().enumerate() {
            out.push_str(&format!("{}. {}\n", i + 1, todo.title));
        }
    }
    out.push_str(
        "\nWhich items does the note complete (done), and what new tasks does it imply (add)?",
    );
    out
}

/// Normalize a raw category to the known set, case-insensitively. Anything
/// unknown, empty or absent on an added item defaults to `other`.
fn normalize_category(raw: Option<&str>) -> String {
    let lowered = raw.map(|c| c.trim().to_ascii_lowercase());
    match lowered.as_deref() {
        Some(c) if VALID_CATEGORIES.contains(&c) => c.to_string(),
        _ => "other".to_string(),
    }
}

/// Pure: turn the LLM's raw `add` items into the concrete `(title, category)`
/// pairs to insert. Titles are trimmed and empties dropped; categories are
/// normalized via [`normalize_category`]. An item whose title duplicates (case-
/// insensitively) an existing goal todo — or an earlier item in this same batch —
/// is skipped. Order is preserved.
fn select_additions(add: &[AddTodo], existing_titles: &[String]) -> Vec<(String, String)> {
    let mut seen: Vec<String> = existing_titles
        .iter()
        .map(|t| t.trim().to_ascii_lowercase())
        .collect();
    let mut out: Vec<(String, String)> = Vec::new();
    for item in add {
        let title = item.title.trim().to_string();
        if title.is_empty() {
            continue;
        }
        let key = title.to_ascii_lowercase();
        if seen.iter().any(|s| s == &key) {
            continue;
        }
        seen.push(key);
        out.push((title, normalize_category(item.category.as_deref())));
    }
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

/// Best-effort: process a check-in note against a goal's CURRENT milestone — both
/// marking pending training items done AND adding new items the note implies.
/// Returns the completed and added items (both empty in the common case). This
/// NEVER fails the caller: any error degrades to two empty lists.
///
/// The "current milestone" is the first roadmap step with status != `done` by
/// `ord`. Its pending (`done = 0`) TRAINING items are offered for completion
/// matching, and only when the note is non-trivial. Exactly one LLM call is made;
/// its cost is debited best-effort. Added training items are filed under that
/// milestone; non-training items (gear/logistics/prep/other) at the goal level.
/// Called from the check-in handler.
pub async fn process_todos_from_note(
    llm: &LlmClient,
    pool: &SqlitePool,
    config: &Config,
    user_id: &str,
    goal_id: &str,
    note: Option<&str>,
) -> CheckinTodoResult {
    match try_process_todos_from_note(llm, pool, config, user_id, goal_id, note).await {
        Ok(result) => result,
        Err(e) => {
            tracing::warn!(error = ?e, goal_id, "process todos from note failed (ignored)");
            CheckinTodoResult::default()
        }
    }
}

/// Fallible inner body of [`process_todos_from_note`]; all errors are swallowed by
/// the public wrapper.
async fn try_process_todos_from_note(
    llm: &LlmClient,
    pool: &SqlitePool,
    config: &Config,
    user_id: &str,
    goal_id: &str,
    note: Option<&str>,
) -> AppResult<CheckinTodoResult> {
    let note = match note.map(str::trim) {
        Some(n) if n.len() >= MIN_NOTE_LEN => n,
        _ => return Ok(CheckinTodoResult::default()),
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
        None => return Ok(CheckinTodoResult::default()),
    };

    // Pending TRAINING items of that milestone, offered for completion matching.
    // (We still proceed when there are none, since the note may imply NEW items.)
    let pending: Vec<TodoView> = sqlx::query_as::<_, TodoView>(&format!(
        "SELECT {TODO_COLUMNS} FROM step_todos \
         WHERE step_id = ? AND done = 0 AND COALESCE(category,'training') = 'training' \
         ORDER BY ord ASC, created_at ASC, id ASC"
    ))
    .bind(&step_id)
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    let model = crate::settings::user_chat_model(pool, config, user_id).await?;
    let user_prompt = note_pass_prompt(note, &pending);
    let (pass, cost_cents) = llm
        .chat_json::<NotePass>(&model, NOTE_PASS_SYSTEM, &user_prompt)
        .await?;

    // Charge best-effort; a debit failure must not lose the work.
    if cost_cents > 0 {
        if let Err(e) =
            crate::wallet::debit_tokens(pool, user_id, cost_cents, "todos-autotick").await
        {
            tracing::warn!(error = ?e, goal_id, "auto-tick debit failed (ignored)");
        }
    }

    let now = now_rfc3339()?;

    // --- Completions: mark matched pending items done. -----------------------
    let ids = select_todo_ids(&pending, &pass.done);
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

    // --- Additions: insert new items, deduped against the goal's todos. ------
    let existing_titles: Vec<String> =
        sqlx::query_as::<_, (String,)>("SELECT title FROM step_todos WHERE goal_id = ?")
            .bind(goal_id)
            .fetch_all(pool)
            .await
            .map_err(|e| AppError::Internal(e.into()))?
            .into_iter()
            .map(|(t,)| t)
            .collect();

    let additions = select_additions(&pass.add, &existing_titles);
    let mut added: Vec<TodoView> = Vec::new();
    for (title, category) in &additions {
        // `ord` continues after the max within the item's own scope: training
        // items are ordered within the milestone step, others within the goal.
        let next_ord: i64 = if category == "training" {
            let max: Option<i64> = sqlx::query_scalar(
                "SELECT MAX(ord) FROM step_todos \
                 WHERE step_id = ? AND COALESCE(category,'training') = 'training'",
            )
            .bind(&step_id)
            .fetch_one(pool)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
            max.map(|m| m + 1).unwrap_or(0)
        } else {
            let max: Option<i64> = sqlx::query_scalar(
                "SELECT MAX(ord) FROM step_todos \
                 WHERE goal_id = ? AND COALESCE(category,'training') <> 'training'",
            )
            .bind(goal_id)
            .fetch_one(pool)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
            max.map(|m| m + 1).unwrap_or(0)
        };

        // step_id is always the current milestone (satisfies the FK) regardless
        // of category; goal_id + category drive how the item is listed.
        let id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO step_todos (id, step_id, ord, title, done, created_at, done_at, goal_id, category) \
             VALUES (?, ?, ?, ?, 0, ?, NULL, ?, ?)",
        )
        .bind(&id)
        .bind(&step_id)
        .bind(next_ord)
        .bind(title)
        .bind(&now)
        .bind(goal_id)
        .bind(category)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;

        if let Some(todo) = fetch_todo(pool, &id).await? {
            added.push(todo);
        }
    }

    Ok(CheckinTodoResult { completed, added })
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

async fn list_goal_todos_handler(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(goal_id): Path<String>,
) -> AppResult<Json<Vec<TodoView>>> {
    owned_goal(&state.db, &user_id, &goal_id).await?;
    Ok(Json(fetch_goal_todos(&state.db, &goal_id).await?))
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

    /// Seed a todo with an explicit `goal_id` + `category` (what check-in adds and
    /// generation write), for exercising the step/goal scope split.
    async fn seed_todo_cat(
        pool: &SqlitePool,
        id: &str,
        step_id: &str,
        goal_id: &str,
        ord: i64,
        title: &str,
        category: &str,
    ) {
        sqlx::query(
            "INSERT INTO step_todos (id, step_id, ord, title, done, created_at, done_at, goal_id, category) \
             VALUES (?, ?, ?, ?, 0, '2026-01-01T00:00:00Z', NULL, ?, ?)",
        )
        .bind(id)
        .bind(step_id)
        .bind(ord)
        .bind(title)
        .bind(goal_id)
        .bind(category)
        .execute(pool)
        .await
        .expect("insert categorized todo");
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
            category: "training".to_string(),
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
    fn note_pass_prompt_numbers_items_from_one() {
        let pending = vec![tv("a"), tv("b")];
        let prompt = note_pass_prompt("did my long ride", &pending);
        assert!(prompt.contains("Check-in note: did my long ride"));
        assert!(prompt.contains("1. item a"));
        assert!(prompt.contains("2. item b"));

        // With no pending items the prompt still asks for additions.
        let empty = note_pass_prompt("need a new tube", &[]);
        assert!(empty.contains("(none)"));
    }

    #[test]
    fn select_additions_dedupes_and_defaults_category() {
        let existing = vec!["Buy a bottle cage".to_string()];
        let add = vec![
            // Duplicate of an existing title (case-insensitive) -> skipped.
            AddTodo {
                title: "  buy a BOTTLE cage ".to_string(),
                category: Some("gear".to_string()),
            },
            // Valid, kept; category normalized (trim + lowercase).
            AddTodo {
                title: "Get a tire patch kit".to_string(),
                category: Some(" Gear ".to_string()),
            },
            // Unknown category -> defaults to "other".
            AddTodo {
                title: "Sort out insurance".to_string(),
                category: Some("misc".to_string()),
            },
            // Missing category -> defaults to "other".
            AddTodo {
                title: "Book a hotel".to_string(),
                category: None,
            },
            // In-batch duplicate of a kept item -> skipped.
            AddTodo {
                title: "get a TIRE patch kit".to_string(),
                category: Some("gear".to_string()),
            },
            // Empty title -> dropped.
            AddTodo {
                title: "   ".to_string(),
                category: Some("prep".to_string()),
            },
            // Explicit training category passes through.
            AddTodo {
                title: "Do a hill repeats session".to_string(),
                category: Some("training".to_string()),
            },
        ];

        let out = select_additions(&add, &existing);
        assert_eq!(
            out,
            vec![
                ("Get a tire patch kit".to_string(), "gear".to_string()),
                ("Sort out insurance".to_string(), "other".to_string()),
                ("Book a hotel".to_string(), "other".to_string()),
                (
                    "Do a hill repeats session".to_string(),
                    "training".to_string()
                ),
            ]
        );

        // Empty input -> empty output.
        assert!(select_additions(&[], &existing).is_empty());
    }

    #[tokio::test]
    async fn auto_process_degrades_to_empty_without_llm() {
        let (_dir, pool) = test_pool().await;
        seed_step(&pool, "u1", "g1", "r1", "s1", "pending", 0).await;
        seed_todo(&pool, "t1", "s1", 0, "Long ride").await;

        // No LLM key in tests, so the pass errors and the helper must degrade to
        // an empty result — never fail — ticking nothing and adding nothing.
        let config = Config::from_env();
        let llm = LlmClient::new(&config);
        let result =
            process_todos_from_note(&llm, &pool, &config, "u1", "g1", Some("did my long ride"))
                .await;
        assert!(result.completed.is_empty());
        assert!(result.added.is_empty());

        let todos = fetch_todos(&pool, "s1").await.expect("fetch");
        assert!(todos.iter().all(|t| !t.done), "nothing ticked on error");
        assert_eq!(todos.len(), 1, "nothing added on error");
    }

    #[tokio::test]
    async fn auto_process_skips_trivial_notes() {
        let (_dir, pool) = test_pool().await;
        seed_step(&pool, "u1", "g1", "r1", "s1", "pending", 0).await;
        seed_todo(&pool, "t1", "s1", 0, "Long ride").await;

        let config = Config::from_env();
        let llm = LlmClient::new(&config);
        // A short note is routine and never reaches the LLM path.
        let result = process_todos_from_note(&llm, &pool, &config, "u1", "g1", Some("ok")).await;
        assert!(result.completed.is_empty());
        assert!(result.added.is_empty());
    }

    #[tokio::test]
    async fn step_list_is_training_only_goal_list_is_non_training() {
        let (_dir, pool) = test_pool().await;
        seed_step(&pool, "u1", "g1", "r1", "s1", "pending", 0).await;
        // A legacy row (NULL goal_id/category) reads back as training via COALESCE.
        seed_todo(&pool, "t0", "s1", 0, "Legacy ride").await;
        // An explicit training item, plus goal-level gear/logistics items.
        seed_todo_cat(&pool, "t1", "s1", "g1", 1, "Interval session", "training").await;
        seed_todo_cat(&pool, "t2", "s1", "g1", 0, "Buy a bottle cage", "gear").await;
        seed_todo_cat(
            &pool,
            "t3",
            "s1",
            "g1",
            1,
            "Register for the event",
            "logistics",
        )
        .await;

        // Step list: only the training items, and every one tagged "training".
        let step_todos = fetch_todos(&pool, "s1").await.expect("step todos");
        let step_titles: Vec<&str> = step_todos.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(step_titles, vec!["Legacy ride", "Interval session"]);
        assert!(step_todos.iter().all(|t| t.category == "training"));

        // Goal list: only the non-training items, none tagged "training".
        let goal_todos = fetch_goal_todos(&pool, "g1").await.expect("goal todos");
        let goal_titles: Vec<&str> = goal_todos.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(
            goal_titles,
            vec!["Buy a bottle cage", "Register for the event"]
        );
        assert!(goal_todos.iter().all(|t| t.category != "training"));
    }

    #[tokio::test]
    async fn goal_todos_route_rejects_unauthorized() {
        let (_dir, pool) = test_pool().await;
        insert_user(&pool, "owner").await;
        seed_step(&pool, "owner", "g1", "r1", "s1", "pending", 0).await;

        // Another user is Forbidden on the goal.
        assert!(matches!(
            owned_goal(&pool, "u1", "g1").await,
            Err(AppError::Forbidden)
        ));
        // A missing goal is NotFound.
        assert!(matches!(
            owned_goal(&pool, "owner", "missing").await,
            Err(AppError::NotFound)
        ));
        // The owner is accepted.
        assert!(owned_goal(&pool, "owner", "g1").await.is_ok());
    }
}
