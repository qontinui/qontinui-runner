/**
 * Productivity Stack — Coordinator dashboard Tauri command wrappers.
 *
 * Thin async wrappers around the new Phase 2 commands the runner exposes
 * for the Coordinator dashboard. Field shapes are camelCase by virtue of
 * `serde(rename_all = "camelCase")` on the Rust structs in
 * `database/pg/coordinator_decisions.rs`.
 */

import { invoke } from "@tauri-apps/api/core";
import { spawnWithResourceGuard } from "@/lib/resourceGuard";

/** Discriminator strings written to `coordinator_decisions.rule`. */
export type CoordinatorRule = "A" | "B" | "C" | "D" | "E" | "LLM" | "idle" | string;

/** All actions the Coordinator can record. Matches the TS contract in
 *  productivity-stack §8. */
export type CoordinatorActionName =
  | "assign-task"
  | "pause-session"
  | "merge-task"
  | "reassign-needs-fix"
  | "escalate"
  | "advise-with-text"
  | "escalate-with-text"
  | "kill-session"
  | "force-promote-to-worktree"
  | "cancel-task"
  | "idle-no-action";

/** A single decision-log row. Open escalations are the subset where
 *  `autoActed === false` AND `action` is in the destructive set. */
export interface CoordinatorDecision {
  id: string;
  sessionId: string;
  iteration: number;
  rule: CoordinatorRule;
  action: CoordinatorActionName | string;
  targetId: string | null;
  reasoning: string;
  autoActed: boolean;
  resolved: boolean;
  resolution: string | null;
  resolvedAt: string | null;
  createdAt: string;
}

/** An open escalation — a `coordinator_decisions` row with
 *  `autoActed=false` and `resolved=false`. The Rust side returns the same
 *  `CoordinatorDecision` shape; alias kept for readability at call sites. */
export type Escalation = CoordinatorDecision;

/** Filter inputs for the Decision Log dropdown. Empty string means "no
 *  filter". */
export interface DecisionLogFilter {
  rule?: string;
  action?: string;
}

/** Fetch the Decision Log feed (newest-first), optionally filtered. */
export async function getCoordinatorDecisions(
  limit = 200,
  filter: DecisionLogFilter = {},
): Promise<CoordinatorDecision[]> {
  const ruleFilter = filter.rule && filter.rule.length > 0 ? filter.rule : null;
  const actionFilter = filter.action && filter.action.length > 0 ? filter.action : null;
  return invoke<CoordinatorDecision[]>("get_coordinator_decisions", {
    limit,
    ruleFilter,
    actionFilter,
  });
}

/** Fetch unresolved escalations (`autoActed=false`, destructive action). */
export async function getEscalations(): Promise<Escalation[]> {
  return invoke<Escalation[]>("get_escalations");
}

/** Mark an escalation resolved with a free-form `resolution` note. */
export async function resolveEscalation(decisionId: string, resolution: string): Promise<boolean> {
  return invoke<boolean>("resolve_escalation", { decisionId, resolution });
}

// ---------------------------------------------------------------------------
// Phase 3 — Coordinator launch controls (header buttons + lease status pill)
// ---------------------------------------------------------------------------

/** A single `coordinator_leader` lease row. Timestamps are RFC3339 UTC
 *  strings (the Rust side serializes them via `::text` from PG). */
export interface CoordinatorLeaderRow {
  instanceId: string;
  leasedUntil: string;
  acquiredAt: string;
  renewedAt: string;
}

/** Result of `get_coordinator_leader`. `leaseStatus` is computed Rust-side
 *  off `leased_until` vs `NOW()` and `renewed_at` heartbeat freshness:
 *  - `active`  — lease holder is alive (renewed within ~90s).
 *  - `stale`   — lease still owned but heartbeat older than ~90s.
 *  - `vacant`  — no row, or `leased_until` already in the past. */
export interface LeaderResponse {
  leader: CoordinatorLeaderRow | null;
  leaseStatus: "active" | "stale" | "vacant";
}

