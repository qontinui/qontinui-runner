/**
 * The one rule every terminal work-mode control applies — the CommandBar
 * toggle, the `Ctrl+Shift+Y` chord, `/terminal.mode` and the UI Bridge
 * `set-terminal-work-mode` action (plan
 * `2026-10-11-runner-terminal-tmux-work-mode`, Phase 4).
 *
 * Four surfaces calling `enterTmuxMode` / `exitTmuxMode` on their own would
 * drift: one would forget to open the Session Manager (in tmux mode it is the
 * only way to reach an off-screen session), another would report a change
 * that did not happen. So the decision, the side effects and the sentence that
 * acknowledges them are made here, once, and every surface renders the same
 * {@link WorkModeChange}.
 *
 * Pure apart from the callbacks it is handed, so the tests assert it without
 * React.
 */

import type { TerminalWorkMode } from "./terminalWorkMode";

/** What a caller asks for: a named mode, or the other one. */
export type WorkModeRequest = TerminalWorkMode | "toggle";

/** The mode names a caller may type or send, in display order. */
export const WORK_MODE_NAMES: readonly TerminalWorkMode[] = ["grid", "tmux"];

/** The operator-facing name of a mode, as the toggle shows it. */
export function workModeLabel(mode: TerminalWorkMode): string {
  return mode === "tmux" ? "tmux" : "Grid";
}

/**
 * Read a mode argument off the wire or the command line. Absent (or empty)
 * means toggle; anything else must name a mode exactly (case-insensitive), and
 * a value that does not is an error naming the accepted set — never silently
 * read as a toggle, which would flip the mode on a typo.
 */
export function parseWorkModeRequest(
  raw: unknown,
): { ok: true; request: WorkModeRequest } | { ok: false; error: string } {
  if (raw === undefined || raw === null) return { ok: true, request: "toggle" };
  if (typeof raw !== "string") {
    return { ok: false, error: `mode must be one of: grid, tmux — got ${JSON.stringify(raw)}` };
  }
  const value = raw.trim().toLowerCase();
  if (value === "") return { ok: true, request: "toggle" };
  if (value === "grid" || value === "tmux") return { ok: true, request: value };
  return { ok: false, error: `mode must be one of: grid, tmux — got ${JSON.stringify(raw)}` };
}

/** The handles a control needs — `useZoneLayout` and `workflowGen` supply them. */
export interface WorkModeControlTarget {
  /** The mode now, read at invocation time (not from a stale render). */
  workMode: TerminalWorkMode;
  enterTmuxMode: () => void;
  exitTmuxMode: () => void;
  /** The Session Manager sidebar's state now. */
  showSidebar: boolean;
  setShowSidebar: (open: boolean) => void;
}

/** The result every control reports — the UI Bridge action returns it verbatim. */
export interface WorkModeChange {
  /** The mode BEFORE the request. */
  previousMode: TerminalWorkMode;
  /** The mode AFTER it. */
  mode: TerminalWorkMode;
  /** `false` when the requested mode was already the current one. */
  changed: boolean;
  /** `true` when this request opened the Session Manager sidebar. */
  sidebarOpened: boolean;
  /** One sentence saying what happened, e.g. "entered tmux mode". */
  message: string;
}

/** Resolve a request against the current mode. */
export function resolveWorkModeRequest(
  request: WorkModeRequest,
  current: TerminalWorkMode,
): TerminalWorkMode {
  if (request === "toggle") return current === "tmux" ? "grid" : "tmux";
  return request;
}

/**
 * Apply a request: enter or exit tmux mode as asked, and whenever the result
 * is tmux, make sure the Session Manager is open — including when tmux was
 * already on, since an operator asking for tmux with the navigator closed has
 * no other way to reach the hidden sessions.
 */
export function applyWorkModeRequest(
  target: WorkModeControlTarget,
  request: WorkModeRequest,
): WorkModeChange {
  const previousMode = target.workMode;
  const mode = resolveWorkModeRequest(request, previousMode);
  const changed = mode !== previousMode;
  if (changed) {
    if (mode === "tmux") target.enterTmuxMode();
    else target.exitTmuxMode();
  }
  const sidebarOpened = mode === "tmux" && !target.showSidebar;
  if (sidebarOpened) target.setShowSidebar(true);
  return {
    previousMode,
    mode,
    changed,
    sidebarOpened,
    message: describeWorkModeChange(mode, changed, sidebarOpened),
  };
}

/** The acknowledgement sentence for a change (or a non-change). */
export function describeWorkModeChange(
  mode: TerminalWorkMode,
  changed: boolean,
  sidebarOpened: boolean,
): string {
  const opened = sidebarOpened ? " — session manager opened" : "";
  if (!changed) return `already in ${mode} mode${opened}`;
  return mode === "tmux" ? `entered tmux mode${opened}` : "left tmux mode — grid restored";
}
