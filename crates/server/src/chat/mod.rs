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

// --- Router -------------------------------------------------------------------

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/conversations",
            get(list_conversations).post(create_conversation),
        )
        .route("/api/conversations/{id}", get(get_conversation))
        .route("/api/conversations/{id}/messages", post(post_message))
}

// --- Helpers ------------------------------------------------------------------

fn now_rfc3339() -> AppResult<String> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|e| AppError::Internal(e.into()))
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

    // 6. Start the upstream token stream. A pre-stream error (missing key / 402 /
    //    provider reject) is returned directly, before any SSE is constructed.
    let mut token_stream = state.llm.chat_stream(&system, &history).await?;

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
