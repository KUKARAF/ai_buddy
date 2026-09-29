//! MCP OAuth 2.1 Resource Server: token validation + OAuth metadata.
//!
//! AI Buddy does not issue tokens — Authentik (the app's OIDC provider) is the
//! Authorization Server. This module makes AI Buddy a compliant Resource
//! Server for the `/mcp` endpoint:
//!
//!   * serves RFC 9728 Protected Resource Metadata so a client discovers the
//!     Authorization Server,
//!   * challenges unauthenticated calls with `WWW-Authenticate: Bearer
//!     resource_metadata="…"`,
//!   * validates the presented JWT access token — signature via the issuer's
//!     JWKS, plus `iss`, `exp`, and (critically) `aud` == our resource URI,
//!     which prevents a confused-deputy replay of a token minted for another
//!     resource, and
//!   * maps the token `sub` to the app `user_id` (the same value the OIDC login
//!     stores; see [`crate::auth::session`]), injecting it into the request
//!     extensions for the MCP handlers to read.

use std::sync::Arc;

use axum::extract::Request;
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::RwLock;

use crate::auth::device_token::bearer_from_headers;
use crate::config::Config;

/// The authenticated MCP user id, injected into request extensions by
/// [`McpAuth::require`] and read back by tool/resource handlers (through the
/// `http::request::Parts` the rmcp transport forwards into the request context).
#[derive(Clone, Debug)]
pub struct AuthUser(pub String);

/// The access-token claims we consume. `aud`/`iss`/`exp` are validated by
/// `jsonwebtoken` from the raw token, so only `sub` needs deserializing here.
#[derive(Debug, Deserialize)]
struct Claims {
    sub: String,
}

/// MCP Resource-Server auth context: issuer/resource/audience config plus a
/// lazily-populated, rotation-aware JWKS cache.
pub struct McpAuth {
    dev_mode: bool,
    /// Issuer (Authentik): the JWT `iss` check, the PRM `authorization_servers`
    /// entry, and the base for OIDC discovery of the JWKS URI.
    issuer: String,
    /// The RFC 9728 `resource` value.
    resource_uri: String,
    /// Expected token `aud`.
    audience: String,
    /// Absolute URL of the PRM document (for the `WWW-Authenticate` challenge).
    prm_url: String,
    http: reqwest::Client,
    jwks: RwLock<Option<JwkSet>>,
}

impl McpAuth {
    pub fn from_config(config: &Config) -> Arc<Self> {
        let prm_url = format!(
            "{}/.well-known/oauth-protected-resource",
            config.base_url.trim_end_matches('/')
        );
        Arc::new(Self {
            dev_mode: config.dev_mode,
            issuer: config.authentik_issuer_url.clone(),
            resource_uri: config.mcp_resource_uri.clone(),
            audience: config.mcp_audience.clone(),
            prm_url,
            http: reqwest::Client::new(),
            jwks: RwLock::new(None),
        })
    }

    /// RFC 9728 Protected Resource Metadata (served unauthenticated).
    pub fn protected_resource_metadata(&self) -> Json<serde_json::Value> {
        Json(json!({
            "resource": self.resource_uri,
            "authorization_servers": [self.issuer],
            "bearer_methods_supported": ["header"],
        }))
    }

    /// A `401` challenge pointing the client at the PRM document.
    fn challenge(&self) -> Response {
        let mut resp = (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        )
            .into_response();
        let value = format!("Bearer resource_metadata=\"{}\"", self.prm_url);
        let header_value =
            HeaderValue::from_str(&value).unwrap_or_else(|_| HeaderValue::from_static("Bearer"));
        resp.headers_mut()
            .insert(header::WWW_AUTHENTICATE, header_value);
        resp
    }

    /// axum middleware body: authenticate the request and stash [`AuthUser`], or
    /// return a `401` challenge.
    ///
    /// In dev mode auth is bypassed with the fixed `admin` user — the same
    /// global bypass `RequireAuth` uses; dev mode is loopback-only and
    /// fail-validated at boot, so this never weakens a real deployment.
    pub async fn require(&self, mut req: Request, next: Next) -> Response {
        if self.dev_mode {
            req.extensions_mut()
                .insert(AuthUser(crate::config::DEV_MODE_USER_ID.to_string()));
            return next.run(req).await;
        }
        let Some(token) = bearer_from_headers(req.headers()) else {
            return self.challenge();
        };
        match self.validate(&token).await {
            Ok(user_id) => {
                req.extensions_mut().insert(AuthUser(user_id));
                next.run(req).await
            }
            Err(err) => {
                tracing::debug!(error = %err, "MCP token validation failed");
                self.challenge()
            }
        }
    }

    /// Validate a bearer JWT and return its subject (the app `user_id`).
    async fn validate(&self, token: &str) -> anyhow::Result<String> {
        let header = decode_header(token)?;
        // Reject symmetric algs (alg-confusion defence): our keys are asymmetric
        // JWKS entries and must never be verified as an HMAC secret.
        if matches!(
            header.alg,
            Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512
        ) {
            anyhow::bail!("unsupported token algorithm {:?}", header.alg);
        }
        let kid = header
            .kid
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("token header missing kid"))?;

