//! Server-side Firebase Cloud Messaging (FCM HTTP v1) push send.
//!
//! This is the SEND side of mobile push. Tokens are already collected by
//! `POST /api/push-tokens` (see [`crate::notify`]); here we turn an in-app
//! notification (a coach review, or a due scheduled reminder) into a data-only
//! FCM message delivered to every Android/iOS token the user has registered.
//!
//! **Feature-flagged OFF when unconfigured.** [`enabled`] is true only when BOTH
//! `AIBUDDY_FCM_PROJECT_ID` and `AIBUDDY_FCM_CREDENTIALS` are set; otherwise
//! [`send_to_user`] is a no-op and in-app notifications keep working unchanged.
//!
//! **Best-effort everywhere.** [`send_to_user`] never returns an error and never
//! panics: an OAuth failure, a bad credentials file, a network error, or a sqlx
//! error is logged and swallowed. An FCM send must never break notification
//! creation, the scheduler, or a check-in.
//!
//! OAuth: we mint a short-lived Google access token from the mounted
//! service-account JSON by signing a JWT (RS256, via `jsonwebtoken`'s
//! `rust_crypto` backend) and exchanging it at the account's `token_uri`. The
//! token is cached process-wide and refreshed ~60s before expiry.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use time::OffsetDateTime;

use crate::config::Config;

/// OAuth scope required to send FCM messages.
const FCM_SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";

/// Access-token lifetime requested in the JWT assertion (seconds).
const TOKEN_LIFETIME_SECS: i64 = 3600;

/// Refresh the cached access token this many seconds before it actually expires,
/// so an in-flight send never races the expiry boundary.
const TOKEN_REFRESH_SKEW_SECS: u64 = 60;

/// Bounded timeout for every outbound FCM / OAuth request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Whether server-side FCM push is configured. Both the project id and the
/// service-account credentials path must be present.
pub fn enabled(config: &Config) -> bool {
    config.fcm_project_id.is_some() && config.fcm_credentials.is_some()
}

/// The relevant fields of a Google service-account JSON key file.
#[derive(Debug, Deserialize)]
struct ServiceAccount {
    client_email: String,
    private_key: String,
    token_uri: String,
}

/// JWT claims for the `jwt-bearer` OAuth grant.
#[derive(Debug, Serialize)]
struct Claims<'a> {
    iss: &'a str,
    scope: &'a str,
    aud: &'a str,
    iat: i64,
    exp: i64,
}

/// Google's token-endpoint response (we only need these two fields).
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
}

/// A cached OAuth access token and the instant it should be considered expired
/// (already adjusted for [`TOKEN_REFRESH_SKEW_SECS`]).
struct CachedToken {
    token: String,
    refresh_at: Instant,
}

/// Process-wide cache of the current access token.
fn token_cache() -> &'static tokio::sync::Mutex<Option<CachedToken>> {
    static CACHE: OnceLock<tokio::sync::Mutex<Option<CachedToken>>> = OnceLock::new();
    CACHE.get_or_init(|| tokio::sync::Mutex::new(None))
}

/// Shared, hardened HTTP client (no redirects, bounded timeout). Falls back to a
/// default client if the builder fails, so this never panics.
fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

/// Build the data-only FCM HTTP v1 message body for one token.
///
/// Data-only (no `notification` block) so the app renders it itself even when
/// backgrounded/closed. `title` and `body` are always present; any extra `data`
/// object entries are flattened in as strings (FCM `data` values must be
/// strings). `title`/`body` keys in `extra` never override the canonical ones.
fn build_message(
    token: &str,
    title: &str,
    body: &str,
    extra: Option<&serde_json::Value>,
) -> serde_json::Value {
    let mut data = serde_json::Map::new();
    data.insert(
        "title".to_string(),
        serde_json::Value::String(title.to_string()),
    );
    data.insert(
        "body".to_string(),
        serde_json::Value::String(body.to_string()),
    );
    if let Some(serde_json::Value::Object(obj)) = extra {
        for (k, v) in obj {
            if k == "title" || k == "body" {
                continue;
            }
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Null => String::new(),
                other => other.to_string(),
            };
            data.insert(k.clone(), serde_json::Value::String(s));
        }
    }

    serde_json::json!({
        "message": {
            "token": token,
            "data": serde_json::Value::Object(data),
            "android": { "priority": "high" },
        }
    })
}

/// Whether an FCM send response indicates the token is dead and should be pruned.
/// A `404 NOT_FOUND`, or an error body mentioning `UNREGISTERED` / `InvalidArgument`,
/// means the registration token is stale/invalid.
fn is_stale_token(status: u16, body: &str) -> bool {
    status == 404 || body.contains("UNREGISTERED") || body.contains("InvalidArgument")
}

