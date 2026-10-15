/**
 * PTY-holder upgrade — the Terminal page's half of plan
 * `2026-09-12-out-of-process-pty-owner-for-terminal-hosted-sessions`, Phase 8
 * runner follow-up (D15: a holder with no common protocol version is "a visible
 * state, not a silent one", listed with honest actions).
 *
 * Pure: the wire types of the `pty_holder_attention` / `pty_holder_upgrade`
 * Tauri commands (`src-tauri/src/terminal/holder_upgrade.rs`), which actions a
 * pane may be offered, and how an outcome is told to the operator. The banner
 * (`HolderUpgradeBanner.tsx`) only renders what these return, so the runner's
 * `environment: "node"` vitest suite pins the rules without a DOM.
 */

/** Why a holder pane needs the operator's decision. */
export type AttentionReason = "incompatible" | "older_build";

/** One census row needing a decision (`holder_upgrade::AttentionPane`). */
export interface AttentionPane {
  paneId: string;
  reason: AttentionReason;
  holderPid: number | null;
  childPid: number | null;
  holderBuild: string | null;
  holderVersions: number[];
  detail: string | null;
}

/** `pty_holder_attention`'s answer (`holder_upgrade::HolderAttention`). */
export interface HolderAttention {
  upgradeSupported: boolean;
  /** Why "Upgrade holder" is not offered here; `null` where it is. */
  upgradeUnsupportedReason: string | null;
  endAndResumeAvailable: boolean;
  runnerHolderBuild: string;
  panes: AttentionPane[];
  /** The census could not read the pane directory: the list is UNKNOWN. */
  censusError: string | null;
}

/** `pty_holder_upgrade`'s answer (`holder_upgrade::HolderUpgradeOutcome`). */
export type HolderUpgradeOutcome =
  | {
      kind: "upgraded";
      successorPid: number;
      handoffOffset: number;
      previousBuild: string | null;
      successorBuild: string | null;
    }
  | { kind: "rolled_back"; reason: string }
  | { kind: "failed"; reason: string }
  | { kind: "unsupported"; reason: string }
  | { kind: "unknown_on_timeout"; reason: string; holderPidNow: number | null }
  | { kind: "refused"; reason: string };

/** Which actions a pane's row offers. */
export interface HolderActions {
  /** "Upgrade holder" — only where an in-place upgrade is supported (Unix). */
  upgrade: boolean;
  /** "Leave running" — always honest: the holder keeps serving its pane. */
  leaveRunning: boolean;
  /** "End and resume" — only where plan Phase 6's resume path exists. */
  endAndResume: boolean;
  /** Said when an action the plan names cannot be offered here. */
  note: string | null;
}

export const END_AND_RESUME_MISSING_NOTE =
  "End and resume is not available in this build (it needs the session-resume path of plan " +
  "Phase 6). Leaving the holder running keeps the pane's program alive.";

export const UPGRADE_UNSUPPORTED_NOTE =
  "This platform cannot upgrade a holder in place (a Windows ConPTY cannot be handed to another " +
  "process; on macOS/BSD the upgrade cannot drain input while the runner is attached).";

/** The actions for one pane, given what the runner says it can do. */
export function holderActions(
  attention: Pick<HolderAttention, "upgradeSupported" | "endAndResumeAvailable"> & {
    upgradeUnsupportedReason?: string | null;
  },
): HolderActions {
  const upgrade = attention.upgradeSupported;
  const endAndResume = attention.endAndResumeAvailable;
  // The runner's own reason (Windows ConPTY, or a non-Linux Unix that cannot
  // drain input while attached) when it gives one.
  const why = attention.upgradeUnsupportedReason
    ? `Upgrade holder is not offered: ${attention.upgradeUnsupportedReason}.`
    : UPGRADE_UNSUPPORTED_NOTE;
  let note: string | null = null;
  if (!upgrade && !endAndResume) {
    note = `${why} ${END_AND_RESUME_MISSING_NOTE}`;
  } else if (!upgrade) {
    note = why;
  }
  return { upgrade, leaveRunning: true, endAndResume, note };
}

