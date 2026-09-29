//! Payment backends: a real Stripe Checkout implementation and a dev-only mock.
//!
//! [`crate::wallet::PaymentProvider`] picks [`StripePaymentBackend`] when a
//! Stripe secret key is configured, else [`MockPaymentBackend`] (instant credit,
//! no external calls).

use anyhow::{anyhow, Context};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use super::{PaymentBackend, TopupSession, WebhookEvent};

const STRIPE_API_BASE: &str = "https://api.stripe.com";
/// Reject webhook events whose timestamp is more than this many seconds from now.
const WEBHOOK_TOLERANCE_SECS: i64 = 300;

/// Dev/default backend: no external calls, instant success. The top-up HTTP
/// handler credits immediately because `checkout_url` is `None`.
pub struct MockPaymentBackend;

#[async_trait::async_trait]
impl PaymentBackend for MockPaymentBackend {
    async fn create_topup(
        &self,
        _user_id: &str,
        _amount_cents: i64,
    ) -> anyhow::Result<TopupSession> {
        Ok(TopupSession {
            checkout_url: None,
            external_ref: format!("mock_{}", uuid::Uuid::new_v4()),
            status: "succeeded".to_string(),
        })
    }

    async fn verify_webhook(&self, _sig: &str, _body: &[u8]) -> anyhow::Result<WebhookEvent> {
        Err(anyhow!(
            "mock payment backend does not process webhooks (no Stripe key configured)"
        ))
    }
}

/// Real Stripe backend: creates a Checkout Session for a top-up and verifies
/// webhook signatures (HMAC-SHA256 over `{timestamp}.{payload}`).
pub struct StripePaymentBackend {
    secret_key: String,
    webhook_secret: Option<String>,
    /// Public base URL of this deployment, used to build success/cancel URLs.
    base_url: String,
    client: reqwest::Client,
}

