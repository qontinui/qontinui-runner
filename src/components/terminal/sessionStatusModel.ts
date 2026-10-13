/**
 * A session's status as two independent axes — WHERE its process is and WHAT
 * it is doing (plan
 * `2026-10-13-session-manager-chrome-style-tab-organization-and-operator-search`,
 * D2 / Phase 1).
 *
 * The enum this replaces merged location into state: `active-in-zone` and
 * `active-external` were both "working" plus a place, so no single word could
 * be precise. Two of its values were also not true:
 *
 *  - "Active (External)" was a GUESS — a tab-less transcript touched in the
 *    last 2–10 minutes while ANY unmanaged `claude` process existed. One stray
 *    process marked every recently-touched transcript external. Here
 *    `Outside runner` is a per-session match: a live row in Claude Code's own
 *    registry, keyed by session id, whose process this runner does not own.
 *  - "Frozen" named both "no PTY output for 60 s" (which an idle Claude at its
 *    prompt matches) and "a tab-less transcript that ended mid-tool-use". Here
 *    the transcript's last record decides: an assistant turn with no pending
 *    tool_use is `Idle at prompt`; a pending tool_use is
 *    `No output 12m (mid-tool)`, carrying its duration.
 *
 * Every value is a word, never color alone, and any input that cannot be read
 * classifies `Unknown` — never a definite default like `Not running`.
 *
 * Pure (no React, no Tauri), so every location × activity cell is unit-tested
 * without mounting the hook.
 */

import type { SessionState } from "./useZoneLayout";

// ── The two axes ─────────────────────────────────────────────────────────────

/** Where the session's process is, relative to this window. */
export type SessionLocation =
  | "on-screen"
  | "hidden-here"
  | "outside-runner"
  | "other-device"
  | "not-running"
  | "unknown";

/** What the session is doing. */
export type SessionActivity =
  | "needs-you"
  | "waiting-on-lock"
  | "working"
  | "idle-at-prompt"
  | "no-output-mid-tool"
  | "finished"
  | "exited"
  | "exited-error"
  | "unknown";

export const SESSION_LOCATIONS: readonly SessionLocation[] = [
  "on-screen",
  "hidden-here",
  "outside-runner",
  "other-device",
  "not-running",
  "unknown",
];

export const SESSION_ACTIVITIES: readonly SessionActivity[] = [
  "needs-you",
  "waiting-on-lock",
  "working",
  "idle-at-prompt",
  "no-output-mid-tool",
  "finished",
  "exited",
  "exited-error",
  "unknown",
];

export interface SessionStatus {
  location: SessionLocation;
  activity: SessionActivity;
  /** Which source decided each axis — the card's tooltip. */
  reason: string;
  /**
   * Epoch ms the activity began, when a source states it (the pending
   * tool_use's timestamp for `no-output-mid-tool`); `null` otherwise.
   */
  since: number | null;
  /** The process's exit code for `exited` / `exited-error`; `null` when unknown. */
  exitCode: number | null;
}

/**
 * The one label table for both axes. Every surface that names a status — the
 * card's words, the panel's "group by status" headers, the header's filter
 * chips — reads it from here, so one status never carries two words (the old
 * `dormant` was "Recent" as a group and "Dormant" as a dot).
 *
 * The activity labels are the BASE words; {@link activityText} adds the
 * quantity a label carries (`No output 12m (mid-tool)`, `… (code 2)`).
 */
export const SESSION_STATUS_LABELS: {
  readonly location: Readonly<Record<SessionLocation, string>>;
  readonly activity: Readonly<Record<SessionActivity, string>>;
} = {
  location: {
    "on-screen": "On screen",
    "hidden-here": "Hidden here",
    "outside-runner": "Outside runner",
    "other-device": "Other device",
    "not-running": "Not running",
    unknown: "Unknown",
  },
  activity: {
    "needs-you": "Needs you",
    "waiting-on-lock": "Waiting on lock",
    working: "Working",
    "idle-at-prompt": "Idle at prompt",
    "no-output-mid-tool": "No output (mid-tool)",
    finished: "Finished",
    exited: "Exited",
    "exited-error": "Exited — error",
    unknown: "Unknown",
  },
};

/**
 * Sort rank (lower = more urgent). `Needs you` is always first; a silent
 * mid-tool session and a crashed one come next because both are owed a look.
 */
export const ACTIVITY_PRIORITY: Readonly<Record<SessionActivity, number>> = {
  "needs-you": 0,
  "no-output-mid-tool": 1,
  "exited-error": 2,
  "waiting-on-lock": 3,
  working: 4,
  "idle-at-prompt": 5,
  unknown: 6,
  finished: 7,
  exited: 8,
};

