/**
 * The decision core of the session-state automations, as a pure function.
 *
 * `useStateTransitionEffects` diffs `sessionStates` against the previous
 * snapshot and, from that diff, decides five things: which tabs just started
 * flashing, which just errored, which just completed, which should be
 * auto-approved, and which zones should be auto-restarted. All five are
 * decisions about DATA. None of them needs React, xterm or Tauri — but until
 * now they lived inside a `useEffect`, interleaved with the side effects they
 * imply, so none of them could be tested.
 *
 * ## Why this is a module and not a hook
 *
 * `vitest.config.ts` sets `environment: "node"`, so a hook cannot be rendered
 * at all — a React or Tauri import makes the logic permanently untestable.
 * Splitting the decision out is what makes the matrix below reachable: pattern
 * matching (including the invalid-regex swallow), exit-code gating for
 * restarts, transition edge detection, and the bypass-session interaction.
 * Same rationale as `outputLineTracking.ts`, `activityDigestTracking.ts`,
 * `scrollbackReplay.ts` and `terminalOutputTap.ts`.
 *
 * ## The purity rule is load-bearing, not stylistic
 *
 * **{@link evaluateTransitions} MUST NOT advance `prev`.** Advancing the
 * previous-state snapshot is the caller's job, and *exactly one* caller may do
 * it. The plan this module comes from
 * (`2026-07-22-runner-cross-page-session-automations`) moves the automations
 * out of the single active-page tree and into every `PageSessionScope`, and
 * the load-bearing constraint there is that the scope becomes the sole owner
 * of the diff: scope effects flush BEFORE the gated `children` tree's effects,
 * so if both the scope and the active page's hook diffed *and advanced* the
 * same ref, the second one would see `prev === next`, detect zero transitions,
 * and silently kill flashing, auto-focus, sounds and the window-title counts on
 * the page the operator is actually looking at.
 *
 * A function that returns edges and mutates nothing is what makes that
 * single-advance invariant expressible. Keep it that way: if this ever needs to
 * write, it needs a different name and a different home.
 *
 * ## What it deliberately does NOT decide
 *
 * Delivery. `approvals` names the tabs whose output matched a pattern — it is
 * an intent, not an outcome. Whether the `y\r` actually reached a PTY is
 * `deliverApprovals`' answer (`approveAll.ts`), which awaits a
 * `TerminalWriteResult` per pane and counts only what came back `success`.
 * That split matters most for exactly the sessions this plan targets: an
 * automated approval fires at a pane nobody is watching, so an unobserved write
 * is indistinguishable from a delivered one, and "the operator can retry" is
 * false by construction.
 */

import type { SessionState } from "./useZoneLayout";

/** How many trailing lines an approval pattern is matched against. */
export const APPROVAL_MATCH_LINES = 5;

/** The minimum shape of a tab this module needs. */
export interface TransitionTab {
  id: string;
  title: string;
  exitCode?: number | null;
}

export interface EvaluateTransitionsInput {
  /** The previous snapshot. NEVER mutated. */
  prev: Readonly<Record<string, SessionState>>;
  /** The current snapshot being diffed against `prev`. */
  next: Readonly<Record<string, SessionState>>;
  tabs: readonly TransitionTab[];
  /** zone index -> tabId. */
  assignments: Readonly<Record<number, string>>;
  /** Operator-configured auto-approve patterns, as regex sources. */
  autoApprovePatterns: readonly string[];
  /** Whether auto-restart is armed. */
  autoRestart: boolean;
  /**
   * Lazy reader for a tab's last rendered output lines. Called ONLY for tabs
   * that just transitioned to `needs-input`, and only when at least one
   * approval pattern is configured — so a page with the feature off pays
   * nothing.
   */
  getLastOutputLines: (tabId: string) => readonly string[];
}

/** One tab's state change, in the order it was observed. */
export interface StateChange {
  tabId: string;
  /** `undefined` when this tab had no previous state (first observation). */
  from: SessionState | undefined;
  to: SessionState;
  /** The zone this tab is assigned to, or `undefined` when unassigned. */
  zoneIdx: number | undefined;
  /** The tab's title, or the id when the tab is not in `tabs`. */
  title: string;
}

/** A zone that should be auto-restarted. */
export interface RestartIntent {
  zoneIdx: number;
  tabId: string;
  title: string;
}

