/**
 * Synthetic-tab derivation for the debug-gated test-fixtures seam.
 *
 * A *tab-backed* injected fake (produced only by the runner's cfg-gated
 * `/ui-bridge/test/inject-session` route) carries an `injected_tab` spec on its
 * transcript-session record. This module turns each such spec into a minimal
 * `TerminalTab` that is fed THROUGH the real `useSessionManager` /
 * `useSessionStateTracking` correlation path — the only mechanism that can
 * place a fake into the `idle` (pre-aged stale tab) or tab-backed
 * `error` / `completed` (dead-tab exit-code) StatusStrip buckets, which the
 * `injected_live_status` short-circuit cannot reach.
 *
 * No build-flag gating is needed: the function is data-driven. `injected_tab`
 * can only be non-null when the cfg-gated Rust seam produced it, so in a
 * release build without `test-fixtures` this path is dead.
 *
 * Synthetic tabs are pure StatusStrip bucketing inputs. They are NEVER passed
 * to the terminal-rendering / zone-layout / file-lock / keyboard consumers, so
 * they never back a real PTY pane (see `TerminalSessionContext.tsx` wiring).
 */

import type { TerminalTab } from "./useTerminalManager";
import type { TranscriptSession } from "./useTranscriptSessions";

/** Prefix for synthetic tab ids so they're greppable and collision-free. */
const SYNTHETIC_TAB_ID_PREFIX = "synthetic-";

/**
 * True when `tabOrId` refers to a synthetic (debug test-fixture) tab.
 *
 * Accepts either a `TerminalTab` object or a bare tab-id string:
 *   - Object form (preferred — call this when the tab is in scope): keys on the
 *     authoritative `__synthetic` flag set by {@link deriveSyntheticTabs}.
 *   - String form (fallback — when only the id is in scope, e.g. a
 *     `UnifiedSession.zoneTabId` pointer): keys on the `synthetic-` id prefix,
 *     which `deriveSyntheticTabs` guarantees on every synthetic tab.
 *
 * Phase 3 / F1: session-level process-control operations (resume, promote,
 * commit) consult this to early-return a no-op against a synthetic-backed
 * target — a fake has no real PTY / ClaudeSession, so driving the action
 * through would either 404 in the backend or spawn a stray `claude --resume`
 * against a non-existent session id.
 */
export function isSyntheticTab(tabOrId: TerminalTab | string | null | undefined): boolean {
  if (tabOrId == null) return false;
  if (typeof tabOrId === "string") {
    return tabOrId.startsWith(SYNTHETIC_TAB_ID_PREFIX);
  }
  return tabOrId.__synthetic === true || tabOrId.id.startsWith(SYNTHETIC_TAB_ID_PREFIX);
}

/**
 * Minimal shape {@link isInjectedSession} needs from a session row: the
 * underlying transcript record plus the resolved tab pointer. Kept structural
 * (rather than importing `UnifiedSession`) so this stays a leaf module with no
 * cycle back into `useSessionManager`.
 */
export interface InjectedSessionProbe {
  /** Synthetic tab id (`synthetic-…`) for a tab-backed fake; else a real id or null. */
  zoneTabId: string | null;
  /** The transcript record this session was projected from. */
  _transcript: Pick<TranscriptSession, "injected_live_status" | "injected_tab">;
}

/**
 * True when a unified session row is a debug-gated injected fake — covering
 * BOTH projection modes:
 *
 *   - **short-circuit** fakes (working / needs-input / orphan-frozen): carry
 *     `_transcript.injected_live_status`, `zoneTabId == null`.
 *   - **tab-backed** fakes (idle / error / completed): carry
 *     `_transcript.injected_tab` and resolve to a synthetic `zoneTabId`.
 *
 * Real transcripts set neither field and never carry a `synthetic-` tab
 * pointer, so this is `false` for every production session.
 *
 * This is the guard predicate for session-level process-control ops that key
 * off a `UnifiedSession` (resume / promote / commit) rather than a `TerminalTab`.
 */
export function isInjectedSession(session: InjectedSessionProbe): boolean {
  return (
    session._transcript.injected_live_status != null ||
    session._transcript.injected_tab != null ||
    isSyntheticTab(session.zoneTabId)
  );
}

export interface DeriveSyntheticTabsResult {
  /** Minimal `TerminalTab`s, one per session carrying an `injected_tab`. */
  tabs: TerminalTab[];
  /**
   * Pre-age seed for `useSessionStateTracking`'s `lastOutputTimeRef`, keyed by
   * synthetic tab id. Present only for live (idle) tabs that carry a
   * `quiet_ms`: the value is `Date.now() - quiet_ms` so the existing 60s
   * staleness sweep classifies the tab stale on its next tick.
   */
  seedLastOutput: Record<string, number>;
}

/**
 * Derive synthetic tabs (and their pre-age seeds) from the transcript-session
 * list. Sessions without an `injected_tab` are ignored. Pure — `Date.now()` is
 * the only ambient read, evaluated once per call.
 */
export function deriveSyntheticTabs(
  transcriptSessions: readonly TranscriptSession[],
): DeriveSyntheticTabsResult {
  const now = Date.now();
  const tabs: TerminalTab[] = [];
  const seedLastOutput: Record<string, number> = {};

  for (const session of transcriptSessions) {
    const spec = session.injected_tab;
    if (!spec) continue;

    const tabId = `${SYNTHETIC_TAB_ID_PREFIX}${session.session_id}`;

    tabs.push({
      id: tabId,
      title: session.display_name,
      pid: null,
      isAlive: spec.is_alive,
      exitCode: spec.exit_code ?? null,
      // Correlate to the injected transcript so `tabSessionMap` (keyed by
      // claudeSessionId → tab) resolves this tab for the fake.
      claudeSessionId: session.session_id,
      __synthetic: true,
    });

    // A live (idle) tab pre-aged past the staleness threshold needs its
    // last-output timestamp seeded so the sweep can mark it stale. Dead tabs
    // (error/completed) carry no quiet_ms — the dead-tab sweep branch handles
    // them via exitCode instead.
    if (spec.is_alive && spec.quiet_ms != null) {
      seedLastOutput[tabId] = now - spec.quiet_ms;
    }
  }

  return { tabs, seedLastOutput };
}