/** Result of `launch_coordinator_session` / `spawn_worker_session`.
 *
 *  - `mode` echoes back which path ran. For `launch_coordinator_session`
 *    it's `"rust"` (in-process scheduler — Phase 1.5 default) or
 *    `"claude_skill"` (legacy pty spawn). `spawn_worker_session` returns
 *    `"worker"`.
 *  - `terminalId` is `null` for `mode === "rust"` (no pty was opened);
 *    for the other modes it identifies the new tab so the sidebar
 *    state-machine can focus it.
 *  - `taskRunId` is the `task_run_id` under which a worker is registered
 *    with `SessionManager` (Phase 6). Populated only for `mode ===
 *    "worker"`; `null` for the other modes. */
export interface LaunchResult {
  mode: string;
  terminalId: string | null;
  taskRunId: string | null;
}

/** Read the current `coordinator_leader` row + derived lease status. */
export async function getCoordinatorLeader(): Promise<LeaderResponse> {
  return invoke<LeaderResponse>("get_coordinator_leader");
}

/** Coordinator launch modes. `"rust"` (the Phase 1.5 default) flips the
 *  in-process Rust scheduler's runtime-toggle flag — no pty, no Claude
 *  CLI dependency. `"claude_skill"` keeps the legacy pty-spawn path for
 *  Joshua's debug use. */
export type CoordinatorLaunchMode = "rust" | "claude_skill";

/** Start the Coordinator. Defaults to `mode = "rust"` (in-process Rust
 *  scheduler). When `mode === "claude_skill"`, spawns a Claude pty in a
 *  fresh Terminal tab — the legacy debug path. */
export async function launchCoordinatorSession(args: {
  mode?: CoordinatorLaunchMode | null;
  planPath?: string | null;
  titleHint?: string | null;
}): Promise<LaunchResult> {
  // Attended spawn — routed through the resource gate's override flow so a
  // CRITICAL refusal becomes the "Start anyway" dialog instead of a raw error
  // string in a panel. `mode: "rust"` opens no pty and never reaches the gate;
  // `mode: "claude_skill"` does.
  return spawnWithResourceGuard((resourceOverride) =>
    invoke<LaunchResult>("launch_coordinator_session", { ...args, resourceOverride }),
  );
}

/** Stop the in-process Rust coordinator scheduler — flips the
 *  `rust_scheduler_enabled` flag back to `false`. Returns the previous
 *  flag value (true ⇒ scheduler was running, false ⇒ already stopped).
 *
 *  Has no effect on `claude_skill`-mode pty sessions; the user closes
 *  those via the terminal UI. */
export async function stopCoordinatorSession(): Promise<boolean> {
  return invoke<boolean>("stop_coordinator_session");
}

/** Spawn an idle worker Claude session in a fresh Terminal tab. */
export async function spawnWorkerSession(args: {
  titleHint?: string | null;
}): Promise<LaunchResult> {
  // Attended spawn — see `launchCoordinatorSession`. A worker is the most
  // memory-hungry thing this app creates (a `claude` CLI that immediately starts
  // cargo builds), so this is the spawn the gate most often has an opinion on.
  return spawnWithResourceGuard((resourceOverride) =>
    invoke<LaunchResult>("spawn_worker_session", { ...args, resourceOverride }),
  );
}

// ---------------------------------------------------------------------------
// POST /coordinator/tasks/reset-stale — flip stale assigned/needs_fix tasks
// back to `ready` when their `assigned_session_id` no longer exists in
// SessionManager. Useful between test runs against pre-decomposed plan
// fixtures. Idempotent — second call sees the rows already in `ready` and
// reports them under `skipped`.
// ---------------------------------------------------------------------------

/** One task examined by the reset-stale sweep that was NOT flipped, with
 *  the stable reason string (`"status not assigned/needs_fix"`,
 *  `"session still alive"`, `"no assigned_session_id"`). */
export interface ResetStaleSkippedTask {
  taskId: string;
  status: string;
  assignedSessionId: string | null;
  reason: string;
}

/** Response shape for `POST /coordinator/tasks/reset-stale`. `reset` is the
 *  set of task ids that were (or, in `dryRun` mode, would be) flipped back
 *  to `ready`. `skipped` is the audit trail. */
export interface ResetStaleTasksResponse {
  reset: string[];
  skipped: ResetStaleSkippedTask[];
  dryRun: boolean;
}

