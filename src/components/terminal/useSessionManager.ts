/**
 * Core hook for the Session Manager panel.
 *
 * Merges its data sources into a unified session view:
 * 1. Transcript sessions on disk (all accounts)
 * 2. Active PTY tabs in the current Runner window
 * 3. Claude Code's own live-session registry (window name, pid, self-reported
 *    status, and whether this runner owns the process)
 * 4. The external `claude`-process scan (can a tab-less session be running?)
 * 5. Transcript tail digests, requested by session id
 * 6. The batched session-info listing (`session_info_list`): each live row's
 *    work-axis finished marker, and the query facts (repo, branch, PRs, …)
 *
 * Each row's two-axis status (location × activity) is classified by the pure
 * `sessionStatusModel`. The Live list is filtered by the one query string of
 * the D5 grammar (`useSessionQuery` over each row's `SessionFacts`); a row the
 * query cannot evaluate is never hidden — it lands in `queryUnknown`, the
 * "Unknown for this filter" tail. Provides sort, grouping, and actions.
 *
 * Organization (groups, pins, tags, mute, snooze, recently closed) is the
 * runner's session-org store, read through `useSessionOrg` — never this
 * webview's localStorage (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * D3). Each row's store key is {@link UseSessionManagerReturn.sessionKeyOf}.
 */

