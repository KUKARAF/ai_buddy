//! Conversations + messages: the goal-setting chat, with RAG + streaming.
//!
//! Routes (all under `RequireAuth`):
//!   - `POST /api/conversations`               create a conversation
//!   - `GET  /api/conversations`               list the caller's conversations
//!   - `GET  /api/conversations/{id}`          conversation + its messages
//!   - `POST /api/conversations/{id}/messages` a streaming (SSE) coach turn
//!
//! The message turn: verify ownership, gate on wallet balance (402 when empty),
//! persist the user message, RAG-retrieve memory, then stream the assistant reply
//! over SSE. When the stream finishes the spawned task persists the assistant
//! message, debits the wallet by the reported cost, embeds it for RAG, and emits a
//! terminal `done` SSE event.
//!
//! Owns migration `0004_chat.sql`.

use std::convert::Infallible;

use axum::extract::{Path, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::WithRejection;
use futures::channel::mpsc;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::session::RequireAuth;
use crate::error::{AppError, AppResult};
use crate::llm::{ChatDelta, ChatMsg};
use crate::state::AppState;

/// The accountability-coach persona driving every chat turn.
const COACH_SYSTEM_PROMPT: &str = "You are AI Buddy, a warm but firm accountability coach. \
Your job is to help the user define ONE concrete goal, a clear deadline, and their current \
starting point / progress. Ask focused questions, ideally one at a time, and reflect back what \
you hear. Once the goal, deadline, and current progress are clear, suggest turning it into a \
concrete step-by-step roadmap and gently propose a small symbolic pledge to strengthen their \
commitment. Keep replies concise, specific, and encouraging.";

/// The agentic coach persona for the tool-calling `/agent` turn. The concrete
/// date is injected at call time (see [`agent_system_prompt`]).
const AGENT_SYSTEM_PROMPT_BODY: &str = "You are AI Buddy, a warm, concise accountability coach. \
Turn the user's rambling intention into ONE concrete goal with a realistic deadline and a \
motivating plan of 3 to 6 chronological milestones building toward it. INFER as much as you can \
from what the user says rather than interrogating them: turn vague timing like \"next spring\" \
into a concrete YYYY-MM-DD date using today's date, and infer the location, category, and success \
criterion where you reasonably can. For training, skill, or endurance goals, ask at most 1 to 2 \
high-value clarifying questions ONLY when you genuinely cannot infer them (e.g. their current \
baseline/fitness, or how many hours per week they have) rather than guessing; otherwise do not \
interrogate. Give EACH milestone its OWN realistic effort (e.g. '4-5 h ride', '3 x 45 min \
interval sessions', '20 min/day') — never one goal-wide number repeated across every milestone — \
and its OWN due_date, spanning today through the deadline, chronologically spaced, with the last \
milestone at or just before the deadline. For endurance or skill goals, apply sound planning \
principles where relevant: progressive overload (gradually increase the key stressor over the \
milestones), a recovery/rest cadence, and a TAPER before the event; and for a MULTI-DAY event, \
include an explicit back-to-back rehearsal milestone (e.g. two long days in a row). Keep this \
guidance goal-agnostic — for a simple goal, skip the training-specific structure. When you have a \
clear picture, PROPOSE the goal and its milestones in prose (optionally a small symbolic euro \
pledge too) and ask the user to confirm. Call the save_goal tool ONLY AFTER the user has \
explicitly confirmed the proposed goal and plan in the conversation — never before. After saving, \
briefly celebrate and tell the user their plan is ready. Keep every reply short and encouraging.";

/// The categories the agent may assign to a goal. Anything else is clamped to
/// `Other` during sanitization.
const ALLOWED_CATEGORIES: [&str; 6] = [
    "Creative", "Learning", "Movement", "Health", "Career", "Other",
];

// --- DTOs ---------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ConversationView {
    pub id: String,
    pub user_id: String,
    pub goal_id: Option<String>,
    pub title: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct MessageView {
    pub id: String,
    pub conversation_id: String,
    pub role: String,
    pub content: String,
    pub token_cost_cents: i64,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
struct ConversationDetail {
    conversation: ConversationView,
    messages: Vec<MessageView>,
}

#[derive(Debug, Deserialize)]
struct CreateConversation {
    #[serde(default)]
    goal_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PostMessage {
    content: String,
}

/// The response of one agentic `/agent` turn: the assistant's prose reply plus
/// the id of any goal the agent created during the turn (`null` otherwise).
#[derive(Debug, Serialize)]
struct AgentReply {
    reply: String,
    created_goal_id: Option<String>,
}

// --- Router -------------------------------------------------------------------

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/conversations",
            get(list_conversations).post(create_conversation),
        )
        .route("/api/conversations/{id}", get(get_conversation))
        .route("/api/conversations/{id}/messages", post(post_message))
        .route("/api/conversations/{id}/agent", post(agent_turn))
}

// --- Helpers ------------------------------------------------------------------

fn now_rfc3339() -> AppResult<String> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|e| AppError::Internal(e.into()))
}

