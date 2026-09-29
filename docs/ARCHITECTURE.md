# AI Buddy — Backend Architecture & Build Contract

Working name: **AI Buddy** (rename later). An AI accountability / personal-trainer /
roadmap-creator service. Rust backend mirroring the `rust_note` stack, serving a web
frontend + Android APK (Tauri) from one service.

This document is the **contract** every implementation subagent builds against. Do not
change shared interfaces here without coordinating.

## Stack (mirrors /var/home/rafa/dev/rust_note)
- Rust workspace: `crates/server` (axum bin), `crates/core` (`ai-buddy-core`, lib `ai_buddy_core`), `crates/mobile` (Tauri 2).
- axum 0.8 + tokio, sqlx + SQLite (WAL), tower-sessions (SQLite store), openidconnect (Authentik OIDC), reqwest → OpenRouter.
- rmcp MCP server mounted at `/mcp`. Web: SvelteKit adapter-static served by axum from `AIBUDDY_STATIC_DIR`.
- Vector search: **sqlite-vec** (vec0 virtual table in the same SQLite DB). Embeddings via OpenRouter/OpenAI embeddings API.
- Clippy panic-safety gate (deny unwrap/expect/panic/todo/unimplemented/unreachable/indexing_slicing); `[lints] workspace = true` in every crate.
- Env prefix: **`AIBUDDY_`**. GHCR image: `ghcr.io/kukaraf/ai_buddy`. App id: `dev.aibuddy.app`.

## Product model
- User subscribes / tops up wallet (min €4, usable up to ~€100). Balance covers LLM token usage AND can be pledged to goals.
- Screen 1: subscribe / wallet. Screen 2: chat to decide a goal, deadline, current progress.
- AI builds a roadmap (steps + due dates), then sends reminders/tips/check-ins.
- Pledge: AI suggests an amount; on success → refund, on failure → forfeit (future: community payout pool).

## Crate layout & module ownership (each module = one subagent, disjoint files)
```
crates/core/src/           # pure domain types/enums/DTOs, no axum/sqlx. lib = ai_buddy_core
crates/server/src/
  main.rs        routes.rs   config.rs   error.rs   state.rs   db.rs      # OWNED BY FOUNDATION (me)
  auth/          # OIDC + tower-sessions + device_token + RequireAuth     # FOUNDATION (port rust_note)
  llm/           # OpenRouter chat + embeddings client + token cost       # AGENT: llm
  vector/        # sqlite-vec store: upsert/query embeddings + RAG        # AGENT: llm (same agent)
  goals/         # goals + roadmap CRUD, roadmap generation               # AGENT: goals
  chat/          # conversations + messages + streaming chat handler      # AGENT: goals (same agent)
  wallet/        # wallet, ledger, topup, pledge hold/forfeit/refund      # AGENT: wallet
  notify/        # reminders store + tokio scheduler + delivery + push    # AGENT: notify
  mcp/           # rmcp server + JWT resource-server auth + tools         # AGENT: mcp
```
Each feature module exposes `pub fn router() -> axum::Router<AppState>` and is `mod`-declared + merged by FOUNDATION.
Feature agents: create files ONLY under your module dir + your assigned migration file. Do NOT edit main.rs/routes.rs/state.rs/config.rs/Cargo.toml/db.rs — FOUNDATION wires you in. Do NOT run `cargo build` (shared target dir); write against this contract.

## AppState (crates/server/src/state.rs) — final field set
```rust
#[derive(Clone)]
pub struct AppState {
    pub db: sqlx::SqlitePool,
    pub config: std::sync::Arc<Config>,
    pub oidc: Option<std::sync::Arc<crate::auth::oidc::OidcClient>>, // None if discovery failed
    pub cookie_key: axum_extra::extract::cookie::Key,
    pub llm: crate::llm::LlmClient,          // Clone (Arc inside); chat + embeddings
    pub payments: crate::wallet::PaymentProvider, // Clone; trait object behind Arc
    pub notifier: crate::notify::Notifier,   // Clone; enqueue/send notifications
}
impl axum::extract::FromRef<AppState> for Key { ... } // for cookie jar
```
FOUNDATION provides `state.rs`. If a feature needs a new AppState field, request it — don't add it yourself.

## Error type (crates/server/src/error.rs) — port rust_note verbatim
`AppError { Internal(#[from] anyhow::Error), NotFound, Unauthorized, Forbidden, BadRequest(String), Conflict(String), PayloadTooLarge, PaymentRequired(String) }` with `IntoResponse` → JSON `{ "message": ... }`. `pub type AppResult<T> = Result<T, AppError>;`. (Add `PaymentRequired` → 402 for "wallet empty".) Reuse `RequireAuth(pub String)` extractor from `auth::session`.

