/**
 * Shared types for the terminal component directory.
 */

/** Standard Tauri command response used by terminal hooks and components. */
export interface CommandResponse {
  success: boolean;
  message: string | null;
  data: unknown;
}

/**
 * A durable Claude-session OPEN record from the backend session registry
 * (`terminal_session_list_open`). Keyed by the stable `claudeSessionId`, this
 * is the source of truth for which Claude sessions exist and the zone each
 * belongs to — replacing the ephemeral-tabId creation-order binding that the
 * localStorage snapshot used to drive on restore.
 */
export interface TerminalSessionRecord {
  /** Stable Claude Code session id — the registry key. */
  claudeSessionId: string;
  /** Claude config dir for the session (for `claude --resume` env). */
  configDir?: string;
  /** Working directory the terminal was opened in. */
  workingDir?: string;
  /** Which terminal page this session belongs to ("default" when unset). */
  pageId: string;
  /** Recorded zone index (-1 when unassigned). */
  zoneIndex: number;
  /** Tab title at record time. */
  title?: string;
  /** Ephemeral terminal/tab id at the time the record was last written. */
  terminalId: string;
  /** Epoch ms the session was first recorded open. */
  openedAt: number;
  /** Epoch ms the record was last refreshed. */
  lastSeenAt: number;
  /** Lifecycle state. */
  state: "open" | "closed";
  /** Epoch ms the session was recorded closed. */
  closedAt?: number;
  /** Why the session closed. */
  closeReason?: string;
  /**
   * How `claudeSessionId` was bound: `"pinned"` (`--session-id` / `--resume`
   * — exact) or `"guessed"` (freshest-transcript mtime guess). Absent on
   * records predating the field — read as guessed. Restore quarantines
   * non-pinned rows behind an operator confirm instead of auto-resuming.
   */
  bindOrigin?: string;
  /**
   * Epoch ms a boot-restore began re-typing this session's resume command
   * (backend-owned; while set the liveness poll never flips the record
   * `poll-dead`). Cleared once the resume handshake is verified.
   */
  restorePendingAt?: number;
}