/// Today's date as `YYYY-MM-DD` (UTC), for grounding the agent's relative-date
/// reasoning.
fn today_ymd() -> AppResult<String> {
    let ts = now_rfc3339()?;
    ts.get(..10)
        .map(str::to_string)
        .ok_or_else(|| AppError::Internal(anyhow::anyhow!("could not derive today's date")))
}

/// The agent system prompt with today's date injected.
fn agent_system_prompt(today: &str) -> String {
    format!("Today is {today}.\n{AGENT_SYSTEM_PROMPT_BODY}")
}

/// Is `s` a well-formed `YYYY-MM-DD` calendar date (with plausible month/day)?
/// Used to drop dates the model hallucinated in a bad format.
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

/// Clamp a free-text category to the allowed set (case-insensitively),
/// defaulting anything unrecognized to `Other`.
fn canonical_category(raw: &str) -> String {
    let trimmed = raw.trim();
    ALLOWED_CATEGORIES
        .iter()
        .find(|c| c.eq_ignore_ascii_case(trimmed))
        .map(|c| (*c).to_string())
        .unwrap_or_else(|| "Other".to_string())
}

/// Sanitize the model's `save_goal` arguments before persisting: clamp the
/// category to the allowed set, drop malformed dates, and reject non-positive
/// time/pledge integers. Pure (unit-tested).
fn sanitize_new_goal(mut draft: crate::goals::NewGoalWithPlan) -> crate::goals::NewGoalWithPlan {
    draft.category = draft.category.map(|c| canonical_category(&c));
    draft.deadline = draft.deadline.filter(|d| valid_ymd(d));
    draft.time_per_session_min = draft.time_per_session_min.filter(|&m| m > 0);
    draft.pledge_cents = draft.pledge_cents.filter(|&c| c > 0);
    for milestone in &mut draft.milestones {
        if let Some(due) = &milestone.due_date {
            if !valid_ymd(due) {
                milestone.due_date = None;
            }
        }
        milestone.effort = milestone
            .effort
            .take()
            .map(|e| e.trim().to_string())
            .filter(|e| !e.is_empty());
    }
    draft
}

/// System prompt for the plan-then-critique pass (P5). The current draft is
/// supplied as JSON in the user message; the model returns corrected JSON of the
/// same shape.
const CRITIQUE_SYSTEM_PROMPT: &str = "You are an expert planning critic. You return ONLY a single \
JSON object, no prose.";

/// Build the critique user message: instruct the model to fix implausible
/// milestone efforts/dates and return corrected JSON of the same shape.
fn critique_prompt(today: &str, deadline: &str, draft_json: &str) -> String {
    format!(
        "Review this training/goal plan for REALISM. Fix any milestone whose effort or date is \
implausible or contradicts the goal (e.g. a 100 km ride marked '90 min'). Ensure every milestone \
has a realistic per-milestone effort and a due_date between {today} and {deadline}, chronologically \
ordered. Return corrected JSON of the EXACT shape \
{{\"title\":...,\"description\":...,\"deadline\":\"YYYY-MM-DD\",\"category\":...,\"location\":...,\
\"skill_level\":...,\"time_per_session_min\":...,\"success_criterion\":...,\
\"milestones\":[{{\"title\":...,\"detail\":...,\"due_date\":\"YYYY-MM-DD\",\"effort\":...}}],\
\"pledge_cents\":...}}. Here is the current draft:\n{draft_json}"
    )
}

