/**
 * Launch-menu "new AI session" command builder (#548 Phase 1): the default
 * command gets a fresh UUIDv4 as `--session-id <uuid>` so the tab knows its
 * id BEFORE the process exists — no transcript mtime guess a concurrent
 * same-cwd session can win. The CLI fails LOUDLY on a reused id (verified
 * 2026-06-12), so call once per typed command — fresh uuid per tab/retry. A
 * custom launch command (possibly an alias that drops extra args) is left
 * untouched and keeps the last-resort mtime-capture fallback (origin
 * "reconciled" — formerly "guessed").
 *
 * Precedence:
 *   1. `customCmd` (per-account override) — typed verbatim, no pin.
 *   2. `defaultTemplate` (operator-configured machine-global default) — env
 *      prefix applied; `{sessionId}` substituted with the fresh pinned id,
 *      or `--session-id <uuid>` appended when the placeholder is absent.
 *      Pinned either way — the identity shim passes extra args through.
 *   3. Built-in default: `claude --permission-mode bypassPermissions`.
 */

export interface AiLaunchCommand {
  /** Full command to type into the PTY (no trailing newline). */
  command: string;
  /** Pre-pinned Claude session id; null = capture fallback. */
  pinnedSessionId: string | null;
}

/** `{sessionId}` placeholder in an operator-configured default template. */
const SESSION_ID_PLACEHOLDER = "{sessionId}";

export function buildAiLaunchCommand(params: {
  configDir: string;
  isWindows: boolean;
  customCmd?: string;
  /**
   * Operator-configured default launch command template (machine-global
   * setting; applies to every account without a `customCmd`).
   */
  defaultTemplate?: string;
  /** Fresh-UUIDv4 source; injected for testability. */
  newSessionId: () => string;
}): AiLaunchCommand {
  const { configDir, isWindows, customCmd, defaultTemplate, newSessionId } = params;
  if (customCmd) {
    return { command: customCmd, pinnedSessionId: null };
  }
  const pinnedSessionId = newSessionId();
  // Autonomous mode (matches the operator's clg/clh/clp wrappers) —
  // runner-spawned AI sessions must not stall on a permission prompt.
  const template = defaultTemplate?.trim() || "claude --permission-mode bypassPermissions";
  const base = template.includes(SESSION_ID_PLACEHOLDER)
    ? template.replaceAll(SESSION_ID_PLACEHOLDER, pinnedSessionId)
    : `${template} --session-id ${pinnedSessionId}`;
  const command = isWindows
    ? `$env:CLAUDE_CONFIG_DIR="${configDir}"; ${base}`
    : `CLAUDE_CONFIG_DIR="${configDir}" ${base}`;
  return { command, pinnedSessionId };
}