/**
 * Pull the human-readable message out of a runner error body.
 *
 * The runner answers a failed control-plane call with its `ApiResponse`
 * envelope — `{"success":false,"error":"…","code":"…","hint":"…"}` — and the
 * three fetch wrappers below used to splice that WHOLE body into the thrown
 * message. It then reached the operator verbatim, so the Workers panel showed
 * JSON punctuation and internal keys (`{"success":false`, `"code":`) around
 * the one sentence that mattered, and a long body pushed that sentence off the
 * visible row entirely.
 *
 * Envelope-first, raw-body-last: anything that isn't a recognisable envelope
 * still surfaces (an HTML 502 from a proxy, a bare string) rather than being
 * swallowed — losing the message is strictly worse than showing it ugly.
 */
export function extractApiErrorMessage(body: string, statusText: string): string {
  const trimmed = body.trim();
  if (!trimmed) return statusText || "request failed";
  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch {
    return trimmed;
  }
  if (typeof parsed === "string") return parsed || statusText || trimmed;
  if (!parsed || typeof parsed !== "object") return trimmed;
  const obj = parsed as Record<string, unknown>;
  const nested = obj.error;
  const candidates: unknown[] = [
    typeof nested === "string" ? nested : undefined,
    nested && typeof nested === "object" ? (nested as Record<string, unknown>).message : undefined,
    obj.message,
    obj.detail,
  ];
  const message = candidates.find((c): c is string => typeof c === "string" && c.trim() !== "");
  if (!message) return trimmed;
  const hint = typeof obj.hint === "string" && obj.hint.trim() !== "" ? obj.hint : null;
  return hint ? `${message.trim()} (${hint.trim()})` : message.trim();
}

/**
 * Describe a THROWN value, whatever shape it arrived in.
 *
 * THE DEFECT this closes: every panel loader wrote
 * `err instanceof Error ? err.message : "<bare constant>"`. The `else` arm is
 * not a rare edge here — `invoke()` rejects with a plain STRING carrying the
 * Rust command's own error text, so a Tauri-backed loader took the branch that
 * DISCARDS the diagnosis on every failure and rendered only the constant. The
 * overlapping-intents panel therefore said `Failed to load overlap pairs` and
 * nothing else, no matter why it failed — a read-value on that element told an
 * operator (or an automation client) exactly as much as an empty panel would.
 *
 * Everything non-`Error` is serialized rather than dropped: a string verbatim,
 * an object's `status` / `code` / `error` / `message` / `detail` fields when it
 * has any (the runner's `ApiResponse` envelope and `fetch`-style rejections
 * both land here), and a JSON dump as the last resort. `fallback` is used ONLY
 * when the value carries no information at all — losing the cause is strictly
 * worse than showing it ugly, the same rule
 * {@link extractApiErrorMessage} follows.
 */
export function describeThrown(err: unknown, fallback: string): string {
  if (err instanceof Error) return err.message || fallback;
  if (typeof err === "string") return err.trim() || fallback;
  if (typeof err === "number" || typeof err === "boolean") return String(err);
  if (err && typeof err === "object") {
    const o = err as Record<string, unknown>;
    const status =
      typeof o.status === "number" || typeof o.status === "string" ? `HTTP ${o.status}` : null;
    const body = [o.error, o.message, o.detail, o.code].find(
      (c): c is string => typeof c === "string" && c.trim() !== "",
    );
    if (status && body) return `${status}: ${body.trim()}`;
    if (body) return body.trim();
    if (status) return status;
    try {
      const dump = JSON.stringify(err);
      if (dump && dump !== "{}") return `${fallback} (${dump})`;
    } catch {
      // Circular / non-serializable — fall through to the fallback.
    }
  }
  return fallback;
}

/**
 * Shared throw site for the runner-port fetch wrappers. One place decides how
 * a non-2xx becomes an Error, so the three call sites can't drift on it.
 */
async function throwApiError(res: Response): Promise<never> {
  const body = await res.text().catch(() => "");
  throw new Error(`HTTP ${res.status}: ${extractApiErrorMessage(body, res.statusText)}`);
}

/** Flip stale `assigned`/`needs_fix` tasks back to `ready` when the worker
 *  they're pinned to is no longer alive in SessionManager. Pass
 *  `{ dryRun: true }` to preview without writing. Mirrors the runner-port
 *  fetch pattern used by the manual-user-fire form (calls
 *  `get_api_port` then POSTs to the resolved port). */
