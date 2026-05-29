/**
 * Suggestion-chip rule definitions for Phase 4.
 *
 * Each rule is a pure function over a `SuggestionContext` that returns
 * zero or more `ChipCandidate`s. The engine (`useSuggestions`) calls
 * every rule on each render, applies the volume cap + per-zone dedupe
 * + suppression filter, and hands the survivors to the per-zone
 * `<SuggestionChip>` mounts inside `ZoneGrid`.
 *
 * The 3 rules shipped in this phase cover the three lowest-state-
 * complexity entries from the plan's §Phase-4 rule list:
 *
 *   - `error-in-zone` — most actionable; no time gate.
 *   - `stuck-needs-input` — urgency signal; 30s gate using
 *     `stateEntryTimeRef` from `useSessionStateTracking`.
 *   - `layout-mismatch` — low-urgency hint when sessions outnumber
 *     visible zones by more than 20%.
 *
 * Rule priority (highest = wins when one zone has multiple candidates):
 *   error → stuck-needs-input → layout-mismatch.
 *
 * Deferred for follow-up:
 *   - `lock-contention` (plan flags overlap with `HoldingLockBanner`;
 *     not shipping both — banner removal lives in Phase 9 demolition).
 *   - `idle-profile-prompt` (needs profile-matching heuristics from
 *     `ZoneProfilePicker`; out of scope until the profile surface
 *     exposes a match query).
 */

import type { SessionState, ZoneAssignments } from "../useZoneLayout";

/** Read-only snapshot of every piece of state the rules can branch on. */
export interface SuggestionContext {
  /** Now, in epoch ms — passed in (not read internally) so the engine's
   *  re-evaluation cadence is explicit and rule tests stay deterministic. */
  nowMs: number;
  /** `tabs` length is the only field used; full list available for
   *  future rules. */
  tabsCount: number;
  /** Zone count from the current layout (`zoneLayout.layout.zones.length`). */
  zoneCount: number;
  /** Layout id (`single`, `quad`, `six-pack`, ...) — used to detect when
   *  the suggested target preset would actually change anything. */
  currentLayoutId: string;
  /** Zone-index → tab-id mapping. */
  assignments: ZoneAssignments;
  /** Per-tab session state. */
  sessionStates: Record<string, SessionState>;
  /** Per-tab epoch-ms when the *current* state was first observed. */
  stateEntryMs: Record<string, number>;
  /** The zone the operator is currently looking at. Used as the host
   *  zone for non-tab-bound chips like `layout-mismatch`. */
  focusedZone: number;
}

/** A single candidate chip emitted by a rule. */
export interface ChipCandidate {
  /** Stable rule id, e.g. `"stuck-needs-input"`. */
  ruleId: string;
  /** Zone index (0-based) the chip should render inside. */
  zoneIdx: number;
  /** Per-rule priority — higher wins when multiple rules fire on the
   *  same zone. Set by the rule, not the operator. */
  priority: number;
  /** Short headline shown in the chip body. */
  headline: string;
  /** Slash equivalent rendered inline (passive teaching surface
   *  per the plan's §3 item 2). */
  slash: string;
  /** Registry action id to invoke when the chip is clicked. */
  actionId: string;
  /** Args passed to the action handler. */
  args: Record<string, unknown>;
}

/** Pick the smallest layout preset that fits N tabs. Mirrors the helper
 *  in `TerminalPage.tsx` (intentionally duplicated — both ride on the
 *  same `LAYOUT_PRESETS` definition; collapsing into one shared helper
 *  is a Phase 9 cleanup). */
function pickLayout(totalTabs: number): string {
  if (totalTabs >= 7) return "full-grid";
  if (totalTabs >= 5) return "six-pack";
  if (totalTabs >= 3) return "quad";
  if (totalTabs >= 2) return "split";
  return "single";
}

/**
 * Pretty-name a layout preset for the chip headline.
 */
function prettyLayoutName(preset: string): string {
  switch (preset) {
    case "full-grid":
      return "Full grid";
    case "six-pack":
      return "6-pack";
    case "quad":
      return "Quad";
    case "split":
      return "Split";
    case "single":
      return "Single";
    default:
      return preset;
  }
}

const ELAPSED_NEEDS_INPUT_MS = 30_000;
const LAYOUT_OVERFLOW_FACTOR = 1.2;

/**
 * Rule: error-in-zone.
 *
 * Trigger: any tab whose `sessionStates[tab.id] === "error"`. One chip
 * per errored zone. Priority 100 — wins over stuck-needs-input and
 * layout-mismatch on the same zone.
 */
export function ruleErrorInZone(ctx: SuggestionContext): ChipCandidate[] {
  const out: ChipCandidate[] = [];
  for (const [zoneStr, tabId] of Object.entries(ctx.assignments)) {
    const state = ctx.sessionStates[tabId];
    if (state !== "error") continue;
    const zoneIdx = Number(zoneStr);
    out.push({
      ruleId: "error-in-zone",
      zoneIdx,
      priority: 100,
      headline: "Restart? Session errored",
      slash: `/restart ${zoneIdx + 1}`,
      actionId: "terminal.restart",
      args: { zone: zoneIdx + 1 },
    });
  }
  return out;
}