/// Mint a fresh OAuth access token from the service-account file. Returns the
/// token and its lifetime in seconds. All failures surface as `anyhow::Error`.
async fn mint_access_token(
    client: &reqwest::Client,
    config: &Config,
) -> anyhow::Result<(String, u64)> {
    let cred_path = config
        .fcm_credentials
        .as_deref()
        .ok_or_else(|| anyhow!("fcm credentials path not configured"))?;

    let raw = tokio::fs::read_to_string(cred_path)
        .await
        .with_context(|| format!("reading FCM service-account file at {cred_path}"))?;
    let sa: ServiceAccount =
        serde_json::from_str(&raw).context("parsing FCM service-account JSON")?;

    let now = OffsetDateTime::now_utc().unix_timestamp();
    let claims = Claims {
        iss: &sa.client_email,
        scope: FCM_SCOPE,
        aud: &sa.token_uri,
        iat: now,
        exp: now + TOKEN_LIFETIME_SECS,
    };

    let key = EncodingKey::from_rsa_pem(sa.private_key.as_bytes())
        .context("loading RSA private key from service account")?;
    let jwt = encode(&Header::new(Algorithm::RS256), &claims, &key)
        .context("signing OAuth JWT assertion")?;

    let resp = client
        .post(&sa.token_uri)
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
            ("assertion", jwt.as_str()),
        ])
        .send()
        .await
        .context("requesting FCM OAuth token")?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("FCM OAuth token request failed: {status} {body}"));
    }

    let parsed: TokenResponse = resp.json().await.context("parsing FCM OAuth response")?;
    let expires_in = parsed.expires_in.unwrap_or(TOKEN_LIFETIME_SECS as u64);
    Ok((parsed.access_token, expires_in))
}

/// Return a valid access token, minting (and caching) a new one when the cache is
/// empty or within the refresh skew of expiry.
async fn access_token(client: &reqwest::Client, config: &Config) -> anyhow::Result<String> {
    let cache = token_cache();
    let mut guard = cache.lock().await;

    if let Some(cached) = guard.as_ref() {
        if cached.refresh_at > Instant::now() {
            return Ok(cached.token.clone());
        }
    }

    let (token, expires_in) = mint_access_token(client, config).await?;
    let ttl = expires_in.saturating_sub(TOKEN_REFRESH_SKEW_SECS);
    *guard = Some(CachedToken {
        token: token.clone(),
        refresh_at: Instant::now() + Duration::from_secs(ttl),
    });
    Ok(token)
}

/// Delete one stale push-token row (best-effort; errors are returned to the
/// caller which only logs them).
async fn prune_token(pool: &SqlitePool, id: &str) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM push_tokens WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .context("pruning stale push token")?;
    Ok(())
}

/// Send a data-only push to every Android/iOS token registered by `user_id`.
///
/// No-op when FCM is not [`enabled`]. Never returns an error and never panics:
/// every failure (OAuth, network, sqlx, per-token HTTP) is logged and swallowed.
/// Stale tokens (HTTP 404 / `UNREGISTERED` / `InvalidArgument`) are pruned.
pub async fn send_to_user(
    pool: &SqlitePool,
    config: &Config,
    user_id: &str,
    title: &str,
    body: &str,
    data: Option<serde_json::Value>,
) {
    if !enabled(config) {
        return;
    }
    if let Err(e) = try_send_to_user(pool, config, user_id, title, body, data).await {
        tracing::warn!(error = ?e, user_id, "fcm push send failed (ignored)");
    }
}