export async function resetStaleTasks(
  args: { dryRun?: boolean } = {},
): Promise<ResetStaleTasksResponse> {
  const port = await invoke<number>("get_api_port");
  if (!port || port <= 0) throw new Error(`invalid runner port: ${port}`);
  const qs = args.dryRun ? "?dryRun=true" : "";
  const res = await fetch(`http://localhost:${port}/coordinator/tasks/reset-stale${qs}`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: "",
  });
  if (!res.ok) {
    await throwApiError(res);
  }
  return (await res.json()) as ResetStaleTasksResponse;
}

// ---------------------------------------------------------------------------
// GET /coordinator/state — snapshot of live sessions + non-terminal tasks.
// Used by the Workers panel so the dashboard can show users the workers they
// just spawned without auto-revealing the terminal tab. Mirrors the runner-
// port fetch pattern (no dedicated Tauri command exists for this endpoint).
// ---------------------------------------------------------------------------

/** A single live session row from `GET /coordinator/state`. `terminalId`
 *  and `title` are populated only for pty-backed Workers (Phase 6); for
 *  ClaudeSessions both are absent (the Rust side uses
 *  `skip_serializing_if = "Option::is_none"`). */
export interface LiveSession {
  taskRunId: string;
  state: string;
  isActive: boolean;
  terminalId?: string | null;
  title?: string | null;
}

/** Subset of the `/coordinator/state` payload the Workers panel consumes.
 *  The endpoint returns more fields (file registries, escalations, etc.)
 *  but the panel only needs `liveSessions` + `tasks` (for the
 *  assigned-task lookup that surfaces "task X — description" on
 *  Processing rows). Extra fields are ignored. */
export interface CoordinatorState {
  liveSessions: LiveSession[];
  tasks: Array<{
    id: string;
    status: string;
    assignedSessionId: string | null;
    updatedAt: string;
    description: string;
  }>;
}

/** Fetch the composite coordinator state snapshot. Mirrors
 *  `resetStaleTasks` — calls `get_api_port` then GETs the HTTP endpoint
 *  on the runner's API server. */
export async function getCoordinatorState(): Promise<CoordinatorState> {
  const port = await invoke<number>("get_api_port");
  if (!port || port <= 0) throw new Error(`invalid runner port: ${port}`);
  const res = await fetch(`http://localhost:${port}/coordinator/state`, {
    method: "GET",
    headers: { Accept: "application/json" },
  });
  if (!res.ok) {
    await throwApiError(res);
  }
  return (await res.json()) as CoordinatorState;
}

/** Discriminated wire shape consumed by `POST /coordinator/dispatch-action`.
 *  Mirrors `CoordinatorAction` (kebab-case `type`, camelCase fields). Only
 *  the variants the deferrals panel actually fires are listed; extend as
 *  more dashboard surfaces need manual dispatch. */
export type DispatchCoordinatorAction = {
  type: "assign-task";
  taskId: string;
  sessionId: string;
  reasoning?: string;
};

// ---------------------------------------------------------------------------
// Row 9 Phase 4 — fleet health + alerts (FleetHealthPanel).
//
// `get_fleet_health` is a thin Tauri proxy to coord's
// `/coord/fleet/health` + `/coord/alerts` (the browser can't watch the
// `fleet-health` NATS KV bucket directly). coord republishes each 30s
// poll over the Redis/JS bridge, so a ~1s panel poll renders fleet
// state well inside the "<1s after KV update" target without a
// browser-side NATS client.
// ---------------------------------------------------------------------------

/** One machine's latest health snapshot (coord
 *  `MachineHealthSnapshot`; snake_case on the wire — coord serializes
 *  the Rust struct field names verbatim). */
export interface FleetMachineSnapshot {
  machine_id: string;
  hostname: string;
  state: "healthy" | "degraded" | "partitioned" | "abandoned";
  state_changed_at: string;
  last_probe_at: string | null;
  last_probe_ok: boolean | null;
  consecutive_failures: number;
  agents_active: number;
  updated_at: string;
}

