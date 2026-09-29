//! Per-user settings: the user's preferred chat/roadmap model.
//!
//! Routes (all under `RequireAuth`):
//!   - `GET /api/settings` — the caller's current chat model + the deployment's
//!     allowed models (with friendly labels).
//!   - `PUT /api/settings` — set the caller's chat model (must be in the allowed
//!     list, else `BadRequest`).
//!
//! [`user_chat_model`] is the stable helper the chat + goals modules call to
//! resolve which model to send a completion to: the user's stored choice when it
//! is still allowed, otherwise the config default.
//!
//! Owns migration `0010_user_settings.sql`.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use axum_extra::extract::WithRejection;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

use crate::auth::session::RequireAuth;
use crate::config::Config;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

// --- DTOs ---------------------------------------------------------------------

/// One selectable chat model: its id plus a human-friendly label.
#[derive(Debug, Clone, Serialize)]
pub struct ModelOption {
    pub id: String,
    pub label: String,
}

/// `GET /api/settings` response.
#[derive(Debug, Serialize)]
struct SettingsView {
    chat_model: String,
    allowed_models: Vec<ModelOption>,
}

/// `PUT /api/settings` request body.
#[derive(Debug, Deserialize)]
struct UpdateSettings {
    chat_model: String,
}

/// `PUT /api/settings` response.
#[derive(Debug, Serialize)]
struct UpdatedSettings {
    chat_model: String,
}

// --- Router -------------------------------------------------------------------

pub fn router() -> Router<AppState> {
    Router::new().route("/api/settings", get(get_settings).put(put_settings))
}

// --- Helpers ------------------------------------------------------------------

fn now_rfc3339() -> AppResult<String> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|e| AppError::Internal(e.into()))
}

/// Friendly label for a known model id; falls back to the id itself.
fn label_for(id: &str) -> String {
    match id {
        "openrouter/~anthropic/claude-haiku-latest" => "Claude Haiku (fast, default)".to_string(),
        "gemma4-26b" => "Gemma 4 26B".to_string(),
        other => other.to_string(),
    }
}

/// Build the allowed-models list (id + label) from config.
fn allowed_models(config: &Config) -> Vec<ModelOption> {
    config
        .allowed_chat_models
        .iter()
        .map(|id| ModelOption {
            id: id.clone(),
            label: label_for(id),
        })
        .collect()
}

/// The caller's stored chat model, or `None` when unset.
async fn stored_chat_model(pool: &SqlitePool, user_id: &str) -> AppResult<Option<String>> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT chat_model FROM user_settings WHERE user_id = ?")
            .bind(user_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
    Ok(row.and_then(|(model,)| model))
}

/// Resolve the chat model to use for `user_id`: their stored choice when it is
/// still in the allowed list, otherwise the config default. Stable helper called
/// by the chat + goals modules.
pub async fn user_chat_model(
    pool: &SqlitePool,
    config: &Config,
    user_id: &str,
) -> AppResult<String> {
    let stored = stored_chat_model(pool, user_id).await?;
    let resolved = match stored {
        Some(model) if config.allowed_chat_models.iter().any(|a| a == &model) => model,
        _ => config.chat_model.clone(),
    };
    Ok(resolved)
}

/// Upsert the caller's chat model choice.
async fn upsert_chat_model(pool: &SqlitePool, user_id: &str, chat_model: &str) -> AppResult<()> {
    let now = now_rfc3339()?;
    sqlx::query(
        "INSERT INTO user_settings (user_id, chat_model, updated_at) \
         VALUES (?, ?, ?) \
         ON CONFLICT(user_id) DO UPDATE SET \
             chat_model = excluded.chat_model, \
             updated_at = excluded.updated_at",
    )
    .bind(user_id)
    .bind(chat_model)
    .bind(&now)
    .execute(pool)
    .await
    .map_err(|e| AppError::Internal(e.into()))?;
    Ok(())
}

// --- Handlers -----------------------------------------------------------------

async fn get_settings(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
) -> AppResult<Json<SettingsView>> {
    let chat_model = user_chat_model(&state.db, &state.config, &user_id).await?;
    Ok(Json(SettingsView {
        chat_model,
        allowed_models: allowed_models(&state.config),
    }))
}

async fn put_settings(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
    WithRejection(Json(body), _): WithRejection<Json<UpdateSettings>, AppError>,
) -> AppResult<Json<UpdatedSettings>> {
    let chat_model = body.chat_model.trim().to_string();
    if !state
        .config
        .allowed_chat_models
        .iter()
        .any(|a| a == &chat_model)
    {
        return Err(AppError::BadRequest(format!(
            "chat_model not allowed: {chat_model}"
        )));
    }

    upsert_chat_model(&state.db, &user_id, &chat_model).await?;
    Ok(Json(UpdatedSettings { chat_model }))
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
    async fn user_chat_model_defaults_then_reflects_choice() {
        let (_dir, pool) = test_pool().await;
        let config = Config::from_env();

        // Unset → config default.
        let model = user_chat_model(&pool, &config, "u1")
            .await
            .expect("default");
        assert_eq!(model, config.chat_model);

        // Store an allowed choice → it is returned.
        upsert_chat_model(&pool, "u1", "gemma4-26b")
            .await
            .expect("upsert");
        let model = user_chat_model(&pool, &config, "u1").await.expect("stored");
        assert_eq!(model, "gemma4-26b");
    }

    #[tokio::test]
    async fn stored_but_disallowed_model_falls_back_to_default() {
        let (_dir, pool) = test_pool().await;
        let config = Config::from_env();

        // Simulate a model that was allowed once but is no longer in the list.
        upsert_chat_model(&pool, "u1", "since-removed-model")
            .await
            .expect("upsert");
        let model = user_chat_model(&pool, &config, "u1")
            .await
            .expect("fallback");
        assert_eq!(model, config.chat_model);
    }

    #[tokio::test]
    async fn allowed_models_carry_friendly_labels() {
        let config = Config::from_env();
        let options = allowed_models(&config);
        let haiku = options
            .iter()
            .find(|o| o.id == "openrouter/~anthropic/claude-haiku-latest")
            .expect("haiku present");
        assert_eq!(haiku.label, "Claude Haiku (fast, default)");
        // Unknown id falls back to the id as its own label.
        assert_eq!(label_for("mystery-model"), "mystery-model");
    }

    #[tokio::test]
    async fn upsert_overwrites_previous_choice() {
        let (_dir, pool) = test_pool().await;

        upsert_chat_model(&pool, "u1", "gemma4-26b")
            .await
            .expect("first");
        upsert_chat_model(&pool, "u1", "openrouter/~anthropic/claude-haiku-latest")
            .await
            .expect("second");

        let stored = stored_chat_model(&pool, "u1").await.expect("stored");
        assert_eq!(
            stored.as_deref(),
            Some("openrouter/~anthropic/claude-haiku-latest")
        );
    }
}
