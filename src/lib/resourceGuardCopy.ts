/**
 * Operator-facing words for the resource guard's blocking dialog and its grant
 * banner (plan
 * `2026-10-01-runner-thread-ceilings-ignore-the-machine-and-the-guard-dialog-says-low-memory`
 * Phases 0 and 3).
 *
 * Pure functions of the pending decision, the OS and the clock, so the copy is
 * unit-testable in vitest's `node` environment without rendering a component —
 * the same constraint `resourceGuard.ts` documents. The components
 * (`ResourceGuardDialog`, `ResourceGuardGrantBanner`) only lay these strings out.
 */

import {
  GRANT_MAX_MS,
  GRANT_IDLE_MS,
  GRANT_SPAWN_LIMIT,
  type KnownResourceGuardMetric,
  type PendingResourceBlock,
  type ResourceGuardGrant,
  type ResourceGuardMetric,
} from "./resourceGuard";

/**
 * Dialog titles per lane. The two named lanes are the Rust `LaneMetric::headline`
 * strings — `resourceGuardWire.fixture.json` pins both sides to the same bytes,
 * so the dialog cannot drift from the toast and log vocabulary. `"unknown"` is
 * webview-only: a refusal that named no lane gets a title that names none, never
 * a guess.
 */
export const RESOURCE_GUARD_HEADLINES: Record<ResourceGuardMetric, string> = {
  free_commit_bytes: "Low memory",
  thread_count: "High thread count",
  unknown: "Resource limit reached",
};

/** The lane as a noun phrase, for "overridden for thread count". */
const LANE_NOUN: Record<KnownResourceGuardMetric, string> = {
  free_commit_bytes: "memory",
  thread_count: "thread count",
};

/** The OS families whose consequence text differs. */
export type GuardOs = "windows" | "linux" | "macos" | "other";

/**
 * The OS this webview runs on, from `navigator.platform` — the same probe the
 * terminal code uses (`useShellIntegration`, `TerminalSessionContext`).
 * `typeof navigator === "undefined"` (node tests) reads `"other"`, whose copy
 * names no OS at all, so an unknown platform is never told about Windows.
 */
export function detectGuardOs(): GuardOs {
  const platform = typeof navigator === "undefined" ? "" : (navigator.platform ?? "");
  if (platform.startsWith("Win")) return "windows";
  if (platform.startsWith("Linux")) return "linux";
  if (platform.startsWith("Mac")) return "macos";
  return "other";
}

/**
 * What happens if the operator overrides, per lane and OS. The memory lane's
 * failure is the OS killing a session — Windows by commit exhaustion, Linux by
 * its OOM killer — so the OS is named only where it is true. The thread lane's
 * failure is the runner itself wedging, which is the same on every OS, so it
 * names none.
 */
function consequence(metric: ResourceGuardMetric, os: GuardOs): string {
  switch (metric) {
    case "free_commit_bytes": {
      const killer =
        os === "windows"
          ? "Windows to kill"
          : os === "linux"
            ? "the Linux out-of-memory killer to end"
            : "the operating system to kill";
      return (
        `Starting anyway may cause ${killer} a session that is already running — that is ` +
        "the failure this guard exists to prevent. Closing a build or an idle session first " +
        "is usually enough."
      );
    }
    case "thread_count":
      return (
        "Every session holds runner threads. Past this ceiling the runner's blocking pool can " +
        "run dry and the whole runner stop responding — that is the failure this guard exists " +
        "to prevent. Letting sessions finish or closing idle terminals first is usually enough."
      );
    case "unknown":
      return (
        "The runner refused this start because a resource limit was reached, but did not name " +
        "one this version recognises."
      );
  }
}

/** `60_000` → `"60 s"`, `300_000` → `"5 min"`: minutes only from two up. */
function formatSpan(ms: number): string {
  return ms >= 120_000 && ms % 60_000 === 0 ? `${ms / 60_000} min` : `${Math.round(ms / 1000)} s`;
}

/** The dialog's strings for one pending decision. */
export interface ResourceGuardDialogCopy {
  title: string;
  message: string;
  description: string;
  confirmText: string;
  cancelText: string;
}

/**
 * Everything the dialog says, in reading order: WHAT refused (title by lane),
 * HOW MANY starts this answers, WHO asked, what overriding risks, how far the
 * answer reaches (the grant), and where the limits live.
 *
 * The grant sentence is part of the dialog, not a surprise discovered later:
 * "Start anyway" admits more than the start on screen, and the operator is told
 * how many, for how long, and where to take it back before they click.
 */
export function resourceGuardDialogCopy(
  block: PendingResourceBlock,
  os: GuardOs,
): ResourceGuardDialogCopy {
  const many = block.pending > 1;
  const title = many
    ? `${RESOURCE_GUARD_HEADLINES[block.metric]} — ${block.pending} pending starts`
    : RESOURCE_GUARD_HEADLINES[block.metric];

  const parts: string[] = [];
  if (block.source) {
    const queued =
      block.source.queued !== undefined && block.source.queued > 1
        ? ` — up to ${block.source.queued} starts queued`
        : "";
    parts.push(`Requested by ${block.source.label}${queued}.`);
  }
  parts.push(consequence(block.metric, os));
  if (block.metric === "unknown") {
    parts.push("Starting anyway overrides the check for this start only.");
  } else {
    parts.push(
      `Starting anyway also lets up to ${GRANT_SPAWN_LIMIT} more refused ` +
        `${LANE_NOUN[block.metric]} starts through without asking, for ` +
        `${formatSpan(GRANT_IDLE_MS)} after the last one (${formatSpan(GRANT_MAX_MS)} at most). ` +
        "A banner shows it and can revoke it.",
    );
  }
  parts.push("The limits are configurable under Settings > Resource Guard.");

  return {
    title,
    message: block.message,
    description: parts.join(" "),
    confirmText: many ? "Start all anyway" : "Start anyway",
    cancelText: many ? "Start none" : "Don't start",
  };
}

/**
 * What a screen reader hears for a live grant: the banner line without its
 * per-second countdown, so the live region speaks when the grant appears or a
 * start is spent — not every second.
 */
export function grantAnnouncement(grant: ResourceGuardGrant): string {
  const starts = grant.remaining === 1 ? "1 start" : `${grant.remaining} starts`;
  return `Resource guard overridden for ${LANE_NOUN[grant.metric]} — ${starts} left`;
}

/**
 * The banner line for a live grant at `now` (epoch ms):
 * "Resource guard overridden for thread count — 7 starts, 42 s left (session restore)".
 * Seconds round UP, so the banner never reads "0 s left" while the grant is live.
 */
export function grantBannerText(grant: ResourceGuardGrant, now: number): string {
  const seconds = Math.max(1, Math.ceil((grant.expiresAt - now) / 1000));
  const starts = grant.remaining === 1 ? "1 start" : `${grant.remaining} starts`;
  const source = grant.sourceLabel ? ` (${grant.sourceLabel})` : "";
  return (
    `Resource guard overridden for ${LANE_NOUN[grant.metric]} — ` +
    `${starts}, ${seconds} s left${source}`
  );
}
