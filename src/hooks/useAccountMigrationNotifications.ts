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
 * - `session-account-migration-stalled` when the resumed session never took
 *   the continue nudge (`account_migration/followup.rs`) — an error: the pane
 *   is sitting idle or waiting and needs the operator.
 * - `session-account-migration-tail` with what became of the lines the old
 *   account wrote after the carry — toasted only when there is something to
 *   act on (lines saved but not handed over) or the check itself was UNKNOWN.
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
  /**
   * Entries NOT followed: a link, a leftover `*.partial`, an earlier
   * migration's `*.diverged-*` backup (it stays with the account that forked),
   * a directory past the nesting depth bound, a refused root.
   */
  skippedOther?: number;
  /**
   * Subagent transcripts the target held in a DIVERGED form: each fork was kept
   * beside it as `<file>.diverged-*` and the live one carried. Absent on older builds.
   */
  subagentsDivergedPreserved?: number;
  /**
   * Diverged subagent transcripts kept in place instead of the live ones (a
   * non-migration carry mode; 0 on a migration today). Absent on older builds.
   */
  subagentsDivergedKept?: number;
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
function carrySummary(c: SessionDirCarry | undefined): {
  text: string;
  partial: boolean;
  level?: ToastType;
} {
  if (!c) return { text: "", partial: false };
  const failed = c.failed ?? 0;
  const divergedPreserved = c.subagentsDivergedPreserved ?? 0;
  const divergedKept = c.subagentsDivergedKept ?? 0;
  const carried = (c.copied ?? 0) + (c.skippedPresent ?? 0) + divergedPreserved;
  // Diverged subagent transcripts are their own cause, never "not followed".
  const preservedText =
    divergedPreserved > 0
      ? ` — ${divergedPreserved} subagent transcript(s) on the target had DIVERGED; each fork was kept beside it as a *.diverged-* file and the live one carried`
      : "";
  const keptText =
    divergedKept > 0
      ? ` — ${divergedKept} subagent transcript(s) on the target had DIVERGED and were kept INSTEAD of the live ones; see the runner log`
      : "";
  if (failed > 0) {
    return {
      text: ` — ${failed} session file(s) (subagent transcripts) could NOT be carried; see the runner log — those subagents may not resume${preservedText}${keptText}`,
      partial: true,
    };
  }
  if (divergedKept > 0) {
    return { text: `${preservedText}${keptText}`, partial: true };
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
  if (divergedPreserved > 0) {
    return { text: preservedText, partial: false, level: "info" };
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

/** `session-account-migration-stalled` — `followup.rs` `report_stalled`. */
export interface StalledPayload {
  claudeSessionId?: string;
  terminalId?: string;
  toConfigDir?: string;
  /** `StallReason::as_str`, e.g. `retry-gave-up`, `limit-wait-on-exhausted-account`. */
  reason?: string;
  escSent?: boolean;
  /** One bounded line of the pane's screen at the give-up. */
  screenLine?: string;
}

/** `TailRescue` status — `followup.rs` `tail_event`. */
export type TranscriptTailStatus =
  | "rescued"
  | "empty"
  | "not-read"
  | "source-shorter-unknown"
  | "offset-unknown";

/** `TailDelivery::as_str`; `null` when the tail is UNKNOWN. */
export type TranscriptTailDelivery =
  | "none_to_deliver"
  | "delivered"
  | "submit_failed"
  | "gave_up"
  | "skipped_auto_continue_off";

/** `session-account-migration-tail` — `followup.rs` `tail_event`. */
export interface TailPayload {
  claudeSessionId?: string;
  newTerminalId?: string;
  toConfigDir?: string;
  transcriptTail?: TranscriptTailStatus;
  /** `null` when the tail is UNKNOWN — never 0. */
  transcriptTailLines?: number | null;
  transcriptTailUnparseable?: number | null;
  transcriptTailTruncated?: boolean | null;
  transcriptTailPath?: string | null;
  transcriptTailDelivery?: TranscriptTailDelivery | null;
  transcriptTailDelivered?: boolean;
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
  const type: ToastType = carry.partial ? "error" : (main.level ?? carry.level ?? "success");
  return {
    message: `Session ${(p.claudeSessionId ?? "").slice(0, 8)} migrated: ${accountLabel(
      p.fromConfigDir,
    )} ran out of tokens → resumed on ${accountLabel(p.toConfigDir)}${main.text}${carry.text}`,
    type,
  };
}

/** The toast for one `session-account-migration-stalled` event. */
export function stalledToast(p: StalledPayload): { message: string; type: ToastType } {
  // The reason says how the retry ended; this only says an Esc preceded it.
  const esc = p.escSent ? " (one Esc was sent before the retry)" : "";
  const screen = p.screenLine ? ` Screen: "${p.screenLine}"` : "";
  return {
    message: `Session ${(p.claudeSessionId ?? "").slice(0, 8)} was resumed on ${accountLabel(
      p.toConfigDir,
    )} but is STALLED — it never took the continue prompt (${
      p.reason ?? "unknown reason"
    })${esc}; open the pane and continue it by hand.${screen}`,
    type: "error",
  };
}

/**
 * The toast for one `session-account-migration-tail` event, or `null` when
 * there is nothing to say (no tail, or a tail with nothing to hand over, or
 * one already handed to the resumed session).
 */
export function tailToast(p: TailPayload): { message: string; type: ToastType } | null {
  const sid = (p.claudeSessionId ?? "").slice(0, 8);
  const lines = p.transcriptTailLines;
  const count = typeof lines === "number" ? `${lines} line(s)` : "lines";
  const where = p.transcriptTailPath ? ` They are saved at ${p.transcriptTailPath}.` : "";
  switch (p.transcriptTail) {
    case "empty":
      return null;
    case "rescued":
      switch (p.transcriptTailDelivery) {
        case "none_to_deliver":
        case "delivered":
          return null;
        case "skipped_auto_continue_off":
          return {
            message: `Session ${sid}: the old account wrote ${count} after the migration; auto-continue is off, so they were NOT handed to the resumed session.${where}`,
            type: "info",
          };
        default:
          // submit_failed / gave_up / an absent delivery on a rescued tail.
          return {
            message: `Session ${sid}: the old account wrote ${count} after the migration (task notifications or prompts) and they could NOT be handed to the resumed session (${
              p.transcriptTailDelivery ?? "delivery unknown"
            }).${where}`,
            type: "error",
          };
      }
    default:
      // not-read / source-shorter-unknown / offset-unknown / an unrecognised
      // status: UNKNOWN, which is never rendered as "nothing was lost".
      return {
        message: `Session ${sid}: could not check whether the old account wrote anything after the migration (${
          p.transcriptTail ?? "unknown"
        }); see the runner log.`,
        type: "info",
      };
  }
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
    const unlistenStalled = listen<StalledPayload>("session-account-migration-stalled", (event) => {
      const toast = stalledToast(event.payload ?? {});
      showToast(toast.message, toast.type);
    });
    const unlistenTail = listen<TailPayload>("session-account-migration-tail", (event) => {
      const toast = tailToast(event.payload ?? {});
      if (toast) showToast(toast.message, toast.type);
    });
    return () => {
      unlistenMigrated.then((fn) => fn());
      unlistenSkipped.then((fn) => fn());
      unlistenStalled.then((fn) => fn());
      unlistenTail.then((fn) => fn());
    };
  }, [showToast]);
}
