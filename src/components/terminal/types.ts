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
  /** Which AI-CLI provider owns this session (`"claude"`, `"gemini"`). Absent
   * on records predating the field — read as `"claude"`. */
  provider?: string;
  /**
   * How `claudeSessionId` was bound: `"authoritative"` (the runner KNOWS the
   * id — `--session-id`/`--resume`/a provider hook) or `"reconciled"`
   * (recovered by a transcript/process-anchored backstop, may be foreign).
   * Absent on records predating the field — read as reconciled. Restore
   * auto-resumes authoritative rows; reconciled rows are treated
   * conservatively. (Migrated from the previous `bindOrigin`
   * `pinned`/`guessed` vocabulary.)
   */
  origin?: string;
  /**
   * Epoch ms a boot-restore began re-typing this session's resume command
   * (backend-owned; while set the liveness poll never flips the record
   * `poll-dead`). Cleared once the resume handshake is verified.
   */
  restorePendingAt?: number;
  /**
   * Epoch ms a provider's SessionStart hook (or the process-start-anchored
   * reconcile backstop) CONFIRMED that a real provider session actually
   * started in this terminal. `undefined`/absent ⇒ PROVISIONAL — a spawn-time
   * authoritative record is written for EVERY terminal (including plain shells
   * that never run a provider), so a `confirmed`-unset authoritative record
   * with no transcript is a phantom shell that must NOT be auto-`--resume`d.
   * Phase 4's classifier gates auto-resume on this (mirrors the Rust
   * `confirmed_at`). Backend reconcile prunes the live-PTY phantoms before the
   * frontend ever classifies; this field is the frontend's defense-in-depth
   * for cold-boot records the reconcile couldn't reach.
   */
  confirmedAt?: number;
}
