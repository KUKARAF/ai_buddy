//! Per-user settings: the user's preferred chat/roadmap model.
//!
//! Routes (all under `RequireAuth`):
//!   - `GET /api/settings` — the caller's current chat model + the deployment's
//!     allowed models (with friendly labels).
//!   - `PUT /api/settings` — set the caller's chat model (must be in the allowed
//!     list, else `BadRequest`).
//!
//!   - `GET /api/settings/suggested-country` — an OPTIONAL, offline IP->country
//!     suggestion for pre-filling the country picker (`None` when geoip is not
//!     configured or the IP can't be resolved).
//!
//! [`user_chat_model`] and [`user_country`] are the stable helpers the chat +
//! goals modules call to resolve which model to send a completion to (the user's
//! stored choice when still allowed, otherwise the config default) and the
//! user's country for holiday-aware planning.
//!
//! Owns migrations `0010_user_settings.sql` and `0014_user_country.sql`.

use axum::extract::State;
use axum::http::HeaderMap;
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

// The geoip module's file lives at `crates/server/src/geoip/mod.rs` (per the
// project layout) but `main.rs` does not declare it, so it is attached here via
// an explicit path. Referenced below simply as `geoip::`.
#[path = "../geoip/mod.rs"]
pub mod geoip;

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
    /// Stored ISO-3166-1 alpha-2 country (uppercased), or `null` when unset.
    country: Option<String>,
    allowed_models: Vec<ModelOption>,
}

/// `PUT /api/settings` request body.
///
/// `chat_model` is required and behaves exactly as before. `country` is
/// OPTIONAL and tri-state:
///   - field absent    => leave the stored country unchanged
///   - explicit `null` => clear the stored country
///   - a 2-letter code => validate + uppercase, then store it
#[derive(Debug, Deserialize)]
struct UpdateSettings {
    chat_model: String,
    #[serde(default, deserialize_with = "deserialize_present_country")]
    country: Option<Option<String>>,
}

/// `PUT /api/settings` response. Reflects the resulting stored state.
#[derive(Debug, Serialize)]
struct UpdatedSettings {
    chat_model: String,
    country: Option<String>,
}

/// `GET /api/settings/suggested-country` response.
#[derive(Debug, Serialize)]
struct SuggestedCountry {
    country: Option<String>,
}

/// Deserialize the `country` field such that "present" (even `null`) is
/// distinguishable from "absent". With `#[serde(default)]`, an absent field
/// yields the outer `None`; a present field yields `Some(inner)` where `inner`
/// is `None` for JSON `null` or `Some(code)` otherwise.
fn deserialize_present_country<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::<String>::deserialize(deserializer)?))
}

// --- Router -------------------------------------------------------------------

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/settings", get(get_settings).put(put_settings))
        .route(
            "/api/settings/suggested-country",
            get(get_suggested_country),
        )
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

/// Validate and normalize a country code: exactly 2 ASCII letters, returned
/// uppercased. Rejects anything else with [`AppError::BadRequest`].
fn normalize_country(raw: &str) -> AppResult<String> {
    let trimmed = raw.trim();
    if trimmed.len() == 2 && trimmed.bytes().all(|b| b.is_ascii_alphabetic()) {
        Ok(trimmed.to_ascii_uppercase())
    } else {
        Err(AppError::BadRequest(format!(
            "country must be an ISO-3166-1 alpha-2 code (exactly 2 letters), got: {raw:?}"
        )))
    }
}

/// The caller's stored country (ISO-3166-1 alpha-2, uppercased), or `None` when
/// unset. Stable helper the goals + chat modules call for holiday-aware
/// planning. Mirrors [`user_chat_model`]'s style.
pub async fn user_country(pool: &SqlitePool, user_id: &str) -> AppResult<Option<String>> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT country FROM user_settings WHERE user_id = ?")
            .bind(user_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
    Ok(row
        .and_then(|(country,)| country)
        .map(|c| c.trim().to_ascii_uppercase())
        .filter(|c| !c.is_empty()))
}

/// Upsert the caller's chat model choice. Only the combined
/// [`upsert_settings`] path is used in production now; this narrower helper is
/// retained for the model-resolution tests.
#[cfg(test)]
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

