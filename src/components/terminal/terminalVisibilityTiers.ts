/**
 * Which service tier each terminal is entitled to, reconciled across this
 * window and pushed to the runner (plan
 * `2026-07-28-runner-many-sessions-performance` Phase 5, root cause A4 —
 * backend half).
 *
 * Phase 2 stopped a hidden pane from *rendering* what it could not show. It
 * could not stop the runner from SENDING it: Rust emitted every chunk of every
 * session app-wide, so 40 streaming sessions with one focused pane still cost
 * 40 sessions' worth of IPC deserialization per chunk in every window. This
 * module closes that half by telling the runner what each terminal is worth:
 *
 *  - `focused`    — a mounted, visible pane. Every coalesced frame, as today.
 *  - `background` — a mounted but hidden pane (parked by `HiddenTerminal`, a
 *                   non-maximized zone under single view, a compact zone
 *                   card). Nothing renders it, but the page-level output tap
 *                   still feeds its state chip and sparkline from the stream,
 *                   so emission continues — coalesced by the runner to ≥250 ms
 *                   flushes.
 *  - `unwatched`  — NO pane anywhere: virtualized off-screen in flow mode, on a
 *                   non-active terminal page, or in a window that isn't
 *                   showing it. The runner emits no `terminal-output` at all;
 *                   the scrollback ring keeps accumulating and the tab's state
 *                   tracking is fed by the ≤1 Hz `terminal-activity` digest
 *                   instead (see `TerminalSessionContext`'s tap).
 *
 * ## Why declarations rather than a derived classification
 *
 * The tier is derived from what actually MOUNTED, not from `classifyTabs`.
 * `classifyTabs` short-circuits every preset layout (≤9 tabs) to `"assigned"`
 * — all mounted, viewport proximity never consulted — so a classification-
 * driven tier would leave preset layouts with no tiering at all, which is the
 * exact trap Phase 2 documented when it keyed its pane tier on `visible`
 * alone. A pane declares itself; a tab nobody declared is `unwatched`. That is
 * true in every layout family, in every window, by construction.
 *
 * ## Rosters
 *
 * A declaration alone cannot produce `unwatched` — the absence of one has to.
 * So each `PageSessionScope` publishes the tab ids it owns, and the union of
 * the live rosters is the id set the reconciler considers. Without that, a tab
 * whose pane just unmounted would simply stop being mentioned and keep its old
 * tier forever.
 *
 * Leaf module (no React, no imports beyond the Tauri `invoke`) so vitest can
 * drive the whole reconciler without mounting a terminal.
 */

import { invoke } from "@tauri-apps/api/core";

/** Wire vocabulary, matching Rust's `terminal::visibility::VisibilityTier`. */
export type VisibilityTier = "focused" | "background" | "unwatched";

/** Most-visible-wins ordering; lower rank = more service. */
const TIER_RANK: Record<VisibilityTier, number> = {
  focused: 0,
  background: 1,
  unwatched: 2,
};

/** One pane's live declaration. Boxed so two panes declaring the same tier for
 *  the same terminal (a transient double mount during a zone move) count — and
 *  release — separately. */
interface DeclarationBox {
  tier: VisibilityTier;
}

/** terminalId → the panes currently declaring a tier for it. */
const declarations = new Map<string, Set<DeclarationBox>>();
/** scopeId → the tab ids that scope owns. */
const rosters = new Map<string, Set<string>>();
/** terminalId → the tier last successfully handed to the runner. */
const sent = new Map<string, VisibilityTier>();

let flushHandle: ReturnType<typeof setTimeout> | null = null;

/**
 * The effective tier for one terminal: the most visible live declaration, or
 * `unwatched` when nothing declares it.
 */
export function effectiveTier(terminalId: string): VisibilityTier {
  const boxes = declarations.get(terminalId);
  if (!boxes || boxes.size === 0) return "unwatched";
  let best: VisibilityTier = "unwatched";
  for (const box of boxes) {
    if (TIER_RANK[box.tier] < TIER_RANK[best]) best = box.tier;
  }
  return best;
}

/**
 * Declare that a mounted pane is showing (`focused`) or holding but not
 * showing (`background`) this terminal.
 *
 * Returns a handle whose `update` re-declares in place (a pane toggling
 * visible/hidden must not churn the registration) and whose `release` removes
 * the declaration on unmount, dropping the terminal to `unwatched` unless
 * another pane still declares it.
 */
