# Notifications — To-Do & ownership split

Tracks the notifications feature. Split into **what Claude can do alone** (code / CI / deploy)
vs **what needs you** (external accounts & secrets Claude cannot create).

Chosen push path: **FCM / Firebase** (you're on standard Android with Play Services).
In-app delivery is already live; this file is mostly about **push-to-closed-app**.

---

## ✅ Done & deployed
- [x] In-app notifications store + `GET /api/notifications`, read-state (`read_at`), `PATCH /api/notifications/{id}/read`, `POST /api/notifications/read-all` (migration 0017).
- [x] Notifications **bell + feed** in the web app (unread badge, coach cards, mark-read).
- [x] **Coach review on every check-in** — progress/risk assessment + warm message, posted in-app + shown inline; anti-spam cadence (status-change or ~1/day).
- [x] `push_tokens` table + `POST /api/push-tokens` (pre-existing; ready for a real token).
- [x] `Notifier::post_now` + channel-agnostic notifications (so push can deliver the same rows).

---

## 🟢 Claude can do alone — ✅ DONE & DEPLOYED (2026-10-03, commit 9605978)
All built, gated, shipped. Server-side FCM is now ACTIVE in prod (credential mounted at
`/secrets/fcm.json`, `AIBUDDY_FCM_PROJECT_ID` + `AIBUDDY_FCM_CREDENTIALS` set, health 200).
The Firebase-injected nightly APK built successfully and is on the GitHub nightly release.

- [x] Server FCM send (`crates/server/src/fcm/mod.rs`): data-only HTTP v1, OAuth via jsonwebtoken (no new dep; enabled `use_pem`), stale-token pruning. Hooked into the coach flow + reminder scheduler, best-effort.
- [x] Config `AIBUDDY_FCM_PROJECT_ID` + `AIBUDDY_FCM_CREDENTIALS`; credential bind-mounted in the prod compose.
- [x] Tauri push plugin `tauri-plugin-notifications` =0.5.0-rc.14 (push-notifications feature) + capabilities.
- [x] CI injection of `google-services.json` (secret `GOOGLE_SERVICES_JSON_B64`) + Google Services gradle plugin in android-nightly.yml + android-release.yml (fail-loud on template drift).
- [x] Frontend `registerPushIfTauri()` → `POST /api/push-tokens` on login (Tauri-guarded).

### 🔴 Only remaining step — you
- [ ] Install the latest **nightly APK** (from GitHub Releases) and **grant notification permission** → then a check-in triggers a coach push to your phone.

### (original plan, for reference)

### Server (Rust)
- [ ] FCM HTTP v1 **send** in `notify::deliver()` + from the coach `post_now` path: mint an OAuth token from the service account (`gcp_auth`) and `POST fcm.googleapis.com/v1/projects/{id}/messages:send` via `reqwest`.
- [ ] Send **data-only** messages (keep coach text off Google's wire; render locally).
- [ ] **Stale-token hygiene**: on FCM `UNREGISTERED`/404, delete that `push_tokens` row.
- [ ] Config plumbing: read `AIBUDDY_FCM_PROJECT_ID` (✅ already in prod .env = `ai-buddy-4063c`) + `GOOGLE_APPLICATION_CREDENTIALS`; **feature-flagged off** when unset (push silently disabled, in-app still works).
- [ ] **Mount the credential into the container**: add a `./fcm-service-account.json:/secrets/fcm.json:ro` bind to `docker-compose.yml` and set `GOOGLE_APPLICATION_CREDENTIALS=/secrets/fcm.json` in `.env` (the key file is already on bigboy at `~/env/osmosis/buddy/fcm-service-account.json`).
- [ ] Tests + clippy/fmt gate.

### Mobile (Tauri Android)
- [ ] Add `tauri-plugin-notifications` (Choochmeque fork, `push-notifications` feature) to `crates/mobile`: register plugin + capability.
- [ ] On login, call `registerForPushNotifications()` → send the FCM token to `POST /api/push-tokens`.
- [ ] Handle incoming data message → show a local notification + refresh the feed.
- [ ] (Bonus, no push infra needed) on-device **scheduled** notifications for milestone due-dates.

### CI
- [ ] `android-nightly.yml` **and** `android-release.yml`: after `tauri android init`, inject `google-services.json` (from a base64 GH secret) + add the Google Services gradle plugin (mirrors the existing icon-overlay step).

### Ops (Claude can do **once you hand over the files** — see below)
- [ ] `gh secret set GOOGLE_SERVICES_JSON_B64` (once you give me the file).
- [ ] `scp` the service-account JSON to `~/env/osmosis/buddy/` on bigboy (chmod 600) + add the env vars to `.env`, then redeploy.

---

## 🔴 Needs you — Firebase setup ✅ DONE (2026-10-03, via Chrome in your account)
Done together in your logged-in `rafal.kuka94` session:

- [x] **Firebase project created**: `ai-buddy` (project id **`ai-buddy-4063c`**), free Spark plan, under rafal.kuka94. (Google Analytics / Gemini / Dev Programme all left OFF.)
- [x] **Android app registered** with package id **`dev.aibuddy.app`** (nickname "AI Buddy").
- [x] **`google-services.json`** downloaded → stored as GH Actions secret **`GOOGLE_SERVICES_JSON_B64`** (base64). Local copy wiped.
- [x] **Service-account key** generated → scp'd to bigboy **`~/env/osmosis/buddy/fcm-service-account.json`** (chmod 600). Local copy shredded. Service account: `firebase-adminsdk-fbsvc@ai-buddy-4063c.iam.gserviceaccount.com`.
- [x] **`AIBUDDY_FCM_PROJECT_ID=ai-buddy-4063c`** added to prod `.env`.

### 🔴 Still needs you (later, after the APK build)
- [ ] On your phone: install the new nightly APK (once the FCM client code + CI injection land) and **grant notification permission** (Android 13+ asks at runtime).
- [ ] (FYI, no action) FCM message **metadata** transits Google — mitigated: we send data-only messages, text rendered on-device.

---

## 🕓 Later / optional
- [ ] Per-user notification **preferences** (opt-out, frequency, quiet hours) — small settings addition.
- [ ] **ntfy** fallback for any de-Googled device (no Google dependency).
- [ ] Consolidate `classify_note` + `coach_review` into one LLM call (save a round-trip/cost).
- [ ] Richer coach actions (e.g. coach can ask a question that you answer inline → feeds next check-in).

---

### Suggested order
1. Claude builds the 🟢 server + mobile + CI pieces now (inert without creds).
2. You do the 🔴 Firebase steps and hand over the two files.
3. Claude sets the secret + credential, redeploys, triggers an APK build.
4. Install APK, grant permission, do a check-in → push lands on your phone. 🎉