/// Upsert both `chat_model` (always set) and, when requested, `country`.
///
/// `country` is tri-state so one column is never clobbered while writing the
/// other:
///   - `None`         => leave the existing `country` untouched
///   - `Some(None)`   => clear `country` to NULL
///   - `Some(Some(c))`=> set `country` to `c` (expected pre-normalized)
async fn upsert_settings(
    pool: &SqlitePool,
    user_id: &str,
    chat_model: &str,
    country: Option<Option<String>>,
) -> AppResult<()> {
    let now = now_rfc3339()?;
    match country {
        // chat_model only — preserve any existing country on conflict.
        None => {
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
        }
        // chat_model + country (country binds to NULL when inner is None).
        Some(country) => {
            sqlx::query(
                "INSERT INTO user_settings (user_id, chat_model, country, updated_at) \
                 VALUES (?, ?, ?, ?) \
                 ON CONFLICT(user_id) DO UPDATE SET \
                     chat_model = excluded.chat_model, \
                     country = excluded.country, \
                     updated_at = excluded.updated_at",
            )
            .bind(user_id)
            .bind(chat_model)
            .bind(country)
            .bind(&now)
            .execute(pool)
            .await
            .map_err(|e| AppError::Internal(e.into()))?;
        }
    }
    Ok(())
}

// --- Handlers -----------------------------------------------------------------

async fn get_settings(
    State(state): State<AppState>,
    RequireAuth(user_id): RequireAuth,
) -> AppResult<Json<SettingsView>> {
    let chat_model = user_chat_model(&state.db, &state.config, &user_id).await?;
    let country = user_country(&state.db, &user_id).await?;
    Ok(Json(SettingsView {
        chat_model,
        country,
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

    // Validate + normalize country when a (non-null) value was provided.
    let country = match body.country {
        None => None,
        Some(None) => Some(None),
        Some(Some(raw)) => Some(Some(normalize_country(&raw)?)),
    };

    upsert_settings(&state.db, &user_id, &chat_model, country).await?;
    let country = user_country(&state.db, &user_id).await?;
    Ok(Json(UpdatedSettings {
        chat_model,
        country,
    }))
}

/// Offline, best-effort IP->country suggestion for pre-filling the country
/// picker. Never fails: returns `{ "country": null }` when geoip is unconfigured
/// or the client IP can't be resolved. The IP is never persisted.
async fn get_suggested_country(
    State(state): State<AppState>,
    RequireAuth(_user_id): RequireAuth,
    headers: HeaderMap,
) -> Json<SuggestedCountry> {
    let country = geoip::suggested_country(&state.config, &headers);
    Json(SuggestedCountry { country })
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

    #[test]
    fn country_validation_accepts_two_letters_and_uppercases() {
        assert_eq!(normalize_country("de").expect("valid"), "DE");
        assert_eq!(normalize_country("PL").expect("valid"), "PL");
        assert_eq!(normalize_country(" us ").expect("trimmed"), "US");
    }

    #[test]
    fn country_validation_rejects_bad_input() {
        assert!(normalize_country("D").is_err());
        assert!(normalize_country("123").is_err());
        assert!(normalize_country("deu").is_err());
        assert!(normalize_country("").is_err());
        assert!(normalize_country("d1").is_err());
    }

    #[tokio::test]
    async fn user_country_roundtrip() {
        let (_dir, pool) = test_pool().await;

        // Unset → None.
        assert_eq!(user_country(&pool, "u1").await.expect("unset"), None);

        // Set then read back (uppercased).
        upsert_settings(&pool, "u1", "gemma4-26b", Some(Some("DE".to_string())))
            .await
            .expect("set country");
        assert_eq!(
            user_country(&pool, "u1").await.expect("read"),
            Some("DE".to_string())
        );

        // Clear via Some(None) → NULL → None.
        upsert_settings(&pool, "u1", "gemma4-26b", Some(None))
            .await
            .expect("clear country");
        assert_eq!(user_country(&pool, "u1").await.expect("cleared"), None);
    }

    #[tokio::test]
    async fn setting_country_does_not_clobber_chat_model() {
        let (_dir, pool) = test_pool().await;

        // Establish a chat model.
        upsert_chat_model(&pool, "u1", "gemma4-26b")
            .await
            .expect("set model");

        // Set the country alongside the same model — model must survive.
        upsert_settings(&pool, "u1", "gemma4-26b", Some(Some("PL".to_string())))
            .await
            .expect("set country");
        assert_eq!(
            stored_chat_model(&pool, "u1")
                .await
                .expect("model kept")
                .as_deref(),
            Some("gemma4-26b")
        );
        assert_eq!(
            user_country(&pool, "u1").await.expect("country set"),
            Some("PL".to_string())
        );

        // Change the model with country ABSENT (None) — country must survive.
        upsert_settings(
            &pool,
            "u1",
            "openrouter/~anthropic/claude-haiku-latest",
            None,
        )
        .await
        .expect("change model");
        assert_eq!(
            user_country(&pool, "u1").await.expect("country kept"),
            Some("PL".to_string())
        );
        assert_eq!(
            stored_chat_model(&pool, "u1")
                .await
                .expect("model changed")
                .as_deref(),
            Some("openrouter/~anthropic/claude-haiku-latest")
        );
    }
}