/**
 * Rule: stuck-needs-input.
 *
 * Trigger: tab in `needs-input` state for more than {@link
 * ELAPSED_NEEDS_INPUT_MS}. One chip per stuck zone. Priority 70.
 *
 * Time gate uses `stateEntryMs` (from `useSessionStateTracking`'s
 * `stateEntryTimeRef`) — the engine re-evaluates on a 5s tick so
 * `(nowMs - stateEntryMs[tabId]) > 30s` becomes true within at most
 * 5s of the actual 30s mark.
 */
export function ruleStuckNeedsInput(ctx: SuggestionContext): ChipCandidate[] {
  const out: ChipCandidate[] = [];
  for (const [zoneStr, tabId] of Object.entries(ctx.assignments)) {
    if (ctx.sessionStates[tabId] !== "needs-input") continue;
    const since = ctx.stateEntryMs[tabId];
    if (typeof since !== "number") continue;
    const elapsedMs = ctx.nowMs - since;
    if (elapsedMs < ELAPSED_NEEDS_INPUT_MS) continue;
    const zoneIdx = Number(zoneStr);
    const elapsedSec = Math.floor(elapsedMs / 1000);
    const human =
      elapsedSec < 60
        ? `${elapsedSec}s`
        : elapsedSec < 3600
          ? `${Math.floor(elapsedSec / 60)}m`
          : `${Math.floor(elapsedSec / 3600)}h`;
    out.push({
      ruleId: "stuck-needs-input",
      zoneIdx,
      priority: 70,
      headline: `Respond — needs input for ${human}`,
      slash: `/focus ${zoneIdx + 1}`,
      actionId: "terminal.focus",
      args: { zone: zoneIdx + 1 },
    });
  }
  return out;
}

/**
 * Rule: layout-mismatch.
 *
 * Trigger: `tabsCount > zoneCount * LAYOUT_OVERFLOW_FACTOR` AND the
 * preset that would fit `tabsCount` differs from the current preset.
 * Surfaces on `focusedZone` (operator's current attention). Priority 30
 * — yields to error/stuck chips on the same zone.
 */
export function ruleLayoutMismatch(ctx: SuggestionContext): ChipCandidate[] {
  if (ctx.zoneCount === 0) return [];
  if (ctx.tabsCount <= ctx.zoneCount * LAYOUT_OVERFLOW_FACTOR) return [];
  const suggested = pickLayout(ctx.tabsCount);
  if (suggested === ctx.currentLayoutId) return [];
  return [
    {
      ruleId: "layout-mismatch",
      zoneIdx: ctx.focusedZone,
      priority: 30,
      headline: `Switch to ${prettyLayoutName(suggested)} — show all ${ctx.tabsCount} sessions`,
      slash: `/layout ${suggested}`,
      actionId: "terminal.layout",
      args: { preset: suggested },
    },
  ];
}

/** Master rule list — append-only. Iteration order is irrelevant; the
 *  engine reconciles by zone + priority. */
export const RULES: Array<(ctx: SuggestionContext) => ChipCandidate[]> = [
  ruleErrorInZone,
  ruleStuckNeedsInput,
  ruleLayoutMismatch,
];

/**
 * Reconcile every rule's candidates into the final per-zone chip list:
 *
 *   1. Run each rule, collect every candidate.
 *   2. Group by `zoneIdx`; within a zone, keep only the highest-priority
 *      candidate (per plan §5(6) — 1 chip per zone max).
 *   3. Filter out suppressed `(zoneIdx, ruleId)` pairs.
 *   4. Sort surviving chips by priority desc and cap at 3 (page max).
 *   5. Return the final list as a `Map<zoneIdx, ChipCandidate>`.
 *
 * Pure function — easy to test without React. The hook
 * (`useSuggestions`) is just a tick + context-read wrapper around this.
 */
export function reconcileChips(
  ctx: SuggestionContext,
  suppressed: ReadonlySet<string>,
  maxOnPage = 3,
): Map<number, ChipCandidate> {
  const byZone = new Map<number, ChipCandidate>();
  for (const rule of RULES) {
    for (const candidate of rule(ctx)) {
      const suppressKey = `${candidate.zoneIdx}:${candidate.ruleId}`;
      if (suppressed.has(suppressKey)) continue;
      const existing = byZone.get(candidate.zoneIdx);
      if (!existing || existing.priority < candidate.priority) {
        byZone.set(candidate.zoneIdx, candidate);
      }
    }
  }
  // Cap at maxOnPage — drop the lowest-priority chips first.
  if (byZone.size <= maxOnPage) return byZone;
  const sorted = [...byZone.entries()].sort(
    (a, b) => b[1].priority - a[1].priority,
  );
  return new Map(sorted.slice(0, maxOnPage));
}