/** Normal census cadence while something needs (or may need) a decision. */
export const CENSUS_REFRESH_MS = 60_000;
/** Backed-off cadence once a census came back empty and error-free. */
export const CENSUS_IDLE_REFRESH_MS = 300_000;

/**
 * When to take the census next. A census is a connect + hello per holder pane,
 * so it backs off while the last one read cleanly with nothing to decide; any
 * listed pane, an unreadable directory, a failed read, or no answer yet keeps
 * the normal cadence.
 */
export function nextCensusDelayMs(
  attention: Pick<HolderAttention, "panes" | "censusError"> | null,
  readError: string | null,
): number {
  if (attention === null || readError !== null || attention.censusError !== null) {
    return CENSUS_REFRESH_MS;
  }
  return attention.panes.length === 0 ? CENSUS_IDLE_REFRESH_MS : CENSUS_REFRESH_MS;
}

/** One line on why the row is listed. */
export function attentionLabel(pane: AttentionPane, runnerBuild: string): string {
  if (pane.reason === "incompatible") {
    const versions = pane.holderVersions.length
      ? ` (speaks protocol ${pane.holderVersions.join(", ")})`
      : "";
    return `Incompatible holder${versions} — this runner cannot drive it`;
  }
  return `Older holder build ${pane.holderBuild ?? "(unknown)"} — this runner ships ${runnerBuild}`;
}

export type OutcomeTone = "ok" | "warn" | "error";

/** How an outcome is shown: a tone and the operator-facing sentence. */
export function describeOutcome(outcome: HolderUpgradeOutcome): {
  tone: OutcomeTone;
  text: string;
} {
  switch (outcome.kind) {
    case "upgraded": {
      const build = outcome.successorBuild;
      let buildText: string;
      if (build === null) {
        buildText = " Its build could not be read back yet (unknown).";
      } else if (outcome.previousBuild !== null && build === outcome.previousBuild) {
        buildText = ` It runs the SAME build (${build}): the holder executable at its path was not replaced, so nothing newer was picked up.`;
      } else {
        buildText = ` It runs ${build}.`;
      }
      return {
        tone: build !== null && build === outcome.previousBuild ? "warn" : "ok",
        text: `Upgraded: holder pid ${outcome.successorPid} now serves the pane — same program, same output stream.${buildText}`,
      };
    }
    case "rolled_back":
      return {
        tone: "warn",
        text: `Rolled back — the existing holder keeps serving the pane: ${outcome.reason}`,
      };
    case "failed":
      return { tone: "error", text: `Failed — nothing changed: ${outcome.reason}` };
    case "unsupported":
      return { tone: "warn", text: `Not supported for this holder: ${outcome.reason}` };
    case "unknown_on_timeout": {
      const now =
        outcome.holderPidNow === null
          ? "No holder answered for the pane afterwards."
          : `Holder pid ${outcome.holderPidNow} answers for the pane now.`;
      return { tone: "error", text: `Outcome UNKNOWN: ${outcome.reason}. ${now}` };
    }
    case "refused":
      return { tone: "warn", text: `Not attempted: ${outcome.reason}` };
  }
}

/** The banner's headline. With nothing listed it names why it is shown. */
export function bannerHeadline(paneCount: number, censusUnreadable: boolean): string {
  if (paneCount === 1) return "1 pane's PTY holder needs a decision";
  if (paneCount > 1) return `${paneCount} panes' PTY holders need a decision`;
  return censusUnreadable ? "PTY holder census unavailable" : "PTY holder upgrade";
}

/** The text of a command that threw instead of answering. */
export function invokeErrorText(err: unknown): string {
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  try {
    return JSON.stringify(err);
  } catch {
    return String(err);
  }
}
