/**
 * Session-manager rows for live tabs that have no transcript row (plan
 * `2026-10-11-runner-terminal-tmux-work-mode`, D5 / Phase 3).
 *
 * The panel's rows used to come from transcripts alone — `message_count > 0`,
 * joined to a live tab through `zoneTabId` — so a plain shell, a session that
 * has not yet written its first message, and a remote session tab whose
 * transcript lives on another machine never appeared. In grid mode that was a
 * masked discoverability defect (the tab still had a zone). In tmux mode the
 * panel is the ONLY way to reach an off-screen session, so a missing row is an
 * unreachable session. Every live tab therefore gets exactly one row: its
 * transcript row when it has one, else the tab-sourced row built here.
 *
 * A tab-sourced row is keyed by the TAB id (`sessionId === zoneTabId ===
 * tab.id`), carries `_transcript: null`, and has no transcript to view, no
 * session id to copy, nothing to resume and no `ClaudeSession` to promote or
 * commit — `SessionCard` gates those actions on `source`.
 *
 * Pure (no React), so the rules are unit-tested without mounting the hook.
 */

import { resolveDisplayTitle } from "./displayTitle";
import { isSyntheticTab } from "./syntheticTabs";
import type { TerminalWorkMode } from "./terminalWorkMode";
import type { TerminalTab } from "./useTerminalManager";
import type { SessionState } from "./useZoneLayout";
import type { SessionLiveStatus, TabSessionRow, UnifiedSession } from "./useSessionManager";

/** `C:\claude\.claude-gmail` → `gmail`; `~/.claude` → `default`. */
export function extractAccountLabel(configDir: string): string {
  const normalized = configDir.replace(/\\/g, "/").replace(/\/$/, "");
  const last = normalized.split("/").pop() ?? "";
  const match = last.match(/^\.claude-(.+)$/);
  return match ? match[1] : last === ".claude" ? "default" : last;
}

/** Last path segment of a project directory (`""` for no path). */
export function extractProjectLabel(projectPath: string): string {
  if (!projectPath) return "";
  const normalized = projectPath.replace(/\\/g, "/").replace(/\/$/, "");
  return normalized.split("/").pop() ?? "";
}

/**
 * The status a tab's tracked state alone gives it — the same chip for a
 * transcript row joined to the tab and for a tab-sourced row, so a hidden
 * session that needs input reads the same wherever it is listed.
 */
export function tabStateStatus(
  tabId: string,
  sessionStates: Readonly<Record<string, SessionState>>,
): SessionLiveStatus {
  const state = sessionStates[tabId];
  if (state === "needs-input") return "needs-input";
  if (state === "error") return "error";
  if (state === "completed") return "completed";
  return "active-in-zone";
}

/**
 * The live status of a TRANSCRIPT row whose session has a tab in this window:
 * its tracked state, else `frozen` when the tab has gone silent (`staleTabs`,
 * no output for 60 s). A Claude conversation that has written messages and
 * then stops producing output mid-run is the case that heuristic was built
 * for, and the card reads it as "appears stuck — may need resume".
 *
 * Tab-sourced rows do NOT take the staleness arm (see {@link buildTabRows}).
 */
export function tabLiveStatus(
  tabId: string,
  sessionStates: Readonly<Record<string, SessionState>>,
  staleTabs: ReadonlySet<string>,
): SessionLiveStatus {
  const status = tabStateStatus(tabId, sessionStates);
  if (status === "active-in-zone" && staleTabs.has(tabId)) return "frozen";
  return status;
}

export type LiveRowTab = Pick<
  TerminalTab,
  | "id"
  | "title"
  | "spawnName"
  | "isAlive"
  | "workingDir"
  | "createdAt"
  | "claudeSessionId"
  | "claudeConfigDir"
  | "remote"
  | "__synthetic"
>;

export interface BuildTabRowsInput {
  /** Every tab of the page, in creation order. */
  tabs: readonly LiveRowTab[];
  /** Tab ids already represented by a transcript row (their `zoneTabId`s). */
  representedTabIds: ReadonlySet<string>;
  sessionStates: Readonly<Record<string, SessionState>>;
  /** Live Claude Code window names, `sessionId → name` (the titlebar's source). */
  registryNames?: ReadonlyMap<string, string>;
}

/**
 * One tab-sourced row per LIVE tab that no transcript row represents, in tab
 * order. Exited tabs are left out (the list is of live sessions; an exited
 * Claude session keeps its transcript row), and so are synthetic test-fixture
 * tabs, which back no pane and are already listed through their transcript.
 *
 * A tab row is never `frozen`. Staleness is "no PTY output for 60 s", and for
 * what a tab row lists — a plain shell, a Claude session that has not written
 * its first message, a remote tab — silence is the idle state, not a stuck
 * run: an idle shell at its prompt would otherwise read "appears stuck — may
 * need resume", and there is nothing to resume. Its status is its tracked
 * state alone, so a row that needs input or errored still says so.
 */
