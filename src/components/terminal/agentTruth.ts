/**
 * The runner's merged agent-state verdict, as the webview sees it (plan
 * `2026-09-20-terminal-session-state-comes-from-events-not-screen-scraping`,
 * Phases 4-5).
 *
 * The merge itself lives in ONE place — the Rust `agent_truth` reducer, one
 * slot per terminal. This module is the TypeScript mirror of its wire shape
 * (the `terminal-agent-state` Tauri event and the `get_terminal_agent_states`
 * command) plus the pure projections the Terminal page renders from it.
 *
 * It is also the ONLY module under `src/components/terminal` allowed to
 * compare a state against `"needs-input"` directly. Everything else asks
 * through {@link isNeedsInputState} (display) or
 * {@link isAuthoritativePermissionAsk} (anything that TYPES into a pane) — a
 * source-scan test (`agentTruth.sourceScan.test.ts`) enforces it, so a new
 * keystroke writer cannot forget the confidence check.
 */

import { invoke } from "@tauri-apps/api/core";
import type { SessionState } from "./useZoneLayout";

// ── Wire types (serde shape of `agent_truth::*`) ────────────────────────────

export type NeedsYouReason = "permission" | "question" | "elicitation" | "idle_prompt" | "unspecified";

export type FailureKind =
  | "rate_limited"
  | "quota_exhausted"
  | "context_exhausted"
  | "auth_required"
  | "overloaded"
  | "transport_lost"
  | "provider_error"
  | "unknown";

/** Internally tagged by `name`. */
export type AgentState =
  | { name: "unknown" }
  | { name: "starting" }
  | { name: "working" }
  | { name: "needs_you"; reason: NeedsYouReason }
  | { name: "turn_ended" }
  | { name: "failed"; kind: FailureKind }
  | { name: "ended"; why: string };

/** Precedence, highest first. */
export type Source =
  | "hook"
  | "sideband"
  | "statusline"
  | "transcript"
  | "screen_stability"
  | "regex";

export type Confidence = "authoritative" | "inferred" | "fallback";

export type Disagreement =
  | { kind: "contradiction"; higher: Source; lower: Source; lowerState: AgentState }
  | { kind: "quiet_while_working"; gridIdleSinceMs: number };

export interface Verdict {
  state: AgentState;
  source: Source | null;
  sinceMs: number | null;
  confidence: Confidence | null;
  disagreement: Disagreement | null;
}

export type HookDeliveryStatus = "installed" | "shadowed" | "version_mismatch" | "absent" | "unknown";

export interface HookDelivery {
  status: HookDeliveryStatus;
  detail?: string;
}

/** Payload of the `terminal-agent-state` Tauri event. */
export interface TerminalAgentStateEvent {
  terminalId: string;
  /**
   * The terminal's publish sequence number, strictly increasing per terminal.
   * On an event: this publish's. On a `get_terminal_agent_states` row: the
   * last publish's when the row was read (the row is at least that new).
   * Absent on a runner build that predates it.
   */
  seq?: number;
  verdict: Verdict;
  hookDelivery: HookDelivery;
}

export interface LastSeenAgeMs {
  hook: number | null;
  sideband: number | null;
  statusline: number | null;
  transcript: number | null;
  screen_stability: number | null;
  regex: number | null;
}

/** One row of `get_terminal_agent_states`. */
export interface TerminalAgentStateRow extends TerminalAgentStateEvent {
  lastSeenAgeMs?: LastSeenAgeMs;
}

/** What the page keeps per terminal: the verdict and how hooks are faring. */
export interface AgentTruthEntry {
  verdict: Verdict;
  hookDelivery: HookDelivery;
}

export const TERMINAL_AGENT_STATE_EVENT = "terminal-agent-state";

// ── Selectors ────────────────────────────────────────────────────────────────

/**
 * THE keystroke gate. True only when the runner's verdict is a permission ask
 * that an EVENT reported (`confidence: "authoritative"`). Every writer that
 * types an approval into a pane — bulk or automatic — acts only when this is
 * true. A regex-inferred needs-input is never enough: a false positive is a
 * keystroke typed into an agent that was not asking.
 */
export function isAuthoritativePermissionAsk(verdict: Verdict | null | undefined): boolean {
  if (!verdict) return false;
  return (
    verdict.state.name === "needs_you" &&
    verdict.state.reason === "permission" &&
    verdict.confidence === "authoritative"
  );
}

