/**
 * The Session Manager's bulk actions over a selection (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * Phase 5).
 *
 * Every action answers ONE result per selected session — done, not done with
 * the reason, or unknown — and {@link summarizeBulkResults} never renders one
 * "done" for a partial batch (served policy `ux-priorities`
 * `an-action-must-acknowledge-itself`).
 *
 * - **Close** is not here: it is the D7 confirmation (`SessionCloseDialog`)
 *   over the graceful path in `sessionClose.ts`, opened on the selection.
 *   {@link idsToClose} is the dialog's re-check at the moment of confirm.
 * - **Mark finished** writes the work-axis marker through
 *   `terminal_session_set_finished`, the Tauri twin of
 *   `POST /sessions/{id}/finish` over the same store and the same response
 *   body; whether coord was told is part of each item's result.
 * - **Group / pin / mute / snooze** are ONE organization-store batch. Each
 *   item is then read back from the snapshot the apply answered, so a session
 *   the store did not record reads "not done" even when the op said ok.
 * - **Move to page** maps the page move's own per-session outcomes.
 * - **Resume** launches each transcript row's resume in a new tab, one at a
 *   time; a tab-sourced row (no transcript) is reported, never skipped silently.
 *
 * Effects are injected, so every action is testable without Tauri.
 */

import { describeThrown } from "@/lib/utils";

import {
  closeNeedsSecondTick,
  closeTickKey,
  type CloseItemResult,
  type CloseTarget,
} from "./sessionClose";
import type {
  MemberOrg,
  SessionGroupColorRole,
  SessionOrg,
  SessionOrgApplyResponse,
  SessionOrgOp,
} from "./sessionOrgTypes";
import type { SessionLocation } from "./sessionStatusModel";

/** One selected session, as every bulk action sees it. */
export interface BulkItem {
  /** The panel row id. */
  sessionId: string;
  title: string;
  /** The organization-store key. */
  sessionKey: string;
  /** The Claude session id — what Mark finished addresses; `null` = none known. */
  claudeSessionId: string | null;
  /** Why the row cannot be resumed; `null` = it can. */
  resumeBlocker: string | null;
}

/** One session's outcome. */
export interface BulkItemResult {
  sessionId: string;
  title: string;
  /** `unknown` — the action was attempted and its effect is not known. */
  state: "done" | "not-done" | "unknown";
  detail: string;
}

/** A finished batch, as the panel lists it. */
export interface BulkOutcome {
  /** "Pinned", "Marked finished", … — the past tense of the action. */
  label: string;
  results: BulkItemResult[];
}

const result = (
  item: Pick<BulkItem, "sessionId" | "title">,
  state: BulkItemResult["state"],
  detail: string,
): BulkItemResult => ({ sessionId: item.sessionId, title: item.title, state, detail });

/** "Pinned 3 of 5 · 2 not done · 1 unknown" — never a lone "done" for a partial batch. */
export function summarizeBulkResults(outcome: BulkOutcome): string {
  const { label, results } = outcome;
  const done = results.filter((r) => r.state === "done").length;
  const notDone = results.filter((r) => r.state === "not-done").length;
  const unknown = results.filter((r) => r.state === "unknown").length;
  const parts = [`${label} ${done} of ${results.length}`];
  if (notDone) parts.push(`${notDone} not done`);
  if (unknown) parts.push(`${unknown} unknown`);
  return parts.join(" · ");
}

// ── Mark finished ────────────────────────────────────────────────────────────

/** The `terminal_session_set_finished` envelope (`CommandResponse`). */
interface FinishResponse {
  success?: boolean;
  message?: string | null;
  data?: {
    changed?: string;
    coord?: { queued?: boolean; reason?: string | null };
  } | null;
}

function describeFinish(raw: unknown): Pick<BulkItemResult, "state" | "detail"> {
  const r = raw as FinishResponse | null;
  if (!r || typeof r !== "object" || typeof r.success !== "boolean") {
    return { state: "unknown", detail: `unrecognised response: ${JSON.stringify(raw)}` };
  }
  if (!r.success) {
    return {
      state: "not-done",
      detail: `not marked: ${r.message ?? "the runner's lifecycle registry does not know this session"}`,
    };
  }
  const changed = r.data?.changed;
  const head = changed === "none" ? "Already marked finished" : "Marked finished";
  const coord = r.data?.coord;
  if (coord?.queued === true) return { state: "done", detail: `${head} · coord told` };
  if (coord?.queued === false) {
    return {
      state: "done",
      detail: `${head} · local only, coord not told (${coord.reason ?? "no reason given"})`,
    };
  }
  return { state: "done", detail: `${head} · whether coord was told is unknown` };
}

