# AI Buddy

> Working name only — to be renamed later.

An AI **accountability / personal-trainer / roadmap-creator**. You subscribe with a
small prepaid balance (min €4, up to €100) that covers your AI token usage **and** can
be pledged to a goal you agree on with the coach. The app helps you define a concrete
goal + deadline, builds a step-by-step roadmap, and sends reminders, tips, and
check-ins to keep you on track.

Rust backend mirroring the [`rust_note`](../rust_note) stack. One service serves a
**web** frontend and an **Android APK** (Tauri).

## Stack
- **Backend:** Rust — axum 0.8, tokio, sqlx + SQLite (WAL), tower-sessions (SQLite store).
- **Auth:** OIDC (Authentik) via `openidconnect` (PKCE) + bearer device tokens for the app.
- **LLM:** OpenRouter (chat, JSON-mode, SSE streaming, embeddings) via `reqwest`.
- **RAG memory:** embeddings stored as `f32` BLOBs in SQLite, brute-force cosine search
  in Rust — no extra service. (sqlite-vec / LanceDB are the documented scale-up path;
  see `docs/ARCHITECTURE.md`.)
- **Payments:** Stripe (real HMAC-verified webhooks) behind a `PaymentBackend` trait,
  with an instant-credit mock for local dev.
- **MCP:** remote Model Context Protocol server at `/mcp` (rmcp), OAuth 2.1 resource
  server validating Authentik-issued JWTs; tools operate on the user's goals/roadmap.
- **Frontend:** SvelteKit (adapter-static) + Tauri 2 Android.

## Workspace layout
```
crates/core     ai-buddy-core: shared domain types + device-token gen (no axum/sqlx/tauri)
crates/server   the axum binary: auth, db, llm, vector, goals, chat, wallet, notify, mcp
crates/mobile   Tauri 2 Android app wrapping web/build
web/            SvelteKit SPA (Subscribe/Wallet + Chat screens)
crates/server/migrations/  0001 users+device_tokens · 0002 wallet · 0003 goals ·
                           0004 chat · 0005 notify · 0006 vectors
```
Full design + module contracts: **`docs/ARCHITECTURE.md`**.

## Run locally (dev mode — auth bypassed, user "admin")
```bash
# Backend (bypasses OIDC, binds loopback only, instant-credit mock payments):
AIBUDDY_ENV=dev cargo run -p server
# → listening on 127.0.0.1:8080 ; GET /health → ok ; GET /auth/me → the admin user

# To exercise the AI features, add an OpenRouter key:
AIBUDDY_ENV=dev AIBUDDY_OPENROUTER_API_KEY=sk-or-... cargo run -p server

# Frontend (separate terminal; talks to the backend via CORS in dev):
cd web && npm install && npm run dev      # http://localhost:5173
```

Check everything the way CI does:
```bash
cargo fmt --all --check
cargo build --workspace --exclude mobile
cargo test  --workspace --exclude mobile
cargo clippy -p server -p ai-buddy-core --lib --bins -- -D warnings
cd web && npm ci && npm run check && npm run build
```

## Environment variables (all prefixed `AIBUDDY_`)
| Var | Default | Notes |
| --- | --- | --- |
| `AIBUDDY_ENV=dev` / `AIBUDDY_DEV_MODE=true` | off | Bypass auth; loopback bind only |
| `AIBUDDY_BASE_URL` | `http://localhost:8080` | Real public URL in prod (required outside dev) |
| `AIBUDDY_BIND_ADDR` | `127.0.0.1:8080` | Set `0.0.0.0:8080` in a container |
| `AIBUDDY_SQLITE_PATH` | `./data/ai_buddy.db` | |
| `AIBUDDY_STATIC_DIR` | unset | Path to `web/build` to serve the SPA from the backend |
| `AIBUDDY_COOKIE_SIGNING_KEY` | insecure dev default | ≥32 bytes; **required** outside dev |
| `AIBUDDY_AUTHENTIK_ISSUER_URL` / `AIBUDDY_OIDC_CLIENT_ID` / `AIBUDDY_OIDC_REDIRECT_URI` | localhost dev values | OIDC (Public/PKCE client; secret optional) |
| `AIBUDDY_OPENROUTER_API_KEY` | unset | Required for chat / roadmap / embeddings |
| `AIBUDDY_CHAT_MODEL` | `anthropic/claude-3.5-sonnet` | OpenRouter model id |
| `AIBUDDY_EMBEDDING_MODEL` / `AIBUDDY_EMBEDDING_DIM` | `openai/text-embedding-3-small` / `1536` | |
| `AIBUDDY_STRIPE_SECRET_KEY` / `AIBUDDY_STRIPE_WEBHOOK_SECRET` | unset | Unset → instant-credit mock |
| `AIBUDDY_MIN_TOPUP_CENTS` / `AIBUDDY_MAX_BALANCE_CENTS` | `400` / `10000` | €4 min, €100 cap |
| `AIBUDDY_CORS_ORIGINS` | dev localhost + `http://tauri.localhost` | Comma-separated; replaces the list |

## API surface (all `/api/*` require auth)
- `GET /health` · OIDC `/auth/login|/auth/callback|/auth/logout|/auth/me`
- **Wallet:** `GET /api/wallet` · `POST /api/wallet/topup` · `GET /api/wallet/ledger` ·
  `POST /api/goals/{id}/pledge` · `POST /api/goals/{id}/pledge/confirm` ·
  `POST /api/webhooks/stripe`
- **Goals:** `GET/POST /api/goals` · `GET/PATCH /api/goals/{id}` ·
  `POST/GET /api/goals/{id}/roadmap` · `PATCH /api/steps/{id}`
- **Chat:** `POST/GET /api/conversations` · `GET /api/conversations/{id}` ·
  `POST /api/conversations/{id}/messages` (SSE stream)
- **Notify:** `GET /api/notifications` · `POST /api/push-tokens`
- **MCP:** `/mcp` (streamable-HTTP) · `/.well-known/oauth-protected-resource`

## Deploy / CI
- GitHub Actions: `ci-required` (fmt + build + test + clippy gate + frontend),
  `ci-advisory` (pedantic clippy PR comment), `docker-publish` (GHCR image),
  `android-nightly` / `android-release` (signed APKs). See `.github/workflows/`.
- Single-container image (`Dockerfile`, 3-stage) + `deploy/docker-compose.yml`
  (GHCR image behind an external Caddy proxy network, named volume for the DB).
- Deploy secret: `AIBUDDY_COOKIE_SIGNING_KEY` in `deploy/.env` (gitignored).
