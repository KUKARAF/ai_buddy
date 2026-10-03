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

## 🟢 Claude can do alone (no external accounts)
Server + client code, CI, deploy. These can be built/feature-flagged **before** credentials exist
(they stay inert until the secrets below are present).

### Server (Rust)
- [ ] FCM HTTP v1 **send** in `notify::deliver()` + from the coach `post_now` path: mint an OAuth token from the service account (`gcp_auth`) and `POST fcm.googleapis.com/v1/projects/{id}/messages:send` via `reqwest`.
- [ ] Send **data-only** messages (keep coach text off Google's wire; render locally).
- [ ] **Stale-token hygiene**: on FCM `UNREGISTERED`/404, delete that `push_tokens` row.
- [ ] Config plumbing: `AIBUDDY_FCM_PROJECT_ID` + `GOOGLE_APPLICATION_CREDENTIALS` (path to the service-account JSON); **feature-flagged off** when unset (push silently disabled, in-app still works).
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

## 🔴 Needs you (Claude cannot — external accounts / credentials)
Claude has no access to your Google account and cannot create these.

- [ ] **Create a Firebase project** (free Spark tier, no billing card). Console → Add project.
- [ ] **Add an Android app** in that project with package id **`dev.aibuddy.app`**.
- [ ] **Download `google-services.json`** → hand it to me (I'll store it as a GH Actions secret; it's low-sensitivity but kept out of git).
- [ ] **Generate a service-account key**: Firebase console → Project settings → Service accounts → *Generate new private key* → download the JSON. This is the **FCM send credential (full send authority — keep secret)**. Hand it to me to place on bigboy, or place it yourself at `~/env/osmosis/buddy/fcm-service-account.json` (chmod 600).
- [ ] Tell me the **Firebase project id**.
- [ ] Acknowledge: FCM message **metadata** transits Google (mitigated — we send data-only messages, text rendered on-device).
- [ ] On your phone: install the new nightly APK and **grant notification permission** (Android 13+ asks at runtime).

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
