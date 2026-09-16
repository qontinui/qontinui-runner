import { useState, useCallback, useRef, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { TerminalExitEvent, TerminalInfo } from "@qontinui/shared-types/tauri-events";
import type { CommandResponse, TerminalSessionRecord } from "./types";
import {
  buildSessionCloseArgs,
  type FrontendSessionCloseReason,
  type SessionCloseArgs,
} from "./sessionRecordArgs";
import { createLogger } from "@/lib/logger";
import { spawnWithResourceGuard } from "@/lib/resourceGuard";
import { applyRemoteMark, type RemoteTabIdentity } from "./remoteTabs";
import {
  EMPTY_HIDDEN_WORKER_STATE,
  beginRestore,
  forgetAdoptedWorker,
  hideWorker,
  recordRestoreMisses,
  type HiddenWorkerState,
} from "./hiddenWorkerReducer";

const logger = createLogger("TerminalManager");

export interface TerminalTab {
  id: string;
  title: string;
  pid: number | null;
  isAlive: boolean;
  exitCode: number | null;
  workingDir?: string;
  createdAt?: number;
  /** Tab type: "terminal" (default) or "plan" (markdown viewer) */
  type?: "terminal" | "plan";
  /** Absolute path to the markdown file (only for plan tabs) */
  planFilePath?: string;
  /** True while the frontend is replaying the scrollback buffer from Rust. */
  isReconnecting?: boolean;
  /**
   * True when a boot-restore typed `claude --resume` into this tab but the
   * Claude UI handshake never appeared (after one retry) — the pane is most
   * likely still a bare shell. Surfaced as an explicit operator-clickable
   * "resume failed — retry" affordance (`ResumeFailedBanner`); cleared when a
   * retry verifies. While set, the durable record keeps its backend
   * restore-pending marker so the liveness poll can't flip it `poll-dead`.
   */
  resumeFailed?: boolean;
  /**
   * True when a boot-restore re-created this tab for a record that restores
   * TERMINAL-ONLY (Phase 5 honest tiers): the terminal + cwd are back but the
   * conversation was NOT resumed. Three sources land here — an authoritative
   * phantom shell (a spawn-time provisional record with no confirmed provider
   * session), a CONFIRMED authoritative record whose provider's
   * `restoreTier()` is `"terminal-only"` (it can re-open the terminal but
   * cannot deterministically `--resume` the chat by id), or a `"reconciled"`
   * (backstop-guessed) origin — the guess isn't strong enough to act on, so it
   * is treated the same as no match found: do nothing beyond an honest
   * restore. No resume is typed and NO confirm banner is shown; instead the
   * `ResumeFailedBanner`'s informational "fresh conversation" note surfaces it
   * so the user is never misled into thinking the conversation came back.
   * Cleared once the user dismisses the note or the tab is otherwise used.
   */
  restoreTerminalOnly?: boolean;
  /**
   * F1/F2 — the tenant this session was spawned under, as sent to
   * `terminal_create` (and therefore exactly what the Rust registry stamped
   * onto `Intent.tenant_id`). Immortal for the tab's life: switching the
   * device's active tenant never migrates a running session, so this is NOT
   * re-read from `TenantContext`. Absent on tabs restored from a durable
   * record written before F2 — `TenantBadge` renders nothing rather than
   * guessing.
   */
  tenantId?: string;
  /** Claude Code session ID running in this tab (set on resume). */
  claudeSessionId?: string;
  /** Claude config dir for the session (set on resume). */
  claudeConfigDir?: string;
  /**
   * Orchestration `task_run_id`, copied from the `TerminalSessionRecord` of a
   * Conductor worker (`dispatch_subtask` in
   * `orchestration_loop/ai_session_executor.rs`). `workerTabFromRecord` is
   * its only writer and always sets {@link sessionBacked} beside it, so a tab
   * carrying this is a worker view and never a pty tab.
   *
   * It is an identity, not a behaviour switch: `WorkerSessionCell` keys the
   * conversation and steering channels on it, and `closeTerminal` records it
   * so a hidden worker can be found again. The `Worker N` title pin is NOT
   * enforced from here — see `ZoneGrid::onTitleChange`, which no longer tests
   * this field because a worker never mounts the `TerminalInstance` that
   * reports OSC titles.
   */
  taskRunId?: string;
  /**
   * True when this tab is backed by an in-process stream-json
   * `ClaudeSession` registered in the Rust `SessionManager` — a Conductor
   * worker (`dispatch_subtask`) — and NOT by a PTY. `id === taskRunId`, `pid`
   * is null, and there is no terminal process to attach to: `terminal_list`
   * never lists it, `terminal_close` cannot kill it, and `terminal-output`
   * never carries its text. The grid renders it through `WorkerSessionCell`
   * (conversation via `ai-output` / `claude-session-state`, steering via
   * `send_user_message`) instead of `TerminalInstance`; `reconcileTabsWithBackend`
   * keeps it across `terminal_list` re-syncs; `closeTerminal` drops the tab
   * without touching the worker, whose lifetime the Conductor owns.
   */
  sessionBacked?: boolean;
  /**
   * True when the PTY child runs Claude with tool permissions bypassed
   * (`--dangerously-skip-permissions` or `--permission-mode bypassPermissions`).
   * Set from the Rust `terminal-bypass-permissions` event, which the runner
   * emits (after `terminal-created`) when it command-sniffs a bypass flag on
   * the spawn command. Threaded into `useSessionStateTracking` →
   * `detectSessionState` so approval-shaped TTY patterns are suppressed for
   * these sessions — a bypass session can never await tool approval, so any
   * approval-shaped match is a phantom (the 12-min `rm -rf` misread,
   * 2026-06-07; see `sessionStateDetector.ts`). Absent/false on every
   * non-bypass tab.
   */
  bypassPermissions?: boolean;
  /**
   * Set when this tab mirrors a session on ANOTHER device (plan
   * `2026-08-31-remote-session-tabs-in-runner-terminal`, Phase 4). Arrives on
   * the `terminal-remote-identity` event that follows `terminal-created`, or
   * from `terminal_remote_identities` on reconnect. Drives the device badge,
   * the reattach / earlier-output affordances, the project-reconcile exemption
   * (a remote cwd is not this machine's), and the restart placeholder. Absent
   * on every local tab.
   */
  remote?: RemoteTabIdentity;
  /**
   * Marks a tab synthesized from a debug-gated test-fixtures `injected_tab`
   * spec (`syntheticTabs.ts`). Synthetic tabs are fed ONLY to
   * `useSessionStateTracking` + `useSessionManager` for StatusStrip
   * bucketing — they never back a real PTY and must never render a terminal
   * pane. Phase 3 uses this flag for inertness guards. Absent/false on every
   * real tab.
   */
  __synthetic?: boolean;
}

/**
 * Durable-close reasons a FRONTEND caller may record (both have a live tab).
 *
 * Re-export of the union `sessionRecordArgs` owns alongside the rest of the
 * record-command arg types, so there is exactly ONE list of frontend close
 * reasons rather than two that can drift.
 */
export type FrontendCloseReason = FrontendSessionCloseReason;

/**
 * Pure helper: resolve the durable session-CLOSE record args for a tab that is
 * being closed — explicitly by the user, or because its pty exited. Returns
 * `null` when the tab has no `claudeSessionId` (a plain shell — nothing to
 * record). Exported so the close-recording contract can be unit-tested without
 * booting React (vitest runs in a `node` environment with no React Testing
 * Library — see existing `useTerminalManager.test.ts`).
 *
 * Carries BOTH halves of the key `terminal_session_record_open` binds. The
 * `claudeSessionId` alone is not a safe close target: it is a real, correctly
 * minted id, but it is not guaranteed to key the record for *this* terminal —
 * a provisional spawn-seam id, a restored id whose pty was respawned under a
 * fresh `--session-id`, or a `reconciled` freshest-mtime bind that "may be
 * foreign" all typecheck here. The backend cross-checks the pair and closes the
 * record the terminal actually owns.
 *
 * `terminalId` is sourced from `closing.id`, not from the `id` parameter: they
 * are equal by the `find` predicate, and reading it off the found tab makes
 * that provenance explicit rather than incidental.
 */
export function buildSessionCloseRecord(
  tabs: TerminalTab[],
  id: string,
  reason: FrontendCloseReason = "explicit",
): SessionCloseArgs | null {
  const closing = tabs.find((t) => t.id === id);
  if (!closing?.claudeSessionId) return null;
  // Through the typed builder rather than an inline object literal: the wire
  // shape is owned in one place (`sessionRecordArgs`), beside the OPEN side.
  return buildSessionCloseArgs({
    claudeSessionId: closing.claudeSessionId,
    terminalId: closing.id,
    reason,
  });
}

/**
 * Pure helper: decide whether a `terminal-created` event belongs to the page
 * this manager instance is scoped to. With the session provider lifted above
 * the page (so every page's manager is mounted simultaneously), each page's
 * `terminal-created` listener must claim ONLY the terminals created for its own
 * page — otherwise a terminal created for page B would be ingested into every
 * page's tab slice.
 *
 * Older wire forms that omit `pageId` hydrate to `"default"` on the Rust side
 * (`TerminalInfo`), so an undefined/empty value here is treated as "default".
 *
 * Exported so `useTerminalManager.test.ts` can drive the page-routing contract
 * without booting React.
 */
export function shouldIngestCreatedTerminal(
  eventPageId: string | undefined | null,
  pageId: string,
): boolean {
  return (eventPageId || "default") === pageId;
}

/**
 * Pure helper: fold a `terminal-created` payload into the existing tab list.
 * Returns the next tabs array — the SAME identity when the terminal already
 * exists (dedup), otherwise a new array with the tab appended. `pendingBypass`
 * is the bypass-permissions mark drained from the race buffer by the caller (a
 * `terminal-bypass-permissions` event that arrived before this
 * `terminal-created`); `pendingRemote` is the remote identity drained the same
 * way.
 *
 * This path never stamps a `taskRunId`. Every tab it builds is a PTY tab, and
 * the only worker mark it ever carried came from the event-based marking drain
 * that went with the Productivity scheduler — a Conductor worker's tab is
 * built by `workerTabFromRecord` instead, off a durable
 * `TerminalSessionRecord`.
 *
 * Exported so `useTerminalManager.test.ts` can drive the ingest + dedup contract
 * without booting React.
 */
export function reduceCreatedTerminal(
  tabs: TerminalTab[],
  info: TerminalInfo,
  pendingBypass = false,
  pendingRemote?: RemoteTabIdentity,
): TerminalTab[] {
  if (tabs.some((t) => t.id === info.id)) return tabs;
  return [
    ...tabs,
    {
      id: info.id,
      title: info.title,
      pid: info.pid ?? null,
      isAlive: info.isAlive,
      exitCode: info.exitCode ?? null,
      workingDir: info.workingDir || undefined,
      createdAt: info.createdAt,
      bypassPermissions: pendingBypass || undefined,
      remote: pendingRemote,
    },
  ];
}

/**
 * Pure helper: resolve the `activeId` to set after a `terminal-created` ingest.
 *
 * Returns `info.id` when this is a genuinely-NEW tab (`isNewTab === true`) so a
 * freshly-docked gate continuation is auto-selected the moment it lands —
 * surfacing it the same way the frontend-initiated `createTerminal` path does
 * (`setActiveId(info.id)`). Returns `null` (caller keeps the current selection)
 * when the ingest is a dedup'd re-delivery so a re-delivered event never steals
 * focus.
 *
 * Exported so `useTerminalManager.test.ts` can drive the auto-select contract
 * without booting React (the listener is a thin `if (id) setActiveId(id)`).
 */
export function nextActiveIdAfterIngest(info: TerminalInfo, isNewTab: boolean): string | null {
  return isNewTab ? info.id : null;
}

/**
 * Pure helper: decide whether `tabs` should be replaced when applying a
 * bypass-permissions mark for `terminalId`. Returns the next tabs array (same
 * identity if no change), and a `buffered` flag the caller uses to record the
 * mark in `pendingBypassMarks` when the tab record hasn't arrived yet.
 *
 * Sibling of `applyRemoteMark` — the `terminal-bypass-permissions` event can
 * arrive before OR after `terminal-created` lands the tab in React state.
 * Exported so `useTerminalManager.test.ts` can drive the race-safety +
 * idempotency contract without booting React.
 */
export function applyBypassMark(
  tabs: TerminalTab[],
  terminalId: string,
): { tabs: TerminalTab[]; buffered: boolean } {
  const idx = tabs.findIndex((t) => t.id === terminalId);
  if (idx < 0) return { tabs, buffered: true };
  if (tabs[idx].bypassPermissions === true) return { tabs, buffered: false };
  const next = tabs.slice();
  next[idx] = { ...tabs[idx], bypassPermissions: true };
  return { tabs: next, buffered: false };
}

/**
 * Grace window (ms) protecting a JUST-created tab from the backend re-sync.
 *
 * `createTerminal` appends its tab from the `terminal_create` response, and the
 * `terminal-created` listener can append one before a concurrently-issued
 * `terminal_list` has observed it. Dropping a tab younger than this would race
 * those two writers and delete a live pane. 5s is far longer than the
 * create→list round trip and far shorter than any human close-then-look loop.
 */
export const RESYNC_CREATE_GRACE_MS = 5_000;

/** Shared empty set, so the default argument allocates nothing per call. */
const EMPTY_ID_SET: ReadonlySet<string> = new Set<string>();

/**
 * Build the grid tab for a Conductor worker's lifecycle record. Pure.
 *
 * Returns `null` for a record that is not a worker (no `taskRunId`) — the
 * caller then takes the ordinary PTY restore path. A worker tab is keyed by
 * the record's `terminalId` (which `dispatch_subtask` sets equal to the task
 * run id), carries `sessionBacked: true` so every PTY-only code path skips it,
 * and is `isAlive` because the record is an OPEN one — the cell reads the
 * session's real state from the SessionManager once mounted.
 */
export function workerTabFromRecord(
  rec: Pick<
    TerminalSessionRecord,
    "claudeSessionId" | "terminalId" | "taskRunId" | "title" | "workingDir" | "openedAt"
  >,
  now: number = Date.now(),
): TerminalTab | null {
  if (!rec.taskRunId) return null;
  return {
    id: rec.terminalId || rec.taskRunId,
    title: rec.title?.trim() || `worker:${rec.taskRunId.slice(0, 8)}`,
    pid: null,
    isAlive: true,
    exitCode: null,
    workingDir: rec.workingDir || undefined,
    createdAt: rec.openedAt || now,
    claudeSessionId: rec.claudeSessionId,
    taskRunId: rec.taskRunId,
    sessionBacked: true,
  };
}

/**
 * Pick the worker record for `taskRunId` on `pageId` out of a
 * `terminal_session_list_open` payload. Pure. A record on another page is
 * NOT this page's to adopt; a non-worker record with a coincidentally equal
 * id is never matched because `taskRunId` (not `claudeSessionId`) is the
 * worker marker.
 */
export function findWorkerRecord(
  sessions: readonly TerminalSessionRecord[],
  taskRunId: string,
  pageId: string,
): TerminalSessionRecord | undefined {
  return sessions.find(
    (rec) => rec.taskRunId === taskRunId && (rec.pageId || "default") === pageId,
  );
}

/**
 * Spacing between `terminal_session_list_open` probes for the SAME
 * not-yet-adopted task run id, by how many probes have already missed. The
 * trigger events (`ai-output`, `claude-session-state`) fire per streamed
 * line, and a worker's record is written AFTER its first state events
 * (`dispatch_subtask` records the worker once the spawn joins), so the first
 * probe can legitimately miss and a later one must be allowed — but not one
 * per line, and not forever: an AI session that is NOT a worker on this page
 * (a Process Manager "Fix with AI" chat) streams the same events, and every
 * page's manager hears them. Backs off 3 s → 9 s → 27 s → 60 s (capped).
 */
export const WORKER_ADOPT_PROBE_BASE_MS = 3_000;
export const WORKER_ADOPT_PROBE_MAX_MS = 60_000;

export function workerAdoptProbeDelayMs(misses: number): number {
  return Math.min(WORKER_ADOPT_PROBE_MAX_MS, WORKER_ADOPT_PROBE_BASE_MS * 3 ** Math.max(0, misses));
}

export interface WorkerProbeEntry {
  at: number;
  misses: number;
}

/**
 * How long a probe entry outlives its last probe before being evicted. An
 * entry is only ever DELETED on successful adoption, and every page's manager
 * hears every AI session's events on the box — so without this a long-lived
 * run page accumulated one entry per FOREIGN session it would never adopt,
 * forever. Ten times the backoff ceiling: long enough that a live session's
 * entry (refreshed every probe) is never evicted, short enough that a session
 * which stopped talking stops costing memory.
 */
export const WORKER_PROBE_ENTRY_TTL_MS = WORKER_ADOPT_PROBE_MAX_MS * 10;

/**
 * Drop probe entries whose last probe is older than `ttlMs`. Mutates and
 * returns the map (it is ref-held state). Pure enough to test.
 *
 * Evicting resets that id's backoff, which is harmless: the eviction can only
 * fire for an id that has produced no event for `ttlMs`, and an id producing
 * events keeps its `at` fresh.
 */
export function pruneWorkerProbes(
  probes: Map<string, WorkerProbeEntry>,
  now: number,
  ttlMs: number = WORKER_PROBE_ENTRY_TTL_MS,
): Map<string, WorkerProbeEntry> {
  for (const [id, entry] of probes) {
    if (now - entry.at > ttlMs) probes.delete(id);
  }
  return probes;
}

/**
 * A worker view the operator closed on this page, kept so the close is
 * REVERSIBLE. Closing a worker cell hides a view of a still-running worker;
 * without a way back the operator had to restart the app to see it again,
 * which is the opposite of the supervision this cell exists to provide.
 */
export interface HiddenWorker {
  /** The tab id the view had (the worker's terminal id). */
  tabId: string;
  /** The worker's task run id, when the tab carried one. */
  taskRunId: string | null;
  /** The title the tab had, so the affordance can name it. */
  title: string;
  hiddenAtMs: number;
  /**
   * Set when an explicit "show" could not bring the worker back — its record
   * is no longer listed open. The affordance says so rather than silently
   * doing nothing.
   */
  restoreMissedAtMs?: number;
}

/**
 * Pure: reconcile the local tab list against the BACKEND's authoritative
 * terminal list for this page.
 *
 * Local tab state is otherwise write-only after boot — `reconnectToExistingSessions`
 * seeds it once and the `terminal-created` event is the only live-ingest path.
 * A single missed event (or a PTY removed out-of-band via `DELETE /terminals/{id}`)
 * therefore desyncs the list PERMANENTLY: the grid renders zones for tabs that
 * no longer exist, or renders nothing for terminals that do, and no refresh,
 * navigation or tab switch repairs it. This is the "never let local tab state be
 * the only source" reconciler (2026-08-20 manual-test loop, item A).
 *
 * Rules, in both directions:
 *  - ADD every backend terminal with no local tab (a missed `terminal-created`).
 *  - DROP every local terminal tab with no backend terminal (closed out-of-band),
 *    EXCEPT plan tabs (no PTY — the backend never lists them), synthetic tabs
 *    (test fixtures, never backed by a PTY) and tabs younger than
 *    `graceMs` (an in-flight create the backend list predates).
 *  - The `graceMs` exemption does NOT apply to a tab in `settledIds` — an id
 *    the runner has already emitted `terminal-exit` for. That event is proof
 *    the backend knew this terminal, so "the list snapshot predates the
 *    create" is no longer a possible explanation for its absence: it is gone.
 *    Without this, a terminal created and then closed out-of-band inside the
 *    grace window kept its tab until some LATER exit happened to trigger
 *    another re-sync — which, on a box whose only close door is the HTTP/MCP
 *    one, can be never.
 *  - Never touch a surviving tab's fields: `claudeSessionId`, worker/bypass
 *    marks, `isReconnecting`, `resumeFailed` etc. are owned by other writers and
 *    a re-sync must not clobber them.
 *
 * Returns the SAME array reference when nothing changed, so the caller can skip
 * a render (same contract as `applyBypassMark` / `reconcilePages`).
 *
 * `backendTerminals` MUST already be filtered to this page — the caller owns
 * that filter because it also owns the `pageId || "default"` normalization.
 * Callers must NOT invoke this at all when the backend list could not be read:
 * an unreadable list is INDETERMINATE, and treating it as "no terminals" would
 * drop every tab (same distinction `reconnectToExistingSessions` draws between
 * `[]` and `null`).
 *
 * Exported so the two-way reconciliation is unit-testable without React/Tauri.
 */
export function reconcileTabsWithBackend(
  tabs: TerminalTab[],
  backendTerminals: readonly TerminalInfo[],
  now: number = Date.now(),
  graceMs: number = RESYNC_CREATE_GRACE_MS,
  settledIds: ReadonlySet<string> = EMPTY_ID_SET,
): TerminalTab[] {
  const backendById = new Map(backendTerminals.map((t) => [t.id, t]));

  const kept = tabs.filter((t) => {
    if (backendById.has(t.id)) return true;
    // Tabs the backend structurally cannot list.
    if (t.type === "plan" || t.id.startsWith("plan-")) return true;
    if (t.__synthetic) return true;
    // A Conductor worker has no PTY; `terminal_list` is the wrong census for
    // it (its liveness is the SessionManager's, read by `WorkerSessionCell`).
    if (t.sessionBacked) return true;
    // The runner has already announced this terminal's exit, so its absence
    // from the list is a real teardown, not a create the snapshot predates.
    if (settledIds.has(t.id)) return false;
    // An in-flight create the list snapshot predates.
    return now - (t.createdAt ?? 0) < graceMs;
  });

  const known = new Set(kept.map((t) => t.id));
  const added = backendTerminals
    .filter((info) => !known.has(info.id))
    .map<TerminalTab>((info) => ({
      id: info.id,
      title: info.title,
      pid: info.pid ?? null,
      isAlive: info.isAlive,
      exitCode: info.exitCode ?? null,
      workingDir: info.workingDir || undefined,
      createdAt: info.createdAt,
    }));

  if (added.length === 0 && kept.length === tabs.length) return tabs;
  return [...kept, ...added];
}

/** The `terminal_id -> { claudeSessionId, configDir }` shape `terminal_list`
 * returns as `sessionIdsByTerminal`, derived from the durable lifecycle
 * store. */
export type SessionIdsByTerminal = Record<
  string,
  { claudeSessionId?: string; configDir?: string | null }
>;

/**
 * Attach `claudeSessionId` (and `claudeConfigDir`) to any tab that is
 * MISSING one, from the durable-store index. Pure — the reconnect path and
 * the periodic reconcile both funnel through this.
 *
 * Only fills gaps: a tab that already has a `claudeSessionId` (e.g. one
 * captured live from a fresh spawn) is never overwritten. Returns the SAME
 * array reference when nothing changed, so callers can pass it straight to a
 * `setTabs` updater without forcing a re-render on a no-op sweep.
 */
export function backfillClaudeSessionIds(
  tabs: TerminalTab[],
  map: SessionIdsByTerminal,
): TerminalTab[] {
  let changed = false;
  const next = tabs.map((t) => {
    const sid = map[t.id];
    if (!t.claudeSessionId && sid?.claudeSessionId) {
      changed = true;
      return {
        ...t,
        claudeSessionId: sid.claudeSessionId,
        claudeConfigDir: t.claudeConfigDir ?? sid.configDir ?? undefined,
      };
    }
    return t;
  });
  return changed ? next : tabs;
}

/**
 * Pure helper: resolve the `working_dir` argument for a spawn.
 *
 * Precedence is explicit-caller-value → the terminal PAGE's
 * `defaultWorkingDir` (set when a project is activated, see
 * `TerminalPageConfig.defaultWorkingDir`) → `null` (Rust picks its own
 * default, the pre-project behavior). Blank / whitespace-only values on either
 * input count as ABSENT, so an empty string from a form field falls through to
 * the page default instead of spawning at "".
 *
 * WHY THE FALLBACK LIVES HERE (projects-dashboard plan §7.2 step 2): it must
 * be applied BEFORE the value reaches the Rust `terminal_create` command. That
 * command derives `intent_repo` from `working_dir`
 * (`src-tauri/src/commands/terminal.rs:97-111`) and then hands it to
 * `isolated_edit::acquire_for_terminal`, which under
 * `QONTINUI_AGENT_WORKTREE_MODE` REASSIGNS `working_dir` to a freshly
 * allocated isolated worktree (`:122-128`). Passing `undefined` and letting
 * Rust guess is therefore not equivalent — the page default has to arrive as
 * the `working_dir` argument or the repo intent (and any worktree allocation)
 * is derived from the wrong directory.
 *
 * Exported so the precedence contract is unit-testable without React or Tauri.
 */
export function resolveSpawnWorkingDir(
  explicit: string | undefined,
  pageDefault: string | undefined,
): string | null {
  const clean = (v: string | undefined): string | undefined => {
    const t = v?.trim();
    return t ? t : undefined;
  };
  return clean(explicit) ?? clean(pageDefault) ?? null;
}

// `TerminalInfo` is imported from `@qontinui/shared-types/tauri-events` —
// generated from the canonical Rust struct in
// `qontinui-schemas/rust/src/terminal.rs`. Field names are camelCase via
// `#[serde(rename_all = "camelCase")]`. Future serde renames break this
// file at compile time instead of silently dropping events.

/**
 * What `terminal_close` answered about a REMOTE tab's relay binding — the
 * `remoteDetach` object the Rust side renders (`commands::remote_attach::
 * render_remote_close`). Only the fields this UI reads are typed; the runner
 * may add more.
 */
export interface RemoteDetachReport {
  /** `queued` | `failed` | `not_attempted` | `unknown`. */
  outcome: string;
  error: string | null;
  /**
   * Whether a relay connection held the outbound pump across the whole close.
   * `null` means the connection CHANGED mid-close, so delivery is unknown —
   * never read it as `false`.
   */
  relayPumpAttached: boolean | null;
  targetDeviceId: string;
  remoteTerminalId: string;
}

/**
 * A remote close worth telling the operator about, with the runner's own
 * wording. Named `...State` because `RemoteCloseNotice` is the COMPONENT that
 * renders it.
 */
export interface RemoteCloseNoticeState {
  tabId: string;
  message: string;
  report: RemoteDetachReport;
}

/**
 * The ONLY outcome that raises no notice: the detach was queued AND one relay
 * connection held the outbound pump for the whole close.
 *
 * WHAT THAT DOES AND DOES NOT PROVE. It does not prove a release. `attached`
 * means a connection HOLDS the pump, not that its socket is alive — a
 * half-open socket still holds it until its write fails
 * (`mcp/remote_terminal.rs`, `outbound_pump_state`) — so this arm still
 * covers a frame that never reaches the relay, and the runner's own message
 * hedges accordingly ("the relay drops the binding IF it receives it").
 * Staying silent here is a deliberate product call, not a claim: a notice on
 * every remote close would be noise on the ordinary path, and only a
 * re-attach of the same terminal can actually prove the binding is gone
 * (that is the plan's Phase 0). The hedge is not lost — the runner's sentence
 * is logged verbatim on this arm.
 *
 * Everything else — a failed queue, no detach attempted, no pane found, or a
 * pump that changed mid-close — leaves the target's terminal claimed until
 * this runner's relay connection drops or the grant expires, and the operator
 * is the one who will try to re-attach. `relayPumpAttached === null` is
 * UNKNOWN and deliberately falls on the noisy side: a silent close is
 * indistinguishable from a broken one.
 *
 * Exported pure so the branch can be unit-tested without rendering.
 */
export function isCleanRemoteClose(report: RemoteDetachReport): boolean {
  return report.outcome === "queued" && report.relayPumpAttached === true;
}

/**
 * The `remoteDetach` object out of a `terminal_close` response, or `null` for
 * a local tab (and for any response shape this build does not recognise —
 * an unreadable answer is UNKNOWN, and UNKNOWN must not manufacture a notice
 * about a tab that was never remote).
 */
export function parseRemoteDetach(data: unknown): RemoteDetachReport | null {
  if (!data || typeof data !== "object") return null;
  const raw = (data as { remoteDetach?: unknown }).remoteDetach;
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (typeof r.outcome !== "string") return null;
  return {
    outcome: r.outcome,
    error: typeof r.error === "string" ? r.error : null,
    relayPumpAttached: typeof r.relayPumpAttached === "boolean" ? r.relayPumpAttached : null,
    targetDeviceId: typeof r.targetDeviceId === "string" ? r.targetDeviceId : "",
    remoteTerminalId: typeof r.remoteTerminalId === "string" ? r.remoteTerminalId : "",
  };
}

export function useTerminalManager(
  pageId: string = "default",
  windowLabel: string = "main",
  /**
   * This page's `defaultWorkingDir` (see `TerminalPageConfig`). Used by
   * `createTerminal` whenever the caller passes no explicit `workingDir`, so
   * every terminal spawned on a project-bound page opens at the project root.
   */
  defaultWorkingDir?: string,
) {
  const [tabs, setTabs] = useState<TerminalTab[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  /**
   * The last remote close that did NOT demonstrably release the target's
   * terminal. One slot, not a queue: the operator closes remote tabs one at a
   * time, and the newest answer is the one that matters.
   */
  const [remoteCloseNotice, setRemoteCloseNotice] =
    useState<RemoteCloseNoticeState | null>(null);
  const nextTitleNum = useRef(1);
  const [initialized, setInitialized] = useState(false);
  /**
   * Bypass-permissions marks (`terminalId`) received from the Rust
   * `terminal-bypass-permissions` event before their tab record exists in
   * React state. The event is emitted right after `terminal-created`, but
   * arrival order at the webview is not strictly guaranteed (and reconnect
   * rebuilds tabs without re-firing it), so a buffer is the simplest
   * race-safe shape.
   */
  const pendingBypassMarks = useRef<Set<string>>(new Set());
  /**
   * Remote identities (`terminalId → RemoteTabIdentity`) received from the
   * Rust `terminal-remote-identity` event before their tab record exists.
   * Same race shape as the bypass marks: emitted right after
   * `terminal-created`, arrival order not guaranteed.
   */
  const pendingRemoteMarks = useRef<Map<string, RemoteTabIdentity>>(new Map());
  /**
   * Ids this manager has already ingested via `terminal-created`. Drives the
   * auto-select decision (`nextActiveIdAfterIngest`) OUTSIDE the `setTabs`
   * updater so the updater stays pure under StrictMode double-invoke — a
   * re-delivered `terminal-created` is a no-op here (already present) and never
   * re-steals focus, mirroring `reduceCreatedTerminal`'s id-dedup.
   */
  const ingestedIds = useRef<Set<string>>(new Set());
  /**
   * Ids the runner has announced a `terminal-exit` for and that the backend
   * has not listed since.
   *
   * These defeat the create-grace exemption in `reconcileTabsWithBackend`: an
   * exit event is proof the backend knew the terminal, so its absence from
   * `terminal_list` is a real tear-down rather than a list snapshot that
   * predates the create. Without it, a terminal created and then closed
   * out-of-band inside the grace window kept its tab until some later exit
   * happened to trigger another re-sync — and on a headless runner, whose only
   * close door is the HTTP/MCP one, that can be never.
   *
   * Pruned on every re-sync (ids the backend still lists, and ids whose tab is
   * gone), so it stays bounded by the live tab count.
   */
  const settledIdsRef = useRef<Set<string>>(new Set());

  /**
   * Add the grid tab for a Conductor worker's lifecycle record. Idempotent:
   * a tab with the same id (or the same `taskRunId`) is left untouched.
   * Never steals `activeId` — a run page fills as workers dispatch, and the
   * operator's focus should not jump each time one lands. Returns the tab id,
   * or `null` when the record is not a worker's.
   */
  const adoptWorkerTab = useCallback((rec: TerminalSessionRecord): string | null => {
    const tab = workerTabFromRecord(rec);
    if (!tab) return null;
    setTabs((prev) => {
      if (prev.some((t) => t.id === tab.id || t.taskRunId === tab.taskRunId)) return prev;
      return [...prev, tab];
    });
    return tab.id;
  }, []);

  // Tabs snapshot for the adoption probe below (reads outside setState).
  const tabsRef = useRef(tabs);
  useEffect(() => {
    tabsRef.current = tabs;
  }, [tabs]);
  const workerProbeRef = useRef<Map<string, WorkerProbeEntry>>(new Map());
  /**
   * Worker views the operator CLOSED on this page since it mounted. A closed
   * worker keeps streaming (`closeTerminal` never touches the worker itself),
   * so without this set the live-adoption probe below would re-add the tab
   * on its very next `ai-output` line and "close" would be a three-second
   * hide.
   *
   * It is no longer write-only: every id in here is mirrored by a
   * `hiddenWorkers` entry, and `restoreHiddenWorkers` clears it. That is the
   * whole reversal — an automatic expiry (on the worker's next state
   * transition, say) would restore exactly the three-second hide this set
   * exists to prevent, so the way back is an EXPLICIT operator action.
   *
   * The dismissal set and the chip's rows are ONE value
   * (`HiddenWorkerState`), transitioned by the pure functions in
   * `hiddenWorkerReducer.ts` — a dismissal without a row is a worker the
   * operator can never get back, and a row without a dismissal is a chip entry
   * for a cell about to reappear on its own, so the two never move apart.
   */
  const hiddenWorkerStateRef = useRef<HiddenWorkerState>(EMPTY_HIDDEN_WORKER_STATE);
  /** The rows, mirrored into React state for the "show worker" chip. */
  const [hiddenWorkers, setHiddenWorkers] = useState<readonly HiddenWorker[]>([]);
  const applyHiddenWorkerState = useCallback((next: HiddenWorkerState) => {
    if (next === hiddenWorkerStateRef.current) return;
    hiddenWorkerStateRef.current = next;
    setHiddenWorkers(next.hidden);
  }, []);

  /**
   * Live-adopt a Conductor worker the moment it starts talking. The restore
   * path (`useTerminalInitialization`) runs ONCE per page, so a worker
   * dispatched after the run page initialised would otherwise have no tab
   * until the next boot. Any `ai-output` / `claude-session-state` event for a
   * task run this page has no tab for triggers ONE throttled read of the
   * durable records; a record on this page with that `taskRunId` becomes a
   * tab. A failed read is logged and retried on the next event — never
   * treated as "no such worker".
   */
  const maybeAdoptWorker = useCallback(
    async (taskRunId: string): Promise<boolean> => {
      if (hiddenWorkerStateRef.current.dismissed.has(taskRunId)) return false;
      if (tabsRef.current.some((t) => t.id === taskRunId || t.taskRunId === taskRunId)) return false;
      const now = Date.now();
      // Bound the probe ledger: this manager hears EVERY AI session's events,
      // not just its own workers', and an entry is otherwise only removed on
      // a successful adoption that will never come for a foreign session.
      pruneWorkerProbes(workerProbeRef.current, now);
      const probe = workerProbeRef.current.get(taskRunId) ?? { at: 0, misses: 0 };
      if (now - probe.at < workerAdoptProbeDelayMs(probe.misses)) return false;
      workerProbeRef.current.set(taskRunId, { at: now, misses: probe.misses });
      let sessions: TerminalSessionRecord[] | undefined;
      try {
        const resp = await invoke<CommandResponse>("terminal_session_list_open");
        sessions = (resp?.data as { sessions?: TerminalSessionRecord[] } | undefined)?.sessions;
      } catch (err) {
        // A failed read is not "no such worker": retry on the base cadence.
        logger.warn(`worker adoption probe for ${taskRunId} failed (will retry): ${err}`);
        return false;
      }
      if (!Array.isArray(sessions)) return false;
      const rec = findWorkerRecord(sessions, taskRunId, pageId);
      if (!rec) {
        workerProbeRef.current.set(taskRunId, { at: now, misses: probe.misses + 1 });
        return false;
      }
      workerProbeRef.current.delete(taskRunId);
      const tabId = adoptWorkerTab(rec);
      if (tabId) {
        logger.info(`Adopted Conductor worker ${taskRunId} onto page ${pageId}`);
        // The worker is on screen again, so it is no longer hidden — drop any
        // "could not be re-opened" entry left over from a failed restore.
        applyHiddenWorkerState(
          forgetAdoptedWorker(hiddenWorkerStateRef.current, tabId, taskRunId),
        );
      }
      return tabId !== null;
    },
    [pageId, adoptWorkerTab, applyHiddenWorkerState],
  );

  /**
   * Bring closed worker views back. With no argument, all of them.
   *
   * Clears the dismissal, drops the probe throttle so the adoption read
   * happens NOW rather than on the worker's next event (a quiet worker would
   * otherwise stay invisible after an explicit "show"), and re-lists anything
   * that could not be adopted with `restoreMissedAtMs` set — a click that
   * silently does nothing is the failure this whole finding is about.
   */
  const restoreHiddenWorkers = useCallback(
    async (tabIds?: readonly string[]) => {
      const begun = beginRestore(hiddenWorkerStateRef.current, tabIds);
      if (begun.restoring.length === 0) return;
      for (const key of begun.probeKeys) workerProbeRef.current.delete(key);
      applyHiddenWorkerState(begun.state);
      const outcomes = await Promise.all(
        begun.restoring.map(async (w) => ({
          worker: w,
          adopted: await maybeAdoptWorker(w.taskRunId ?? w.tabId),
        })),
      );
      const missed = outcomes.filter((o) => !o.adopted).map((o) => o.worker);
      if (missed.length === 0) return;
      // Note: the DISMISSAL stays cleared (see `beginRestore`). If the miss was
      // transient (a failed `terminal_session_list_open`, or a record not yet
      // written), the live adoption probe brings the worker in on its next
      // event and `forgetAdoptedWorker` clears the entry re-listed here.
      applyHiddenWorkerState(
        recordRestoreMisses(hiddenWorkerStateRef.current, missed, Date.now()),
      );
    },
    [maybeAdoptWorker, applyHiddenWorkerState],
  );

  useEffect(() => {
    let disposed = false;
    const unlisteners: Array<() => void> = [];
    const attach = (fn: () => void) => {
      if (disposed) fn();
      else unlisteners.push(fn);
    };
    listen<{ taskRunId?: string | null }>("ai-output", (event) => {
      const id = event.payload?.taskRunId;
      if (id) void maybeAdoptWorker(id);
    }).then(attach);
    listen<{ taskRunId?: string | null }>("claude-session-state", (event) => {
      const id = event.payload?.taskRunId;
      if (id) void maybeAdoptWorker(id);
    }).then(attach);
    return () => {
      disposed = true;
      for (const fn of unlisteners) fn();
    };
  }, [maybeAdoptWorker]);

  const markAsBypass = useCallback((terminalId: string) => {
    setTabs((prev) => {
      const result = applyBypassMark(prev, terminalId);
      if (result.buffered) {
        pendingBypassMarks.current.add(terminalId);
      }
      return result.tabs;
    });
  }, []);

  const markAsRemote = useCallback((terminalId: string, remote: RemoteTabIdentity) => {
    setTabs((prev) => {
      const result = applyRemoteMark(prev, terminalId, remote);
      if (result.buffered) {
        pendingRemoteMarks.current.set(terminalId, remote);
      }
      return result.tabs;
    });
  }, []);

  // Remote identity for a tab `terminal_attach_remote` opened (Phase 4). Fired
  // by Rust right after `terminal-created`; page-agnostic because the identity
  // is keyed by terminal id — for an id this page's manager never holds the
  // mark sits in the buffer and is never applied.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let disposed = false;
    listen<{ id: string; remote: RemoteTabIdentity }>("terminal-remote-identity", (event) => {
      const { id, remote } = event.payload;
      if (!id || !remote) return;
      markAsRemote(id, remote);
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [markAsRemote]);

  // Listen for terminals created externally (e.g. via HTTP API) and add them as
  // tabs. This is the ONLY live-ingest path for externally-created terminals
  // (e.g. a docked gate continuation). With the session provider lifted above
  // TerminalPage (so every page's manager is mounted at once), the listener
  // ALWAYS runs regardless of which page the operator is viewing — a
  // `terminal-created` event is therefore never dropped by a page switch. Each
  // page's listener claims ONLY the terminals tagged with its own `pageId`
  // (`shouldIngestCreatedTerminal`), and dedups by id (`reduceCreatedTerminal`).
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    listen<TerminalInfo>("terminal-created", (event) => {
      const info = event.payload;
      // Route the event to the page it belongs to. A terminal created for
      // another page is ignored here — its own page's manager will claim it.
      if (!shouldIngestCreatedTerminal(info.pageId, pageId)) return;
      // Drain the race buffer OUTSIDE the reducer so the `setTabs` updater
      // stays pure (React StrictMode double-invokes updaters in dev; a
      // delete-inside-reducer would miss on the second pass).
      const pendingBypass = pendingBypassMarks.current.has(info.id);
      if (pendingBypass) {
        pendingBypassMarks.current.delete(info.id);
      }
      const pendingRemote = pendingRemoteMarks.current.get(info.id);
      if (pendingRemote !== undefined) {
        pendingRemoteMarks.current.delete(info.id);
      }
      // Decide auto-select OUTSIDE the `setTabs` updater so the updater stays
      // pure (StrictMode double-invokes updaters in dev). `ingestedIds` is a
      // ref-backed dedup set so a re-delivered `terminal-created` is a no-op
      // here just as `reduceCreatedTerminal` dedups it in the updater — only a
      // genuinely-new tab triggers selection, so a re-delivery never steals
      // focus. The pure decision lives in `nextActiveIdAfterIngest`, fed a
      // synthetic prev/next pair reflecting whether this id is new.
      const wasNew = !ingestedIds.current.has(info.id);
      const selectId = nextActiveIdAfterIngest(info, wasNew);
      setTabs((prev) =>
        reduceCreatedTerminal(prev, info, pendingBypass, pendingRemote),
      );
      if (selectId !== null) {
        ingestedIds.current.add(info.id);
        logger.info(`External terminal created: ${info.id} (${info.title}) [page ${pageId}]`);
        // Auto-select the just-appended externally-created tab so a docked gate
        // continuation is surfaced (selected) the moment it lands — matching
        // `createTerminal`'s `setActiveId(info.id)` (the frontend-initiated
        // path). Without this the continuation tab is appended un-selected and
        // the operator sees nothing (the surfacing bug). The App-level
        // `terminal-focus-request` listener handles the complementary main-view
        // switch (`setActiveTab("terminal")`).
        setActiveId(selectId);
      }
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, [pageId]);

  // Bypass-aware needs-input detection (plan
  // `2026-06-07-runner-continuation-defer-and-phantom-needs-input.md`).
  // The Rust `TerminalManager::create` emits `terminal-bypass-permissions`
  // (after `terminal-created`) when the spawn command implies bypassed tool
  // permissions; marking the tab here lets `useSessionStateTracking` skip
  // approval-shaped TTY patterns for it (those can only ever be phantoms on a
  // bypass session).
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    listen<{ id: string }>("terminal-bypass-permissions", (event) => {
      const { id } = event.payload;
      if (!id) return;
      markAsBypass(id);
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, [markAsBypass]);

  // Live terminal-page moves (plan `2026-07-18-runner-api-account-selection.md`
  // Phase 5). `POST /terminals/{id}/move` mutates a terminal's page on the Rust
  // side and emits `terminal-page-changed` { id, pageId }. Every page's manager
  // is mounted simultaneously (session-provider lift), so each instance decides,
  // from its OWN `pageId`, whether the move concerns it — the same
  // unmount-source / mount-target model `WindowAssignmentsContext` uses for
  // `session-assignment-changed`, and a sibling of the `terminal-created` ingest
  // above:
  //   - TARGET page (`event.pageId === pageId`) → adopt the tab (mount) if not
  //     already present. The move event carries only `{ id, pageId }`, so we
  //     fetch authoritative info via `terminal_list` (which reflects the
  //     just-applied page move) and fold it in through `reduceCreatedTerminal`.
  //   - SOURCE page (holds the tab, but is no longer its page) → evict the tab
  //     (unmount) WITHOUT calling `terminal_close`: the PTY lives on and now
  //     belongs to the target page.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    listen<{ id: string; pageId: string }>("terminal-page-changed", (event) => {
      const { id, pageId: targetPageId } = event.payload;
      if (!id) return;
      const target = targetPageId || "default";
      if (target === pageId) {
        // Adopt onto THIS (target) page. `reduceCreatedTerminal` dedups by id,
        // so an idempotent re-delivery for a tab we already hold is a no-op.
        invoke<CommandResponse>("terminal_list")
          .then((result) => {
            if (!result.success || !result.data) return;
            const terminals = (result.data as { terminals: TerminalInfo[] }).terminals;
            const info = terminals.find((t) => t.id === id);
            if (!info) return;
            const pendingBypass = pendingBypassMarks.current.has(id);
            if (pendingBypass) pendingBypassMarks.current.delete(id);
            const pendingRemote = pendingRemoteMarks.current.get(id);
            if (pendingRemote !== undefined) pendingRemoteMarks.current.delete(id);
            setTabs((prev) =>
              reduceCreatedTerminal(prev, info, pendingBypass, pendingRemote),
            );
            ingestedIds.current.add(id);
            setActiveId(id);
            logger.info(`Terminal ${id} moved onto page ${pageId}`);
          })
          .catch((err) => {
            logger.warn(`terminal-page-changed adopt failed for ${id}: ${err}`);
          });
      } else {
        // Evict from THIS page if we hold it — do NOT close the PTY.
        setTabs((prev) => {
          if (!prev.some((t) => t.id === id)) return prev;
          const closedIndex = prev.findIndex((t) => t.id === id);
          const next = prev.filter((t) => t.id !== id);
          setActiveId((currentActive) => {
            if (currentActive !== id) return currentActive;
            return next[Math.min(closedIndex, next.length - 1)]?.id ?? null;
          });
          ingestedIds.current.delete(id);
          return next;
        });
      }
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      unlisten?.();
    };
  }, [pageId]);

  /**
   * Reconnect to existing Rust PTY sessions that survived a React remount.
   *
   * Return contract (P1 restore idempotence — the restore path aborts on
   * `null`, so the two cases must never be conflated):
   * - `string[]` (possibly EMPTY) — the backend terminal list was read
   *   successfully; the array is the definitive ordered set of reconnected
   *   tab ids for this page. `[]` means "definitely nothing to reconnect"
   *   (e.g. a cold start), and the caller may safely cold-restore records.
   * - `null` — INDETERMINATE: the list could not be read (invoke failed or
   *   the response was unsuccessful/malformed). Callers must NOT treat this
   *   as "nothing alive": cold-respawning `claude --resume` for records
   *   whose previous terminal generation is still alive forks the live
   *   sessions (measured 2026-07-23: 22 of 80 live session ids had >1 live
   *   process). `useTerminalInitialization` aborts restore on `null` and
   *   retries on the next activation.
   */
  const reconnectToExistingSessions = useCallback(async (): Promise<string[] | null> => {
    try {
      const result = await invoke<CommandResponse>("terminal_list");
      if (!result.success || !result.data) return null;

      const terminals = (result.data as { terminals: TerminalInfo[] }).terminals;
      // Malformed payload (no `terminals` key) is indeterminate — never
      // claim "definitely empty" off a shape we didn't understand.
      if (!terminals) return null;
      if (terminals.length === 0) return [];

      // `TerminalInfo` carries no Claude session id, so a reconnected tab
      // would otherwise come back with `claudeSessionId: undefined` and any
      // session-scoped UI (e.g. the per-session PR dropdown) would never
      // mount for it. `terminal_list` now returns the durable-store's
      // `terminal_id -> { claudeSessionId, configDir }` index; attach it at
      // tab-build time so reconnected sessions light up immediately, without
      // waiting on the transcript-poll backfill (which only runs for fresh
      // spawns).
      const sessionIdsByTerminal =
        (result.data as { sessionIdsByTerminal?: SessionIdsByTerminal }).sessionIdsByTerminal ?? {};

      // Filter to terminals belonging to this page
      const pageTerminals = terminals.filter((t) => (t.pageId || "default") === pageId);

      // Only reconnect to alive sessions; silently close dead ones
      const dead = pageTerminals.filter((t) => !t.isAlive);
      const alive = pageTerminals.filter((t) => t.isAlive);

      for (const t of dead) {
        invoke("terminal_close", { terminalId: t.id }).catch(() => {});
      }

      // List read fine, nothing alive on this page — a DEFINITIVE empty
      // result (cold start), not an indeterminate one.
      if (alive.length === 0) return [];

      logger.info(`Reconnecting to ${alive.length} existing PTY session(s)`);

      // Remote tabs (Phase 4): `TerminalInfo` cannot carry the remote
      // identity, so re-badge reconnected remote tabs from the runner's
      // identity map. A failed read leaves them un-badged (UNKNOWN), never
      // mis-badged as local.
      let remoteById: Record<string, RemoteTabIdentity> = {};
      try {
        const r = await invoke<CommandResponse>("terminal_remote_identities");
        if (r.success && r.data && typeof r.data === "object") {
          remoteById = r.data as Record<string, RemoteTabIdentity>;
        }
      } catch (err) {
        logger.warn(`terminal_remote_identities failed; remote tabs stay un-badged: ${err}`);
      }

      // Rebuild tabs from Rust session data (already sorted by created_at)
      const reconnectedTabs: TerminalTab[] = alive.map((info) => {
        const sid = sessionIdsByTerminal[info.id];
        return {
          id: info.id,
          title: info.title,
          pid: info.pid ?? null,
          isAlive: info.isAlive,
          exitCode: info.exitCode ?? null,
          workingDir: info.workingDir || undefined,
          createdAt: info.createdAt,
          isReconnecting: true,
          claudeSessionId: sid?.claudeSessionId,
          claudeConfigDir: sid?.configDir ?? undefined,
          remote: remoteById[info.id],
        };
      });

      // Update nextTitleNum to avoid collisions
      for (const tab of reconnectedTabs) {
        const match = tab.title.match(/^Terminal (\d+)$/);
        if (match) {
          nextTitleNum.current = Math.max(nextTitleNum.current, parseInt(match[1], 10) + 1);
        }
      }

      setTabs(reconnectedTabs);
      // Select the last tab (most recently created)
      setActiveId(reconnectedTabs[reconnectedTabs.length - 1].id);

      return reconnectedTabs.map((t) => t.id);
    } catch (err) {
      console.error("[TerminalManager] Failed to reconnect:", err);
      return null;
    }
  }, [pageId]);

  /** Mark a tab as having completed reconnection (buffer replayed). */
  const markReconnected = useCallback((id: string) => {
    setTabs((prev) => prev.map((t) => (t.id === id ? { ...t, isReconnecting: false } : t)));
  }, []);

  /**
   * Catch-all backfill of `claudeSessionId` for any tab still missing it,
   * from the durable-store index `terminal_list` returns
   * (`sessionIdsByTerminal`).
   *
   * The reconnect path attaches ids at tab-build time and fresh spawns get
   * theirs from the transcript poll / shell-integration; this periodic sweep
   * guarantees EVERY session with a recorded id ends up with it on its tab —
   * including one whose durable record was written *after* the initial
   * reconnect, or a tab created outside the spawn-poll path. It only fills
   * MISSING ids (never overwrites a live-captured one) and no-ops when
   * nothing changed, so it can't fight the other writers or churn renders.
   */
  const reconcileClaudeSessionIds = useCallback(async () => {
    try {
      const result = await invoke<CommandResponse>("terminal_list");
      if (!result.success || !result.data) return;
      const map =
        (result.data as { sessionIdsByTerminal?: SessionIdsByTerminal }).sessionIdsByTerminal ?? {};
      if (Object.keys(map).length === 0) return;
      setTabs((prev) => backfillClaudeSessionIds(prev, map));
    } catch {
      // Best-effort backfill; the reconnect + transcript-poll writers still
      // cover the common cases if this sweep transiently fails.
    }
  }, []);

  useEffect(() => {
    const timer = setInterval(() => void reconcileClaudeSessionIds(), 30_000);
    return () => clearInterval(timer);
  }, [reconcileClaudeSessionIds]);

  /**
   * Re-sync `tabs` against the BACKEND terminal list for this page — the
   * "backend is the source of truth" repair path (see
   * `reconcileTabsWithBackend`).
   *
   * Runs on mount, after every close, and on every `terminal-exit`, which are
   * exactly the moments local tab state can have diverged: a `terminal-created`
   * missed while the listener was being (re)registered, a PTY removed
   * out-of-band by `DELETE /terminals/{id}`, or a close that raced a create.
   * Before this, the boot reconnect was the ONLY reader of the backend list, so
   * any divergence after boot was permanent — the grid rendered zero zones (or
   * stale ones) and no refresh, navigation or tab switch recovered it.
   *
   * Best-effort and INDETERMINATE-SAFE: a failed/malformed `terminal_list` is
   * NOT "no terminals" — we return without touching `tabs` rather than dropping
   * every tab (the same `[]`-vs-`null` distinction `reconnectToExistingSessions`
   * draws).
   */
  const resyncTabs = useCallback(async () => {
    let terminals: TerminalInfo[] | undefined;
    try {
      const result = await invoke<CommandResponse>("terminal_list");
      if (!result.success || !result.data) return; // indeterminate — leave tabs alone
      terminals = (result.data as { terminals?: TerminalInfo[] }).terminals;
    } catch {
      return; // indeterminate — leave tabs alone
    }
    if (!Array.isArray(terminals)) return; // malformed — never read as "empty"

    const mine = terminals.filter((t) => (t.pageId || "default") === pageId);
    // Ids the runner has announced an exit for. They defeat the create grace
    // (see `reconcileTabsWithBackend`), and anything the backend still lists
    // is pruned back out so the set cannot grow without bound.
    const settled = settledIdsRef.current;
    for (const info of mine) settled.delete(info.id);
    setTabs((prev) => {
      const next = reconcileTabsWithBackend(prev, mine, Date.now(), RESYNC_CREATE_GRACE_MS, settled);
      if (next === prev) return prev;
      const dropped = new Set(next.map((t) => t.id));
      for (const tab of prev) if (!dropped.has(tab.id)) settled.delete(tab.id);
      logger.info(
        `Tab re-sync on page ${pageId}: ${prev.length} → ${next.length} (backend has ${mine.length})`,
      );
      return next;
    });
  }, [pageId]);

  // Mount + `terminal-exit` re-sync. `terminal-created` already has its own
  // ingest path (which this only backstops), but an EXIT — and especially an
  // out-of-band `DELETE /terminals/{id}` — has no other reader of the backend
  // list at all.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    let disposed = false;
    let timer: ReturnType<typeof setTimeout> | null = null;

    // Async: its `setTabs` runs in a later microtask, never during this
    // effect's render pass (same reasoning as `useTerminalPages`'s reconcile).
    void resyncTabs();

    listen<TerminalExitEvent>("terminal-exit", (event) => {
      // Record the id as SETTLED before the re-sync reads the list.
      //
      // The runner announces a pane's exit on BOTH tear-downs, because they
      // are different facts: the waiter thread when the child process ends,
      // and `close_with_deadline` when the SESSION goes away — the latter
      // reached from the HTTP/MCP `terminal_close` door, which is the only
      // close door a headless runner has. Only the second one arrives after
      // `terminal_list` has stopped listing the terminal, which is why the
      // re-sync below is what decides the tab's fate rather than this
      // listener. See `terminal::exit_notice` on the Rust side.
      const exitedId = event.payload?.terminalId;
      if (exitedId) settledIdsRef.current.add(exitedId);
      // Debounced: a burst of exits (window close, batch kill) collapses into
      // one list read.
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => {
        timer = null;
        void resyncTabs();
      }, 400);
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    });

    return () => {
      disposed = true;
      if (timer) clearTimeout(timer);
      unlisten?.();
    };
  }, [resyncTabs]);

  const createTerminal = useCallback(
    async (title?: string, workingDir?: string, tenantId?: string): Promise<string | null> => {
      try {
        const displayTitle = title ?? `Terminal ${nextTitleNum.current++}`;
        // Attended spawn: the first invoke goes without an override. If the
        // spawn-time resource gate refuses (below the CRITICAL free-commit
        // floor), `spawnWithResourceGuard` shows the blocking dialog and
        // re-invokes with `resourceOverride: true` only if the operator picks
        // "Start anyway". Declining re-throws the refusal, so the existing
        // catch below still runs and the tab is not created.
        const result = await spawnWithResourceGuard((resourceOverride) =>
          invoke<CommandResponse>("terminal_create", {
            title: displayTitle,
            // Page-default fallback applied HERE, before the Rust command sees
            // it — `terminal_create` derives `intent_repo` from this value and
            // may reassign it to an isolated worktree, so `null` is not
            // equivalent to the page default. See `resolveSpawnWorkingDir`.
            workingDir: resolveSpawnWorkingDir(workingDir, defaultWorkingDir),
            pageId: pageId !== "default" ? pageId : null,
            // F2/F3 — the tenant the operator picked for THIS spawn. Sent
            // EXPLICITLY (the caller resolves picker-choice ?? active pin) so
            // the stamped tenant is exactly what the picker showed, with no
            // read-then-stamp race against a concurrent tenant switch. `null`
            // means the caller's `resolveTenantForSpawn` found no pin to send
            // (single-tenant OR unpaired device) — Rust then applies its own
            // device-default stamping, exactly as before F2.
            tenantId: tenantId ?? null,
            // Phase 2 (pop-out windows): tag the pane with its owning window so
            // its coord-session identity doesn't collide with a same-(title,cwd)
            // pane in another window. "main" → omitted (legacy/back-compat key).
            windowLabel: windowLabel !== "main" ? windowLabel : null,
            resourceOverride,
          }),
        );

        if (!result.success || !result.data) return null;

        const info = result.data as unknown as TerminalInfo;
        const tab: TerminalTab = {
          id: info.id,
          title: info.title,
          pid: info.pid ?? null,
          isAlive: info.isAlive,
          exitCode: info.exitCode ?? null,
          workingDir: info.workingDir || undefined,
          createdAt: info.createdAt,
          tenantId,
        };

        setTabs((prev) => {
          // Deduplicate: the terminal-created event listener may have already
          // added this tab. It cannot know the spawn tenant (the event payload
          // carries no tenant), so patch it on rather than dropping it — the
          // race would otherwise silently un-badge every tab whose event won.
          if (prev.some((t) => t.id === info.id)) {
            if (!tenantId) return prev;
            return prev.map((t) => (t.id === info.id ? { ...t, tenantId } : t));
          }
          return [...prev, tab];
        });
        setActiveId(info.id);
        return info.id;
      } catch (err) {
        console.error("Failed to create terminal:", err);
        return null;
      }
    },
    [pageId, windowLabel, defaultWorkingDir],
  );

  const createPlanTab = useCallback((filePath: string): string => {
    const fileName = filePath.replace(/\\/g, "/").split("/").pop() || "Plan";
    const id = `plan-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
    const tab: TerminalTab = {
      id,
      title: fileName,
      pid: null,
      isAlive: true,
      exitCode: null,
      type: "plan",
      planFilePath: filePath,
      createdAt: Date.now(),
    };
    setTabs((prev) => [...prev, tab]);
    setActiveId(id);
    return id;
  }, []);

  const closeTerminal = useCallback((id: string) => {
    // Capture the closing tab's Claude session id (read-only) so we can record
    // an EXPLICIT durable close after the state update. We read it out of the
    // updater's `prev` without mutating inside the updater (StrictMode double-
    // invokes updaters in dev).
    let closeRecord: ReturnType<typeof buildSessionCloseRecord> = null;
    // A Conductor worker's tab is a VIEW: closing it hides the cell and
    // nothing more. The Conductor owns the worker's lifetime, and its durable
    // record must stay open so the next restore brings the cell back while
    // the worker is still live. No close record, no `terminal_close` (there
    // is no PTY to close), no re-sync. Read off the tabs snapshot rather than
    // inside the updater, which React may run later than this handler.
    const closingTab = tabsRef.current.find((t) => t.id === id);
    const sessionBacked = closingTab?.sessionBacked === true;
    if (sessionBacked) {
      // Record the dismissal AND the chip row together, so the close is
      // reversible (`restoreHiddenWorkers`) — see `HiddenWorker`. The worker
      // keeps running either way.
      applyHiddenWorkerState(
        hideWorker(hiddenWorkerStateRef.current, {
          tabId: id,
          taskRunId: closingTab?.taskRunId ?? null,
          title: closingTab?.title ?? id,
          hiddenAtMs: Date.now(),
        }),
      );
    }
    // Update React state immediately so the UI is responsive.
    // The Rust-side close (process kill + thread join) runs in the background.
    setTabs((prev) => {
      closeRecord = sessionBacked ? null : buildSessionCloseRecord(prev, id);
      const next = prev.filter((t) => t.id !== id);
      setActiveId((currentActive) => {
        if (currentActive !== id) return currentActive;
        const closedIndex = prev.findIndex((t) => t.id === id);
        return next[Math.min(closedIndex, next.length - 1)]?.id ?? null;
      });
      return next;
    });

    // Record the durable session CLOSE for an explicit user close (only when
    // the tab was running a Claude session). Fire-and-forget.
    if (closeRecord) {
      invoke<CommandResponse>("terminal_session_record_close", closeRecord).catch(() => {
        // Best-effort — the live close still proceeds below.
      });
    }

    // Only invoke Rust close for terminal tabs (plan tabs and worker views
    // have no PTY)
    if (!id.startsWith("plan-") && !sessionBacked) {
      // Whatever the last close left on screen, it does not describe THIS
      // one. Clearing first means a stale warning can never be read as the
      // outcome of the close the operator just performed — and a close that
      // never answers (the `.catch` below) leaves no notice at all rather
      // than the previous one.
      setRemoteCloseNotice(null);
      invoke<CommandResponse>("terminal_close", { terminalId: id })
        .then((res) => {
          // A REMOTE tab's close reports what it did about the relay binding
          // (plan 2026-09-16-remote-tab-cannot-be-released-so-the-target-
          // terminal-stays-claimed, Phase 1). Discarding it — which this
          // handler did until now — made a failed detach look exactly like a
          // clean one, since the tab vanishes either way.
          //
          // `success === false` is guarded even though today's Tauri command
          // rejects instead: a door that ever answers a failed close WITH a
          // report must not raise a notice about a close that did not happen.
          if (res?.success === false) return;
          const report = parseRemoteDetach(res?.data);
          if (!report) return; // local tab: nothing extra happened
          const message = res.message ?? `Remote close outcome: ${report.outcome}`;
          if (isCleanRemoteClose(report)) {
            // Logged, not shown: see `isCleanRemoteClose` on what this arm
            // does and does not prove. The runner's hedge is kept verbatim.
            logger.info(`Remote tab ${id} closed: ${message}`);
            return;
          }
          logger.warn(`Remote tab ${id} closed without a confirmed detach: ${message}`);
          setRemoteCloseNotice({ tabId: id, message, report });
        })
        .catch(() => {
          // Terminal may already be gone (e.g. removed out-of-band by
          // `DELETE /terminals/{id}`) — the re-sync below is what repairs the
          // list either way. No notice: we have no answer to report, and an
          // UNKNOWN must not render as a claim about the binding.
        })
        .finally(() => {
          // Re-read the authoritative list AFTER the close settles. The
          // optimistic local removal above is a UI-responsiveness shortcut, not
          // a source of truth: without this, a close that raced an out-of-band
          // delete (or that closed the last tab the frontend knew about) left
          // the tab list permanently diverged from the backend and the grid
          // rendered zero zones forever.
          void resyncTabs();
        });
    }
  }, [resyncTabs, applyHiddenWorkerState]);

  const dismissRemoteCloseNotice = useCallback(() => setRemoteCloseNotice(null), []);

  const renameTab = useCallback((id: string, title: string) => {
    setTabs((prev) => prev.map((t) => (t.id === id ? { ...t, title } : t)));
  }, []);

  const updateTab = useCallback(
    (
      id: string,
      updates: Partial<
        Pick<
          TerminalTab,
          | "isAlive"
          | "exitCode"
          | "workingDir"
          | "claudeSessionId"
          | "claudeConfigDir"
          | "isReconnecting"
          | "resumeFailed"
          | "restoreTerminalOnly"
        >
      >,
    ) => {
      setTabs((prev) => prev.map((t) => (t.id === id ? { ...t, ...updates } : t)));
    },
    [],
  );

  return {
    tabs,
    activeId,
    setActiveId,
    initialized,
    setInitialized,
    createTerminal,
    createPlanTab,
    closeTerminal,
    renameTab,
    updateTab,
    reconnectToExistingSessions,
    /**
     * Force a backend re-sync of the tab list (see `reconcileTabsWithBackend`).
     * Already wired to mount / close / `terminal-exit`; exposed so an operator
     * affordance or a future recovery path can demand one.
     */
    resyncTabs,
    markReconnected,
    markAsBypass,
    markAsRemote,
    adoptWorkerTab,
    /**
     * Worker views the operator closed on this page — the input to the
     * "N hidden worker(s)" chip. Empty when none are hidden.
     */
    hiddenWorkers,
    /** Bring hidden worker views back (all of them, or the named tab ids). */
    restoreHiddenWorkers,
    /**
     * The last remote tab close that did not demonstrably release the
     * target's terminal, or `null`. Rendered by `RemoteCloseNotice`.
     */
    remoteCloseNotice,
    dismissRemoteCloseNotice,
  };
}
