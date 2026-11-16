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
 * - `session-limit-parked` when a session whose transcript says it stopped at
 *   a usage limit cannot move yet (its reset is still ahead, or a migration
 *   was wanted and cannot run) and is parked (backend `terminal::limit_reset`,
 *   plan 2026-11-05 Phase 2). Not an error: one coalesced info toast per burst.
 * - `session-limit-reset-continued` when such a session's limit had reset and
 *   the runner typed a continue prompt into it, in place. Coalesced the same way.
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

/** `session-limit-parked` — `terminal::limit_reset::parked_payload`. */
export interface LimitParkedPayload {
  claudeSessionId?: string;
  terminalId?: string;
  configDir?: string;
  /** When the park is due (unix ms): the stop's reset plus a margin, or a retry. */
  dueAtMs?: number;
  /** `reset-pending`, `probe-unknown`, `migration-disabled`, `single-account`,
   * `migration-cap-reached` or `no-usable-target`. */
  reason?: string;
}

/** `session-limit-reset-continued` — `terminal::limit_reset::continued_payload`. */
export interface LimitContinuedPayload {
  claudeSessionId?: string;
  terminalId?: string;
  configDir?: string;
  entryUuid?: string;
}

/** How long a burst of limit events is gathered into one toast. A restart can
 * park ~100 sessions within seconds; one toast each would bury the screen. */
export const LIMIT_EVENT_COALESCE_MS = 2000;

/** Wall-clock `HH:MM` in the operator's locale, with the date when it is not
 * today — a weekly reset is often days away. */
function clockTime(ms: number): string {
  const at = new Date(ms);
  if (at.toDateString() === new Date().toDateString()) {
    return at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  }
  return at.toLocaleString([], {
    weekday: "short",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/** One key per session, so a session parked twice in a burst counts once. */
function sessionKey(p: { claudeSessionId?: string; terminalId?: string }): string {
  return p.claudeSessionId ?? p.terminalId ?? "unknown";
}

/**
 * The one toast for a burst of `session-limit-parked` events, or `null` for
 * an empty burst. The time is the LATEST due time in the burst: every parked
 * session is due by then. `formatTime` is injectable for tests.
 */
export function limitParkedToast(
  parks: LimitParkedPayload[],
  formatTime: (ms: number) => string = clockTime,
): { message: string; type: ToastType } | null {
  const latest = new Map<string, LimitParkedPayload>();
  for (const p of parks) latest.set(sessionKey(p), p);
  if (latest.size === 0) return null;
  const dues = [...latest.values()]
    .map((p) => p.dueAtMs)
    .filter((d): d is number => typeof d === "number" && Number.isFinite(d));
  const until = dues.length > 0 ? ` until ${formatTime(Math.max(...dues))}` : "";
  if (latest.size === 1) {
    const [only] = latest.values();
    return {
      message: `Session ${sessionKey(only).slice(0, 8)} stopped at its usage limit on ${accountLabel(
        only.configDir,
      )} and is parked${until} (${only.reason ?? "unknown reason"})`,
      type: "info",
    };
  }
  return {
    message: `${latest.size} sessions stopped at their usage limit and are parked${until}`,
    type: "info",
  };
}

/** The one toast for a burst of `session-limit-reset-continued` events. */
export function limitContinuedToast(
  continued: LimitContinuedPayload[],
): { message: string; type: ToastType } | null {
  const sessions = new Map<string, LimitContinuedPayload>();
  for (const c of continued) sessions.set(sessionKey(c), c);
  if (sessions.size === 0) return null;
  if (sessions.size === 1) {
    const [only] = sessions.values();
    return {
      message: `Session ${sessionKey(only).slice(0, 8)}'s usage limit on ${accountLabel(
        only.configDir,
      )} has reset — told it to continue`,
      type: "success",
    };
  }
  return {
    message: `${sessions.size} sessions' usage limits have reset — told each to continue`,
    type: "success",
  };
}

/**
 * Gathers items pushed within `windowMs` of the first one and hands them to
 * `flush` together. `cancel` drops a pending burst (unmount).
 */
export function createBurstCoalescer<T>(
  windowMs: number,
  flush: (items: T[]) => void,
): { push: (item: T) => void; cancel: () => void } {
  let items: T[] = [];
  let timer: ReturnType<typeof setTimeout> | undefined;
  return {
    push(item: T) {
      items.push(item);
      if (timer !== undefined) return;
      timer = setTimeout(() => {
        const burst = items;
        items = [];
        timer = undefined;
        flush(burst);
      }, windowMs);
    },
    cancel() {
      if (timer !== undefined) clearTimeout(timer);
      timer = undefined;
      items = [];
    },
  };
}

export function useAccountMigrationNotifications(showToast: ShowToastFn) {
  useEffect(() => {
    const parked = createBurstCoalescer<LimitParkedPayload>(LIMIT_EVENT_COALESCE_MS, (burst) => {
      const toast = limitParkedToast(burst);
      if (toast) showToast(toast.message, toast.type);
    });
    const continued = createBurstCoalescer<LimitContinuedPayload>(
      LIMIT_EVENT_COALESCE_MS,
      (burst) => {
        const toast = limitContinuedToast(burst);
        if (toast) showToast(toast.message, toast.type);
      },
    );
    const unlistenParked = listen<LimitParkedPayload>("session-limit-parked", (event) => {
      parked.push(event.payload ?? {});
    });
    const unlistenContinued = listen<LimitContinuedPayload>(
      "session-limit-reset-continued",
      (event) => {
        continued.push(event.payload ?? {});
      },
    );
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
      unlistenParked.then((fn) => fn());
      unlistenContinued.then((fn) => fn());
      parked.cancel();
      continued.cancel();
    };
  }, [showToast]);
}
