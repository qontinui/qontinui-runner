/**
 * Did a FRESH Claude launch start? (plan
 * `2026-11-20-terminal-launch-failure-analyze-button`, Phase 4.)
 *
 * A failed `claude` leaves its shell alive, so no pane state ever says so.
 * After the launch menu types the command this runs two observers side by
 * side and combines what they READ:
 *
 * - the resume path's handshake wait (`waitForClaudeHandshake`, its default
 *   `HandshakeWaitOptions`), with Claude's SUCCESS markers only. The resume
 *   FAILURE markers ("No conversation found", the session picker) describe a
 *   `--resume` of a missing id and are not a fresh-launch signal, so they are
 *   left out rather than allowed to misname a fresh launch's outcome;
 * - the shim's per-pane launch-failure record (`get_terminal_launch_failure`),
 *   polled until the wait settles, plus once after.
 *
 * Verdicts claim only what was observed:
 * - `started`: the handshake was seen. Never a banner, whatever else was read.
 * - `failed`: the wait ran out over a READABLE scrollback, or the shim
 *   recorded a failure, or both. Both "failed" and "timeout" from the wait
 *   count as not seen.
 * - `unknown`: nothing usable was read — the scrollback never answered and
 *   the registry never recorded anything. No banner, and no "started" claim.
 */

import { waitForClaudeHandshake, readScrollbackTail } from "./resumeVerification";
import { CLAUDE_HANDSHAKE_REGEXES, CLAUDE_TITLE_REGEXES } from "./providerAdapter";
import type { HandshakePatterns } from "./providerAdapter";
import type { LaunchFailureRead } from "./launchFailureApi";

/** Claude's launch success markers, with no resume-failure markers. */
export const FRESH_LAUNCH_HANDSHAKE_PATTERNS: HandshakePatterns = {
  success: [],
  failure: [],
  successPatterns: CLAUDE_HANDSHAKE_REGEXES,
  titlePatterns: CLAUDE_TITLE_REGEXES,
  failurePatterns: [],
};

export interface LaunchFailureSignals {
  /** The handshake was not seen; how long the wait actually ran. */
  handshakeWaitedMs?: number;
  /** The shim's recorded failure kind (`resolve-failed`, `exec-failed`, …). */
  beaconKind?: string;
  beaconDetail?: string;
}

export type FreshLaunchVerdict =
  | { kind: "started" }
  | { kind: "failed"; signals: LaunchFailureSignals }
  | { kind: "unknown" };

export interface FreshLaunchDeps {
  /** Wait for the handshake; defaults to `waitForClaudeHandshake`. */
  waitHandshake?: (
    tabId: string,
    readTail: (tabId: string) => Promise<string | null>,
  ) => Promise<"verified" | "failed" | "timeout">;
  /** Scrollback tail reader; defaults to `readScrollbackTail`. */
  readTail?: (tabId: string) => Promise<string | null>;
  /** The shim registry read (`readTerminalLaunchFailure`). */
  readFailure: (tabId: string) => Promise<LaunchFailureRead>;
  /** Interval between registry polls. */
  pollIntervalMs?: number;
  now?: () => number;
  sleep?: (ms: number) => Promise<void>;
}

const defaultWait = (tabId: string, readTail: (tabId: string) => Promise<string | null>) =>
  waitForClaudeHandshake(tabId, { readTail, handshakePatterns: FRESH_LAUNCH_HANDSHAKE_PATTERNS });

/** Observe a fresh launch in `tabId` and say what was seen. Never throws. */
export async function verifyFreshLaunch(
  tabId: string,
  deps: FreshLaunchDeps,
): Promise<FreshLaunchVerdict> {
  const {
    waitHandshake = defaultWait,
    readTail = readScrollbackTail,
    readFailure,
    pollIntervalMs = 1_000,
    now = Date.now,
    sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms)),
  } = deps;

  let tailRead = false;
  const trackedRead = async (id: string) => {
    const tail = await readTail(id);
    if (tail !== null) tailRead = true;
    return tail;
  };

  let beacon: Extract<LaunchFailureRead, { state: "recorded" }> | null = null;
  const pollOnce = async () => {
    if (beacon) return;
    const read = await readFailure(tabId).catch((): LaunchFailureRead => ({ state: "unread" }));
    if (read.state === "recorded") beacon = read;
  };

  let settled = false;
  const poller = (async () => {
    while (!settled && !beacon) {
      await pollOnce();
      if (settled || beacon) break;
      await sleep(pollIntervalMs);
    }
  })();

  const started = now();
  let outcome: "verified" | "failed" | "timeout";
  try {
    outcome = await waitHandshake(tabId, trackedRead);
  } catch {
    outcome = "timeout";
    tailRead = false;
  }
  const waitedMs = Math.max(0, now() - started);
  settled = true;
  await poller;
  // One read after the wait: a beacon written late in the window still counts.
  await pollOnce();

  if (outcome === "verified") return { kind: "started" };
  const signals: LaunchFailureSignals = {};
  if (tailRead) signals.handshakeWaitedMs = waitedMs;
  const recorded = beacon as Extract<LaunchFailureRead, { state: "recorded" }> | null;
  if (recorded) {
    signals.beaconKind = recorded.kind;
    if (recorded.detail) signals.beaconDetail = recorded.detail;
  }
  if (signals.handshakeWaitedMs === undefined && signals.beaconKind === undefined) {
    return { kind: "unknown" };
  }
  return { kind: "failed", signals };
}

/** The banner's "observed signal" line, e.g. `handshake not seen in 15 s · shim: resolve-failed`. */
export function describeLaunchFailure(signals: LaunchFailureSignals): string {
  const parts: string[] = [];
  if (signals.handshakeWaitedMs !== undefined) {
    parts.push(`handshake not seen in ${Math.round(signals.handshakeWaitedMs / 1000)} s`);
  }
  if (signals.beaconKind !== undefined) {
    parts.push(
      `shim: ${signals.beaconKind}${signals.beaconDetail ? ` (${signals.beaconDetail})` : ""}`,
    );
  }
  return parts.join(" · ");
}