/**
 * Dot color and pulse per activity. A separate semantic set from any group
 * color (D4), so a group never reads as an error.
 */
export const ACTIVITY_DOT: Readonly<Record<SessionActivity, { color: string; pulse: boolean }>> = {
  "needs-you": { color: "#e0af68", pulse: true },
  "waiting-on-lock": { color: "#e0af68", pulse: false },
  working: { color: "#9ece6a", pulse: true },
  "idle-at-prompt": { color: "#7aa2f7", pulse: false },
  "no-output-mid-tool": { color: "#f7768e", pulse: false },
  finished: { color: "#565f89", pulse: false },
  exited: { color: "#565f89", pulse: false },
  "exited-error": { color: "#f7768e", pulse: false },
  unknown: { color: "#414868", pulse: false },
};

/** `12m`, `3h`, `45s` — the duration a `No output` label carries. */
export function formatSilence(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 48) return `${h}h`;
  return `${Math.floor(h / 24)}d`;
}

/** The activity word as rendered, with the quantity its label carries. */
export function activityText(
  status: Pick<SessionStatus, "activity" | "since" | "exitCode">,
  now: number,
): string {
  switch (status.activity) {
    case "no-output-mid-tool":
      return status.since !== null
        ? `No output ${formatSilence(now - status.since)} (mid-tool)`
        : SESSION_STATUS_LABELS.activity["no-output-mid-tool"];
    case "exited-error":
      return status.exitCode !== null
        ? `Exited — error (code ${status.exitCode})`
        : SESSION_STATUS_LABELS.activity["exited-error"];
    default:
      return SESSION_STATUS_LABELS.activity[status.activity];
  }
}

/** The location word as rendered. */
export function locationText(status: Pick<SessionStatus, "location">): string {
  return SESSION_STATUS_LABELS.location[status.location];
}

/** Parse a persisted activity filter; anything unrecognized is `"all"`. */
export function parseActivityFilter(raw: string | null): SessionActivity | "all" {
  return raw !== null && (SESSION_ACTIVITIES as readonly string[]).includes(raw)
    ? (raw as SessionActivity)
    : "all";
}

/** Is this a {@link SessionLocation} literal? (Injected fixtures arrive as strings.) */
export function isSessionLocation(v: unknown): v is SessionLocation {
  return typeof v === "string" && (SESSION_LOCATIONS as readonly string[]).includes(v);
}

/** Is this a {@link SessionActivity} literal? */
export function isSessionActivity(v: unknown): v is SessionActivity {
  return typeof v === "string" && (SESSION_ACTIVITIES as readonly string[]).includes(v);
}

// ── Inputs ───────────────────────────────────────────────────────────────────

/** The session's tab in this window, when it has one. */
export interface StatusTabInput {
  alive: boolean;
  /** The tab's pane is showing: on screen in tmux mode, zoned in grid mode. */
  visible: boolean;
  exitCode: number | null;
  /** The page's screen/regex tab-state tracker (`useSessionStateTracking`). */
  state: SessionState | undefined;
  /** No PTY output for 60 s (`staleTabs`). */
  silent: boolean;
  /** A file-lock wait is open on this tab (`useFileLockTracking`). */
  lockWaiting: boolean;
}

/** This session's row in Claude Code's live-session registry. */
export interface RegistryMatch {
  pid: number;
  /** Claude Code's self-report: `idle` / `busy` / `waiting` / … */
  status: string;
  /** This runner owns the process (its subtree, or `ai_pid_tracker`). */
  runnerManaged: boolean;
}

/**
 * The registry read. `pending` (not yet answered) and `failed` (the command
 * errored, or its liveness snapshot came back empty) both mean NOTHING is known
 * about which sessions are live — a tab-less row then reads `Unknown`, never
 * `Not running`.
 */
export type RegistryRead =
  | { kind: "pending" }
  | { kind: "failed" }
  | { kind: "ok"; match: RegistryMatch | null };

/**
 * The external `claude`-process scan (`transcript_find_external_processes`).
 * `stale` is the runner's labelled previous COMPLETE answer, so it is as good
 * as `ok`; `degraded` and `pending` know nothing.
 */
export type ExternalScanState = "pending" | "ok" | "stale" | "degraded";

/** The transcript tail digest (`transcript_session_digests`). */
export interface DigestInput {
  lastMessageType: string;
  lastAssistantHadToolUse: boolean;
  lastMessageTimestamp: string;
}