## Config (crates/server/src/config.rs) — env vars, all `AIBUDDY_`
Mirror rust_note config.rs (dev defaults, `validate()`, signing key, cookie_secure, dev_mode, cors, static_dir, mcp_resource_uri/audience). Additional fields:
- `openrouter_api_key: Option<String>`  (AIBUDDY_OPENROUTER_API_KEY)
- `chat_model: String` default `"anthropic/claude-3.5-sonnet"` (AIBUDDY_CHAT_MODEL) — OpenRouter model id
- `embedding_model: String` default `"openai/text-embedding-3-small"` (AIBUDDY_EMBEDDING_MODEL)
- `embedding_dim: usize` default `1536` (AIBUDDY_EMBEDDING_DIM)
- `stripe_secret_key: Option<String>`, `stripe_webhook_secret: Option<String>` (payments; None → MockPaymentProvider)
- `min_topup_cents: i64` default `400`, `max_balance_cents: i64` default `10000`
- Drop rust_note-only: notes_repo_path.

## Database (crates/server/src/db.rs) — port rust_note init_pool (WAL, busy_timeout, pool)
PLUS: after connecting, load the sqlite-vec extension on the pool (see llm/vector contract) so `vec0` virtual tables work. Runtime-checked sqlx queries (no macros / no DATABASE_URL). Migrations in `crates/server/migrations/`.

### Migration file assignment (avoid collisions)
- `0001_init.sql` — FOUNDATION: `users` (id TEXT PK = OIDC sub, email, display_name, created_at), `device_tokens` (port rust_note 0003).
- `0002_wallet.sql` — wallet agent: `wallets`, `ledger_entries`, `pledges`.
- `0003_goals.sql` — goals agent: `goals`, `roadmaps`, `roadmap_steps`.
- `0004_chat.sql` — goals agent: `conversations`, `messages`.
- `0005_notify.sql` — notify agent: `reminders`, `push_tokens`.
- `0006_vectors.sql` — llm agent: `embedding_meta` table + `CREATE VIRTUAL TABLE embeddings USING vec0(...)`.
All timestamps TEXT ISO-8601 (`datetime('now')`). Money as INTEGER cents. IDs TEXT (uuid v4) unless noted.

## HTTP API (all under session/bearer auth via RequireAuth unless noted)
- `GET /health` (open), OIDC `/auth/login|/auth/callback|/auth/logout|/auth/me` (FOUNDATION, port rust_note).
- Chat: `POST /api/conversations`, `GET /api/conversations`, `GET /api/conversations/:id`, `POST /api/conversations/:id/messages` (SSE stream). (goals agent → chat module)
- Goals: `GET/POST /api/goals`, `GET/PATCH /api/goals/:id`, `POST /api/goals/:id/roadmap` (generate), `GET /api/goals/:id/roadmap`, `PATCH /api/steps/:id`. (goals agent)
- Wallet: `GET /api/wallet`, `POST /api/wallet/topup`, `GET /api/wallet/ledger`, `POST /api/goals/:id/pledge`, `POST /api/goals/:id/pledge/confirm`, `POST /api/webhooks/stripe` (open, sig-verified). (wallet agent)
- Notify: `GET /api/notifications`, `POST /api/push-tokens`. (notify agent)
- MCP: `/mcp` streamable-http + `/.well-known/oauth-protected-resource` (mcp agent, port rust_note mcp/).

## Module contracts

### llm module (`crate::llm`)
```rust
#[derive(Clone)] pub struct LlmClient { /* Arc<Inner>: reqwest client, api key, models, db pool for cost logging */ }
impl LlmClient {
  pub fn new(config: &Config) -> Self;
  // Non-streaming chat completion. Returns (assistant_text, cost_cents_estimate).
  pub async fn chat(&self, system: &str, messages: &[ChatMsg]) -> AppResult<ChatOutcome>;
  // Streaming: returns an SSE-friendly stream of token deltas + a final usage event.
  pub async fn chat_stream(&self, system: &str, messages: &[ChatMsg]) -> AppResult<impl futures::Stream<Item=ChatDelta>>;
  // JSON-mode structured completion (roadmap gen etc). Deserializes T.
  pub async fn chat_json<T: DeserializeOwned>(&self, system: &str, user: &str) -> AppResult<(T, i64/*cost_cents*/)>;
  // Embed one text → Vec<f32> of len embedding_dim, plus cost.
  pub async fn embed(&self, text: &str) -> AppResult<(Vec<f32>, i64)>;
}
pub struct ChatMsg { pub role: String, pub content: String } // role: user|assistant|system
```
Token cost estimate: use OpenRouter usage (prompt/completion tokens) × a per-model rate table → cents; conservative default rate if unknown. This cents value is what wallet debits.