/** Mark each item's work finished; one call per session, all at once. */
export async function bulkMarkFinished(
  items: readonly BulkItem[],
  setFinished: (claudeSessionId: string) => Promise<unknown>,
): Promise<BulkItemResult[]> {
  return Promise.all(
    items.map(async (item) => {
      if (!item.claudeSessionId) {
        return result(item, "not-done", "not marked: no Claude session id is known for it yet");
      }
      try {
        const d = describeFinish(await setFinished(item.claudeSessionId));
        return result(item, d.state, d.detail);
      } catch (e) {
        return result(
          item,
          "not-done",
          `not marked: ${describeThrown(e, "terminal_session_set_finished failed")}`,
        );
      }
    }),
  );
}

// ── Organization-store actions ───────────────────────────────────────────────

/** One org-store bulk action: the op over the keys, and how to read each item back. */
export interface OrgBulkAction {
  label: string;
  /** The op over every selected key. */
  op: (sessionKeys: string[]) => SessionOrgOp;
  /**
   * Whether the snapshot shows the item done. `groupId` is the id the batch's
   * `createGroup` answered (else `null`).
   */
  holds: (member: MemberOrg | undefined, org: SessionOrg, groupId: string | null) => boolean;
  /** What a not-held item says. */
  missed: string;
}

export const bulkPin = (pinned: boolean): OrgBulkAction => ({
  label: pinned ? "Pinned" : "Unpinned",
  op: (sessionKeys) => ({ op: "setPinned", sessionKeys, pinned }),
  holds: (m) => (m?.pinned ?? false) === pinned,
  missed: pinned ? "not pinned" : "still pinned",
});

export const bulkMute = (muted: boolean): OrgBulkAction => ({
  label: muted ? "Muted" : "Unmuted",
  op: (sessionKeys) => ({ op: "setMuted", sessionKeys, muted }),
  holds: (m) => (m?.muted ?? false) === muted,
  missed: muted ? "not muted" : "still muted",
});

export const bulkSnooze = (until: number | null): OrgBulkAction => ({
  label: until === null ? "Unsnoozed" : "Snoozed",
  op: (sessionKeys) => ({ op: "snooze", sessionKeys, until }),
  holds: (m) => (m?.snoozedUntil ?? null) === until,
  missed: until === null ? "still snoozed" : "not snoozed",
});

export const bulkAddToGroup = (groupId: string, groupTitle: string): OrgBulkAction => ({
  label: `Added to ${groupTitle}`,
  op: (sessionKeys) => ({ op: "addMembers", groupId, sessionKeys }),
  holds: (m) => m?.groupId === groupId,
  missed: `not in ${groupTitle}`,
});

export const bulkNewGroup = (title: string, colorRole?: SessionGroupColorRole): OrgBulkAction => ({
  label: `Grouped as ${title}`,
  op: (sessionKeys) => ({ op: "createGroup", title, colorRole, sessionKeys }),
  holds: (m, _org, groupId) => groupId !== null && m?.groupId === groupId,
  missed: `not in the new group ${title}`,
});

/** Run one org action over the items and read each item back from the answer. */
export async function bulkApplyOrg(
  items: readonly BulkItem[],
  action: OrgBulkAction,
  apply: (ops: SessionOrgOp[]) => Promise<SessionOrgApplyResponse>,
): Promise<BulkItemResult[]> {
  if (items.length === 0) return [];
  let response: SessionOrgApplyResponse;
  try {
    response = await apply([action.op(items.map((i) => i.sessionKey))]);
  } catch (e) {
    const why = describeThrown(e, "session_org_apply failed");
    return items.map((i) => result(i, "unknown", `whether it applied is unknown: ${why}`));
  }
  const opResult = response.results[0];
  const org = response.snapshot.status === "known" ? response.snapshot.org : null;
  if (!org) {
    const why = response.snapshot.reason ?? "the organization store is unknown";
    return items.map((i) => result(i, "unknown", `the store could not be read back: ${why}`));
  }
  const groupId = opResult?.groupId ?? null;
  return items.map((i) => {
    if (action.holds(org.members[i.sessionKey], org, groupId))
      return result(i, "done", action.label);
    const refusal = opResult && !opResult.ok ? ` (refused: ${opResult.error ?? "no reason"})` : "";
    return result(i, "not-done", `${action.missed}${refusal}`);
  });
}

// ── Move to page ─────────────────────────────────────────────────────────────

