/**
 * Launch-menu "new AI session" command builder (#548/#779 seam, Phase 3): a
 * thin async wrapper over the `build_ai_launch_command` tauri command. The Rust
 * launch-spec builder (`session/launch_spec.rs`) is the SINGLE source of truth
 * for the composed flag set: every CLI spelling comes from the `provider`'s
 * served CLI profile, and for Claude the operator's per-account override and
 * machine-global `claude_default_launch_command` template layer in there, so
 * the PTY-typed spawn and the argv spawn sites can never drift.
 *
 * The frontend still mints the fresh UUIDv4 itself (it needs the id
 * synchronously to record the tab BEFORE the process exists — no transcript
 * mtime guess a concurrent same-cwd session can win) and passes it as
 * `sessionId`. Claude fails LOUDLY on a reused id (verified 2026-06-12), so
 * call once per typed command — fresh uuid per tab/retry.
 *
 * A provider whose profile reads its id back (Codex mints its own) gets
 * `pinnedSessionId: null`: the record is provisional until the runner's
 * read-back capture finds the session and stamps the tab (`session-bound`).
 *
 * Precedence, `{sessionId}` substitution, the blank→built-in fallback, and the
 * opaque per-account alias escape hatch (typed verbatim, no pin) all live in
 * Rust; their coverage now lives in `launch_spec.rs` Rust tests.
 */

import { invoke } from "@tauri-apps/api/core";
import type { CommandResponse } from "../../types";
import { describeThrown } from "@/lib/utils";

export interface AiLaunchCommand {
  /** Full command to type into the PTY (no trailing newline). */
  command: string;
  /**
   * The session id the command pins; `null` when it pins none — a read-back
   * provider (the runner captures the id) or an opaque Claude alias (the
   * transcript-capture fallback applies).
   */
  pinnedSessionId: string | null;
}

export interface AiLaunchParams {
  /** Served CLI profile id to launch (`"claude"`, `"codex"`). */
  provider: string;
  /** Account dir for the profile's account variable; `null` = the CLI's default account. */
  configDir: string | null;
  isWindows: boolean;
  /** Fresh UUIDv4 minted by the caller; pinned when the profile pins. */
  sessionId: string;
}

export async function buildAiLaunchCommand(params: AiLaunchParams): Promise<AiLaunchCommand> {
  const { provider, configDir, isWindows, sessionId } = params;
  const resp = await invoke<
    CommandResponse<{ command: string; pinnedSessionId: string | null }>
  >("build_ai_launch_command", { provider, configDir, sessionId, isWindows });
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
 * types the CLI's command into it, so every throw between those two steps
 * (this builder rejects on a Tauri error and on a `data`-less response) used to
 * leave a bare shell open with no toast and no log — the genuinely silent
 * failure. On a throw this closes that tab, reports the reason through the
 * page's own notification channel, and answers `null` so the caller skips the
 * tab rather than typing into one that no longer exists.
 */
export async function buildAiLaunchCommandForTab(
  tabId: string,
  params: AiLaunchParams,
  handlers: AiLaunchTabHandlers,
): Promise<AiLaunchCommand | null> {
  try {
    return await buildAiLaunchCommand(params);
  } catch (e) {
    const detail = describeThrown(e, "Failed to build AI launch command");
    // Logged as well as toasted: the toast is dismissible and the operator may
    // not be looking, but a launch that produced no session must leave a trace.
    console.error(`[LaunchAI] launch-spec build failed for ${tabId}: ${detail}`);
    handlers.disposeTab(tabId);
    const account = params.configDir ?? "its default account";
    handlers.notify(`Could not launch a ${params.provider} session in ${account}: ${detail}`);
    return null;
  }
}