### vector module (`crate::vector`) — sqlite-vec
```rust
pub async fn ensure_extension(pool: &SqlitePool) -> anyhow::Result<()>; // load sqlite-vec on each conn (via pool after_connect in db.rs)
pub async fn upsert(pool: &SqlitePool, user_id: &str, kind: &str, source_id: &str, text: &str, embedding: &[f32]) -> AppResult<()>;
pub async fn search(pool: &SqlitePool, user_id: &str, embedding: &[f32], k: usize, kinds: &[&str]) -> AppResult<Vec<VecHit>>;
pub struct VecHit { pub kind: String, pub source_id: String, pub text: String, pub distance: f32 }
```
Use `sqlite-vec` crate for extension loading. If sqlx extension-loading proves awkward, register via `SqliteConnectOptions::extension`/`after_connect` — document whatever works. `kind` ∈ goal|step|message|tip.

### wallet module (`crate::wallet`)
```rust
#[derive(Clone)] pub struct PaymentProvider(Arc<dyn PaymentBackend>);
#[async_trait] pub trait PaymentBackend: Send+Sync {
  async fn create_topup(&self, user_id: &str, amount_cents: i64) -> anyhow::Result<TopupSession>; // returns checkout url or mock
  async fn verify_webhook(&self, sig: &str, body: &[u8]) -> anyhow::Result<WebhookEvent>;
}
// Ledger ops (SQL): credit_topup, debit_tokens(user, cents), pledge_hold, pledge_forfeit, pledge_refund, balance(user).
pub async fn debit_tokens(pool:&SqlitePool, user_id:&str, cents:i64, memo:&str) -> AppResult<()>; // called by chat handlers after LLM
pub async fn balance_cents(pool:&SqlitePool, user_id:&str) -> AppResult<i64>;
```
MockPaymentProvider (default when no Stripe key): instantly credits on topup, returns a fake success. Enforce min_topup/max_balance.

### notify module (`crate::notify`)
```rust
#[derive(Clone)] pub struct Notifier { /* Arc: db pool + sender channel */ }
impl Notifier {
  pub fn new(pool: SqlitePool) -> Self;
  pub fn spawn_scheduler(self, state: AppState); // background tokio loop: poll due reminders, deliver, mark sent, maybe LLM tip
  pub async fn enqueue(&self, user_id:&str, goal_id:Option<&str>, kind:&str, scheduled_at:&str, payload:serde_json::Value) -> AppResult<()>;
}
```
Delivery abstraction: for MVP store as in-app notification rows (GET /api/notifications) + log; push (FCM/web-push) behind a trait, stub ok. Scheduler tick e.g. every 60s.

### goals + chat
- Chat handler: load conversation history, RAG-retrieve top-k memory via vector::search over the user's embeddings, build system prompt (accountability coach), call `llm.chat_stream`, persist user+assistant messages, embed+upsert them, debit wallet by cost. Block with 402 PaymentRequired if balance ≤ 0.
- Roadmap generation: `llm.chat_json::<RoadmapDraft>` from a finalized goal → persist roadmap+steps, seed reminders via notifier, embed steps.

### mcp
Port rust_note `mcp/` (mod.rs mount, auth.rs JWT resource-server, server.rs #[tool_router]). Tools operate on the authenticated user's goals/roadmap/progress (e.g. list_goals, get_roadmap, mark_step_done, add_progress_note). Reuse config.mcp_resource_uri/audience.

## CI/CD & deploy (copy-adapt rust_note)
- `.github/workflows/`: ci-required.yml, ci-advisory.yml, docker-publish.yml, android-nightly.yml, android-release.yml — rename image to `ghcr.io/kukaraf/ai_buddy`, crates `server -p ai-buddy-core`, hostnames TBD, PUBLIC_API_BASE_URL placeholder.
- Dockerfile (3-stage: frontend node:22 → backend rust:bookworm → debian:bookworm-slim runtime). Env `AIBUDDY_*`. Note: sqlite-vec needs the extension present — either static-link/bundle via the `sqlite-vec` crate (preferred, no runtime .so) or COPY the .so into runtime. Prefer the crate's bundled approach.
- `.dockerignore`, `deploy/docker-compose.yml` (GHCR image, external caddy_proxy net, named volume for /data/db), `deploy/.env` (fresh AIBUDDY_COOKIE_SIGNING_KEY — generate, do not reuse rust_note's).
- `rust-toolchain.toml` (stable), `clippy.toml` (allow-*-in-tests).

## Frontend scaffold (may change — minimal)
- `web/`: SvelteKit + adapter-static like rust_note. Two screens: (1) Subscribe/Wallet, (2) Chat. Minimal api client (`$lib/api/client.ts`) hitting the API above. PUBLIC_API_BASE_URL + PUBLIC_APP_MODE env like rust_note.
- `crates/mobile/`: Tauri 2 wrapping web/build; identifier `dev.aibuddy.app`, deep-link scheme `dev.aibuddy.app`, product "AI Buddy". Copy structure from rust_note/crates/mobile.