/// Fallible inner body of [`send_to_user`]; its error is logged + swallowed by
/// the public wrapper. Per-token send failures are handled inline (logged /
/// pruned) and do not abort the loop.
async fn try_send_to_user(
    pool: &SqlitePool,
    config: &Config,
    user_id: &str,
    title: &str,
    body: &str,
    data: Option<serde_json::Value>,
) -> anyhow::Result<()> {
    let project_id = config
        .fcm_project_id
        .as_deref()
        .ok_or_else(|| anyhow!("fcm project id not configured"))?;

    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, token FROM push_tokens WHERE user_id = ? AND platform IN ('android', 'ios')",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .context("loading push tokens")?;

    if rows.is_empty() {
        // Nothing to push — avoid minting an OAuth token needlessly.
        return Ok(());
    }

    let client = http_client();
    let access = access_token(client, config).await?;
    let url = format!("https://fcm.googleapis.com/v1/projects/{project_id}/messages:send");

    for (id, tok) in &rows {
        let message = build_message(tok, title, body, data.as_ref());
        let resp = match client
            .post(&url)
            .bearer_auth(&access)
            .json(&message)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = ?e, user_id, "fcm message send failed (ignored)");
                continue;
            }
        };

        let status = resp.status();
        if status.is_success() {
            continue;
        }

        let resp_body = resp.text().await.unwrap_or_default();
        if is_stale_token(status.as_u16(), &resp_body) {
            if let Err(e) = prune_token(pool, id).await {
                tracing::warn!(error = ?e, user_id, "pruning stale push token failed (ignored)");
            } else {
                tracing::debug!(user_id, "pruned stale push token");
            }
        } else {
            tracing::warn!(
                status = status.as_u16(),
                body = %resp_body,
                user_id,
                "fcm message rejected (ignored)"
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disabled_config() -> Config {
        let mut c = Config::from_env();
        c.fcm_project_id = None;
        c.fcm_credentials = None;
        c
    }

    #[test]
    fn enabled_requires_both_fields() {
        let mut c = disabled_config();
        assert!(!enabled(&c), "neither set => disabled");

        c.fcm_project_id = Some("proj".to_string());
        assert!(!enabled(&c), "only project id => disabled");

        c.fcm_project_id = None;
        c.fcm_credentials = Some("/path/sa.json".to_string());
        assert!(!enabled(&c), "only credentials => disabled");

        c.fcm_project_id = Some("proj".to_string());
        c.fcm_credentials = Some("/path/sa.json".to_string());
        assert!(enabled(&c), "both set => enabled");
    }

    #[test]
    fn build_message_is_data_only_with_title_and_body() {
        let msg = build_message("tok-123", "Hi there", "You did it", None);
        let message = msg.get("message").expect("message object");

        assert_eq!(
            message.get("token").and_then(|v| v.as_str()),
            Some("tok-123")
        );
        // Data-only: there must be NO top-level notification block.
        assert!(
            message.get("notification").is_none(),
            "message must be data-only (no notification block)"
        );
        assert_eq!(
            message
                .get("android")
                .and_then(|a| a.get("priority"))
                .and_then(|p| p.as_str()),
            Some("high")
        );

        let data = message.get("data").expect("data object");
        assert_eq!(data.get("title").and_then(|v| v.as_str()), Some("Hi there"));
        assert_eq!(
            data.get("body").and_then(|v| v.as_str()),
            Some("You did it")
        );
    }

    #[test]
    fn build_message_flattens_extra_data_as_strings() {
        let extra = serde_json::json!({
            "goal_id": "g-7",
            "status": "on_track",
            "count": 3,
            "nested": { "k": "v" },
            // These must not override the canonical title/body.
            "title": "SHOULD NOT WIN",
            "body": "SHOULD NOT WIN",
        });
        let msg = build_message("tok", "Real Title", "Real Body", Some(&extra));
        let data = msg
            .get("message")
            .and_then(|m| m.get("data"))
            .expect("data object");

        assert_eq!(
            data.get("title").and_then(|v| v.as_str()),
            Some("Real Title")
        );
        assert_eq!(data.get("body").and_then(|v| v.as_str()), Some("Real Body"));
        assert_eq!(data.get("goal_id").and_then(|v| v.as_str()), Some("g-7"));
        assert_eq!(
            data.get("status").and_then(|v| v.as_str()),
            Some("on_track")
        );
        // Non-string values are stringified (FCM data values must be strings).
        assert_eq!(data.get("count").and_then(|v| v.as_str()), Some("3"));
        assert!(
            data.get("count").map(|v| v.is_string()).unwrap_or(false),
            "numeric extra is coerced to a JSON string"
        );
        assert_eq!(
            data.get("nested").and_then(|v| v.as_str()),
            Some("{\"k\":\"v\"}")
        );
    }

    #[test]
    fn build_message_ignores_non_object_extra() {
        let extra = serde_json::json!(["not", "an", "object"]);
        let msg = build_message("tok", "T", "B", Some(&extra));
        let data = msg
            .get("message")
            .and_then(|m| m.get("data"))
            .and_then(|d| d.as_object())
            .expect("data object");
        // Only title + body remain.
        assert_eq!(data.len(), 2);
    }

    #[test]
    fn stale_token_detected_from_status_and_body() {
        // 404 is stale regardless of body.
        assert!(is_stale_token(404, ""));
        // Typical v1 error payloads.
        assert!(is_stale_token(
            404,
            r#"{"error":{"status":"NOT_FOUND","details":[{"errorCode":"UNREGISTERED"}]}}"#
        ));
        assert!(is_stale_token(
            400,
            r#"{"error":{"status":"INVALID_ARGUMENT","message":"InvalidArgument"}}"#
        ));
        // A transient server error is NOT a stale token.
        assert!(!is_stale_token(500, "internal"));
        assert!(!is_stale_token(429, "quota exceeded"));
        assert!(!is_stale_token(200, ""));
    }
}
