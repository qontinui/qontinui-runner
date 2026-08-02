/**
 * The `Open` state machine (plan §7.1) — pure half.
 *
 * §7.1 specifies "a small state machine, surfaced as prose, not a spinner". The
 * prose is the point: a non-developer must be able to see *why* it is slow, and
 * "Starting your website…" with the last stderr line beats a spinner that says
 * nothing for forty seconds.
 *
 * Everything here is pure so it is testable — the runner's vitest config is
 * `environment: "node"` (no jsdom), so the effectful half lives in
 * `useProjectOpen.ts` and only this file is unit-tested.
 */

import type { ProjectSnapshot } from "./types";
import { isLiveProcessState } from "./types";

/** Where an `Open` run currently is. */
export type OpenPhase =
  | { kind: "idle" }
  /** Starting one of the project's processes; `stderrTail` is its latest output. */
  | {
      kind: "starting";
      processId: string;
      processName: string;
      /** 1-based, for "Starting 2 of 3". */
      step: number;
      total: number;
      stderrTail?: string;
    }
  /** Every process is ready; the preview window is being pointed at the site. */
  | { kind: "opening"; url: string }
  | { kind: "done"; url: string }
  | { kind: "failed"; message: string; detail?: string };

/**
 * What an `Open` run has to do, derived from the project and its snapshot.
 *
 * `toStart` deliberately keys on [`isLiveProcessState`] rather than an equality
 * check on `"running"`: a process that is `healthy` or `externally_owned` is
 * already serving the site, and restarting it would take the user's site DOWN
 * in response to them asking to see it.
 */
export interface OpenPlan {
  /** Process ids that are not currently live, in the project's declared order. */
  toStart: { id: string; name: string }[];
  /** The address to show once everything is ready. */
  url: string | null;
  /**
   * Set when the run cannot even be attempted; the UI shows this instead of
   * starting anything. `null` means go ahead.
   */
  blockedReason: string | null;
}

export interface OpenPlanInput {
  frontPageUrl?: string | null;
  processIds: string[];
  /** `undefined` while the snapshot is still loading. */
  snapshot?: ProjectSnapshot;
}

/**
 * Decide what `Open` must do. Pure.
 *
 * An absent snapshot is NOT treated as "nothing is running" — the same
 * distinction the card draws between "Checking…" and "Not running". Without a
 * snapshot we cannot tell a stopped process from an unread one, and starting a
 * process that is already up is the one outcome that actively breaks the site.
 */
export function planOpen({ frontPageUrl, processIds, snapshot }: OpenPlanInput): OpenPlan {
  const url = frontPageUrl?.trim() || null;

  if (!url) {
    return {
      toStart: [],
      url: null,
      blockedReason:
        "This project has no front page address yet. Add one (for example http://localhost:3000) and try again.",
    };
  }

  if (processIds.length === 0) {
    // Nothing to start — a static site or an already-deployed URL. Showing it
    // is still the right response to Open.
    return { toStart: [], url, blockedReason: null };
  }

  if (!snapshot) {
    return {
      toStart: [],
      url,
      blockedReason: "Still checking what this project is doing — try again in a moment.",
    };
  }

  const byId = new Map(snapshot.processes.map((p) => [p.id, p]));
  const toStart = processIds
    .filter((id) => {
      const proc = byId.get(id);
      // An id the snapshot does not know about is a stale binding (the process
      // config was deleted). Skip it rather than failing the whole run: the
      // remaining processes may well be all the site needs.
      return proc !== undefined && !isLiveProcessState(proc.state);
    })
    .map((id) => ({ id, name: byId.get(id)?.name || id }));

  return { toStart, url, blockedReason: null };
}

/**
 * The one line shown to the user for a phase. Plain English on purpose — this
 * is the surface a non-developer reads while their site boots.
 *
 * `projectName` is threaded through so the copy names the thing the user
 * clicked rather than saying "the project".
 */
export function openProse(phase: OpenPhase, projectName: string): string {
  switch (phase.kind) {
    case "idle":
      return "";
    case "starting":
      return phase.total > 1
        ? `Starting ${phase.processName} (${phase.step} of ${phase.total})…`
        : `Starting ${projectName}…`;
    case "opening":
      return `Opening ${projectName}…`;
    case "done":
      return `${projectName} is open.`;
    case "failed":
      return phase.message;
  }
}

/** True while an `Open` run is in flight — the button stays disabled. */
export function isOpenBusy(phase: OpenPhase): boolean {
  return phase.kind === "starting" || phase.kind === "opening";
}

/**
 * Turn a failed `start_managed_process` into something a non-developer can act
 * on.
 *
 * The Rust side returns a rich diagnostic — `await_ready` appends the captured
 * stderr tail on an early exit, which is exactly what a developer wants and
 * exactly what buries the actionable sentence for everyone else. So: lead with
 * plain English naming the process, and carry the raw text through as `detail`
 * for the disclosure.
 */
export function describeStartFailure(processName: string, error: unknown): OpenPhase {
  const raw = error instanceof Error ? error.message : String(error);
  return {
    kind: "failed",
    message: `${processName} didn't start. The details below say why.`,
    detail: raw.trim() || undefined,
  };
}

/**
 * Last few stderr lines, newest last — the tail shown under "Starting …".
 *
 * Mirrors `ProcessManagerTab`'s treatment (last 5 of an 80-line pull) so the
 * two surfaces show the same thing for the same process.
 */
export function stderrTailOf(
  lines: { stream: string; line: string }[] | undefined,
  keep = 5,
): string {
  if (!lines?.length) return "";
  return lines
    .filter((l) => l.stream === "stderr")
    .slice(-keep)
    .map((l) => l.line)
    .join("\n");
}
