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
 * - `session-limit-parked` when a session that stopped at a usage limit was
 *   parked until its reset (backend `terminal::limit_reset`, plan 2026-11-05
 *   Phase 2). Not an error: nothing failed. The scan loop hands the session
 *   back to the usage-limit handler at its due time (plan Phase 3). A restart
 *   can park ~100 sessions at once, so parks are coalesced into one info toast
 *   per burst. A `continue-retry` park (a continue attempt that did not reach
 *   the pane, retried shortly) raises no toast.
 * - `session-limit-reset-continued` when such a session was continued in
 *   place after its reset — coalesced the same way.
 * - `session-limit-reset-continue-gave-up` when the runner stopped trying to
 *   continue such a session (its continue never reached the pane) — a
 *   coalesced error toast, since the session now needs the operator.
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

/** `session-limit-parked` — `limit_reset::parked_event` in the backend. */
export interface LimitParkedPayload {
  claudeSessionId?: string;
  configDir?: string;
  /** When the session is handed back, ms since the epoch. */
  dueAtMs?: number;
  /** `not-yet-reset`, `probe-unknown`, `no-usable-target`, … */
  reason?: string;
}

/** `session-limit-reset-continued` — `limit_reset::continued_event`. */
export interface LimitContinuedPayload {
  claudeSessionId?: string;
  configDir?: string;
  terminalId?: string;
  entryUuid?: string;
}

/** `session-limit-reset-continue-gave-up` — `limit_reset::gave_up_event`. */
export interface LimitGaveUpPayload {
  claudeSessionId?: string;
  configDir?: string;
  terminalId?: string;
  attempts?: number;
}

/** "Session 0c4e3025" for one burst member, "N sessions" for several. */
function burstSubject(items: { claudeSessionId?: string }[]): string {
  if (items.length !== 1) return `${items.length} sessions`;
  const id = (items[0].claudeSessionId ?? "").slice(0, 8);
  return id ? `Session ${id}` : "A session";
}

/** Events arriving within this window of a burst's first one share its toast. */
export const LIMIT_BURST_MS = 2000;

/** `HH:MM` in the operator's locale, 24-hour. */
function clockTime(ms: number): string {
  return new Date(ms).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  });
}

/** The one toast for a burst of parks. */
export function parkedBurstToast(
  parks: LimitParkedPayload[],
  formatTime: (ms: number) => string = clockTime,
): { message: string; type: ToastType } {
  const dues = parks.map((p) => p.dueAtMs).filter((d): d is number => typeof d === "number");
  let until = "";
  if (dues.length > 0) {
    const first = formatTime(Math.min(...dues));
    const last = formatTime(Math.max(...dues));
    until = first === last ? ` until ${first}` : ` until ${first}–${last}`;
  }
  return {
    message: `${burstSubject(parks)} parked at a usage limit${until}`,
    type: "info",
  };
}

/** The one toast for a burst of reset-continues. */
export function continuedBurstToast(continued: LimitContinuedPayload[]): {
  message: string;
  type: ToastType;
} {
  return {
    message: `${burstSubject(continued)} continued after the usage limit reset`,
    type: "info",
  };
}

/** The one toast for a burst of reset-continues the runner gave up on. */
export function gaveUpBurstToast(gaveUp: LimitGaveUpPayload[]): {
  message: string;
  type: ToastType;
} {
  const one = gaveUp.length === 1;
  return {
    message: `${burstSubject(gaveUp)} stopped at a usage limit that has reset, but the runner could not continue ${
      one ? "it; it needs" : "them; they need"
    } a prompt by hand`,
    type: "error",
  };
}

/** Whether a park is worth a toast: not the runner's own short retry. */
export function parkIsToasted(p: LimitParkedPayload): boolean {
  return p.reason !== "continue-retry";
}

/**
 * Collects items for `windowMs` after the first one, then hands the whole
 * burst to `flush` once. `cancel` drops a pending burst (unmount).
 */
export function createBurstCoalescer<T>(
  windowMs: number,
  flush: (items: T[]) => void,
): { push: (item: T) => void; cancel: () => void } {
  let pending: T[] = [];
  let timer: ReturnType<typeof setTimeout> | null = null;
  return {
    push(item: T) {
      pending.push(item);
      if (timer !== null) return;
      timer = setTimeout(() => {
        const burst = pending;
        pending = [];
        timer = null;
        flush(burst);
      }, windowMs);
    },
    cancel() {
      if (timer !== null) clearTimeout(timer);
      timer = null;
      pending = [];
    },
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
    const parked = createBurstCoalescer<LimitParkedPayload>(LIMIT_BURST_MS, (burst) => {
      const toast = parkedBurstToast(burst);
      showToast(toast.message, toast.type);
    });
    const continued = createBurstCoalescer<LimitContinuedPayload>(LIMIT_BURST_MS, (burst) => {
      const toast = continuedBurstToast(burst);
      showToast(toast.message, toast.type);
    });
    const gaveUp = createBurstCoalescer<LimitGaveUpPayload>(LIMIT_BURST_MS, (burst) => {
      const toast = gaveUpBurstToast(burst);
      showToast(toast.message, toast.type);
    });
    const unlistenGaveUp = listen<LimitGaveUpPayload>(
      "session-limit-reset-continue-gave-up",
      (event) => {
        gaveUp.push(event.payload ?? {});
      },
    );
    const unlistenParked = listen<LimitParkedPayload>("session-limit-parked", (event) => {
      const payload = event.payload ?? {};
      if (parkIsToasted(payload)) parked.push(payload);
    });
    const unlistenContinued = listen<LimitContinuedPayload>(
      "session-limit-reset-continued",
      (event) => {
        continued.push(event.payload ?? {});
      },
    );
    return () => {
      unlistenMigrated.then((fn) => fn());
      unlistenSkipped.then((fn) => fn());
      unlistenParked.then((fn) => fn());
      unlistenContinued.then((fn) => fn());
      unlistenGaveUp.then((fn) => fn());
      parked.cancel();
      continued.cancel();
      gaveUp.cancel();
    };
  }, [showToast]);
}