        let key = self.decoding_key_for(kid).await?;
        self.decode_claims(token, &key, header.alg)
    }

    /// Decode + validate a token's claims against this server's issuer and
    /// audience (and expiry), returning the subject. Split out from [`validate`]
    /// so the security-critical `iss`/`aud`/`exp` checks are unit-testable
    /// without a live JWKS. Rejecting a token whose `aud` isn't our resource URI
    /// is the confused-deputy defence.
    fn decode_claims(
        &self,
        token: &str,
        key: &DecodingKey,
        alg: Algorithm,
    ) -> anyhow::Result<String> {
        let mut validation = Validation::new(alg);
        validation.set_issuer(&[self.issuer.as_str()]);
        validation.set_audience(&[self.audience.as_str()]);
        validation.validate_exp = true;

        let data = decode::<Claims>(token, key, &validation)?;
        Ok(data.claims.sub)
    }

    /// Resolve the decoding key for `kid`, fetching/refreshing the JWKS if it
    /// isn't cached (covers first use and issuer key rotation).
    async fn decoding_key_for(&self, kid: &str) -> anyhow::Result<DecodingKey> {
        if let Some(key) = self.cached_key(kid).await? {
            return Ok(key);
        }
        self.refresh_jwks().await?;
        self.cached_key(kid)
            .await?
            .ok_or_else(|| anyhow::anyhow!("no JWKS key for kid {kid}"))
    }

    async fn cached_key(&self, kid: &str) -> anyhow::Result<Option<DecodingKey>> {
        let guard = self.jwks.read().await;
        match guard.as_ref().and_then(|set| set.find(kid)) {
            Some(jwk) => Ok(Some(DecodingKey::from_jwk(jwk)?)),
            None => Ok(None),
        }
    }

    async fn refresh_jwks(&self) -> anyhow::Result<()> {
        let jwks_uri = self.discover_jwks_uri().await?;
        let set: JwkSet = self
            .http
            .get(jwks_uri)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        *self.jwks.write().await = Some(set);
        Ok(())
    }

    /// OIDC discovery against the issuer to find its `jwks_uri`.
    async fn discover_jwks_uri(&self) -> anyhow::Result<String> {
        let url = format!(
            "{}/.well-known/openid-configuration",
            self.issuer.trim_end_matches('/')
        );
        let doc: serde_json::Value = self
            .http
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        doc.get("jwks_uri")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| anyhow::anyhow!("issuer discovery document has no jwks_uri"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use jsonwebtoken::{encode, EncodingKey, Header};

    const ISS: &str = "https://issuer.example/";
    const AUD: &str = "https://aibuddy.example.com/mcp";
    const SECRET: &[u8] = b"test-secret-for-hs256-claims-checks";
    // Far-future / far-past unix timestamps so the tests never depend on the
    // clock (only that exp validation compares against "now").
    const FUTURE: i64 = 4_102_444_800; // 2100-01-01
    const PAST: i64 = 1_000_000_000; // 2001-09-09

    fn test_auth() -> Arc<McpAuth> {
        let mut cfg = Config::from_env();
        cfg.authentik_issuer_url = ISS.to_string();
        cfg.base_url = "https://aibuddy.example.com".to_string();
        cfg.mcp_resource_uri = AUD.to_string();
        cfg.mcp_audience = AUD.to_string();
        McpAuth::from_config(&cfg)
    }

    fn make_token(iss: &str, aud: &str, exp: i64) -> String {
        let claims = json!({ "sub": "alice", "iss": iss, "aud": aud, "exp": exp });
        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(SECRET),
        )
        .expect("sign test token")
    }

    fn key() -> DecodingKey {
        DecodingKey::from_secret(SECRET)
    }

    #[test]
    fn accepts_token_with_matching_iss_and_aud() {
        let auth = test_auth();
        let token = make_token(ISS, AUD, FUTURE);
        let sub = auth
            .decode_claims(&token, &key(), Algorithm::HS256)
            .expect("valid token accepted");
        assert_eq!(sub, "alice");
    }

    #[test]
    fn rejects_wrong_audience() {
        // Confused-deputy defence: a token minted for another resource is refused.
        let auth = test_auth();
        let token = make_token(ISS, "https://evil.example/api", FUTURE);
        assert!(auth
            .decode_claims(&token, &key(), Algorithm::HS256)
            .is_err());
    }

    #[test]
    fn rejects_wrong_issuer() {
        let auth = test_auth();
        let token = make_token("https://evil.example/", AUD, FUTURE);
        assert!(auth
            .decode_claims(&token, &key(), Algorithm::HS256)
            .is_err());
    }

    #[test]
    fn rejects_expired() {
        let auth = test_auth();
        let token = make_token(ISS, AUD, PAST);
        assert!(auth
            .decode_claims(&token, &key(), Algorithm::HS256)
            .is_err());
    }

    #[test]
    fn prm_advertises_resource_and_issuer() {
        let auth = test_auth();
        let Json(doc) = auth.protected_resource_metadata();
        assert_eq!(doc["resource"], AUD);
        assert_eq!(doc["authorization_servers"][0], ISS);
        assert_eq!(doc["bearer_methods_supported"][0], "header");
    }
}