import { useState, useCallback, useEffect, useMemo, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { instanceStorage } from "@/lib/instance-storage";
import { describeThrown } from "@/lib/utils";
import { resolvePort } from "@/lib/runner-api";
import type { TranscriptSession } from "./useTranscriptSessions";
import type { TerminalTab } from "./useTerminalManager";
import type { SessionState } from "./useZoneLayout";
import type { CommandResponse } from "./types";
import type { LockState } from "./useFileLockTracking";
import { injectedStatusOf, isInjectedSession, isInjectedStatus } from "./syntheticTabs";
import {
  extractAccountLabel,
  extractProjectLabel,
  mergeLiveRows,
  type UnclassifiedSession,
} from "./sessionManagerLiveRows";
import { refreshLiveClaudeSessions, useLiveClaudeSessions } from "./useLiveClaudeSessions";
import { refreshExternalClaudeScan, useExternalClaudeScan } from "./useExternalClaudeScan";
import { finishedMarker, rowLastActivityAt, type SessionFacts } from "./sessionFacts";
import { useSessionUnseen, type UnseenRow } from "./useSessionUnseen";
import { focusedSessionKey, type UnseenVerdict } from "./sessionUnseen";
import { useSessionFacts, useSessionInfoList, type SessionFactsRowInput } from "./useSessionFacts";
import { useSessionQuery, type SessionQueryState } from "./useSessionQuery";
import type { UnknownRow } from "./sessionQuery";
import { digestRequestIds, useSessionDigests } from "./useSessionDigests";
import { computeStatusCounts } from "./sessionCounts";
import {
  ACTIVITY_PRIORITY,
  SESSION_ACTIVITIES,
  classifyLocation,
  classifySession,
  countUnidentified,
  type RegistryRead,
  type SessionActivity,
  type SessionStatus,
  type StatusTabInput,
} from "./sessionStatusModel";
import { useSessionOrg, memberOf, sessionKeyFor, type SessionOrgView } from "./useSessionOrg";
import type { SessionOrgApplyResponse, SessionOrgOp } from "./sessionOrgTypes";
import { parseGroupBy, type SessionGroupBy } from "./sessionGrouping";
import {
  closeSessionsGracefully,
  forceCloseSessions as forceCloseItems,
  type CloseItem,
  type CloseItemResult,
} from "./sessionClose";
import { useRecentlyClosed, type UseRecentlyClosedResult } from "./useRecentlyClosed";

export type { SessionGroupBy } from "./sessionGrouping";

// ── Types ────────────────────────────────────────────────────────────────────

export interface AccountUsageInfo {
  config_dir: string;
  label: string;
  utilization: number;
  rate_limit_type: string | null;
  resets_at: number | null;
  status: string | null;
  error: string | null;
  /**
   * Actual minus expected utilization (negative = under projected usage).
   * Supplied by the `check_accounts_usage` backend. Its **sign** is what
   * places the account in a pace tier for `compareByUsageHeadroom` — it is
   * not itself a ranking key. Within the under-pace tier the ranking key is
   * `expected_utilization` DESCENDING, because unused weekly capacity expires
   * at the account's reset and does not roll over, so the account to spend is
   * the one whose spare capacity is about to be lost. Optional because older
   * probe results / error rows may omit it; an account missing it lands in the
   * `unknown` tier.
   */
  usage_delta?: number | null;
  /**
   * Expected utilization at this point in the account's 7-day window — the
   * linear elapsed fraction of that window, not a budget. Supplied by the
   * same `check_accounts_usage` command as `usage_delta` (both come off the
   * full Rust `AccountUsageInfo`), and ranked on by
   * `compareByUsageHeadroom`. Optional because a probe with no reset header
   * yields no expected value at all.
   */
  expected_utilization?: number | null;
}

export interface FileLockInfo {
  file_path: string;
  holder_task_run_id: string;
  holder_name: string;
  acquired_at: number;
}

/**
 * One file currently held in `FileRegistryManager`. Distinct from
 * {@link FileLockInfo} above — different field names (`registered_at` vs
 * `acquired_at`) and includes `worktree_id`. Sourced from the
 * `/file-registry/probe-conflicts` endpoint's `live_holdings` array.
 *
 * See `qontinui-runner/src-tauri/src/executor/file_registry.rs:412`
 * (`FileRegistryInfo`) for the canonical Rust shape.
 */
export interface FileRegistryInfoEntry {
  file_path: string;
  worktree_id?: string | null;
  holder_task_run_id: string;
  holder_name: string;
  registered_at: number;
}

/** A non-self holder of a candidate file in a {@link FileConflict}. */
export interface ConflictHolder {
  task_run_id: string;
  holder_name: string;
  registered_at: number;
}

/**
 * One file in `predicted_collisions`: a candidate path the user's prompt
 * mentioned that's already held by one or more sessions. The current
 * launch is NOT included in `other_holders` — the probe filters it out
 * before serializing.
 */
export interface FileConflict {
  file_path: string;
  worktree_id?: string | null;
  other_holders: ConflictHolder[];
}

/**
 * Predicted-collision row carried in {@link ConflictReport.predicted_collisions}.
 *
 * The Rust side flattens `PredictedCollision { conflict: FileConflict,
 * confidence: f32 }` via `#[serde(flatten)]`, so on the wire each row is
 * a `FileConflict` shape with a `confidence: number` field appended:
 *
 * ```json
 * { "file_path": "src/lib.rs", "worktree_id": null,
 *   "other_holders": [ ... ], "confidence": 0.7 }
 * ```
 *
 * Confidence buckets (Phase 1 plan delta gap §1):
 * - `>= 0.9` — `extracted_candidates` contains the literal path (red "match")
 * - `0.5..0.9` — substring/basename match against a `live_holdings` path
 *   (orange "likely")
 * - `< 0.5` — directory-level overlap only (gray "maybe")
 */
export interface PredictedCollision extends FileConflict {
  /** 0.0..1.0 — see bucket reference above. */
  confidence: number;
}

/**
 * The most recent prior editor of a candidate path, from
 * `session_touched_files`. Hint for "X knows this file" even when no
 * current holder.
 */
export interface RecentEditor {
  file_path: string;
  task_run_id: string;
}

/**
 * Status of the AI extractor's call. Mirrors the Rust `AiStatus` enum
 * (default `serde::Serialize` → PascalCase variant names). Frontend
 * renders a small badge for any non-`"Ok"` value.
 */
export type AiStatus = "Ok" | "Offline" | "Disabled" | "RateLimited";

/**
 * Response from `POST /file-registry/probe-conflicts`. Drives the
 * Currently-editing panel + predicted-collision warning in
 * `LaunchMenu`.
 */
export interface ConflictReport {
  live_holdings: FileRegistryInfoEntry[];
  /**
   * Each row is a {@link PredictedCollision} — a {@link FileConflict}
   * with a `confidence` number appended.
   */
  predicted_collisions: PredictedCollision[];
  recent_editors: RecentEditor[];
  extracted_candidates: string[];
  /**
   * Status of the AI extractor for this probe. When non-`"Ok"`, the
   * UI surfaces an `AiStatusBadge` so the user knows the extractor
   * fell back to regex-only (or is off entirely) — this avoids
   * silently trusting an empty `predicted_collisions` list when the
   * AI half of the pipeline degraded.
   */
  ai_status: AiStatus;
  /** Number of candidates contributed by the AI extractor. */
  ai_extracted_count: number;
  /** Number of candidates contributed by the regex extractor. */
  regex_extracted_count: number;
}

/**
 * Request body for `POST /file-registry/probe-conflicts`.
 *
 * `hinted_files` (Phase 1 plan delta gap §2) lets callers that already
 * know paths (drag-and-drop into the launch sheet, plan-derived launch)
 * skip the round-trip through prompt extraction. The runner unions
 * these with the result of `extract_candidate_paths(prompt, cwd)`
 * before consulting the registry / PG.
 */
export interface ProbeConflictsRequest {
  /** Prompt text to extract candidate paths from. `null` → no extraction. */
  prompt: string | null;
  /** Optional working directory for relative-path resolution. */
  cwd?: string;
  /**
   * Optional pre-known candidate paths. Unioned with the prompt-extracted
   * set on the runner side; either input alone is sufficient.
   */
  hinted_files?: string[];
}

/**
 * Fields every session-manager row carries, whichever source produced it.
 * {@link UnifiedSession} adds the `source` discriminant and `_transcript`.
 */
export interface UnifiedSessionFields {
  /**
   * The row's key. The Claude session id for a transcript row; the TAB id for
   * a tab-sourced row (which has no transcript, and possibly no session id).
   */
  sessionId: string;
  accountLabel: string;
  configDir: string;
  projectPath: string;
  projectLabel: string;

  // Transcript metadata
  displayName: string;
  /**
   * The operator's real `/rename` name (or Claude's auto `summary`) when the
   * transcript carries one; `null` otherwise. Preferred over `displayName` (a
   * first-message preview) as the card headline, matching the Previous-Sessions
   * surface and `claude --resume`.
   */
  resumeName: string | null;
  /**
   * The name Claude Code itself is showing in this session's window right now,
   * read from its live-session registry via {@link useLiveClaudeSessions}.
   *
   * **Outranks every other name field** — it is the string the operator is
   * literally looking at, whereas `resumeName` is transcript-derived and matched
   * the window name in only 11 of 33 measured cases (2026-07-23).
   *
   * `null` in two cases, both of which mean "keep showing what you show today":
   * the session is not live (the registry only knows running processes), or its
   * registry row is `nameSource: "derived"` — Claude Code's own cwd slug, which
   * is weaker than `resumeName`/`displayName` and must not preempt them.
   */
  registryName: string | null;
  firstMessagePreview: string | null;
  messageCount: number;
  lastModified: string;
  hasPlans: boolean;

  /**
   * Where the session's process is and what it is doing — the two axes of
   * `sessionStatusModel` (plan
   * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
   * D2). Classified per row from the tab, the live-session registry, the
   * external scan, the transcript digest and the finished marker.
   */
  status: SessionStatus;
  zoneTabId: string | null;
  zoneIndex: number | null;
  /**
   * The device a remote session tab mirrors (`TerminalTab.remote.deviceLabel`);
   * `null` for a local session and for every transcript row.
   */
  remoteDeviceLabel: string | null;

  // Timing
  startedAt: string | null;
  durationMs: number | null; // total duration from start to last modified

  // Digest data
  lastMessageType: string;
  lastMessageTimestamp: string;
  lastMessagePreview: string;
  workSummaryHint: string;
}

/** A row projected from a Claude transcript (`message_count > 0`). */
export interface TranscriptSessionRow extends UnifiedSessionFields {
  source: "transcript";
  // Original transcript session (for resume)
  _transcript: TranscriptSession;
}

/**
 * A row for a live tab no transcript row represents — a plain shell, a session
 * not yet past its first message, a remote session tab. Built by
 * `sessionManagerLiveRows.ts`; keyed by the tab id, with no transcript, so the
 * transcript-only actions (view transcript, copy session id, resume, promote,
 * commit) do not apply to it.
 */
export interface TabSessionRow extends UnifiedSessionFields {
  source: "tab";
  _transcript: null;
}

export type UnifiedSession = TranscriptSessionRow | TabSessionRow;

export type SessionSortBy = "recent" | "oldest" | "messages" | "staleness";

export interface UseSessionManagerParams {
  tabs: TerminalTab[];
  sessionStates: Record<string, SessionState>;
  staleTabs: Set<string>;
  transcriptSessions: TranscriptSession[];
  sessionsLoading: boolean;
  desktopNotify: boolean;
  onRefreshSessions: () => void;
  onResumeSession: (session: TranscriptSession) => void;
  onSelectSession: (sessionId: string) => void;
  /**
   * Bring a session's open terminal tab into view. Called by `openSession`
   * instead of `onSelectSession` when the session is backed by a live tab in
   * this window, so a card click takes the operator to the running session
   * rather than to a read-only transcript of it.
   */
  onFocusSessionTab: (sessionId: string, tabId: string, source: UnifiedSession["source"]) => void;
  /**
   * Close a session's terminal tab — the page's own close path
   * (`useTerminalManager().closeTerminal`, which records the durable close).
   * In tmux mode the panel is the only navigator, so without a close on the
   * row a hidden session could be closed only by showing it first.
   */
  onCloseSessionTab?: (tabId: string) => void;
  /**
   * The tabs whose pane is showing right now — the `On screen` vs `Hidden
   * here` split. In tmux mode that is the shown tab; in grid mode, the zoned
   * tabs (only the maximized one while a zone is maximized). See
   * `visibleTabIdsForLayout`.
   */
  visibleTabIds: ReadonlySet<string>;
  /** Per-tab file-lock state (`useHotField(pageId, "lockStates")`). */
  tabLockStates?: Readonly<Record<string, LockState | undefined>>;
  /**
   * This page and window — the pane identity a tab with no Claude session id
   * is organized under (`sessionKeyFor`), and the window the lens is
   * persisted for. Default: the default page in the main window.
   */
  pageId?: string;
  windowLabel?: string;
  /**
   * The tab the operator is looking at (the page's active / focused tab);
   * `null` = none. Focusing a session clears its unseen dot (Phase 7).
   */
  focusedTabId?: string | null;
}

/** One session's Move-to-page outcome. */
export interface MoveToPageResult {
  sessionId: string;
  ok: boolean;
  detail: string;
}

export interface UseSessionManagerReturn {
  /** The rows the filters and sort select, in display order. */
  sessions: UnifiedSession[];
  /**
   * Every Live row, unfiltered — what a close confirmation re-reads while it
   * is open and what the `session-org` bridge addresses by id.
   */
  allSessions: UnifiedSession[];
  loading: boolean;
  accountUsage: AccountUsageInfo[];
  launchCommands: Record<string, string>;
  /** Machine-global default AI launch command template (null = built-in default). */
  defaultLaunchCommand: string | null;
  fileLocks: FileLockInfo[];

  // Filter: the one query string (D5), persisted per window.
  /** The query, its parse, the filter result and every chip's count. */
  query: SessionQueryState<SessionFacts>;
  /**
   * Rows the query could not evaluate — a fact it needs is UNKNOWN — each
   * with the reason, in display order. Never hidden: the panel lists them
   * under "Unknown for this filter (N)". Empty on a parse error (nothing is
   * filtered then) and for the empty query.
   */
  queryUnknown: readonly UnknownRow<SessionFacts>[];
  /** Every Live row's query facts (`allSessions` order). */
  liveFacts: readonly SessionFacts[];
  /** Facts by row id (`UnifiedSession.sessionId`). */
  factsById: ReadonlyMap<string, SessionFacts>;
  /**
   * The unseen-activity dot of a row (Phase 7) — the same verdict
   * `is:unseen` filters on (`SessionFacts.unseen`).
   */
  unseenOf: (session: UnifiedSession) => UnseenVerdict;
  groupBy: SessionGroupBy;
  setGroupBy: (g: SessionGroupBy) => void;
  sortBy: SessionSortBy;
  setSortBy: (s: SessionSortBy) => void;

  // Actions
  refresh: () => void;
  resumeSession: (session: UnifiedSession) => void;
  viewTranscript: (session: UnifiedSession) => void;
  /**
   * The card-body click: go to the session's open terminal tab when it has a
   * live one in this window, else show its transcript. `viewTranscript` stays
   * the explicit "show me the transcript" action.
   */
  openSession: (session: UnifiedSession) => void;
  copySessionId: (session: UnifiedSession) => void;
  /**
   * The tab a row's close action would close — live or exited — or `null`
   * when the row has none in this window (a transcript-only row): its card
   * then offers no close.
   */
  closableTabId: (session: UnifiedSession) => string | null;
  /**
   * Close the rows' sessions the D7 way — `graceful_exit`, per-item outcome,
   * never a kill — and record each one that closed as recently closed.
   */
  closeSessions: (sessions: readonly UnifiedSession[]) => Promise<CloseItemResult[]>;
  /**
   * Force close: the page's plain close, which kills the pane tree. Its own,
   * separately confirmed action; records each closed session the same way.
   */
  forceCloseSessions: (sessions: readonly UnifiedSession[]) => CloseItemResult[];
  /** Move the rows' live tabs to another Terminal page. */
  moveSessionsToPage: (
    sessions: readonly UnifiedSession[],
    pageId: string,
  ) => Promise<MoveToPageResult[]>;

  // Organization (the runner's session-org store)
  /** The store as this window sees it: loading, unknown, or known. */
  org: SessionOrgView;
  /**
   * Apply org ops. A refusal or transport failure lands on
   * `org.lastError`; the returned promise never rejects.
   */
  applyOrg: (ops: SessionOrgOp[]) => Promise<void>;
  /**
   * Apply org ops and answer the runner's per-op results and snapshot —
   * what a bulk action reads each session back from. Rejects when the runner
   * could not be asked.
   */
  applyOrgChecked: (ops: SessionOrgOp[]) => Promise<SessionOrgApplyResponse>;
  /** The store's Recently closed list (newest first). */
  recentlyClosed: UseRecentlyClosedResult["recentlyClosed"];
  /** Reopen one closed session: resume it and put it back in its group. */
  reopenClosed: UseRecentlyClosedResult["reopenClosed"];
  /** Reopen the newest closed session that can be reopened. */
  reopenLastClosed: UseRecentlyClosedResult["reopenLastClosed"];
  /** A row's organization-store key (`sessionKeyFor`). */
  sessionKeyOf: (session: UnifiedSession) => string;

  // Derived
  allAccounts: string[];
  /**
   * The operator-CONFIGURED Claude account config dirs (from
   * `claude_config_dirs` settings) — distinct from `accountUsage`, which
   * also probes transcript-derived dirs (e.g. the default `~/.claude`).
   * New AI sessions must only spawn into a configured account.
   */
  savedConfigDirs: string[];

  // Stats
  /** Rows per activity, over every row (the header's dataset counts). */
  activityCounts: Readonly<Record<SessionActivity, number>>;
  /** Rows reading `No output (mid-tool)` — the Ctrl+Shift+J cycle. */
  noOutputCount: number;
  /** Rows reading `Needs you`. */
  needsInputCount: number;
  /**
   * Live unmanaged `claude` processes with no registry row — counted, never
   * attributed to a session. `null` = UNKNOWN (the scan or the registry read
   * did not answer), never rendered as zero.
   */
  unidentifiedCount: number | null;
  // Session-model counts for the Terminal-page StatusStrip (Claude
  // sessions, not PTY tabs). See `computeStatusCounts` for the bucketing.
  claudeSessionCount: number;
  workingCount: number;
  /** The `Outside runner` share of `workingCount` — see {@link StatusCounts}. */
  outsideWorkingCount: number;
  idleCount: number;
  completedCount: number;
  errorCount: number;
  /** Rows counted live whose activity is `Unknown`. */
  unknownCount: number;
}

// ── Helpers ──────────────────────────────────────────────────────────────────

function computeDuration(
  startedAt: string | null | undefined,
  lastModified: string,
): number | null {
  if (!startedAt) return null;
  try {
    const start = new Date(startedAt).getTime();
    const end = new Date(lastModified).getTime();
    const diff = end - start;
    return diff > 0 ? diff : null;
  } catch {
    return null;
  }
}

// ── Hook ─────────────────────────────────────────────────────────────────────

/**
 * The tab a card click should take the operator to, or null when the session
 * has no LIVE tab in this window and its transcript is all there is to show.
 *
 * - `zoneTabId` is set for any tab carrying the session's id, exited ones
 *   included; revealing an exited stub would push a live terminal out of its
 *   zone to show a dead one, so only a live tab qualifies.
 * - Injected fixtures point `zoneTabId` at a synthetic tab no zone can hold.
 * - A tab-sourced row (`_transcript: null`) points `zoneTabId` at the live tab
 *   it was built from, so it resolves exactly like a transcript row's join.
 */
export function liveTabIdForSession(
  session: Pick<UnifiedSession, "zoneTabId" | "_transcript">,
  tabs: readonly Pick<TerminalTab, "id" | "isAlive">[],
): string | null {
  const tabId = session.zoneTabId;
  if (!tabId || isInjectedSession(session)) return null;
  return tabs.some((t) => t.id === tabId && t.isAlive) ? tabId : null;
}

/**
 * The tab a row's close would act on: its tab in this window whether it is
 * live or exited (closing an exited one only removes the tab), or null when
 * the row has none here. Injected fixtures have no real tab.
 */
export function tabIdForSession(
  session: Pick<UnifiedSession, "zoneTabId" | "_transcript">,
  tabs: readonly Pick<TerminalTab, "id">[],
): string | null {
  const tabId = session.zoneTabId;
  if (!tabId || isInjectedSession(session)) return null;
  return tabs.some((t) => t.id === tabId) ? tabId : null;
}

/**
 * Where a session-card click goes.
 *
 *   - `focus-tab`: the row has a live tab in this window — bring it into view
 *     (in tmux mode, swap it on screen).
 *   - `transcript`: a transcript row with no live tab — open its transcript.
 *   - `none`: a tab-sourced row whose tab is gone. Its key is a TAB id, so
 *     handing it to the transcript panel would show a "transcript" for an id
 *     no transcript has — the panel would keep painting the PREVIOUS session's
 *     messages under it. There is nothing to open.
 */
export type OpenSessionRoute =
  | { kind: "focus-tab"; tabId: string }
  | { kind: "transcript" }
  | { kind: "none" };

export function routeOpenSession(
  session: Pick<UnifiedSession, "source" | "zoneTabId" | "_transcript">,
  tabs: readonly Pick<TerminalTab, "id" | "isAlive">[],
): OpenSessionRoute {
  const tabId = liveTabIdForSession(session, tabs);
  if (tabId) return { kind: "focus-tab", tabId };
  return session.source === "transcript" ? { kind: "transcript" } : { kind: "none" };
}

export function useSessionManager(params: UseSessionManagerParams): UseSessionManagerReturn {
  const {
    tabs,
    sessionStates,
    staleTabs,
    transcriptSessions,
    sessionsLoading,
    desktopNotify,
    onRefreshSessions,
    onResumeSession,
    onSelectSession,
    onFocusSessionTab,
    onCloseSessionTab,
    visibleTabIds,
    tabLockStates,
    pageId = "default",
    windowLabel = "main",
    focusedTabId = null,
  } = params;

  // Account usage state
  const [accountUsage, setAccountUsage] = useState<AccountUsageInfo[]>([]);

  // Per-account launch commands (e.g. "clg" for gmail, "clh" for hotmail)
  const [launchCommands, setLaunchCommands] = useState<Record<string, string>>({});

  // Machine-global default AI launch command template (null = built-in default)
  const [defaultLaunchCommand, setDefaultLaunchCommand] = useState<string | null>(null);

  // Active file locks (for Launch Menu visibility)
  const [fileLocks, setFileLocks] = useState<FileLockInfo[]>([]);

  // Filter/sort state (persisted across restarts)
  const STORAGE_PREFIX = "session-manager-";
  // The lens is persisted per WINDOW; "My groups" when nothing is stored.
  const groupByKey = `${STORAGE_PREFIX}group-by:${windowLabel}`;
  const [groupBy, setGroupBy] = useState<SessionGroupBy>(() =>
    parseGroupBy(instanceStorage.getItem(groupByKey)),
  );
  const [sortBy, setSortBy] = useState<SessionSortBy>(
    () => (instanceStorage.getItem(`${STORAGE_PREFIX}sort-by`) as SessionSortBy) || "recent",
  );

  // Persist filter/sort changes
  useEffect(() => {
    instanceStorage.setItem(groupByKey, groupBy);
  }, [groupByKey, groupBy]);
  useEffect(() => {
    instanceStorage.setItem(`${STORAGE_PREFIX}sort-by`, sortBy);
  }, [sortBy]);

  // ── Organization (the runner's session-org store) ────────────────────────

  const { view: org, apply: applyOrgRaw, importLegacyKeys } = useSessionOrg();

  // The one-time import of the retired `session-manager-pinned` /
  // `session-manager-labels` localStorage keys; the client runs it at most
  // once per webview and deletes the keys after.
  useEffect(() => {
    void importLegacyKeys();
  }, [importLegacyKeys]);

  const applyOrg = useCallback(
    async (ops: SessionOrgOp[]) => {
      if (ops.length === 0) return;
      try {
        await applyOrgRaw(ops);
      } catch {
        // Surfaced as `org.lastError` by the client; the org is re-read.
      }
    },
    [applyOrgRaw],
  );

  // ── Fetch account usage (every 5 minutes) ─────────────────────────────────

  // Merge config dirs from transcript sessions AND saved settings so the
  // launch menu shows accounts even when no sessions exist yet.
  const [savedConfigDirs, setSavedConfigDirs] = useState<string[]>([]);
  useEffect(() => {
    (async () => {
      try {
        const result = await invoke<CommandResponse>("get_claude_config_dirs");
        if (result.success && result.data) {
          const data = result.data as { dirs: string[] };
          if (data.dirs?.length) setSavedConfigDirs(data.dirs);
        }
      } catch {
        // Silently fail
      }
      try {
        const result = await invoke<CommandResponse>("get_claude_account_launch_commands");
        if (result.success && result.data) {
          const data = result.data as { commands: Record<string, string> };
          if (data.commands) setLaunchCommands(data.commands);
        }
      } catch {
        // Silently fail
      }
      try {
        const result = await invoke<CommandResponse>("get_claude_default_launch_command");
        if (result.success && result.data) {
          const data = result.data as { command: string | null };
          if (data.command) setDefaultLaunchCommand(data.command);
        }
      } catch {
        // Silently fail
      }
    })();
  }, []);

  // Stable key for config dirs so the interval doesn't reset on every session list change
  const configDirsKey = useMemo(() => {
    const fromSessions = transcriptSessions.map((s) => s.config_dir);
    const dirs = [...new Set([...fromSessions, ...savedConfigDirs])].sort();
    return dirs.join("|");
  }, [transcriptSessions, savedConfigDirs]);

  const configDirsRef = useRef<string[]>([]);
  useEffect(() => {
    configDirsRef.current = configDirsKey ? configDirsKey.split("|") : [];
  }, [configDirsKey]);

  const fetchAccountUsage = useCallback(async () => {
    const configDirs = configDirsRef.current;
    if (configDirs.length === 0) return;

    try {
      const result = await invoke<CommandResponse>("check_accounts_usage", {
        configDirs,
      });
      if (result.success && result.data) {
        setAccountUsage(result.data as AccountUsageInfo[]);
      }
    } catch {
      // Silently fail
    }
  }, []);

  useEffect(() => {
    if (configDirsKey) {
      fetchAccountUsage();
      const interval = setInterval(fetchAccountUsage, 5 * 60 * 1000);
      return () => clearInterval(interval);
    }
  }, [fetchAccountUsage, configDirsKey]);

  // ── Fetch active file locks (every 10s) ────────────────────────────────────

  const fetchFileLocks = useCallback(async () => {
    try {
      // Resolve at call time so secondary runners pick up the
      // actually-bound port once `useApiReady` populates it (the
      // `api-ready` event may arrive after this hook mounts).
      const port = resolvePort();
      const resp = await fetch(`http://127.0.0.1:${port}/file-locks/info`);
      if (resp.ok) {
        const data = (await resp.json()) as FileLockInfo[];
        setFileLocks(data);
      }
    } catch {
      // Silently fail — endpoint may not be available yet
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    // Defer via microtask so the effect body itself doesn't synchronously
    // trigger setState inside fetchFileLocks.
    void Promise.resolve().then(() => {
      if (!cancelled) void fetchFileLocks();
    });
    const interval = setInterval(fetchFileLocks, 10_000);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [fetchFileLocks]);

  // ── Build tab lookup for live status correlation ───────────────────────────

  const tabSessionMap = useMemo(() => {
    const map = new Map<string, TerminalTab>();
    for (const tab of tabs) {
      if (tab.claudeSessionId) {
        map.set(tab.claudeSessionId, tab);
      }
    }
    return map;
  }, [tabs]);

  // ── Claude Code's live-session registry and the external scan (webview-wide
  // stores, polled once for every page scope) ───────────────────────────────

  const liveRegistry = useLiveClaudeSessions();
  const registryNames = liveRegistry.names;
  const externalScan = useExternalClaudeScan();

  // ── Merge into rows (status classified below) ────────────────────────────

  const transcriptRowsBase = useMemo((): UnclassifiedSession[] => {
    return transcriptSessions
      .filter((s) => s.message_count > 0)
      .map((s): UnclassifiedSession => {
        // Injected fakes state their status outright (see `statusForRow`), so
        // they carry no tab: a short-circuit fake has no real PTY in any zone.
        const tab = isInjectedStatus(s) ? undefined : tabSessionMap.get(s.session_id);
        return {
          source: "transcript",
          sessionId: s.session_id,
          accountLabel: extractAccountLabel(s.config_dir),
          configDir: s.config_dir,
          projectPath: s.project_path,
          projectLabel: extractProjectLabel(s.project_path),
          displayName: s.display_name,
          resumeName: s.resume_name ?? null,
          registryName: registryNames.get(s.session_id) ?? null,
          firstMessagePreview: s.first_message_preview,
          messageCount: s.message_count,
          lastModified: s.last_modified,
          hasPlans: s.has_plans,
          zoneTabId: tab?.id ?? null,
          zoneIndex: null, // Could be enriched from zone assignments
          remoteDeviceLabel: null,
          startedAt: s.started_at ?? null,
          durationMs: computeDuration(s.started_at, s.last_modified),
          lastMessageType: "",
          lastMessageTimestamp: s.last_modified,
          lastMessagePreview: "",
          workSummaryHint: "",
          _transcript: s,
        };
      });
  }, [transcriptSessions, tabSessionMap, registryNames]);

  // Every live tab gets a row (plan 2026-10-11-runner-terminal-tmux-work-mode,
  // D5): its transcript row when it has one, else a tab-sourced row. In tmux
  // mode the panel is the only way to reach an off-screen session.
  const mergedRows = useMemo(
    () => mergeLiveRows(transcriptRowsBase, { tabs, registryNames }),
    [transcriptRowsBase, tabs, registryNames],
  );

  // ── Status inputs per row ────────────────────────────────────────────────

  const tabsById = useMemo(() => new Map(tabs.map((t) => [t.id, t])), [tabs]);

  // The store key: the Claude session id when the row has one — a transcript
  // row's own id, a tab row's tab's — else the tab's window-aware pane key.
  const sessionKeyOf = useCallback(
    (session: Pick<UnifiedSession, "source" | "sessionId">): string => {
      if (session.source === "transcript") return session.sessionId;
      const tab = tabsById.get(session.sessionId);
      return sessionKeyFor(tab?.claudeSessionId ?? null, {
        pageId,
        title: tab?.spawnName ?? tab?.title ?? session.sessionId,
        workingDir: tab?.workingDir ?? "",
        windowLabel,
        remoteDeviceId: tab?.remote?.deviceId,
      });
    },
    [tabsById, pageId, windowLabel],
  );

  const tabInputFor = useCallback(
    (row: UnclassifiedSession): StatusTabInput | null => {
      const tab = row.zoneTabId ? tabsById.get(row.zoneTabId) : undefined;
      if (!tab) return null;
      return {
        alive: tab.isAlive,
        visible: visibleTabIds.has(tab.id),
        exitCode: tab.exitCode,
        state: sessionStates[tab.id],
        silent: staleTabs.has(tab.id),
        lockWaiting: tabLockStates?.[tab.id]?.kind === "waiting",
      };
    },
    [tabsById, visibleTabIds, sessionStates, staleTabs, tabLockStates],
  );

  const registryFor = useCallback(
    (row: UnclassifiedSession): RegistryRead => {
      if (liveRegistry.read !== "ok") return { kind: liveRegistry.read };
      // A tab-sourced row is keyed by its TAB id; its Claude session id, when
      // the tab has one, is the registry key.
      const claudeId =
        row.source === "transcript"
          ? row.sessionId
          : (tabsById.get(row.sessionId)?.claudeSessionId ?? null);
      return {
        kind: "ok",
        match: claudeId ? (liveRegistry.bySessionId.get(claudeId) ?? null) : null,
      };
    },
    [liveRegistry, tabsById],
  );

  /** The Claude session id a row carries: its own, or its tab's. */
  const claudeIdOf = useCallback(
    (row: Pick<UnclassifiedSession, "source" | "sessionId">): string | null =>
      row.source === "transcript"
        ? row.sessionId
        : (tabsById.get(row.sessionId)?.claudeSessionId ?? null),
    [tabsById],
  );

  // ── The batched session-info listing: ONE call for every row's Claude
  // session id. Each live row's finished marker comes from its lifecycle
  // record here; the same listing feeds the query facts below. A missing
  // source is UNKNOWN, never "not finished" (`finishedMarker`). ────────────
  const infoIds = useMemo(
    () =>
      mergedRows
        .filter((r) => !(r.source === "transcript" && isInjectedStatus(r._transcript)))
        .map(claudeIdOf)
        .filter((id): id is string => id !== null),
    [mergedRows, claudeIdOf],
  );
  const { info: sessionInfo, refresh: refreshSessionInfo } = useSessionInfoList(infoIds);

  // The location axis first: it decides which sessions get a digest (every
  // one that may be running, however many — no 50-row cap).
  const digestIds = useMemo(
    () =>
      digestRequestIds(
        mergedRows
          .filter((r) => r.source === "transcript" && !isInjectedStatus(r._transcript))
          .map((r) => ({
            sessionId: r.sessionId,
            lastModified: r.lastModified,
            location: classifyLocation({
              tab: tabInputFor(r),
              registry: registryFor(r),
              externalScan: externalScan.state,
            }).value,
          })),
      ),
    [mergedRows, tabInputFor, registryFor, externalScan.state],
  );
  const {
    digests,
    loading: digestsLoading,
    refresh: refreshDigests,
  } = useSessionDigests(digestIds);

  // ── Classify ─────────────────────────────────────────────────────────────

  const allSessions = useMemo(
    (): UnifiedSession[] =>
      mergedRows.map((row): UnifiedSession => {
        if (row.source === "transcript") {
          const injected = injectedStatusOf(row._transcript);
          if (injected) return { ...row, status: injected };
          const digest = digests[row.sessionId];
          return {
            ...row,
            lastMessageType: digest?.last_message_type ?? "",
            lastMessageTimestamp: digest?.last_message_timestamp ?? row.lastModified,
            lastMessagePreview: digest?.last_message_preview ?? "",
            workSummaryHint: digest?.work_summary_hint ?? "",
            status: classifySession({
              tab: tabInputFor(row),
              registry: registryFor(row),
              externalScan: externalScan.state,
              digest: digest
                ? {
                    lastMessageType: digest.last_message_type,
                    lastAssistantHadToolUse: digest.last_assistant_had_tool_use,
                    lastMessageTimestamp: digest.last_message_timestamp,
                  }
                : null,
              finished: finishedMarker(sessionInfo, row.sessionId),
            }),
          };
        }
        return {
          ...row,
          status: classifySession({
            tab: tabInputFor(row),
            registry: registryFor(row),
            externalScan: externalScan.state,
            digest: "not-applicable",
            finished: finishedMarker(sessionInfo, claudeIdOf(row)),
          }),
        };
      }),
    [mergedRows, digests, tabInputFor, registryFor, externalScan.state, sessionInfo, claudeIdOf],
  );

  const transcriptRows = useMemo(
    () => allSessions.filter((s): s is TranscriptSessionRow => s.source === "transcript"),
    [allSessions],
  );

  // ── Unseen activity (Phase 7) ────────────────────────────────────────────
  // Activity changed or output arrived after the session's `lastSeenAt` in
  // the org store; cleared by focusing it. The verdict feeds both the card's
  // dot and the `is:unseen` fact, so the filter shows exactly the dotted rows.

  const unseenRows = useMemo(
    (): UnseenRow[] =>
      allSessions.map((row) => ({
        key: sessionKeyOf(row),
        activity: row.status.activity,
        lastActivityAt: rowLastActivityAt(row),
      })),
    [allSessions, sessionKeyOf],
  );
  const focusedKey = useMemo(
    () => focusedSessionKey(allSessions, focusedTabId, sessionKeyOf),
    [allSessions, focusedTabId, sessionKeyOf],
  );
  const { verdictOf: unseenVerdictOf, markSeen } = useSessionUnseen({
    rows: unseenRows,
    org,
    apply: applyOrg,
    focusedKey,
  });
  const unseenOf = useCallback(
    (session: UnifiedSession) => unseenVerdictOf(sessionKeyOf(session)),
    [unseenVerdictOf, sessionKeyOf],
  );

  // ── Query facts and the query (D5) ───────────────────────────────────────

  const factsRows = useMemo(
    (): SessionFactsRowInput[] =>
      allSessions.map((row) => {
        const sessionKey = sessionKeyOf(row);
        return {
          row,
          sessionKey,
          claudeSessionId: claudeIdOf(row),
          // A row with a tab in this window is on this page.
          page:
            row.zoneTabId !== null || row.source === "tab" ? { id: pageId, name: pageId } : null,
          unseen: unseenVerdictOf(sessionKey),
        };
      }),
    [allSessions, sessionKeyOf, claudeIdOf, pageId, unseenVerdictOf],
  );
  const { facts: factsById } = useSessionFacts({ rows: factsRows, org, info: sessionInfo });
  const liveFacts = useMemo(
    () => allSessions.map((s) => factsById.get(s.sessionId)).filter((f): f is SessionFacts => !!f),
    [allSessions, factsById],
  );
  const query = useSessionQuery({ rows: liveFacts, windowLabel });

  // ── Filter and sort ────────────────────────────────────────────────────────

  const sortRows = useCallback(
    (rows: readonly UnifiedSession[]): UnifiedSession[] => {
      const sorted = [...rows];
      switch (sortBy) {
        case "recent":
          sorted.sort((a, b) => b.lastModified.localeCompare(a.lastModified));
          break;
        case "oldest":
          sorted.sort((a, b) => a.lastModified.localeCompare(b.lastModified));
          break;
        case "messages":
          sorted.sort((a, b) => b.messageCount - a.messageCount);
          break;
        case "staleness":
          // Sort by activity priority (Needs you first), then by recency
          sorted.sort((a, b) => {
            const pa = ACTIVITY_PRIORITY[a.status.activity];
            const pb = ACTIVITY_PRIORITY[b.status.activity];
            if (pa !== pb) return pa - pb;
            return b.lastModified.localeCompare(a.lastModified);
          });
          break;
      }

      // When grouping by status, always apply status priority as primary sort
      if (groupBy === "status") {
        sorted.sort((a, b) => {
          const pa = ACTIVITY_PRIORITY[a.status.activity];
          const pb = ACTIVITY_PRIORITY[b.status.activity];
          if (pa !== pb) return pa - pb;
          return b.lastModified.localeCompare(a.lastModified);
        });
      }

      // Pinned sessions always float to the top (the "My groups" lens gives
      // them their own section instead).
      if (org.phase === "known") {
        const pinned = (s: UnifiedSession) => (memberOf(org, sessionKeyOf(s))?.pinned ? 0 : 1);
        sorted.sort((a, b) => pinned(a) - pinned(b));
      }
      return sorted;
    },
    [sortBy, groupBy, org, sessionKeyOf],
  );

  const rowById = useMemo(() => new Map(allSessions.map((s) => [s.sessionId, s])), [allSessions]);

  // A parse error filters nothing (the bar shows the error); otherwise only
  // the rows the query decided TRUE. Rows it could not decide go to the
  // Unknown tail, never silently into or out of the list.
  const sessions = useMemo(() => {
    const result = query.result;
    if (!result.ok) return sortRows(allSessions);
    return sortRows(
      result.matched
        .map((f) => rowById.get(f.rowId))
        .filter((s): s is UnifiedSession => s !== undefined),
    );
  }, [query.result, allSessions, rowById, sortRows]);

  const queryUnknown = useMemo((): UnknownRow<SessionFacts>[] => {
    const result = query.result;
    if (!result.ok || result.unknown.length === 0) return [];
    const reasonOf = new Map(result.unknown.map((u) => [u.facts.rowId, u]));
    return sortRows(
      result.unknown
        .map((u) => rowById.get(u.facts.rowId))
        .filter((s): s is UnifiedSession => s !== undefined),
    ).flatMap((s) => {
      const u = reasonOf.get(s.sessionId);
      return u ? [u] : [];
    });
  }, [query.result, rowById, sortRows]);

  // ── Derived ─────────────────────────────────────────────────────────────────

  const allAccounts = useMemo(() => {
    const set = new Set<string>();
    for (const s of allSessions) {
      set.add(s.accountLabel);
    }
    return Array.from(set).sort();
  }, [allSessions]);

  // ── Stats ──────────────────────────────────────────────────────────────────

  const activityCounts = useMemo(() => {
    const counts = Object.fromEntries(SESSION_ACTIVITIES.map((a) => [a, 0])) as Record<
      SessionActivity,
      number
    >;
    for (const s of allSessions) counts[s.status.activity]++;
    return counts;
  }, [allSessions]);
  const noOutputCount = activityCounts["no-output-mid-tool"];
  const needsInputCount = activityCounts["needs-you"];

  const unidentifiedCount = useMemo(
    () => countUnidentified(externalScan, liveRegistry.read === "ok" ? liveRegistry.pids : null),
    [externalScan, liveRegistry],
  );

  // Per-bucket breakdown for the Terminal-page StatusStrip pills — Claude
  // sessions only (transcript rows; a tab-sourced row is a PTY tab), and only
  // the ones that may be running. See `computeStatusCounts`.
  const statusCounts = useMemo(() => computeStatusCounts(transcriptRows), [transcriptRows]);

  // ── Desktop notification for sessions that went silent mid-tool ──────────

  const prevNoOutputCountRef = useRef(0);
  useEffect(() => {
    if (
      desktopNotify &&
      noOutputCount > prevNoOutputCountRef.current &&
      document.hidden &&
      "Notification" in window &&
      Notification.permission === "granted"
    ) {
      const added = noOutputCount - prevNoOutputCountRef.current;
      new Notification("Session Manager", {
        body: `${added} session${added > 1 ? "s have" : " has"} produced no output mid-tool`,
        tag: "session-manager-no-output",
      });
    }
    prevNoOutputCountRef.current = noOutputCount;
  }, [noOutputCount, desktopNotify]);

  // ── Actions ────────────────────────────────────────────────────────────────

  const refresh = useCallback(() => {
    onRefreshSessions();
    refreshDigests();
    void refreshLiveClaudeSessions();
    void refreshExternalClaudeScan();
    refreshSessionInfo();
  }, [onRefreshSessions, refreshDigests, refreshSessionInfo]);

  const resumeSession = useCallback(
    (session: UnifiedSession) => {
      // A tab-sourced row has no transcript to resume — it IS the live tab.
      if (session.source !== "transcript") return;
      // F1 inertness: a debug-injected fake has no real transcript on disk —
      // resuming it would spawn a stray `claude --resume <fake-id>` against a
      // session id that doesn't exist. No-op it.
      if (isInjectedSession(session)) {
        console.warn(
          `[test-fixtures] ignoring resume on synthetic tab ${session.zoneTabId ?? session.sessionId}`,
        );
        return;
      }
      onResumeSession(session._transcript);
    },
    [onResumeSession],
  );

  const viewTranscript = useCallback(
    (session: UnifiedSession) => {
      if (session.source !== "transcript") return;
      onSelectSession(session.sessionId);
    },
    [onSelectSession],
  );

  const openSession = useCallback(
    (session: UnifiedSession) => {
      const route = routeOpenSession(session, tabs);
      if (route.kind === "focus-tab") {
        onFocusSessionTab(session.sessionId, route.tabId, session.source);
      } else if (route.kind === "transcript") {
        onSelectSession(session.sessionId);
        // Reading its transcript is seeing it; a focused tab is marked by
        // `useSessionUnseen` when focus arrives.
        markSeen(sessionKeyOf(session));
      }
    },
    [tabs, onSelectSession, onFocusSessionTab, markSeen, sessionKeyOf],
  );

  const copySessionId = useCallback((session: UnifiedSession) => {
    // A tab-sourced row's key is a tab id, not a session id.
    if (session.source !== "transcript") return;
    navigator.clipboard.writeText(session.sessionId).catch(() => {});
  }, []);

  const closableTabId = useCallback(
    (session: UnifiedSession) => (onCloseSessionTab ? tabIdForSession(session, tabs) : null),
    [tabs, onCloseSessionTab],
  );

  const closeItemOf = useCallback(
    (session: UnifiedSession): CloseItem => ({
      sessionId: session.sessionId,
      title: session.registryName || session.resumeName || session.displayName,
      tabId: closableTabId(session),
      tabAlive: liveTabIdForSession(session, tabs) !== null,
      sessionKey: sessionKeyOf(session),
      resumeCmd:
        session.source === "transcript" && !isInjectedSession(session)
          ? `claude --resume ${session.sessionId}`
          : null,
    }),
    [closableTabId, sessionKeyOf, tabs],
  );

  const closeEffects = useMemo(
    () => ({
      gracefulClose: (tabId: string) =>
        invoke<unknown>("terminal_graceful_close", { terminalId: tabId }),
      forceClose: (tabId: string) => onCloseSessionTab?.(tabId),
      // The runner already closed this tab; the page's close path drops it
      // from the window and records the durable close (its own
      // `terminal_close` then finds nothing left to kill).
      dropTab: (tabId: string) => onCloseSessionTab?.(tabId),
      recordClosed: (ops: SessionOrgOp[]) => void applyOrg(ops),
    }),
    [onCloseSessionTab, applyOrg],
  );

  const closeSessions = useCallback(
    (rows: readonly UnifiedSession[]) =>
      closeSessionsGracefully(rows.map(closeItemOf), closeEffects),
    [closeItemOf, closeEffects],
  );

  const forceCloseSessions = useCallback(
    (rows: readonly UnifiedSession[]) => forceCloseItems(rows.map(closeItemOf), closeEffects),
    [closeItemOf, closeEffects],
  );

  const moveSessionsToPage = useCallback(
    (rows: readonly UnifiedSession[], targetPageId: string) =>
      Promise.all(
        rows.map(async (session): Promise<MoveToPageResult> => {
          const tabId = liveTabIdForSession(session, tabs);
          if (!tabId) {
            return {
              sessionId: session.sessionId,
              ok: false,
              detail: "not moved: it has no live tab in this window",
            };
          }
          try {
            await invoke("terminal_set_page", { terminalId: tabId, pageId: targetPageId });
            return { sessionId: session.sessionId, ok: true, detail: "moved" };
          } catch (e) {
            return {
              sessionId: session.sessionId,
              ok: false,
              detail: `not moved: ${describeThrown(e, "terminal_set_page failed")}`,
            };
          }
        }),
      ),
    [tabs],
  );

  // ── Recently closed ────────────────────────────────────────────────────────

  const { recentlyClosed, reopenClosed, reopenLastClosed } = useRecentlyClosed({
    org,
    rows: allSessions,
    applyOrg: applyOrgRaw,
    resume: resumeSession,
    focus: openSession,
  });

  return {
    sessions,
    allSessions,
    loading: sessionsLoading || digestsLoading,
    accountUsage,
    launchCommands,
    defaultLaunchCommand,
    fileLocks,
    query,
    queryUnknown,
    liveFacts,
    factsById,
    unseenOf,
    groupBy,
    setGroupBy,
    sortBy,
    setSortBy,
    refresh,
    resumeSession,
    viewTranscript,
    openSession,
    copySessionId,
    closableTabId,
    closeSessions,
    forceCloseSessions,
    moveSessionsToPage,
    org,
    applyOrg,
    applyOrgChecked: applyOrgRaw,
    recentlyClosed,
    reopenClosed,
    reopenLastClosed,
    sessionKeyOf,
    allAccounts,
    savedConfigDirs,
    activityCounts,
    noOutputCount,
    needsInputCount,
    unidentifiedCount,
    // Session-model counts for the StatusStrip (excludes PTY-only tabs).
    claudeSessionCount: statusCounts.claudeSessionCount,
    workingCount: statusCounts.workingCount,
    outsideWorkingCount: statusCounts.outsideWorkingCount,
    idleCount: statusCounts.idleCount,
    completedCount: statusCounts.completedCount,
    errorCount: statusCounts.errorCount,
    unknownCount: statusCounts.unknownCount,
  };
}
