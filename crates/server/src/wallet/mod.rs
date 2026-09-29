//! Wallet / payments / pledges.
//!
//! STUB — implemented by the `wallet` feature agent. Must keep:
//!   - `pub struct PaymentProvider` deriving `Clone` (Arc interior),
//!   - `pub fn new(config: &Config) -> Self`,
//!   - `pub fn router() -> Router<AppState>`.
//! Then add (per docs/ARCHITECTURE.md): the `PaymentBackend` trait + Stripe/Mock
//! impls, ledger ops (`credit_topup`, `debit_tokens`, `pledge_hold`,
//! `pledge_forfeit`, `pledge_refund`, `balance_cents`) and the wallet/pledge
//! routes. Owns migration `0002_wallet.sql`.

use axum::Router;

use crate::config::Config;
use crate::state::AppState;

#[derive(Clone)]
pub struct PaymentProvider {
    #[allow(dead_code)]
    inner: std::sync::Arc<()>,
}

impl PaymentProvider {
    pub fn new(_config: &Config) -> Self {
        Self {
            inner: std::sync::Arc::new(()),
        }
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
}