/** One active/firing alert row from `coord.alerts`. */
export interface FleetAlert {
  id: number;
  alert_key: string;
  severity: "info" | "warning" | "critical";
  kind: string;
  machine_id: string | null;
  summary: string;
  detail: Record<string, unknown>;
  first_seen_at: string;
  last_seen_at: string;
  occurrences: number;
  resolved_at: string | null;
  page_due_at: string | null;
}

/** Structured device-auth state for the fleet reads.
 *
 * coord is anonymous today, so this is `ok` in practice; once coord
 * starts gating `/coord/fleet/health` + `/coord/alerts` (a later phase)
 * the backend distinguishes an auth rejection (401/403) from a transport
 * error so the panel can render an honest auth state rather than the
 * "coord unreachable" banner:
 * - `ok`           — normal
 * - `unauthorized` — a device token was present but coord rejected it
 *                    (rejected/expired) → re-pair to restore the view
 * - `unpaired`     — no device token locally and coord 401/403'd → pair
 *
 * Optional for back-compat with an older backend during dev (absent →
 * treated as `ok`). */
export interface FleetAuth {
  state: "ok" | "unauthorized" | "unpaired";
}

/** Merged payload from the `get_fleet_health` command.
 *
 * `health` is `null` when coord returned an auth rejection (401/403) —
 * the structured `auth` state drives the panel in that case rather than
 * the stale machine grid. */
export interface FleetHealth {
  health: {
    machines: FleetMachineSnapshot[];
    count: number;
    by_state: Record<string, number>;
    alerts: { critical: number; warning: number; info: number };
    kv_bucket: string;
    as_of: string;
  } | null;
  alerts: FleetAlert[];
  /** `null` on the isolated arm — there is no coordinator, and naming a
   *  guessed one is what routing through the Option family removed. */
  coordBase: string | null;
  /** True when the BACKEND resolved the runner as isolated and refused to
   *  dial. Distinct from the `CoordModeContext` gate, which is the same
   *  answer read from the frontend: the gate fails OPEN on an unresolved
   *  mode, so during the mount window — and permanently on a runner build
   *  whose `get_coord_mode` rejects — this field is the only thing that
   *  stops an empty grid rendering as a healthy fleet of zero machines.
   *
   *  Optional for back-compat with an older backend (absent → not isolated,
   *  i.e. exactly the pre-§6.4 behaviour). */
  isolated?: boolean;
  /** Optional for back-compat with an older backend (absent → `ok`). */
  auth?: FleetAuth;
}

/** Fetch coord's fleet-health rollup + active alerts via the runner
 *  proxy command. Throws on coord-unreachable so the panel can show a
 *  retriable error (coord down ≠ runner down). */
export async function getFleetHealth(): Promise<FleetHealth> {
  return invoke<FleetHealth>("get_fleet_health");
}

/** Fire an arbitrary CoordinatorAction through the manual dispatch path.
 *  Audited as `rule = "manual"`, `sessionId = "manual-<uuid>"`. */
export async function dispatchCoordinatorAction(
  action: DispatchCoordinatorAction,
  reasoning?: string,
): Promise<void> {
  const port = await invoke<number>("get_api_port");
  if (!port || port <= 0) throw new Error(`invalid runner port: ${port}`);
  const res = await fetch(`http://localhost:${port}/coordinator/dispatch-action`, {
    method: "POST",
    headers: { "Content-Type": "application/json", Accept: "application/json" },
    body: JSON.stringify({ action, reasoning }),
  });
  if (!res.ok) {
    await throwApiError(res);
  }
}

// ---------------------------------------------------------------------------
// Coordination Phase 1B (§4.10) — Overlapping intents
// ---------------------------------------------------------------------------

/** Unordered pair of agents whose declared_overlap_paths intersect.
 *  `agentA` <= `agentB` lexically so the pair is stable across calls.
 *  `intentA` / `intentB` are free-text strings shown in the dashboard;
 *  `overlappingPaths` is the de-duplicated intersection (literal +
 *  glob-expanded — same shape coord publishes on
 *  `events.coord.overlap.detected`). */
export interface OverlappingIntentPair {
  agentA: string;
  agentB: string;
  intentA: string | null;
  intentB: string | null;
  overlappingPaths: string[];
}

