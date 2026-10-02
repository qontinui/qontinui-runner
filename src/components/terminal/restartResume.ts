/**
 * Decide HOW a restarted zone's replacement pane is spawned.
 *
 * The "Restart? Session errored" chip (and `/restart`) used to spawn a bare
 * shell for every pane, even when the old pane hosted a Claude session whose id
 * is known — the operator's session was replaced by an empty `powershell.exe`.
 * A pane that hosted a Claude session is resumed (`claude --resume <id>`, the
 * same path Past Sessions → Resume takes); only a genuine shell pane gets a
 * shell.
 *
 * Pure so the decision is testable without React or Tauri.
 */

import type { TranscriptSession } from "./useTranscriptSessions";

/** The slice of a tab this decision reads. */
export interface RestartableTab {
  title?: string;
  workingDir?: string | null;
  claudeSessionId?: string;
  claudeConfigDir?: string;
}

export type RestartPlan =
  /** No Claude session on the pane — a plain shell replaces it. */
  | { kind: "shell" }
  /** Resume the pane's Claude session in the replacement. */
  | { kind: "resume"; session: TranscriptSession }
  /**
   * Resuming now could fork the transcript: a live process already hosts the id,
   * or liveness could not be read. Fail CLOSED — a missing tab is recoverable, a
   * duplicate live session corrupts the transcript
   * (see `fetchLiveClaudeSessionIds`).
   */
  | { kind: "blocked"; detail: string };

/**
 * @param liveSessionIds ids some live Claude process hosts, or `null` when the
 *   registry could not be read (indeterminate, never "nothing alive").
 */
export function planRestart(
  tab: RestartableTab | undefined,
  liveSessionIds: ReadonlySet<string> | null,
): RestartPlan {
  const id = tab?.claudeSessionId;
  if (!tab || !id) return { kind: "shell" };
  if (liveSessionIds === null) {
    return {
      kind: "blocked",
      detail: `could not confirm session ${id.slice(0, 8)} is not already running; not resuming it to avoid forking the transcript`,
    };
  }
  if (liveSessionIds.has(id)) {
    return {
      kind: "blocked",
      detail: `session ${id.slice(0, 8)} is still running in a live process; not resuming it a second time`,
    };
  }
  return {
    kind: "resume",
    session: {
      session_id: id,
      project_path: tab.workingDir ?? "",
      config_dir: tab.claudeConfigDir ?? "",
      message_count: 0,
      last_modified: "",
      started_at: null,
      first_message_preview: null,
      has_plans: false,
      display_name: tab.title ?? "",
    },
  };
}
