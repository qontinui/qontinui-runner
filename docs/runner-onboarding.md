# Runner onboarding — coord access checklist

This is the checklist a fresh runner must satisfy before it can reach coord and set gates. Each item below is one of the ordered checks run by `coord doctor` (plan 2026-06-13 Phase 4/5). A runner is not provisioning-complete until **all** of them pass.

<!-- GENERATED FILE — do not edit by hand. Regenerate via `coord_doctor --onboarding-doc` (or `cargo run --bin coord_doctor -- --onboarding-doc > docs/runner-onboarding.md`). The source of truth is `CHECK_SPECS` in `src-tauri/src/coord_doctor.rs`. -->

## Provisioning checklist

### 1. Claude account signed in (`claude_account`)

Verifies: a Claude account with live credentials is present (a valid ~/.claude/.credentials.json)

**Fix:** run /login

### 2. Runner tier is Qontinui account (`tier`)

Verifies: the runner tier RESOLVES to qontinui_account — either settings.json::tier says so, or the shared inference (`profiles::infer_tier`: a device pairing, or a legacy web_integration.runner_token) supplies it on a document that records no explicit operator choice. When a box that IS credentialed still resolves non-account, the report says so — "credentialed but not authorized" is a different failure from "no credential" and has a different fix

**Fix:** set runner tier to Qontinui account — app: Settings → Account; headless: `qontinui_profile device pair --pair-code <code>` promotes it, or launch with QONTINUI_SERVER_MODE=1; if this box is already paired and still reads non-account, a tier choice is pinning it — run `qontinui_profile tier --clear-choice`, or, on a `local_provider` document (which clearing alone does not re-open), `qontinui_profile tier --set qontinui_account`

### 3. Credential store readable (`credential_store_readable`)

Verifies: the credential store (OS keychain / on-disk slot) can be READ — placed ahead of every bearer-consuming check so an unreadable store reports itself instead of being misdiagnosed as 'not signed in' or 'no tenant'

**Fix:** credential store unreadable — check file permissions / OS keychain access

### 4. Paired and signed in (`paired_signed_in`)

Verifies: paired_user.json is present and a bearer is stored in the access-token slot

**Fix:** sign in / re-pair

### 5. Tenant resolvable (`tenant_resolvable`)

Verifies: a tenant_id resolves from the OAuth/runner-bearer claim, the outgoing device-JWT, or machine.json::active_tenant_id

**Fix:** machine.json missing active_tenant_id

### 6. Coord device JWT live (`device_jwt_live`)

Verifies: a live coord device JWT is present in the access-token slot and is not near expiry

**Fix:** kick refresher / re-pair

### 7. .mcp.json valid (`mcp_json_valid`)

Verifies: the session .mcp.json coord-mcp port equals the bound API port, its nonce is a registered proxy key, and the bearer is a coord device JWT

**Fix:** stale config — reprovision

### 8. Coord reachable (`coord_reachable`) — BLOCKING, ALWAYS RUNS

Verifies: a one-shot tools/list JSON-RPC round-trips 200 against the configured coord /mcp endpoint, using the SAME bearer the coord-mcp proxy would select

Always runs: this check's input does not depend on any check before it, so an earlier red does not suppress it. It is still **blocking** — a failure here withholds gate registration exactly as any other blocking check does.

**Fix:** coord unreachable

### 9. No inherited Claude session markers (`no_inherited_session_markers`) — ADVISORY

Verifies: this runner process did NOT inherit Claude Code's process-topology markers (CLAUDECODE, CLAUDE_CODE_CHILD_SESSION) from whatever launched it — a marked runner is mislabelled as a nested session

Advisory: a failure here is a **warning**, not a blocker — it does not stop gate registration and does not fail the report. It also runs even when an earlier check went red.

**Fix:** restart the runner from a shell without the markers (via dev-start.ps1 / the supervisor); spawns are stripped either way

### 10. .mcp.json carries the non-escalating header shape (`mcp_json_not_dcr_escalating`) — ADVISORY

Verifies: the coord-mcp proxy .mcp.json carries the nonce in a static `Authorization: Bearer` header, not only in the legacy `X-Coord-Mcp-Proxy-Key` one — a legacy-only file authenticates fine today and still makes the next MCP client launched against it escalate a stale-key 401 into OAuth discovery, Dynamic Client Registration, this runner's own 404, and a durable client-side poison entry

Advisory: a failure here is a **warning**, not a blocker — it does not stop gate registration and does not fail the report. It also runs even when an earlier check went red.

**Fix:** spawn a terminal in that workdir (every session spawn rewrites the file through the current emitter), or restart the runner so the boot self-heal upgrades it in place with the same nonce

---

`coord doctor` runs these checks live. The **blocking** checks stop at the first failure, naming that one link plus its fix — except any marked ALWAYS RUNS, which are blocking but independent of everything before them, so they execute anyway; **advisory** checks always run and only ever warn. Run it from **Settings → Account** in the runner app, or headless via the `coord_doctor` bin (`cargo run --bin coord_doctor`). Green on all of them ⇒ this runner can set gates.
