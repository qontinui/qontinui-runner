/**
 * Launch-menu "new AI session" command builder (#548 Phase 1): the default
 * command gets a fresh UUIDv4 as `--session-id <uuid>` so the tab knows its
 * id BEFORE the process exists — no transcript mtime guess a concurrent
 * same-cwd session can win. The CLI fails LOUDLY on a reused id (verified
 * 2026-06-12), so call once per typed command — fresh uuid per tab/retry. A
 * custom launch command (possibly an alias that drops extra args) is left
 * untouched and keeps the capture fallback ("guessed").
 */

export interface AiLaunchCommand {
  /** Full command to type into the PTY (no trailing newline). */
  command: string;
  /** Pre-pinned Claude session id; null = capture fallback. */
  pinnedSessionId: string | null;
}

export function buildAiLaunchCommand(params: {
  configDir: string;
  isWindows: boolean;
  customCmd?: string;
  /** Fresh-UUIDv4 source; injected for testability. */
  newSessionId: () => string;
}): AiLaunchCommand {
  const { configDir, isWindows, customCmd, newSessionId } = params;
  if (customCmd) {
    return { command: customCmd, pinnedSessionId: null };
  }
  const pinnedSessionId = newSessionId();
  // Autonomous mode (matches the operator's clg/clh/clp wrappers) —
  // runner-spawned AI sessions must not stall on a permission prompt.
  const base = `claude --permission-mode bypassPermissions --session-id ${pinnedSessionId}`;
  const command = isWindows
    ? `$env:CLAUDE_CONFIG_DIR="${configDir}"; ${base}`
    : `CLAUDE_CONFIG_DIR="${configDir}" ${base}`;
  return { command, pinnedSessionId };
}