export function declarePaneTier(
  terminalId: string,
  tier: Exclude<VisibilityTier, "unwatched">,
): { update: (next: Exclude<VisibilityTier, "unwatched">) => void; release: () => void } {
  let boxes = declarations.get(terminalId);
  if (!boxes) {
    boxes = new Set();
    declarations.set(terminalId, boxes);
  }
  const box: DeclarationBox = { tier };
  boxes.add(box);
  scheduleReconcile();

  let released = false;
  return {
    update: (next) => {
      if (released || box.tier === next) return;
      box.tier = next;
      scheduleReconcile();
    },
    release: () => {
      if (released) return;
      released = true;
      const live = declarations.get(terminalId);
      if (live) {
        live.delete(box);
        if (live.size === 0) declarations.delete(terminalId);
      }
      scheduleReconcile();
    },
  };
}

/**
 * Publish the tab ids one page scope owns. Call with an empty array (or
 * {@link releaseRoster}) when the scope unmounts.
 */
export function publishRoster(scopeId: string, ids: readonly string[]): void {
  const next = new Set(ids);
  const prev = rosters.get(scopeId);
  if (prev && prev.size === next.size && [...next].every((id) => prev.has(id))) return;
  rosters.set(scopeId, next);
  scheduleReconcile();
}

/** Drop a scope's roster (its page unmounted). */
export function releaseRoster(scopeId: string): void {
  if (!rosters.delete(scopeId)) return;
  scheduleReconcile();
}

/**
 * Batch reconciles to the end of the current task. One layout change (a
 * maximize, a scroll that re-virtualizes a row of zones, a page switch) fires
 * many declarations and roster updates in the same React commit; without the
 * batch each would be its own IPC round trip.
 */
function scheduleReconcile(): void {
  if (flushHandle !== null) return;
  flushHandle = setTimeout(reconcileNow, 0);
}

/**
 * Compute every owned tab's tier, and push the ones that CHANGED to the runner
 * — grouped by tier, so a layout change costs at most three IPC calls no
 * matter how many tabs moved.
 *
 * Exported for tests and for the unmount path; production code goes through
 * {@link scheduleReconcile}.
 */
export function reconcileNow(): void {
  if (flushHandle !== null) {
    clearTimeout(flushHandle);
    flushHandle = null;
  }

  const owned = new Set<string>();
  for (const roster of rosters.values()) {
    for (const id of roster) owned.add(id);
  }
  // A pane can legitimately declare a terminal no roster has published yet
  // (the pane mounted before its scope's `tabs` effect ran). Serve it: an
  // undeclared tab defaults to `unwatched`, so the reverse omission is the
  // dangerous one.
  for (const id of declarations.keys()) owned.add(id);

  const groups = new Map<VisibilityTier, string[]>();
  for (const id of owned) {
    const tier = effectiveTier(id);
    if (sent.get(id) === tier) continue;
    const group = groups.get(tier);
    if (group) group.push(id);
    else groups.set(tier, [id]);
  }

  // Forget tabs that are gone so a terminal id that comes back (a restored
  // session rebinding to its old id) is re-declared rather than assumed.
  for (const id of [...sent.keys()]) {
    if (!owned.has(id)) sent.delete(id);
  }

  for (const [tier, ids] of groups) {
    // Record the intent BEFORE the await: the runner defaults an unreported
    // session to `focused`, so a dropped call degrades to full service rather
    // than to silence, and the next real change re-sends anyway.
    for (const id of ids) sent.set(id, tier);
    void invoke("terminal_set_visibility", { ids, tier }).catch(() => {
      // Best-effort. A failed push leaves the runner on the previous tier
      // (worst case: today's untiered behavior). Clear the record so the next
      // reconcile retries instead of believing the tier landed.
      for (const id of ids) {
        if (sent.get(id) === tier) sent.delete(id);
      }
    });
  }
}

/** Test-only: the tiers this module believes the runner is on. */
export function __sentTiers(): Record<string, VisibilityTier> {
  return Object.fromEntries(sent);
}

/** Test-only hard reset. */
export function __resetVisibilityTiersForTest(): void {
  if (flushHandle !== null) {
    clearTimeout(flushHandle);
    flushHandle = null;
  }
  declarations.clear();
  rosters.clear();
  sent.clear();
}
