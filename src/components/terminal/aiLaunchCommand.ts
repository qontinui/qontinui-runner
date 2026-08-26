/**
 * Launch-menu "new AI session" command builder (#548/#779 seam, Phase 3): a
 * thin async wrapper over the `build_ai_launch_command` tauri command. The Rust
 * launch-spec builder (`claude_session/launch_spec.rs`) is now the SINGLE source
 * of truth for the composed flag set — the operator's per-account override and
 * machine-global `claude_default_launch_command` template both layer in there,
 * so the PTY-typed spawn and the argv spawn sites can never drift.
 *
 * The frontend still mints the fresh UUIDv4 itself (it needs the id
 * synchronously to record the tab BEFORE the process exists — no transcript
 * mtime guess a concurrent same-cwd session can win) and passes it as
 * `sessionId`. The CLI fails LOUDLY on a reused id (verified 2026-06-12), so
 * call once per typed command — fresh uuid per tab/retry.
 *
 * Precedence, `{sessionId}` substitution, the blank→built-in fallback, and the
 * opaque per-account alias escape hatch (typed verbatim, no pin) all live in
 * Rust; their coverage now lives in `launch_spec.rs` Rust tests.
 */

import { invoke } from "@tauri-apps/api/core";
import type { CommandResponse } from "../../types";

export interface AiLaunchCommand {
  /** Full command to type into the PTY (no trailing newline). */
  command: string;
  /** Pre-pinned Claude session id; null = capture fallback (opaque alias). */
  pinnedSessionId: string | null;
}

export async function buildAiLaunchCommand(params: {
  configDir: string;
  isWindows: boolean;
  /** Fresh UUIDv4 minted by the caller; pinned as `--session-id`/`{sessionId}`. */
  sessionId: string;
}): Promise<AiLaunchCommand> {
  const { configDir, isWindows, sessionId } = params;
  const resp = await invoke<
    CommandResponse<{ command: string; pinnedSessionId: string | null }>
  >("build_ai_launch_command", { configDir, sessionId, isWindows });
  const data = resp.data;
  if (!data) {
    throw new Error(resp.message ?? "build_ai_launch_command returned no data");
  }
  return { command: data.command, pinnedSessionId: data.pinnedSessionId ?? null };
}

/**
 * What the failure path needs from the page. Kept as plain callbacks (no
 * React, no Tauri) so the orphan-cleanup contract is unit-testable under the
 * runner's `environment: "node"` vitest config — same precedent as
 * `buildCreatePlainTerminalAction`.
 */
export interface AiLaunchTabHandlers {
  /** Dispose the already-created tab — `TerminalPage`'s `closeTerminal`. */
  disposeTab: (tabId: string) => void;
  /** Surface the failure — the page's existing error toast. */
  notify: (message: string) => void;
}

/**
 * `buildAiLaunchCommand` for a tab that ALREADY EXISTS, with the orphan cleaned
 * up on failure.
 *
 * The AI-session launch path creates a plain PTY first and only afterwards
 * types the `claude` command into it, so every throw between those two steps
 * (this builder rejects on a Tauri error and on a `data`-less response) used to
 * leave a bare shell open with no toast and no log — the genuinely silent
 * failure. On a throw this closes that tab, reports the reason through the
 * page's own notification channel, and answers `null` so the caller skips the
 * tab rather than typing into one that no longer exists.
 */
export async function buildAiLaunchCommandForTab(
  tabId: string,
  params: { configDir: string; isWindows: boolean; sessionId: string },
  handlers: AiLaunchTabHandlers,
): Promise<AiLaunchCommand | null> {
  try {
    return await buildAiLaunchCommand(params);
  } catch (e) {
    const detail = e instanceof Error ? e.message : String(e);
    // Logged as well as toasted: the toast is dismissible and the operator may
    // not be looking, but a launch that produced no session must leave a trace.
    console.error(`[LaunchAI] launch-spec build failed for ${tabId}: ${detail}`);
    handlers.disposeTab(tabId);
    handlers.notify(`Could not launch a Claude session in ${params.configDir}: ${detail}`);
    return null;
  }
}