/// P5 — one bounded critique/refinement pass over a parsed+sanitized plan before
/// persisting. Makes a single `chat_json` call; on success the corrected plan is
/// sanitized and returned along with the call's cost. On ANY error (serialize,
/// network, parse) it falls back to the original plan with zero added cost — the
/// request never fails because of the critique.
async fn refine_plan(
    state: &AppState,
    model: &str,
    today: &str,
    user_id: &str,
    original: crate::goals::NewGoalWithPlan,
) -> (crate::goals::NewGoalWithPlan, i64) {
    let draft_json = match serde_json::to_string(&original) {
        Ok(json) => json,
        Err(e) => {
            tracing::warn!(error = ?e, "refine_plan: serializing draft failed; using original");
            return (original, 0);
        }
    };
    let deadline = original
        .deadline
        .clone()
        .unwrap_or_else(|| "the deadline".to_string());
    let mut user_prompt = critique_prompt(today, &deadline, &draft_json);
    user_prompt.push_str(
        &crate::goals::holiday_prompt_suffix(
            &state.db,
            user_id,
            today,
            original.deadline.as_deref(),
        )
        .await,
    );

    match state
        .llm
        .chat_json::<crate::goals::NewGoalWithPlan>(model, CRITIQUE_SYSTEM_PROMPT, &user_prompt)
        .await
    {
        Ok((refined, cost)) => {
            let refined = sanitize_new_goal(refined);
            // Guard against a degenerate refinement that dropped all milestones.
            if refined.title.trim().is_empty() || refined.milestones.is_empty() {
                tracing::warn!("refine_plan: refinement was degenerate; using original");
                (original, cost)
            } else {
                (refined, cost)
            }
        }
        Err(e) => {
            tracing::warn!(error = ?e, "refine_plan: critique call failed; using original");
            (original, 0)
        }
    }
}

/// The single `save_goal` tool exposed to the agent, as an OpenAI `tools` array.
fn save_goal_tools() -> serde_json::Value {
    serde_json::json!([{
        "type": "function",
        "function": {
            "name": "save_goal",
            "description": "Create the user's goal and milestone roadmap. ONLY call this AFTER \
    the user has explicitly confirmed the proposed goal and plan in the conversation.",
            "parameters": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "A short, concrete goal title." },
                    "description": { "type": "string", "description": "Optional fuller description of the goal." },
                    "deadline": { "type": "string", "description": "Target completion date as YYYY-MM-DD." },
                    "category": {
                        "type": "string",
                        "enum": ["Creative", "Learning", "Movement", "Health", "Career", "Other"],
                        "description": "One of the allowed categories."
                    },
                    "location": { "type": "string", "description": "Where the goal happens, if relevant." },
                    "skill_level": { "type": "string", "description": "The user's current starting point / skill level." },
                    "time_per_session_min": { "type": "integer", "description": "Minutes the user can spend per session." },
                    "success_criterion": { "type": "string", "description": "How the user will know they've succeeded." },
                    "milestones": {
                        "type": "array",
                        "description": "3 to 6 chronological milestones building toward the goal.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": { "type": "string", "description": "Short milestone title." },
                                "detail": { "type": "string", "description": "Optional milestone detail." },
                                "due_date": { "type": "string", "description": "A realistic target date for THIS milestone, YYYY-MM-DD, between today and the goal deadline." },
                                "effort": { "type": "string", "description": "Realistic effort for THIS specific milestone, e.g. '4-5 h ride', '3 x 45 min interval sessions', '20 min/day'. NOT a single goal-wide number." }
                            },
                            "required": ["title"]
                        }
                    },
                    "pledge_cents": { "type": "integer", "description": "Optional symbolic pledge in euro-cents." }
                },
                "required": ["title"]
            }
        }
    }])
}

/// Fetch a conversation only if it belongs to `user_id`; `None` otherwise (the
/// caller maps that to `NotFound`, so a wrong owner is indistinguishable from a
/// missing row).
async fn owned_conversation(
    pool: &SqlitePool,
    user_id: &str,
    id: &str,
) -> AppResult<Option<ConversationView>> {
    sqlx::query_as::<_, ConversationView>(
        "SELECT id, user_id, goal_id, title, created_at, updated_at \
         FROM conversations WHERE id = ? AND user_id = ?",
    )
    .bind(id)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))
}

