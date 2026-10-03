//! Defense-in-depth HTTP response security headers.
//!
//! This is a backstop: even if some future rich-text / markdown rendering path
//! has a sanitization flaw, these headers (primarily the Content-Security-Policy)
//! block script execution from remote origins and block data exfiltration via
//! `connect-src`/`object-src`. They are applied to EVERY response — the REST
//! API, the MCP endpoint, and the static SPA served by `ServeDir`/`ServeFile`.
//!
//! All header values are compile-time constants set via `HeaderValue::from_static`
//! (no runtime parsing, so no `unwrap`/`expect` and no panic path — satisfies the
//! workspace panic-safety deny lints).

use axum::extract::Request;
use axum::http::header::{
    HeaderName, HeaderValue, CONTENT_SECURITY_POLICY, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS,
    X_FRAME_OPTIONS,
};
use axum::middleware::Next;
use axum::response::Response;

/// The Content-Security-Policy served with the website (same-origin SPA + API,
/// both at e.g. `buddy.osmosis.page`).
///
/// Directive rationale:
/// - `default-src 'self'` — deny-by-default; every fetch directive not listed below falls back to same-origin only.
/// - `base-uri 'self'` — stop injected `<base>` tags from rewriting relative-URL resolution.
/// - `object-src 'none'` — no `<object>/<embed>/<applet>` (legacy plugin-based script/exfil vectors).
/// - `frame-ancestors 'none'` — clickjacking defense (CSP equivalent of `X-Frame-Options: DENY`).
/// - `form-action 'self'` — a sanitizer-escaped `<form action=...>` can't POST data to an attacker origin.
/// - `img-src 'self' data: blob:` — app renders inline/data + blob images (e.g. check-in photo attachments).
/// - `script-src 'self' 'unsafe-inline'` — PRAGMATIC COMPROMISE: the SvelteKit static build emits an inline bootstrap `<script>` with NO nonce, so a strict `script-src 'self'` would break app boot. This still blocks ALL remote script loads (no third-party hosts, no `data:` scripts). FUTURE FIX: emit a nonce/hash and drop `'unsafe-inline'`.
/// - `style-src 'self' 'unsafe-inline'` — Svelte injects `<style>` blocks and components use inline `style=` attributes.
/// - `connect-src 'self'` — THE key exfiltration guard: XHR/fetch/WebSocket/EventSource may only reach the same origin (the API is same-origin here).
/// - `font-src 'self' data:` — self-hosted + data-URI fonts only (the app uses a system font stack, no CDN).
const WEB_CSP: &str = "default-src 'self'; \
base-uri 'self'; \
object-src 'none'; \
frame-ancestors 'none'; \
form-action 'self'; \
img-src 'self' data: blob:; \
script-src 'self' 'unsafe-inline'; \
style-src 'self' 'unsafe-inline'; \
connect-src 'self'; \
font-src 'self' data:";

/// `Permissions-Policy` has no associated constant in the `http` crate, so the
/// name is built from a static string (infallible, `const`-friendly).
const PERMISSIONS_POLICY_HEADER: HeaderName = HeaderName::from_static("permissions-policy");

/// Deny the powerful features this app does not use.
const PERMISSIONS_POLICY_VALUE: &str = "geolocation=(), camera=(), microphone=()";

/// Axum middleware that stamps defense-in-depth security headers onto every
/// response. Applied once, at the outermost layer in `main.rs`, so it covers the
/// API, the MCP mount, and the static SPA fallback alike.
///
/// Note: HSTS is intentionally NOT set here — TLS/HSTS is terminated and managed
/// by the external reverse proxy (Caddy).
pub async fn set_security_headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let headers = res.headers_mut();

    headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(WEB_CSP));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(
        REFERRER_POLICY,
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        PERMISSIONS_POLICY_HEADER,
        HeaderValue::from_static(PERMISSIONS_POLICY_VALUE),
    );

    res
}
