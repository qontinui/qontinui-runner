import { useCallback } from "react";
import type { TerminalTab } from "./useTerminalManager";
import { useLiveClaudeSessionNames } from "./useLiveClaudeSessionNames";
import { paneLabel, type PaneLabel } from "./paneLabel";

/**
 * Zone-title resolver (plan 2026-10-02-terminal-titlebar-shows-session-name,
 * D2). One fixed precedence, so every surface that names a terminal tab shows
 * the SAME string and the cwd is never a candidate:
 *
 *   1. the live Claude Code registry name for `tab.claudeSessionId`
 *      (operator `/rename`, or `--name`, within one registry poll);
 *   2. `tab.spawnName` — the immutable name the runner launched the session
 *      with (covers the first poll window);
 *   3. `tab.title`, unless it is path-shaped;
 *   4. `Terminal`.
 *
 * `tab.title` is NOT a stable identity on its own. Before plan
 * `2026-10-27-click-resume-forks-a-live-conversation-and-pane-titles-hide-its-identity`
 * D3, the OSC 0/2 relay (`ZoneGrid.onTitleChange` -> `renameTab`) rewrote it to
 * the shell cwd or to a Claude session's current activity. The relay now
 * writes `tab.activityTitle` instead. A plain shell (no `claudeSessionId`)
 * still titles from its OSC title, so for such a tab `activityTitle` stands in
 * for `title` in rung 3. A conversation tab never titles from its activity.
 */

/** The tab fields every display-title surface reads. */
export type DisplayTitleTab = Pick<
  TerminalTab,
  "title" | "spawnName" | "claudeSessionId" | "workingDir" | "activityTitle"
>;

export const FALLBACK_TITLE = "Terminal";

/** A bare drive (`C:`, `c:\`) or home (`~`, `~/`) path. */
const BARE_ROOT_RE = /^(?:[A-Za-z]:[\\/]?|~[\\/]?)$/;

/**
 * A shell's default OSC 0/2 title, `user@host: <cwd>` (bash/zsh prompt titles).
 * A shell in the home directory sends `user@host: ~`, which carries no path
 * separator, so the separator test alone lets the cwd through (seen live on a
 * temp runner: a titlebar reading `spinak@merytshost: ~`).
 */
const SHELL_PROMPT_TITLE_RE = /^[^\s@]+@[^\s:]+:/;

function clean(s: string | undefined | null): string | undefined {
  if (typeof s !== "string") return undefined;
  const t = s.trim();
  return t.length > 0 ? t : undefined;
}

/** True when `title` looks like a filesystem path / the tab's cwd. */
export function isPathShapedTitle(title: string, workingDir?: string): boolean {
  const t = title.trim();
  if (t.length === 0) return true;
  if (t.includes("/") || t.includes("\\")) return true;
  if (BARE_ROOT_RE.test(t)) return true;
  if (SHELL_PROMPT_TITLE_RE.test(t)) return true;
  const wd = clean(workingDir);
  return wd !== undefined && t === wd;
}

export function resolveDisplayTitle({
  tab,
  registryNames,
}: {
  tab: DisplayTitleTab;
  registryNames?: ReadonlyMap<string, string>;
}): string {
  if (tab.claudeSessionId) {
    const live = clean(registryNames?.get(tab.claudeSessionId));
    if (live) return live;
  }
  const spawn = clean(tab.spawnName);
  if (spawn) return spawn;
  // A plain shell's OSC title is its title; a conversation's is its activity,
  // which never names it.
  const candidates = tab.claudeSessionId ? [tab.title] : [tab.activityTitle, tab.title];
  for (const candidate of candidates) {
    const title = clean(candidate);
    if (title && !isPathShapedTitle(title, tab.workingDir)) return title;
  }
  return FALLBACK_TITLE;
}

/**
 * The name a conversation pane's label leads with: the live registry name,
 * else the immutable spawn name, else a non-path `tab.title` (which, since D3,
 * the OSC relay no longer writes). `undefined` when none is known; the label
 * then reads `(unnamed) · <id8>`. Never the activity title, and never the
 * `Terminal` fallback, which names no conversation.
 */
export function resolveConversationName({
  tab,
  registryNames,
}: {
  tab: DisplayTitleTab;
  registryNames?: ReadonlyMap<string, string>;
}): string | undefined {
  if (tab.claudeSessionId) {
    const live = clean(registryNames?.get(tab.claudeSessionId));
    if (live) return live;
  }
  const spawn = clean(tab.spawnName);
  if (spawn) return spawn;
  const title = clean(tab.title);
  if (title && !isPathShapedTitle(title, tab.workingDir)) return title;
  return undefined;
}

/**
 * The pane label for one tab (plan
 * `2026-10-27-click-resume-forks-a-live-conversation-and-pane-titles-hide-its-identity`,
 * Phase 4). A conversation tab leads with `<name> · <id8>`, with its activity
 * title as the secondary line. A plain shell keeps its resolved shell title.
 */
export function resolvePaneLabel({
  tab,
  registryNames,
}: {
  tab: DisplayTitleTab;
  registryNames?: ReadonlyMap<string, string>;
}): PaneLabel {
  return paneLabel({
    conversationName: resolveConversationName({ tab, registryNames }),
    claudeSessionId: tab.claudeSessionId,
    activityTitle: tab.activityTitle,
    shellTitle: resolveDisplayTitle({ tab, registryNames }),
  });
}

/** Pane label for one tab, reading the shared registry store (one poller). */
export function usePaneLabel(tab: DisplayTitleTab): PaneLabel {
  const registryNames = useLiveClaudeSessionNames();
  return resolvePaneLabel({ tab, registryNames });
}

/**
 * Display title for one tab. Reads the module-scope, ref-counted registry
 * store — no per-component poller.
 */
export function useDisplayTitle(tab: DisplayTitleTab): string {
  const registryNames = useLiveClaudeSessionNames();
  return resolveDisplayTitle({ tab, registryNames });
}

/**
 * Resolver for call sites that title MANY tabs (list rows, maps) where a hook
 * per row is impossible. Still one subscription.
 */
export function useDisplayTitleResolver(): (tab: DisplayTitleTab) => string {
  const registryNames = useLiveClaudeSessionNames();
  return useCallback((tab) => resolveDisplayTitle({ tab, registryNames }), [registryNames]);
}

/** Inline text for a tab's display title (usable inside `.map` rows). */
export function TabTitle({ tab }: { tab: DisplayTitleTab }) {
  return <>{useDisplayTitle(tab)}</>;
}