export function buildTabRows({
  tabs,
  representedTabIds,
  sessionStates,
  registryNames,
}: BuildTabRowsInput): TabSessionRow[] {
  const rows: TabSessionRow[] = [];
  for (const tab of tabs) {
    if (!tab.isAlive || tab.__synthetic === true || isSyntheticTab(tab.id)) continue;
    if (representedTabIds.has(tab.id)) continue;
    const configDir = tab.claudeConfigDir ?? "";
    const projectPath = tab.workingDir ?? "";
    const opened = tab.createdAt ? new Date(tab.createdAt).toISOString() : "";
    rows.push({
      source: "tab",
      sessionId: tab.id,
      accountLabel: configDir ? extractAccountLabel(configDir) : "default",
      configDir,
      projectPath,
      projectLabel: extractProjectLabel(projectPath),
      displayName: resolveDisplayTitle({ tab, registryNames }),
      resumeName: null,
      // `displayName` already ranks the registry name first (the titlebar's
      // resolver), so it is not repeated here.
      registryName: null,
      firstMessagePreview: null,
      messageCount: 0,
      lastModified: opened,
      hasPlans: false,
      liveStatus: tabStateStatus(tab.id, sessionStates),
      zoneTabId: tab.id,
      zoneIndex: null,
      remoteDeviceLabel: tab.remote?.deviceLabel ?? null,
      startedAt: opened || null,
      durationMs: null,
      lastMessageType: "",
      lastMessageTimestamp: opened,
      lastMessagePreview: "",
      workSummaryHint: "",
      likelyFrozen: false,
      isOrphaned: false,
      _transcript: null,
    });
  }
  return rows;
}

/**
 * The Live view's full row set: the transcript rows, then a tab-sourced row
 * for every live tab none of them represents.
 */
export function mergeLiveRows(
  transcriptRows: readonly UnifiedSession[],
  input: Omit<BuildTabRowsInput, "representedTabIds">,
): UnifiedSession[] {
  const representedTabIds = new Set<string>();
  for (const row of transcriptRows) if (row.zoneTabId) representedTabIds.add(row.zoneTabId);
  return [...transcriptRows, ...buildTabRows({ ...input, representedTabIds })];
}

/**
 * tmux mode: is this row the session on screen? `onScreenTabId` is the hook's
 * `shownTabId`, `null` in grid mode — where no row is marked, since every live
 * session has a zone of its own there.
 */
export function isRowOnScreen(
  session: Pick<UnifiedSession, "zoneTabId">,
  onScreenTabId: string | null,
): boolean {
  return onScreenTabId !== null && session.zoneTabId === onScreenTabId;
}

/**
 * tmux mode: is this row a session of this window that is NOT the one on
 * screen? Such a session sits in no zone — tmux zones only the shown tab — so
 * a row for it must not claim "in zone". Always `false` in grid mode, where
 * the card wording is unchanged.
 */
export function isRowOffScreen(
  session: Pick<UnifiedSession, "zoneTabId">,
  workMode: TerminalWorkMode,
  onScreenTabId: string | null,
): boolean {
  return workMode === "tmux" && session.zoneTabId !== null && session.zoneTabId !== onScreenTabId;
}

/**
 * The status a session card SHOWS (its dot, its label, its
 * `data-session-status`). The same as the row's `liveStatus`, except a running
 * session that tmux mode keeps off screen reads `active-off-screen` instead of
 * `active-in-zone`: it is in no zone. The other chips (needs input, error, …)
 * describe the session, not where it is, so they stay as they are. Counts and
 * filters keep reading `liveStatus` — an off-screen session is still active.
 */
export type SessionCardStatus = SessionLiveStatus | "active-off-screen";

export function sessionCardStatus(
  liveStatus: SessionLiveStatus,
  isOffScreen: boolean,
): SessionCardStatus {
  return isOffScreen && liveStatus === "active-in-zone" ? "active-off-screen" : liveStatus;
}

/**
 * The right-hand panel mode after a session row is selected.
 *
 * Selecting a row also selects it as the "current" session
 * (`selectedTranscriptSessionId`, which the card highlight reads). For a
 * tab-sourced row that id is a TAB id with no transcript behind it, so a
 * transcript panel left open would render the previous session's messages
 * under the new selection. A tab row therefore closes the TRANSCRIPT panel;
 * every other panel (workflow, analysis, findings, file ownership) is not
 * about the selected session and stays as it is. A transcript row changes
 * nothing here — its own panel follows the selection.
 */
export function rightPanelModeAfterRowSelect<M extends string>(
  source: "transcript" | "tab",
  current: M | null,
): M | null {
  return source === "tab" && current === "transcript" ? null : current;
}