async fn insert_conversation(
    pool: &SqlitePool,
    user_id: &str,
    goal_id: Option<&str>,
    title: Option<&str>,
) -> AppResult<ConversationView> {
    let id = Uuid::new_v4().to_string();
    let now = now_rfc3339()?;
    sqlx::query(
        "INSERT INTO conversations (id, user_id, goal_id, title, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(goal_id)
    .bind(title)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    owned_conversation(pool, user_id, &id)
        .await?
        .ok_or(AppError::NotFound)
}

async fn list_messages(pool: &SqlitePool, conversation_id: &str) -> AppResult<Vec<MessageView>> {
    sqlx::query_as::<_, MessageView>(
        "SELECT id, conversation_id, role, content, token_cost_cents, created_at \
         FROM messages WHERE conversation_id = ? ORDER BY created_at ASC, id ASC",
    )
    .bind(conversation_id)
    .fetch_all(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))
}

async fn insert_message(
    pool: &SqlitePool,
    id: &str,
    conversation_id: &str,
    role: &str,
    content: &str,
    cost_cents: i64,
) -> AppResult<()> {
    let now = now_rfc3339()?;
    sqlx::query(
        "INSERT INTO messages (id, conversation_id, role, content, token_cost_cents, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(conversation_id)
    .bind(role)
    .bind(content)
    .bind(cost_cents)
    .bind(&now)
    .execute(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;

    // Bump the conversation's recency for list ordering.
    sqlx::query("UPDATE conversations SET updated_at = ? WHERE id = ?")
        .bind(&now)
        .bind(conversation_id)
        .execute(pool)
        .await
        .map_err(|e| AppError::Internal(e.into()))?;
    Ok(())
}

/// Best-effort embed + upsert into the vector store; RAG failures never fail the
/// request (all errors are logged and swallowed).
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

/// Retrieve top-k RAG memory for `content` and render it as a system-prompt
/// preamble. Best-effort: any embed/search failure yields an empty block.
async fn rag_context(state: &AppState, user_id: &str, content: &str) -> String {
    let embedding = match state.llm.embed(content).await {
        Ok((emb, _cost)) => emb,
        Err(e) => {
            tracing::warn!(error = ?e, "RAG embed failed; skipping context");
            return String::new();
        }
    };
    let hits = match crate::vector::search(
        &state.db,
        user_id,
        &embedding,
        5,
        &["goal", "step", "message"],
    )
    .await
    {
        Ok(hits) => hits,
        Err(e) => {
            tracing::warn!(error = ?e, "RAG search failed; skipping context");
            return String::new();
        }
    };
    if hits.is_empty() {
        return String::new();
    }
    let mut block = String::from("Relevant context from the user's history:\n");
    for hit in &hits {
        block.push_str("- ");
        block.push_str(hit.text.trim());
        block.push('\n');
    }
    block
}

// --- Handlers -----------------------------------------------------------------

async fn create_conversation(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    WithRejection(Json(body), _): WithRejection<Json<CreateConversation>, AppError>,
) -> AppResult<Json<ConversationView>> {
    let conv = insert_conversation(
        &state.db,
        &user_id,
        body.goal_id.as_deref(),
        body.title.as_deref(),
    )
    .await?;
    Ok(Json(conv))
}

async fn list_conversations(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
) -> AppResult<Json<Vec<ConversationView>>> {
    let rows = sqlx::query_as::<_, ConversationView>(
        "SELECT id, user_id, goal_id, title, created_at, updated_at \
         FROM conversations WHERE user_id = ? ORDER BY updated_at DESC, created_at DESC LIMIT 100",
    )
    .bind(&user_id)
    .fetch_all(&state.db)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    Ok(Json(rows))
}

async fn get_conversation(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(id): Path<String>,
) -> AppResult<Json<ConversationDetail>> {
    let conversation = owned_conversation(&state.db, &user_id, &id)
        .await?
        .ok_or(AppError::NotFound)?;
    let messages = list_messages(&state.db, &id).await?;
    Ok(Json(ConversationDetail {
        conversation,
        messages,
    }))
}

async fn post_message(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(id): Path<String>,
    WithRejection(Json(body), _): WithRejection<Json<PostMessage>, AppError>,
) -> AppResult<impl axum::response::IntoResponse> {
    let content = body.content.trim().to_string();
    if content.is_empty() {
        return Err(AppError::BadRequest(
            "message content is required".to_string(),
        ));
    }

    // 1. Ownership (wrong owner or missing -> NotFound).
    owned_conversation(&state.db, &user_id, &id)
        .await?
        .ok_or(AppError::NotFound)?;

    // 2. Wallet gate.
    if crate::wallet::balance_cents(&state.db, &user_id).await? <= 0 {
        return Err(AppError::PaymentRequired(
            "Top up your wallet to keep chatting".to_string(),
        ));
    }

    // 3. Persist the user message + embed it (best-effort).
    let user_msg_id = Uuid::new_v4().to_string();
    insert_message(&state.db, &user_msg_id, &id, "user", &content, 0).await?;
    embed_upsert(&state, &user_id, "message", &user_msg_id, &content).await;

    // 4. RAG context.
    let rag = rag_context(&state, &user_id, &content).await;

    // 5. Full history (oldest first) — includes the message just inserted.
    let history: Vec<ChatMsg> = list_messages(&state.db, &id)
        .await?
        .into_iter()
        .map(|m| ChatMsg {
            role: m.role,
            content: m.content,
        })
        .collect();

    let system = if rag.is_empty() {
        COACH_SYSTEM_PROMPT.to_string()
    } else {
        format!("{rag}\n{COACH_SYSTEM_PROMPT}")
    };

    // 6. Resolve the user's chosen chat model (falls back to the config default).
    let model = crate::settings::user_chat_model(&state.db, &state.config, &user_id).await?;

    // 7. Start the upstream token stream. A pre-stream error (missing key / 402 /
    //    provider reject) is returned directly, before any SSE is constructed.
    let mut token_stream = state.llm.chat_stream(&model, &system, &history).await?;

    let (tx, rx) = mpsc::unbounded::<Result<Event, Infallible>>();
    let task_state = state.clone();
    let conv_id = id.clone();
    let owner = user_id.clone();

    tokio::spawn(async move {
        let mut accumulated = String::new();

        while let Some(delta) = token_stream.next().await {
            match delta {
                ChatDelta::Token(token) => {
                    accumulated.push_str(&token);
                    if tx.unbounded_send(Ok(Event::default().data(token))).is_err() {
                        // Client hung up; stop forwarding.
                        return;
                    }
                }
                ChatDelta::Done { cost_cents } => {
                    let assistant_id = Uuid::new_v4().to_string();
                    if let Err(e) = insert_message(
                        &task_state.db,
                        &assistant_id,
                        &conv_id,
                        "assistant",
                        &accumulated,
                        cost_cents,
                    )
                    .await
                    {
                        tracing::error!(error = ?e, "persisting assistant message failed");
                    }
                    if cost_cents > 0 {
                        if let Err(e) =
                            crate::wallet::debit_tokens(&task_state.db, &owner, cost_cents, "chat")
                                .await
                        {
                            tracing::error!(error = ?e, "wallet debit failed");
                        }
                    }
                    embed_upsert(&task_state, &owner, "message", &assistant_id, &accumulated).await;

                    let done = match Event::default()
                        .event("done")
                        .json_data(serde_json::json!({ "cost_cents": cost_cents }))
                    {
                        Ok(ev) => ev,
                        Err(_) => Event::default()
                            .event("done")
                            .data(format!("{{\"cost_cents\":{cost_cents}}}")),
                    };
                    let _ = tx.unbounded_send(Ok(done));
                    return;
                }
                ChatDelta::Error(message) => {
                    let _ = tx.unbounded_send(Ok(Event::default().event("error").data(message)));
                    return;
                }
            }
        }
        // Stream ended without an explicit Done — close gracefully.
    });

    Ok(Sse::new(rx).keep_alive(KeepAlive::default()))
}

/// One agentic coach turn: the model converses, and — once the user has
/// confirmed — calls the `save_goal` tool to create the goal + roadmap. Returns
/// the assistant's prose reply plus any goal id created during the turn.
async fn agent_turn(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    Path(id): Path<String>,
    WithRejection(Json(body), _): WithRejection<Json<PostMessage>, AppError>,
) -> AppResult<Json<AgentReply>> {
    let content = body.content.trim().to_string();
    if content.is_empty() {
        return Err(AppError::BadRequest(
            "message content is required".to_string(),
        ));
    }

    // a. Ownership (wrong owner or missing -> NotFound).
    owned_conversation(&state.db, &user_id, &id)
        .await?
        .ok_or(AppError::NotFound)?;

    // b. Wallet gate.
    if crate::wallet::balance_cents(&state.db, &user_id).await? <= 0 {
        return Err(AppError::PaymentRequired(
            "Top up your wallet to keep chatting".to_string(),
        ));
    }

    // c. Persist the user message + embed it (best-effort).
    let user_msg_id = Uuid::new_v4().to_string();
    insert_message(&state.db, &user_msg_id, &id, "user", &content, 0).await?;
    embed_upsert(&state, &user_id, "message", &user_msg_id, &content).await;

    // d. Build the OpenAI messages array: system prompt + user/assistant history
    //    (which already includes the message just inserted).
    let today = today_ymd()?;
    let mut messages: Vec<serde_json::Value> =
        vec![serde_json::json!({ "role": "system", "content": agent_system_prompt(&today) })];
    for m in list_messages(&state.db, &id).await? {
        if m.role == "user" || m.role == "assistant" {
            messages.push(serde_json::json!({ "role": m.role, "content": m.content }));
        }
    }

    // e. The single tool the agent may call.
    let tools = save_goal_tools();

    // f. Resolve the model, then run the bounded tool-calling loop.
    let model = crate::settings::user_chat_model(&state.db, &state.config, &user_id).await?;

    let mut total_cost: i64 = 0;
    let mut created_goal_id: Option<String> = None;
    let mut reply: Option<String> = None;

    for _ in 0..4 {
        let turn = state
            .llm
            .chat_tools(&model, messages.clone(), &tools)
            .await?;
        total_cost = total_cost.saturating_add(turn.cost_cents);

        // Remember any prose as the (possibly final) reply.
        if let Some(text) = &turn.content {
            if !text.trim().is_empty() {
                reply = Some(text.clone());
            }
        }

        // No tool calls -> this is the final assistant reply.
        if turn.tool_calls.is_empty() {
            break;
        }

        // Append the assistant message carrying the tool calls, then a tool
        // result for each, so the model can produce a natural follow-up reply.
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
            let result = if tc.name == "save_goal" {
                match serde_json::from_str::<crate::goals::NewGoalWithPlan>(&tc.arguments) {
                    Ok(draft) => {
                        let draft = sanitize_new_goal(draft);
                        // P5: one bounded critique/refinement pass before persisting.
                        let (draft, refine_cost) =
                            refine_plan(&state, &model, &today, &user_id, draft).await;
                        total_cost = total_cost.saturating_add(refine_cost);
                        match crate::goals::create_goal_with_milestones(&state, &user_id, draft)
                            .await
                        {
                            Ok(goal_id) => {
                                created_goal_id = Some(goal_id.clone());
                                serde_json::json!({ "ok": true, "goal_id": goal_id })
                            }
                            Err(e) => {
                                tracing::warn!(error = ?e, "save_goal creation failed");
                                serde_json::json!({ "ok": false, "error": "could not create goal" })
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = ?e, "save_goal arguments were invalid");
                        serde_json::json!({ "ok": false, "error": "invalid arguments" })
                    }
                }
            } else {
                serde_json::json!({ "ok": false, "error": "unknown tool" })
            };
            messages.push(serde_json::json!({
                "role": "tool",
                "tool_call_id": tc.id,
                "content": result.to_string(),
            }));
        }
    }

    let reply =
        reply.unwrap_or_else(|| "Your goal and plan are ready — let's get started!".to_string());

    // g. Debit the wallet (best-effort), persist + embed the assistant reply.
    if total_cost > 0 {
        if let Err(e) = crate::wallet::debit_tokens(&state.db, &user_id, total_cost, "chat").await {
            tracing::error!(error = ?e, "wallet debit failed");
        }
    }
    let assistant_id = Uuid::new_v4().to_string();
    insert_message(
        &state.db,
        &assistant_id,
        &id,
        "assistant",
        &reply,
        total_cost,
    )
    .await?;
    embed_upsert(&state, &user_id, "message", &assistant_id, &reply).await;

    Ok(Json(AgentReply {
        reply,
        created_goal_id,
    }))
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
    fn valid_ymd_accepts_and_rejects() {
        assert!(valid_ymd("2026-09-29"));
        assert!(valid_ymd("  2026-12-01  "));
        assert!(!valid_ymd("next spring"));
        assert!(!valid_ymd("2026/09/29"));
        assert!(!valid_ymd("2026-13-01")); // bad month
        assert!(!valid_ymd("2026-09-40")); // bad day
        assert!(!valid_ymd("26-9-1")); // wrong widths
    }

    #[test]
    fn canonical_category_clamps() {
        assert_eq!(canonical_category("learning"), "Learning");
        assert_eq!(canonical_category("  Health "), "Health");
        assert_eq!(canonical_category("nonsense"), "Other");
        assert_eq!(canonical_category(""), "Other");
    }

    #[test]
    fn sanitize_new_goal_cleans_fields() {
        let draft = crate::goals::NewGoalWithPlan {
            title: "Learn to surf".to_string(),
            description: None,
            deadline: Some("someday".to_string()),
            category: Some("sports".to_string()),
            location: None,
            skill_level: None,
            time_per_session_min: Some(-30),
            success_criterion: None,
            milestones: vec![
                crate::goals::NewMilestone {
                    title: "Book lessons".to_string(),
                    detail: None,
                    due_date: Some("2026-10-01".to_string()),
                    effort: Some("  90 min lesson  ".to_string()),
                },
                crate::goals::NewMilestone {
                    title: "Practice".to_string(),
                    detail: None,
                    due_date: Some("not a date".to_string()),
                    effort: Some("   ".to_string()),
                },
            ],
            pledge_cents: Some(-5),
        };

        let clean = sanitize_new_goal(draft);
        // Bad category clamps to Other.
        assert_eq!(clean.category.as_deref(), Some("Other"));
        // Bad deadline dropped.
        assert_eq!(clean.deadline, None);
        // Negative ints dropped.
        assert_eq!(clean.time_per_session_min, None);
        assert_eq!(clean.pledge_cents, None);
        // Good milestone date kept, bad one dropped.
        assert_eq!(
            clean.milestones.first().and_then(|m| m.due_date.as_deref()),
            Some("2026-10-01")
        );
        assert_eq!(
            clean.milestones.get(1).and_then(|m| m.due_date.as_deref()),
            None
        );
        // Effort is trimmed; whitespace-only effort becomes None.
        assert_eq!(
            clean.milestones.first().and_then(|m| m.effort.as_deref()),
            Some("90 min lesson")
        );
        assert_eq!(
            clean.milestones.get(1).and_then(|m| m.effort.as_deref()),
            None
        );
    }

    #[tokio::test]
    async fn create_and_fetch_conversation() {
        let (_dir, pool) = test_pool().await;
        let conv = insert_conversation(&pool, "u1", None, Some("My goal chat"))
            .await
            .expect("insert");
        assert_eq!(conv.title.as_deref(), Some("My goal chat"));

        let fetched = owned_conversation(&pool, "u1", &conv.id)
            .await
            .expect("fetch")
            .expect("present");
        assert_eq!(fetched.id, conv.id);
    }

    #[tokio::test]
    async fn conversation_ownership_is_scoped() {
        let (_dir, pool) = test_pool().await;
        let conv = insert_conversation(&pool, "owner", None, None)
            .await
            .expect("insert");

        // Another user cannot see it.
        let other = owned_conversation(&pool, "intruder", &conv.id)
            .await
            .expect("query");
        assert!(other.is_none());
    }

    #[tokio::test]
    async fn messages_roundtrip_in_order() {
        let (_dir, pool) = test_pool().await;
        let conv = insert_conversation(&pool, "u1", None, None)
            .await
            .expect("insert");

        insert_message(&pool, "m1", &conv.id, "user", "hello", 0)
            .await
            .expect("user msg");
        insert_message(&pool, "m2", &conv.id, "assistant", "hi there", 3)
            .await
            .expect("assistant msg");

        let msgs = list_messages(&pool, &conv.id).await.expect("list");
        let roles: Vec<&str> = msgs.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, vec!["user", "assistant"]);
        assert_eq!(msgs.get(1).map(|m| m.token_cost_cents), Some(3));
    }
}