/**
 * Display-only predicate: is this (rendered) state the needs-input chip?
 * Accepts any string so filter values and loosely typed maps can use it too.
 * Never use this to decide whether to TYPE into a pane — that is
 * {@link isAuthoritativePermissionAsk}.
 */
export function isNeedsInputState(state: string | null | undefined): boolean {
  return state === "needs-input";
}

/**
 * True when the verdict came from an event channel (hooks or the OSC 9999
 * self-report) — for such a tab the webview's screen regex is NOT run.
 */
export function isEventSourced(verdict: Verdict | null | undefined): boolean {
  return verdict?.source === "hook" || verdict?.source === "sideband";
}

/**
 * True when what the page shows for this tab is inferred rather than reported
 * by an event: no verdict at all, or one whose confidence is not
 * authoritative. Drives the "inferred" affordance.
 */
export function isInferredVerdict(verdict: Verdict | null | undefined): boolean {
  return !verdict || verdict.confidence !== "authoritative";
}

/**
 * Tooltip for a state chip: the state, plus how it is known. The "inferred"
 * affordance every chip carries when no event reported the state.
 */
export function stateChipTitle(state: SessionState, entry: AgentTruthEntry | null | undefined): string {
  if (state === "unknown") return "State unknown — nothing observed yet";
  return isInferredVerdict(entry?.verdict)
    ? `${state} (inferred from screen)`
    : `${state} (reported by ${entry?.verdict.source ?? "event"})`;
}

/** Why a bulk writer skipped a needs-input pane, for its result text. */
export const SKIPPED_INFERRED_REASON =
  "inferred from screen, not reported by a hook — approve it from the pane itself";

/**
 * Split a bulk command's candidate panes into those an event says are asking
 * for permission (safe to type into) and those that only LOOK like they are.
 */
export function partitionPermissionAsks<T extends { id: string }>(
  tabs: readonly T[],
  sessionStates: Readonly<Record<string, string | undefined>>,
  verdicts: Readonly<Record<string, AgentTruthEntry | undefined>>,
): { actionable: T[]; skippedInferred: T[] } {
  const actionable: T[] = [];
  const skippedInferred: T[] = [];
  for (const t of tabs) {
    if (isAuthoritativePermissionAsk(verdicts[t.id]?.verdict)) {
      actionable.push(t);
    } else if (isNeedsInputState(sessionStates[t.id])) {
      skippedInferred.push(t);
    }
  }
  return { actionable, skippedInferred };
}

/**
 * One line for a bulk command's result naming the panes it skipped because
 * their needs-input was only inferred. Empty when nothing was skipped.
 */
export function describeSkippedInferred(skipped: readonly { id: string; title?: string }[]): string {
  if (skipped.length === 0) return "";
  const names = skipped.map((t) => t.title || t.id).join(", ");
  return `skipped ${skipped.length} inferred (${names}): ${SKIPPED_INFERRED_REASON}`;
}

/**
 * Pure projection of a runner verdict onto the chip vocabulary. `unknown`
 * stays `unknown` — never `idle`.
 */
export function verdictToSessionState(verdict: Verdict): SessionState {
  switch (verdict.state.name) {
    case "unknown":
      return "unknown";
    case "starting":
    case "working":
      return "working";
    case "needs_you":
      return "needs-input";
    case "turn_ended":
      return "idle";
    case "failed":
      return "error";
    case "ended":
      return "completed";
  }
}

// ── Ordering and precedence of what the runner sends ────────────────────────

/** Where a verdict reached the webview from. */
export type AgentStateOrigin = "event" | "row";

/**
 * Is an incoming verdict newer than the one held for its terminal?
 *
 * `held` is the highest `seq` already applied. An EVENT is newer only with a
 * strictly higher `seq` (an equal one is a duplicate, or older than a row
 * read after that publish). A ROW (the initial `get_terminal_agent_states`
 * snapshot) carries the seq of the last publish BEFORE it was read, so it is
 * at least as new as that publish: it applies at an equal `seq`, and loses to
 * any event with a higher one — the snapshot that resolves after a live event
 * must not overwrite it. A runner build that sends no `seq` is applied as
 * before (no ordering to go on).
 */
