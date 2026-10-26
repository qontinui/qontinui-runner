/**
 * useAccountMigrationNotifications — toasts for automatic session account
 * migration (backend `terminal::account_migration`).
 *
 * The runner backend emits:
 * - `session-account-migrated` when a token-exhausted Claude session was
 *   respawned under a fresh account (the new pane appears via the normal
 *   `terminal-created` flow — this toast explains WHY a pane just cycled).
 * - `session-account-migration-skipped` when exhaustion was confirmed but no
 *   migration happened (no usable target / cap reached / mechanics failed) —
 *   surfaced as an error so the operator knows the session is stranded.
 *
 * Mount once at App level, next to `useErrorNotifications`.
 */

import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import type { ShowToastFn, ToastType } from "./useToast";

/** Tally of the `<sid>/` directory carry — the main `<sid>.jsonl` is NOT in it. */
export interface SessionDirCarry {
  copied?: number;
  skippedPresent?: number;
  skippedOther?: number;
  failed?: number;
}

/**
 * What the migration did with the main `<sid>.jsonl` — `MainCarry::event_name`
 * in `src-tauri/src/terminal/session_carry/mod.rs`.
 */
export type MainTranscriptCarry =
  | "copied"
  | "extended"
  | "present"
  | "diverged_preserved"
  | "kept_existing";

export interface MigratedPayload {
  claudeSessionId?: string;
  fromConfigDir?: string;
  toConfigDir?: string;
  /** Absent on older builds. */
  mainTranscript?: MainTranscriptCarry;
  /** Where the diverged fork was moved; set only for `diverged_preserved`. */
  mainTranscriptBackup?: string | null;
  /** Tally of the `<sid>/` (subagent transcripts) carry; absent on older builds. */
  sessionDirCarry?: SessionDirCarry;
}

/**
 * Suffix describing the main-transcript carry, when it is worth saying, and
 * the toast level it calls for (`undefined` = a plain success).
 */
function mainTranscriptSummary(
  main: MainTranscriptCarry | undefined,
  backup: string | null | undefined,
): { text: string; level?: ToastType } {
  switch (main) {
    case "diverged_preserved":
      return {
        text: ` — the target account held a DIVERGED copy of this conversation; it was kept at ${
          backup ?? "a <session>.jsonl.diverged-* file beside the transcript"
        } and the live conversation was resumed`,
        level: "info",
      };
    case "kept_existing":
      return {
        text: " — the target account's own copy of the transcript was resumed, NOT the live one; see the runner log",
        level: "error",
      };
    default:
      return { text: "" };
  }
}

/**
 * Suffix describing the subagent-transcript carry. A partial carry is an
 * error-level toast: those subagents cannot be resumed on the new account.
 */
function carrySummary(c: SessionDirCarry | undefined): { text: string; partial: boolean } {
  if (!c) return { text: "", partial: false };
  const failed = c.failed ?? 0;
  const carried = (c.copied ?? 0) + (c.skippedPresent ?? 0);
  if (failed > 0) {
    return {
      text: ` — ${failed} session file(s) (subagent transcripts) could NOT be carried; see the runner log — those subagents may not resume`,
      partial: true,
    };
  }
  const refused = c.skippedOther ?? 0;
  if (refused > 0 && carried === 0) {
    // The carry refused its root (a linked `<sid>` or a rejected session id):
    // not "no subagents", so never a plain success.
    return {
      text: ` — the session directory was NOT carried (${refused} entr(ies) not followed; see the runner log)`,
      partial: true,
    };
  }
  return {
    text: carried > 0 ? ` (${carried} session file(s) incl. subagent transcripts carried)` : "",
    partial: false,
  };
}

interface SkippedPayload {
  claudeSessionId?: string;
  fromConfigDir?: string;
  reason?: string;
}

/** Last path segment of a config dir (`C:\claude\.claude-gmail` → `.claude-gmail`). */
function accountLabel(dir: string | undefined): string {
  if (!dir) return "unknown";
  const parts = dir.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? dir;
}

/** The toast for one `session-account-migrated` event. */
export function migratedToast(p: MigratedPayload): { message: string; type: ToastType } {
  const main = mainTranscriptSummary(p.mainTranscript, p.mainTranscriptBackup);
  const carry = carrySummary(p.sessionDirCarry);
  const type: ToastType = carry.partial ? "error" : (main.level ?? "success");
  return {
    message: `Session ${(p.claudeSessionId ?? "").slice(0, 8)} migrated: ${accountLabel(
      p.fromConfigDir,
    )} ran out of tokens → resumed on ${accountLabel(p.toConfigDir)}${main.text}${carry.text}`,
    type,
  };
}

export function useAccountMigrationNotifications(showToast: ShowToastFn) {
  useEffect(() => {
    const unlistenMigrated = listen<MigratedPayload>("session-account-migrated", (event) => {
      const toast = migratedToast(event.payload ?? {});
      showToast(toast.message, toast.type);
    });
    const unlistenSkipped = listen<SkippedPayload>("session-account-migration-skipped", (event) => {
      const p = event.payload ?? {};
      showToast(
        `Session ${(p.claudeSessionId ?? "").slice(0, 8)} is out of tokens on ${accountLabel(
          p.fromConfigDir,
        )} but was NOT migrated: ${p.reason ?? "unknown reason"}`,
        "error",
      );
    });
    return () => {
      unlistenMigrated.then((fn) => fn());
      unlistenSkipped.then((fn) => fn());
    };
  }, [showToast]);
}