impl StripePaymentBackend {
    pub fn new(secret_key: String, webhook_secret: Option<String>, base_url: String) -> Self {
        Self {
            secret_key,
            webhook_secret,
            base_url: base_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait::async_trait]
impl PaymentBackend for StripePaymentBackend {
    async fn create_topup(&self, user_id: &str, amount_cents: i64) -> anyhow::Result<TopupSession> {
        if amount_cents <= 0 {
            return Err(anyhow!("top-up amount must be positive"));
        }
        let success_url = format!("{}/wallet?topup=success", self.base_url);
        let cancel_url = format!("{}/wallet?topup=cancelled", self.base_url);

        // Stripe expects application/x-www-form-urlencoded with nested keys.
        let params = [
            ("mode", "payment".to_string()),
            ("success_url", success_url),
            ("cancel_url", cancel_url),
            ("client_reference_id", user_id.to_string()),
            ("metadata[user_id]", user_id.to_string()),
            ("line_items[0][quantity]", "1".to_string()),
            ("line_items[0][price_data][currency]", "eur".to_string()),
            (
                "line_items[0][price_data][unit_amount]",
                amount_cents.to_string(),
            ),
            (
                "line_items[0][price_data][product_data][name]",
                "AI Buddy wallet top-up".to_string(),
            ),
        ];

        let resp = self
            .client
            .post(format!("{STRIPE_API_BASE}/v1/checkout/sessions"))
            .bearer_auth(&self.secret_key)
            .form(&params)
            .send()
            .await
            .context("sending Stripe checkout session request")?;

        let status = resp.status();
        let body: serde_json::Value = resp
            .json()
            .await
            .context("decoding Stripe checkout session response")?;

        if !status.is_success() {
            let msg = body
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("unknown Stripe error");
            return Err(anyhow!("Stripe checkout session creation failed: {msg}"));
        }

        let external_ref = body
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Stripe response missing session id"))?
            .to_string();
        let checkout_url = body
            .get("url")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Ok(TopupSession {
            checkout_url,
            external_ref,
            status: "pending".to_string(),
        })
    }

    async fn verify_webhook(&self, sig: &str, body: &[u8]) -> anyhow::Result<WebhookEvent> {
        let secret = self
            .webhook_secret
            .as_deref()
            .ok_or_else(|| anyhow!("Stripe webhook secret is not configured"))?;

        // Header form: "t=<unix>,v1=<hex>,v0=<hex>". Parse t and v1.
        let mut timestamp: Option<&str> = None;
        let mut v1: Option<&str> = None;
        for part in sig.split(',') {
            if let Some((k, val)) = part.split_once('=') {
                match k.trim() {
                    "t" => timestamp = Some(val.trim()),
                    "v1" => v1 = Some(val.trim()),
                    _ => {}
                }
            }
        }
        let timestamp =
            timestamp.ok_or_else(|| anyhow!("missing timestamp in Stripe-Signature"))?;
        let v1 = v1.ok_or_else(|| anyhow!("missing v1 signature in Stripe-Signature"))?;

        // signed_payload = timestamp + "." + raw body
        let mut signed = Vec::with_capacity(timestamp.len() + 1 + body.len());
        signed.extend_from_slice(timestamp.as_bytes());
        signed.push(b'.');
        signed.extend_from_slice(body);

        let expected = to_hex(&hmac_sha256(secret.as_bytes(), &signed));
        if !constant_time_eq(expected.as_bytes(), v1.as_bytes()) {
            return Err(anyhow!("Stripe webhook signature mismatch"));
        }

        // Replay protection: reject stale timestamps.
        if let Ok(ts) = timestamp.parse::<i64>() {
            let now = OffsetDateTime::now_utc().unix_timestamp();
            if (now - ts).abs() > WEBHOOK_TOLERANCE_SECS {
                return Err(anyhow!("Stripe webhook timestamp outside tolerance"));
            }
        }

        let event: serde_json::Value =
            serde_json::from_slice(body).context("parsing Stripe webhook JSON")?;
        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let obj = event
            .get("data")
            .and_then(|d| d.get("object"))
            .ok_or_else(|| anyhow!("Stripe webhook missing data.object"))?;

        let external_ref = obj
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let amount_cents = obj
            .get("amount_total")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        let user_id = obj
            .get("client_reference_id")
            .and_then(|v| v.as_str())
            .or_else(|| {
                obj.get("metadata")
                    .and_then(|m| m.get("user_id"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or_default()
            .to_string();
        let payment_status = obj
            .get("payment_status")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let succeeded = event_type == "checkout.session.completed" && payment_status == "paid";

        Ok(WebhookEvent {
            user_id,
            amount_cents,
            external_ref,
            succeeded,
        })
    }
}

/// HMAC-SHA256 built on `sha2` (no extra dependency). Returns the 32-byte tag.
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut key_block = [0u8; BLOCK];
    if key.len() > BLOCK {
        let digest = Sha256::digest(key);
        for (dst, src) in key_block.iter_mut().zip(digest.iter()) {
            *dst = *src;
        }
    } else {
        for (dst, src) in key_block.iter_mut().zip(key.iter()) {
            *dst = *src;
        }
    }

    let mut i_pad = [0u8; BLOCK];
    let mut o_pad = [0u8; BLOCK];
    for ((ip, op), k) in i_pad.iter_mut().zip(o_pad.iter_mut()).zip(key_block.iter()) {
        *ip = *k ^ 0x36;
        *op = *k ^ 0x5c;
    }

    let mut inner = Sha256::new();
    inner.update(i_pad);
    inner.update(msg);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(o_pad);
    outer.update(inner_digest);
    let out = outer.finalize();

    let mut result = [0u8; 32];
    for (dst, src) in result.iter_mut().zip(out.iter()) {
        *dst = *src;
    }
    result
}

/// Lowercase hex encoding without indexing (panic-safe).
fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let byte = *b;
        s.push(char::from_digit((byte >> 4) as u32, 16).unwrap_or('0'));
        s.push(char::from_digit((byte & 0x0f) as u32, 16).unwrap_or('0'));
    }
    s
}

/// Length-independent, constant-time byte comparison.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= *x ^ *y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_sha256_matches_known_vector() {
        // RFC 4231 test case 2: key = "Jefe", data = "what do ya want ...".
        let tag = to_hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?"));
        assert_eq!(
            tag,
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn constant_time_eq_works() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }

    #[tokio::test]
    async fn mock_topup_has_no_checkout_url() {
        let backend = MockPaymentBackend;
        let session = backend.create_topup("u1", 500).await.expect("session");
        assert!(session.checkout_url.is_none());
        assert_eq!(session.status, "succeeded");
        assert!(session.external_ref.starts_with("mock_"));
    }

    #[tokio::test]
    async fn stripe_verify_webhook_roundtrip() {
        let secret = "whsec_test";
        let backend = StripePaymentBackend::new(
            "sk_test".to_string(),
            Some(secret.to_string()),
            "https://example.com".to_string(),
        );
        let ts = OffsetDateTime::now_utc().unix_timestamp();
        let body = serde_json::json!({
            "type": "checkout.session.completed",
            "data": { "object": {
                "id": "cs_test_123",
                "amount_total": 1000,
                "payment_status": "paid",
                "client_reference_id": "u1"
            }}
        })
        .to_string();
        let mut signed = Vec::new();
        signed.extend_from_slice(ts.to_string().as_bytes());
        signed.push(b'.');
        signed.extend_from_slice(body.as_bytes());
        let sig = format!(
            "t={ts},v1={}",
            to_hex(&hmac_sha256(secret.as_bytes(), &signed))
        );

        let event = backend
            .verify_webhook(&sig, body.as_bytes())
            .await
            .expect("verify");
        assert!(event.succeeded);
        assert_eq!(event.amount_cents, 1000);
        assert_eq!(event.user_id, "u1");
        assert_eq!(event.external_ref, "cs_test_123");
    }

    #[tokio::test]
    async fn stripe_verify_webhook_rejects_bad_signature() {
        let backend = StripePaymentBackend::new(
            "sk_test".to_string(),
            Some("whsec_test".to_string()),
            "https://example.com".to_string(),
        );
        let ts = OffsetDateTime::now_utc().unix_timestamp();
        let sig = format!("t={ts},v1=deadbeef");
        assert!(backend.verify_webhook(&sig, b"{}").await.is_err());
    }
}