/** Fetch the L2 overlapping-intents snapshot.
 *  Coord computes the pairs and serves them over HTTP
 *  (GET /coord/agent-worktrees/overlapping-intents); the runner no longer
 *  queries coord.agent_worktrees itself. An isolated or unreachable coord
 *  yields an empty list, never an error.
 *  Read-only — the panel is informational, no actions taken from
 *  client-side. */
export async function listOverlappingIntents(limit = 200): Promise<OverlappingIntentPair[]> {
  return invoke<OverlappingIntentPair[]>("list_overlapping_intents", { limit });
}

// ---------------------------------------------------------------------------
// Wave 4 (Phase 4) — Spawn-from-Plan
//
// `spawnFromPlan` is the runner-side mirror of the qontinui-web
// SpawnModal. Coord owns the spawn endpoint (`POST /agents/spawn`);
// the dashboard's "Spawn from Plan" button gives Joshua + small-fleet
// operators an in-runner affordance so they don't have to flip to the
// browser console to spawn agents during readiness rollouts.
//
// The coord base is now resolved by the runner backend
// (`get_coord_http_base`: env → profile `coord_url` → localhost), so the
// in-app button reaches the SAME coord as backend session-sync rather than
// a hardcoded localhost. The POST itself is still a plain `fetch` and coord
// is LAN-trusted in the current pilot posture; the auth hardening (device
// JWT on this call + coord-side gating, tracked in the Bar-2 / Phase-7
// fleet-auth plan) lands separately before serving non-trusted networks.
// ---------------------------------------------------------------------------

/** Wire shape returned by coord's POST /agents/spawn. Extra fields are
 *  passed through; only `agentId` is load-bearing on the dashboard
 *  toast (so the operator sees "agent-deadbeef spawned"). */
export interface SpawnAgentResult {
  agentId?: string;
  agentSessionId?: string;
  deviceId?: string;
  status?: string;
  [k: string]: unknown;
}

/** Spawn an agent from a work-unit/phase tuple via coord.
 *
 *  Mirrors the web SpawnModal contract verbatim:
 *    - `workUnitSlug` + `phase` identify the work unit + phase the agent
 *      is working under. Coord renamed this wire key from `plan_slug`
 *      (plan `2026-07-28-coord-post-plan-slug-surfaces-rename`); the Rust
 *      command sends `work_unit_slug` and only that key — see
 *      `spawn_request_body` for why sending both would 400;
 *    - `phase` is free text here but a NUMBER on coord's wire — the Rust
 *      command reduces "4" / "Phase 4" to `4` and rejects non-numeric input
 *      rather than silently dropping it;
 *    - `repos` is the list of repo slugs the agent declares — the Rust
 *      command expands them into coord's `[{repo}]` spec shape;
 *    - `intent` is a one-liner describing the work;
 *    - `initialPrompt` is the first-tick message coord delivers.
 *
 *  `target_device_id` is NOT a parameter: coord requires it and the runner
 *  fills in its own device, since this modal has no device picker.
 *
 *  `declaredOverlapPaths` is optional — when the operator declares
 *  overlap up-front, coord can pre-detect conflicts in the L2
 *  overlap-detection layer. When omitted, coord computes the overlap
 *  lazily as the agent starts editing.
 */
export async function spawnFromPlan(
  workUnitSlug: string,
  phase: string,
  repos: string[],
  intent: string,
  initialPrompt: string,
  declaredOverlapPaths?: string[],
  coordBase?: string,
): Promise<SpawnAgentResult> {
  // The spawn runs through an authenticated Rust Tauri command: production
  // coord gates `POST /agents/spawn` on operator SSO, and the operator's
  // Cognito bearer lives ONLY in the Rust backend (never the frontend). The
  // command resolves the coord base itself (same source as
  // `get_coord_http_base`) and attaches the Cognito access token. Tauri
  // auto-converts snake_case Rust params to camelCase JS keys.
  //
  // `coordBase` is retained on the signature for the unit tests that pass it,
  // but the backend command owns base resolution now (it is not forwarded).
  void coordBase;
  const res = await invoke<SpawnAgentResult>("spawn_from_plan", {
    workUnitSlug,
    planPhase: phase,
    repos,
    intent,
    initialPrompt,
    declaredOverlapPaths: declaredOverlapPaths ?? [],
  });
  return res;
}