export function isNewerAgentState(
  held: number | undefined,
  incoming: number | undefined,
  origin: AgentStateOrigin,
): boolean {
  if (typeof incoming !== "number" || typeof held !== "number") return true;
  return origin === "row" ? incoming >= held : incoming > held;
}

/**
 * May this runner verdict overwrite the chip the webview derived locally?
 *
 * A verdict whose source is `regex` or `screen_stability` is the runner
 * ECHOING the webview's own fallback offer back — it knows nothing the
 * webview did not, and is older than what the webview has derived since
 * (`completed` on process exit, an `idle` from the quiet sweep). It may only
 * seed a tab that has no local state yet. A verdict from an event channel or
 * any runner-side source (`hook`, `sideband`, `statusline`, `transcript`)
 * always overrides.
 */
export function verdictOverridesLocalState(
  verdict: Verdict,
  local: SessionState | undefined,
): boolean {
  const echo = verdict.source === "regex" || verdict.source === "screen_stability";
  if (!echo) return true;
  return local === undefined || local === "unknown";
}

// ── Offering observations to the runner ─────────────────────────────────────

export type ObservationState =
  | "working"
  | "approval_shaped"
  | "question_shaped"
  | "completed"
  | "error"
  | "idle";

export interface AgentObservation {
  terminalId: string;
  source: "regex" | "screen_stability";
  state?: ObservationState;
  busy?: boolean;
}

/** Map a locally inferred chip state onto the observation vocabulary. */
export function sessionStateToObservation(
  state: SessionState,
  needsInputShape: "approval_shaped" | "question_shaped" = "question_shaped",
): ObservationState | null {
  switch (state) {
    case "working":
      return "working";
    case "needs-input":
      return needsInputShape;
    case "completed":
      return "completed";
    case "error":
      return "error";
    case "idle":
      return "idle";
    case "unknown":
      return null;
  }
}

/**
 * Offer a fallback observation to the runner's reducer. Fire-and-forget: a
 * runner build without the command (or any IPC failure) leaves the webview's
 * own fallback rendering in place, so the rejection is swallowed.
 */
export function offerAgentObservation(observation: AgentObservation): void {
  try {
    void invoke("offer_agent_observation", { ...observation }).catch(() => {});
  } catch {
    // No Tauri bridge (tests, a plain browser) — nothing to offer to.
  }
}

/** Initial load / reload of every terminal's verdict. `[]` on any failure. */
export async function fetchTerminalAgentStates(): Promise<TerminalAgentStateRow[]> {
  try {
    const rows = await invoke<TerminalAgentStateRow[]>("get_terminal_agent_states");
    return Array.isArray(rows) ? rows : [];
  } catch {
    return [];
  }
}

/** Shape guard for an event payload arriving over IPC. */
export function isTerminalAgentStateEvent(value: unknown): value is TerminalAgentStateEvent {
  if (!value || typeof value !== "object") return false;
  const v = value as Partial<TerminalAgentStateEvent>;
  return (
    typeof v.terminalId === "string" &&
    !!v.verdict &&
    typeof v.verdict === "object" &&
    !!v.verdict.state &&
    typeof v.verdict.state.name === "string"
  );
}

// ── Phase 4: what the session-info panel says about the state source ─────────

const HOOK_DELIVERY_REASON: Record<HookDeliveryStatus, string> = {
  installed: "installed, but no event has reported yet",
  shadowed: "shadowed by settings",
  version_mismatch: "CLI version not probed",
  absent: "hooks absent",
  unknown: "delivery unknown",
};

/**
 * The "State source" line of the session-info dropdown. `null` when the runner
 * has told us nothing about this terminal (build predates the verdict) — the
 * row then renders as unknown, never as a guess.
 */
export function stateSourceText(entry: AgentTruthEntry | null | undefined): string | null {
  if (!entry) return null;
  const { verdict, hookDelivery } = entry;
  if (verdict.source === "hook") return "hooks";
  if (verdict.source === "sideband") return "agent self-report (OSC 9999)";
  const base = HOOK_DELIVERY_REASON[hookDelivery.status] ?? "delivery unknown";
  const reason = hookDelivery.detail ? `${base}: ${hookDelivery.detail}` : base;
  const what =
    verdict.source === null
      ? "nothing observed yet"
      : verdict.source === "regex" || verdict.source === "screen_stability"
        ? "inferred from screen"
        : `inferred from ${verdict.source}`;
  return `${what} (hooks not firing: ${reason})`;
}
