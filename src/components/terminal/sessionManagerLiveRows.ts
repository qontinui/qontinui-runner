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
import type { TabSessionRow, UnifiedSession } from "./useSessionManager";

/**
 * A row before its status is classified. Rows are built from their sources
 * first, then `useSessionManager` classifies every one in a single pass over
 * the same inputs (`sessionStatusModel`), so a transcript row and a tab row
 * for the same kind of tab read the same.
 */
type Unclassified<T> = T extends unknown ? Omit<T, "status"> : never;
export type UnclassifiedSession = Unclassified<UnifiedSession>;

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
  /** Live Claude Code window names, `sessionId → name` (the titlebar's source). */
  registryNames?: ReadonlyMap<string, string>;
}

/**
 * One tab-sourced row per LIVE tab that no transcript row represents, in tab
 * order. Exited tabs are left out (the list is of live sessions; an exited
 * Claude session keeps its transcript row), and so are synthetic test-fixture
 * tabs, which back no pane and are already listed through their transcript.
 *
 * A tab row has no transcript, so the classifier reads its silence as `Idle
 * at prompt` rather than as a stuck run: for what a tab row lists — a plain
 * shell, a Claude session before its first message, a remote tab — silence is
 * the idle state.
 */
export function buildTabRows({
  tabs,
  representedTabIds,
  registryNames,
}: BuildTabRowsInput): Unclassified<TabSessionRow>[] {
  const rows: Unclassified<TabSessionRow>[] = [];
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
      zoneTabId: tab.id,
      zoneIndex: null,
      remoteDeviceLabel: tab.remote?.deviceLabel ?? null,
      startedAt: opened || null,
      durationMs: null,
      lastMessageType: "",
      lastMessageTimestamp: opened,
      lastMessagePreview: "",
      workSummaryHint: "",
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
  transcriptRows: readonly UnclassifiedSession[],
  input: Omit<BuildTabRowsInput, "representedTabIds">,
): UnclassifiedSession[] {
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
 * The tabs whose pane is showing — the `On screen` / `Hidden here` split of
 * the location axis.
 *
 * - tmux mode: only the shown tab (tmux zones nothing else).
 * - grid mode: every zoned tab, or only the maximized zone's tab while one is
 *   maximized (the others are covered). An unzoned tab is hidden.
 */
export function visibleTabIdsForLayout(layout: {
  workMode: TerminalWorkMode;
  shownTabId: string | null;
  assignments: Readonly<Record<number, string>>;
  maximizedZone: number | null;
}): Set<string> {
  if (layout.workMode === "tmux") {
    return new Set(layout.shownTabId ? [layout.shownTabId] : []);
  }
  if (layout.maximizedZone !== null) {
    const tabId = layout.assignments[layout.maximizedZone];
    return new Set(tabId ? [tabId] : []);
  }
  return new Set(Object.values(layout.assignments).filter((id): id is string => Boolean(id)));
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