/** A Fleet row from coord (Phase 9 / Fleet view). */
export interface FleetInput {
  /** The row's device is this one. */
  thisDevice: boolean;
  /** coord's `session_status` (`waiting_human`, `finished`, …), when served. */
  sessionStatus: string | null;
}

/**
 * The precedence-ranked state of plan
 * `2026-09-20-terminal-session-state-comes-from-events-not-screen-scraping`
 * (`Hook` > `Sideband` > `Statusline`), consumed as the top activity source
 * when present.
 */
export type EventState = "needs-input" | "working" | "idle";

export interface SessionStatusInputs {
  tab: StatusTabInput | null;
  registry: RegistryRead;
  externalScan: ExternalScanState;
  /**
   * The transcript digest; `null` when the row has a transcript but no digest
   * was served for it; `"not-applicable"` for a row with no transcript at all
   * (a plain shell, a session before its first message).
   */
  digest: DigestInput | null | "not-applicable";
  /**
   * The work-axis finished marker (`/finish-session`). `null` when no source
   * covers this session — read as UNKNOWN, never as "not finished".
   */
  finished: boolean | null;
  fleet?: FleetInput | null;
  events?: EventState | null;
}

// ── Classification ───────────────────────────────────────────────────────────

interface Axis<T> {
  value: T;
  reason: string;
}

/**
 * The location axis alone. Activity reads it (a process that is gone cannot be
 * idle), so it is resolved first, and the digest request set is built from it.
 */
export function classifyLocation(
  inputs: Pick<SessionStatusInputs, "tab" | "registry" | "externalScan" | "fleet">,
): Axis<SessionLocation> {
  const { tab, registry, externalScan, fleet } = inputs;
  if (fleet && !fleet.thisDevice)
    return { value: "other-device", reason: "fleet row on another device" };
  if (tab) {
    return tab.visible
      ? { value: "on-screen", reason: "tab here, on screen" }
      : { value: "hidden-here", reason: "tab here, not on screen" };
  }
  if (registry.kind !== "ok") {
    return {
      value: "unknown",
      reason:
        registry.kind === "pending"
          ? "live-session registry not read yet"
          : "live-session registry read failed",
    };
  }
  if (registry.match) {
    return registry.match.runnerManaged
      ? {
          value: "hidden-here",
          reason: `runs in this runner (pid ${registry.match.pid}), no tab on this page`,
        }
      : {
          value: "outside-runner",
          reason: `live process ${registry.match.pid} this runner does not own`,
        };
  }
  if (externalScan === "pending" || externalScan === "degraded") {
    return {
      value: "unknown",
      reason:
        externalScan === "pending"
          ? "process scan not answered yet"
          : "process scan degraded — cannot tell whether it runs",
    };
  }
  return { value: "not-running", reason: "no tab here and no live registry process" };
}

function parseMs(iso: string): number | null {
  if (!iso) return null;
  const ms = new Date(iso).getTime();
  return Number.isFinite(ms) ? ms : null;
}

interface ActivityAxis extends Axis<SessionActivity> {
  since: number | null;
  exitCode: number | null;
}

const act = (
  value: SessionActivity,
  reason: string,
  extra?: { since?: number | null; exitCode?: number | null },
): ActivityAxis => ({
  value,
  reason,
  since: extra?.since ?? null,
  exitCode: extra?.exitCode ?? null,
});