/** Map the page move's own outcomes onto the items, in item order. */
export function moveResults(
  items: readonly BulkItem[],
  outcomes: readonly { sessionId: string; ok: boolean; detail: string }[],
  pageName: string,
): BulkItemResult[] {
  return items.map((item) => {
    const o = outcomes.find((x) => x.sessionId === item.sessionId);
    if (!o) return result(item, "unknown", "the move answered nothing for it");
    return o.ok ? result(item, "done", `Moved to ${pageName}`) : result(item, "not-done", o.detail);
  });
}

// ── Resume ───────────────────────────────────────────────────────────────────

/**
 * Why a row cannot be resumed, or `null` when it can. A second
 * `claude --resume` of a conversation that is still running forks it, so a
 * row that may be running — anywhere, or nobody knows — is not resumed.
 */
export function resumeBlockerOf(row: {
  source: "transcript" | "tab";
  injected: boolean;
  status: { location: SessionLocation };
}): string | null {
  if (row.source !== "transcript") return "a tab row has no transcript to resume";
  if (row.injected) return "a test fixture has no transcript on disk";
  switch (row.status.location) {
    case "not-running":
      return null;
    case "unknown":
      return "whether it is still running is unknown — a resume could fork it";
    default:
      return "it is still running — a resume would fork it";
  }
}

export interface ResumeEffects {
  resume: (item: BulkItem) => void;
  /** Spacing between launches, so each tab initialises before the next. */
  sleep: (ms: number) => Promise<void>;
  gapMs?: number;
}

/** Launch each resumable item's resume in turn; report the rest with the reason. */
export async function bulkResumeItems(
  items: readonly BulkItem[],
  effects: ResumeEffects,
): Promise<BulkItemResult[]> {
  const out: BulkItemResult[] = [];
  let launched = 0;
  for (const item of items) {
    if (item.resumeBlocker) {
      out.push(result(item, "not-done", `not resumed: ${item.resumeBlocker}`));
      continue;
    }
    if (launched > 0) await effects.sleep(effects.gapMs ?? 500);
    try {
      effects.resume(item);
      launched += 1;
      out.push(result(item, "done", "Resume launched in a new tab"));
    } catch (e) {
      out.push(result(item, "not-done", `not resumed: ${describeThrown(e, "resume failed")}`));
    }
  }
  return out;
}

// ── Close: the re-check at confirm ───────────────────────────────────────────

/** What a confirm fired NOW closes, and the included rows it leaves alone. */
export interface CloseConfirmation {
  /** The rows to close: included, with a tab here, and ticked where D7 asks. */
  ids: string[];
  /**
   * Included rows the confirm will NOT close, each with why — reported
   * beside the close's own results so no included row goes unanswered.
   */
  skipped: CloseItemResult[];
  /**
   * The subset of `skipped` that reads `Needs you` / `Working` / `Unknown`
   * with no tick for THAT activity — a row that changed while the dialog was
   * open. Non-empty means the dialog must show the new tick requirement.
   */
  needsTick: CloseTarget[];
}

/**
 * The confirm's re-check, from the targets as they read at this instant.
 *
 * Every included target with a tab here closes — EXCEPT one that now reads
 * `Needs you` / `Working` / `Unknown` without a tick for that activity (the
 * tick is keyed by session AND activity, so a tick given while it read `Idle`
 * does not cover it). Such a row turned risky while the dialog was open: it
 * is excluded from this close and answered "not closed" with the reason,
 * while the rows the operator did confirm still close. A row whose tab went
 * away is answered too, never dropped silently.
 */
export function idsToClose(
  targets: readonly CloseTarget[],
  excluded: ReadonlySet<string>,
  ticks: ReadonlySet<string>,
): CloseConfirmation {
  const ids: string[] = [];
  const skipped: CloseItemResult[] = [];
  const needsTick: CloseTarget[] = [];
  for (const t of targets) {
    if (excluded.has(t.sessionId)) continue;
    if (!t.closable) {
      skipped.push({
        sessionId: t.sessionId,
        title: t.title,
        state: "not-closed",
        detail: "not closed: it has no live tab in this window",
      });
      continue;
    }
    if (closeNeedsSecondTick(t.activity) && !ticks.has(closeTickKey(t))) {
      needsTick.push(t);
      skipped.push({
        sessionId: t.sessionId,
        title: t.title,
        state: "not-closed",
        detail: `not closed: it changed to ${t.activityWord} before the close ran — tick it to close it anyway`,
      });
      continue;
    }
    ids.push(t.sessionId);
  }
  return { ids, skipped, needsTick };
}