export interface TransitionOutcome {
  /** Tabs that just entered `needs-input`. */
  newNeedsInput: string[];
  /** Tabs that just entered `error`. */
  newErrors: string[];
  /** Tabs that just entered `completed`. */
  newCompleted: string[];
  /**
   * Tabs whose trailing output matched an approval pattern. An INTENT to
   * approve — delivery is `deliverApprovals`' business, and its envelope is
   * what may be counted.
   */
  approvals: string[];
  /** Zones to auto-restart, in observation order. */
  restarts: RestartIntent[];
  /** Every observed state change, for history logging and dwell-time accounting. */
  stateChanges: StateChange[];
}

/**
 * Does any pattern match the tab's trailing output?
 *
 * Patterns are operator-authored regex sources and are matched
 * case-insensitively against the last {@link APPROVAL_MATCH_LINES} lines
 * joined by newlines. **An invalid pattern matches nothing rather than
 * throwing** — one malformed entry must not take the whole automation down,
 * and a throw inside the transition effect would do exactly that.
 */
export function matchesApprovalPattern(
  lines: readonly string[],
  patterns: readonly string[],
): boolean {
  if (patterns.length === 0) return false;
  const haystack = lines.slice(-APPROVAL_MATCH_LINES).join("\n");
  return patterns.some((pattern) => {
    try {
      return new RegExp(pattern, "i").test(haystack);
    } catch {
      return false;
    }
  });
}

/**
 * Is this tab eligible for auto-restart?
 *
 * A clean exit only. `exitCode` `null` / `undefined` counts as clean because a
 * session that ended without reporting a code is the ordinary shape for an
 * interactive shell — the alternative would be to never restart those, which
 * is the common case. A non-zero code is a failure the operator should see,
 * so it is deliberately NOT restarted; `error`-state panes are excluded by the
 * caller's state predicate for the same reason.
 */
export function isRestartable(tab: TransitionTab | undefined): boolean {
  if (!tab) return false;
  return tab.exitCode === 0 || tab.exitCode === null || tab.exitCode === undefined;
}

/** zone index for a tabId, or `undefined` when the tab is unassigned. */
function zoneOf(assignments: Readonly<Record<number, string>>, tabId: string): number | undefined {
  for (const [zone, id] of Object.entries(assignments)) {
    if (id === tabId) return Number(zone);
  }
  return undefined;
}

/**
 * Diff `prev` against `next` and return every edge the automations act on.
 *
 * Pure: mutates neither argument, performs no I/O, and — see the module
 * header — deliberately does NOT advance `prev`.
 */
export function evaluateTransitions(input: EvaluateTransitionsInput): TransitionOutcome {
  const {
    prev,
    next,
    tabs,
    assignments,
    autoApprovePatterns,
    autoRestart,
    getLastOutputLines,
  } = input;

  const newNeedsInput: string[] = [];
  const newErrors: string[] = [];
  const newCompleted: string[] = [];
  const restarts: RestartIntent[] = [];
  const stateChanges: StateChange[] = [];

  const tabById = new Map(tabs.map((t) => [t.id, t]));

  for (const [tabId, state] of Object.entries(next)) {
    const before = prev[tabId];
    if (before !== state) {
      const tab = tabById.get(tabId);
      const zoneIdx = zoneOf(assignments, tabId);
      stateChanges.push({
        tabId,
        from: before,
        to: state,
        zoneIdx,
        title: tab?.title ?? tabId,
      });

      // Auto-restart is armed on the completed edge only, and only for a zone
      // the tab actually occupies — a restart is a ZONE operation, so an
      // unassigned tab has nowhere to be restarted into.
      if (
        state === "completed" &&
        autoRestart &&
        zoneIdx !== undefined &&
        isRestartable(tab)
      ) {
        restarts.push({ zoneIdx, tabId, title: tab?.title ?? tabId });
      }
    }

    if (state === "needs-input" && before !== "needs-input") newNeedsInput.push(tabId);
    if (state === "error" && before !== "error") newErrors.push(tabId);
    if (state === "completed" && before !== "completed") newCompleted.push(tabId);
  }

  // Auto-approve is evaluated only on the needs-input edge, and the output
  // reader is called only when a pattern could match — `getLastOutputLines` is
  // a hot-store read per tab per frame otherwise.
  const approvals: string[] = [];
  if (autoApprovePatterns.length > 0) {
    for (const tabId of newNeedsInput) {
      if (matchesApprovalPattern(getLastOutputLines(tabId), autoApprovePatterns)) {
        approvals.push(tabId);
      }
    }
  }

  return { newNeedsInput, newErrors, newCompleted, approvals, restarts, stateChanges };
}
