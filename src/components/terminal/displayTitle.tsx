import { useCallback } from "react";
import type { TerminalTab } from "./useTerminalManager";
import { useLiveClaudeSessionNames } from "./useLiveClaudeSessionNames";

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
 * `tab.title` is NOT a stable identity: OSC 0/2 rewrites it to the shell cwd
 * (`ZoneGrid.onTitleChange` -> `renameTab`), which is the exact string this
 * resolver exists to keep off the titlebar.
 */

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
  tab: Pick<TerminalTab, "title" | "spawnName" | "claudeSessionId" | "workingDir">;
  registryNames?: ReadonlyMap<string, string>;
}): string {
  if (tab.claudeSessionId) {
    const live = clean(registryNames?.get(tab.claudeSessionId));
    if (live) return live;
  }
  const spawn = clean(tab.spawnName);
  if (spawn) return spawn;
  const title = clean(tab.title);
  if (title && !isPathShapedTitle(title, tab.workingDir)) return title;
  return FALLBACK_TITLE;
}

/**
 * Display title for one tab. Reads the module-scope, ref-counted registry
 * store — no per-component poller.
 */
export function useDisplayTitle(
  tab: Pick<TerminalTab, "title" | "spawnName" | "claudeSessionId" | "workingDir">,
): string {
  const registryNames = useLiveClaudeSessionNames();
  return resolveDisplayTitle({ tab, registryNames });
}

/**
 * Resolver for call sites that title MANY tabs (list rows, maps) where a hook
 * per row is impossible. Still one subscription.
 */
export function useDisplayTitleResolver(): (
  tab: Pick<TerminalTab, "title" | "spawnName" | "claudeSessionId" | "workingDir">,
) => string {
  const registryNames = useLiveClaudeSessionNames();
  return useCallback((tab) => resolveDisplayTitle({ tab, registryNames }), [registryNames]);
}

/** Inline text for a tab's display title (usable inside `.map` rows). */
export function TabTitle({
  tab,
}: {
  tab: Pick<TerminalTab, "title" | "spawnName" | "claudeSessionId" | "workingDir">;
}) {
  return <>{useDisplayTitle(tab)}</>;
}