function classifyActivity(inputs: SessionStatusInputs, location: SessionLocation): ActivityAxis {
  const { tab, registry, digest, finished, fleet, events } = inputs;

  if (location === "other-device") {
    if (fleet?.sessionStatus === "waiting_human") return act("needs-you", "fleet: waiting_human");
    if (fleet?.sessionStatus === "finished") return act("finished", "fleet: finished");
    return act("unknown", `fleet: ${fleet?.sessionStatus ?? "no session status"}`);
  }

  // The process is gone.
  if (tab && !tab.alive) {
    if (tab.exitCode !== null && tab.exitCode !== 0) {
      return act("exited-error", `tab exited with code ${tab.exitCode}`, {
        exitCode: tab.exitCode,
      });
    }
    return act("exited", tab.exitCode === 0 ? "tab exited with code 0" : "tab exited", {
      exitCode: tab.exitCode,
    });
  }
  if (location === "not-running") {
    return finished === true
      ? act("finished", "marked finished; process gone")
      : act("exited", "process gone, exit code unknown");
  }

  const reg = registry.kind === "ok" ? registry.match : null;

  // Highest precedence: event-sourced state, when that plan has landed.
  if (events === "needs-input") return act("needs-you", "event: needs input");
  // Needs you — any source saying so wins.
  if (reg?.status === "waiting") return act("needs-you", "registry: waiting");
  if (tab?.state === "needs-input") return act("needs-you", "tab: needs input");
  if (tab?.lockWaiting) return act("waiting-on-lock", "waiting on a file lock");
  if (events === "working") return act("working", "event: working");
  if (reg?.status === "busy") return act("working", "registry: busy");
  if (finished === true) return act("finished", "marked finished");
  if (events === "idle") return act("idle-at-prompt", "event: idle");
  if (reg?.status === "idle") return act("idle-at-prompt", "registry: idle");

  // Below the registry: the transcript tail and the screen.
  if (tab) {
    if (!tab.silent) return act("working", "output within 60s");
    if (digest === "not-applicable") return act("idle-at-prompt", "no output 60s, no transcript");
    if (digest === null) return act("unknown", "no output 60s, no transcript digest");
    const since = parseMs(digest.lastMessageTimestamp);
    if (digest.lastMessageType === "assistant" && !digest.lastAssistantHadToolUse) {
      return act("idle-at-prompt", "last record: assistant turn, nothing pending", { since });
    }
    if (
      (digest.lastMessageType === "assistant" && digest.lastAssistantHadToolUse) ||
      digest.lastMessageType === "user"
    ) {
      return act("no-output-mid-tool", "no output, last record a pending tool step", { since });
    }
    return act("unknown", `no output 60s, last record "${digest.lastMessageType || "none"}"`);
  }

  // Tab-less and live: the registry is the only activity source.
  if (reg) return act("unknown", `registry status "${reg.status || "none"}"`);
  return act("unknown", "no source could classify it");
}

/** Classify one session. Pure: same inputs, same answer. */
export function classifySession(inputs: SessionStatusInputs): SessionStatus {
  const location = classifyLocation(inputs);
  const activity = classifyActivity(inputs, location.value);
  return {
    location: location.value,
    activity: activity.value,
    reason: `${location.reason}; ${activity.reason}`,
    since: activity.since,
    exitCode: activity.exitCode,
  };
}

/**
 * A status stated outright rather than classified — the debug-gated
 * `/ui-bridge/test/inject-session` fixture's short-circuit.
 */
export function injectedStatus(
  location: SessionLocation,
  activity: SessionActivity,
): SessionStatus {
  return { location, activity, reason: "injected test fixture", since: null, exitCode: null };
}

/**
 * Is the session's process running somewhere (or could it be)? Every
 * location but `not-running` — `unknown` included, because a row that could
 * not be placed must not be treated as gone.
 */
export function isPossiblyLive(location: SessionLocation): boolean {
  return location !== "not-running";
}

/** Does this session run in THIS runner (a tab here or a runner-owned pid)? */
export function isLiveInThisRunner(status: Pick<SessionStatus, "location" | "activity">): boolean {
  return (
    (status.location === "on-screen" || status.location === "hidden-here") &&
    status.activity !== "exited" &&
    status.activity !== "exited-error"
  );
}

// ── Unidentified processes ───────────────────────────────────────────────────

/**
 * Live `claude` processes this runner does not own that have NO registry row
 * (an old CLI build that writes none, or one not yet registered). Counted —
 * "N unidentified Claude processes" — and never attributed to a session.
 *
 * `null` (UNKNOWN) whenever either input could not be read: the count of a
 * scan that did not run is not zero.
 */
export function countUnidentified(
  scan: { state: ExternalScanState; pids: readonly number[] },
  registryPids: ReadonlySet<number> | null,
): number | null {
  if (scan.state === "pending" || scan.state === "degraded" || registryPids === null) return null;
  let n = 0;
  for (const pid of scan.pids) if (!registryPids.has(pid)) n++;
  return n;
}

/**
 * The next row after `afterId` (wrapping) whose activity is `activity` — the
 * "next session that went silent mid-tool" cycle (Ctrl+Shift+J). Starts at
 * the first match when `afterId` is null or no longer listed.
 */
export function nextWithActivity<
  T extends { sessionId: string; status: Pick<SessionStatus, "activity"> },
>(rows: readonly T[], activity: SessionActivity, afterId: string | null): T | null {
  const matches = rows.filter((r) => r.status.activity === activity);
  if (matches.length === 0) return null;
  const at = afterId === null ? -1 : matches.findIndex((r) => r.sessionId === afterId);
  return matches[(at + 1) % matches.length];
}
