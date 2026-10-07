/**
 * Frontend half of the spawn-time resource gate (plan
 * `2026-08-07-runner-resource-guard-and-session-protection.md` §Part D).
 *
 * The gate itself lives in Rust (`src-tauri/src/resource_guard.rs`) on the two
 * spawn seams — `TerminalSession::spawn` and
 * `InstanceManager::launch_instance_with_app` — because those are reachable from
 * gate continuations, looping agents and the HTTP/relay doors, not only from a
 * button in this app. What lives here is the *attended* half: recognising the
 * typed CRITICAL refusal, asking the operator, and replaying the spawn with the
 * override set.
 *
 * ## Why a module store rather than context or props
 *
 * The refusal surfaces wherever a spawn is invoked — `useTerminalManager`, the
 * coordinator/worker launch buttons, the Instances panel — but the dialog is
 * rendered once, at the App root. Threading a callback from the root down to
 * every one of those call sites would be a prop chain whose only payload is
 * "here is how to ask a question". Same shape, same reason as
 * `src/lib/authOverlayStore.ts`.
 */

import { useSyncExternalStore } from "react";
import { describeThrown } from "./utils";

/**
 * Prefix the Rust side stamps on an overridable CRITICAL refusal. Must match
 * `resource_guard::CRITICAL_REFUSAL_PREFIX` byte for byte — it is the only thing
 * that distinguishes "the guard said no, and you may overrule it" from a real
 * spawn failure (no PTY, bad cwd, dead account) which must NEVER be offered an
 * override.
 */
export const CRITICAL_REFUSAL_PREFIX = "resource_guard:critical:";

/**
 * The lane a refusal came from, in the Rust `LaneMetric::wire_name` vocabulary
 * (the same names the `resource-guard-notice` payload carries), or `"unknown"`
 * when the refusal named none this build recognises.
 *
 * `"unknown"` is a real state, not a default: an older runner build sends the
 * bare `resource_guard:critical: <message>` with no lane token, and an external
 * relay may forward a future lane this webview has never heard of. Either way
 * the dialog must not guess — guessing "memory" is exactly how a thread-lane
 * refusal on a box with hundreds of GB free came to be titled "Low memory".
 */
export type ResourceGuardMetric = "free_commit_bytes" | "thread_count" | "unknown";

/** A lane this webview can name — every {@link ResourceGuardMetric} but `"unknown"`. */
export type KnownResourceGuardMetric = Exclude<ResourceGuardMetric, "unknown">;

const KNOWN_METRICS: readonly KnownResourceGuardMetric[] = ["free_commit_bytes", "thread_count"];

/** A recognised CRITICAL refusal, split into its lane and its operator-facing text. */
export interface ResourceGuardRefusal {
  metric: ResourceGuardMetric;
  /** Message from Rust: names the lane, the live reading and the limit. */
  message: string;
}

/**
 * Lane token grammar after the prefix: `<snake_case>: ` (digits allowed after
 * the first letter, so a future `gpu0_mem` lane still parses as a token). The
 * old wire put a SPACE straight after the prefix, so it can never match this
 * and falls to `"unknown"` with its whole text kept.
 */
const LANE_TOKEN = /^([a-z][a-z0-9_]*): /;

/**
 * The lane and operator-facing text of a CRITICAL refusal, or `null` when `err`
 * is anything else.
 *
 * Wire: `resource_guard:critical:<metric>: <message>` (see the Rust
 * `CRITICAL_REFUSAL_PREFIX` doc). Tauri hands a Rust `Err(String)` to the
 * frontend as a plain string, but the invoke layer can also reject with an
 * `Error` (transport failure) or an arbitrary value, so this normalises before
 * matching rather than assuming. A well-formed token naming a lane this build
 * does not know is stripped from the text (it is a machine word, not prose) and
 * reported as `"unknown"`.
 */
export function parseResourceGuardRefusal(err: unknown): ResourceGuardRefusal | null {
  const text = describeThrown(err, "");
  if (!text.startsWith(CRITICAL_REFUSAL_PREFIX)) return null;
  const rest = text.slice(CRITICAL_REFUSAL_PREFIX.length);
  const token = LANE_TOKEN.exec(rest);
  if (!token) return { metric: "unknown", message: rest.trim() };
  const named = token[1] as KnownResourceGuardMetric;
  return {
    metric: KNOWN_METRICS.includes(named) ? named : "unknown",
    message: rest.slice(token[0].length).trim(),
  };
}

/**
 * Who is asking for the spawn, in the operator's words. Passed by the caller of
 * {@link spawnWithResourceGuard} so the dialog can say "session restore — up to
 * 281 starts queued" instead of an anonymous "a terminal". A burst is only
 * recognisable as a burst if the dialog names its source.
 */
export interface ResourceGuardSource {
  /** Short noun phrase: "session restore", "new terminal", "runner instance launch". */
  label: string;
  /**
   * Starts this caller may still make, this one included. An UPPER bound — a
   * restore loop skips records it can reconnect — so the dialog says "up to".
   * Omitted when the caller has no queue.
   */
  queued?: number;
}

/**
 * What the dialog shows: the head group of refusals, all from one lane.
 *
 * `pending` counts every refusal coalesced into this one decision. Concurrent
 * refusals of the same lane share a dialog — answering the head and leaving
 * N-1 more modals stacked behind it trains the operator to click through, which
 * defeats the guard.
 */
export interface PendingResourceBlock {
  metric: ResourceGuardMetric;
  /** The first refusal's text — every coalesced one names the same lane. */
  message: string;
  /** How many spawns this decision answers (≥ 1). */
  pending: number;
  /** The first refusal's caller, or `null` when it named none. */
  source: ResourceGuardSource | null;
}

interface QueuedBlock {
  message: string;
  source: ResourceGuardSource | null;
  decide: (startAnyway: boolean) => void;
}

interface BlockGroup {
  metric: ResourceGuardMetric;
  blocks: QueuedBlock[];
}

/**
 * FIFO of decisions waiting on the operator, one group per lane.
 *
 * A queue rather than a single slot because two spawns can be in flight at once
 * (the operator opens a terminal while a worker launch is mid-invoke, or two
 * pop-out windows both spawn). Replacing a pending refusal would strand the
 * first promise forever — the caller would hang, not fail — which is the one
 * outcome a guard whose entire contract is "never a silent refusal" cannot
 * produce.
 *
 * Grouped by lane so a refusal joins any group already waiting on ITS lane —
 * not only the head — and one answer settles them all. Two lanes are never
 * merged: "start anyway past the thread ceiling" is not an answer to a memory
 * question. `"unknown"` refusals are never merged either, even with each other,
 * because two unnamed lanes may be two different lanes.
 */
const groups: BlockGroup[] = [];

/** Spawns one "Start anyway" admits past the one it answered. */
export const GRANT_SPAWN_LIMIT = 10;
/** A grant lapses this long after it was given or last used. */
export const GRANT_IDLE_MS = 60_000;
/** …and never outlives this, however often it is used. */
export const GRANT_MAX_MS = 5 * 60_000;

/**
 * A lane-scoped override the operator granted by answering "Start anyway".
 *
 * ## Why a grant exists at all
 *
 * The bursts this dialog meets are SERIAL: the cold-restore loop
 * (`useTerminalInitialization`) and a workspace load (`ZoneControlPanel`) await
 * each `createTerminal` in turn, so refusal k+1 is not even attempted until the
 * operator answers refusal k. Coalescing only ever sees one of them. Without a
 * remembered answer a restore of N refused records is N dialogs — 34 overrides
 * in four minutes on 2026-10-01 — and an operator taught to click through has
 * no guard left.
 *
 * ## Why it is bounded by COUNT as well as time
 *
 * Measured the same day (plan
 * `2026-10-01-runner-thread-ceilings-ignore-the-machine-and-the-guard-dialog-says-low-memory`
 * §1 and the Phase 3 author correction, from the merytshost journal and
 * `terminal_session_list_open`): of ~180 terminals those clicks admitted, 178 were bare
 * shells a restore materialised one per disk-only candidate (281 offered), each
 * adding two threads to the pressure the dialog was guarding. A time-only grant
 * would have admitted that whole burst on ONE click, silently — an override that
 * multiplies is how a guard becomes the amplifier. So a grant covers at most
 * {@link GRANT_SPAWN_LIMIT} spawns, lapses {@link GRANT_IDLE_MS} after its last
 * use, and never outlives {@link GRANT_MAX_MS}. When it is spent the next
 * refusal asks again. (Stopping the burst at its source is the sibling plan
 * `2026-10-01-runner-spawn-bursts-are-unregulated-coord-must-admit-spawns-per-machine`.)
 *
 * Every spawn a grant admits still re-invokes with `resourceOverride: true`, so
 * it reaches Rust's OVERRIDDEN arm, logs, and emits the override notice — the
 * audit trail is the same one a click leaves. And it is always on screen
 * (`ResourceGuardGrantBanner`) with a Revoke, because an override the operator
 * cannot see is a surprise, not a convenience.
 */
export interface ResourceGuardGrant {
  metric: KnownResourceGuardMetric;
  /** Spawns it may still admit. */
  remaining: number;
  /** Epoch ms it lapses at: idle deadline, capped by the absolute one. */
  expiresAt: number;
  /** The source of the refusal that was answered, so the banner can say what it is for. */
  sourceLabel: string | null;
}

interface LiveGrant extends ResourceGuardGrant {
  hardExpiresAt: number;
  timer: ReturnType<typeof setTimeout>;
}

const grants = new Map<KnownResourceGuardMetric, LiveGrant>();

const listeners = new Set<() => void>();
let pendingSnapshot: PendingResourceBlock | null = null;
let grantsSnapshot: readonly ResourceGuardGrant[] = [];

/**
 * Rebuild both snapshots, then wake subscribers. `useSyncExternalStore`
 * compares snapshots by identity, so they are rebuilt here — once per change —
 * and returned unchanged by the getters in between.
 */
function notify(): void {
  const head = groups[0];
  pendingSnapshot = head
    ? {
        metric: head.metric,
        message: head.blocks[0].message,
        pending: head.blocks.length,
        source: head.blocks[0].source,
      }
    : null;
  grantsSnapshot = [...grants.values()].map(({ metric, remaining, expiresAt, sourceLabel }) => ({
    metric,
    remaining,
    expiresAt,
    sourceLabel,
  }));
  for (const l of [...listeners]) l();
}

/**
 * Store half of the `useSyncExternalStore` contract. Exported so it can be unit
 * tested directly — vitest runs `environment: "node"` here with no React
 * Testing Library, the same constraint `authOverlayStore.ts` documents.
 */
export function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** The decision currently on screen, or `null`. See {@link subscribe}. */
export function getSnapshot(): PendingResourceBlock | null {
  return pendingSnapshot;
}

/** Every live grant. See {@link subscribe}. */
export function getGrantsSnapshot(): readonly ResourceGuardGrant[] {
  return grantsSnapshot;
}

/** The decision the dialog should be showing, or `null` when there is none. */
export function usePendingResourceBlock(): PendingResourceBlock | null {
  return useSyncExternalStore(subscribe, getSnapshot);
}

/** Every live grant, for the banner. */
export function useResourceGuardGrants(): readonly ResourceGuardGrant[] {
  return useSyncExternalStore(subscribe, getGrantsSnapshot);
}

function endGrant(metric: KnownResourceGuardMetric): void {
  const grant = grants.get(metric);
  if (!grant) return;
  clearTimeout(grant.timer);
  grants.delete(metric);
  notify();
}

/** End a grant now. The banner's Revoke; a no-op when none is live. */
export function revokeResourceGuardGrant(metric: KnownResourceGuardMetric): void {
  endGrant(metric);
}

function giveGrant(metric: KnownResourceGuardMetric, sourceLabel: string | null): void {
  const existing = grants.get(metric);
  if (existing) clearTimeout(existing.timer);
  const now = Date.now();
  const expiresAt = now + GRANT_IDLE_MS;
  grants.set(metric, {
    metric,
    remaining: GRANT_SPAWN_LIMIT,
    expiresAt,
    hardExpiresAt: now + GRANT_MAX_MS,
    sourceLabel,
    timer: setTimeout(() => endGrant(metric), GRANT_IDLE_MS),
  });
}

/**
 * Spend one spawn of the lane's grant, if one is live. Each use pushes the idle
 * deadline out again (never past the absolute cap); the last use ends it.
 */
function consumeGrant(metric: ResourceGuardMetric): boolean {
  if (metric === "unknown") return false;
  const grant = grants.get(metric);
  if (!grant) return false;
  const now = Date.now();
  // The timer should already have ended a lapsed grant; the clock is the
  // authority if it has not fired yet (a throttled background webview).
  if (now >= grant.expiresAt) {
    endGrant(metric);
    return false;
  }
  grant.remaining -= 1;
  if (grant.remaining <= 0) {
    endGrant(metric);
    return true;
  }
  clearTimeout(grant.timer);
  grant.expiresAt = Math.min(now + GRANT_IDLE_MS, grant.hardExpiresAt);
  grant.timer = setTimeout(() => endGrant(metric), grant.expiresAt - now);
  notify();
  return true;
}

/**
 * Answer the decision at the head of the queue — every refusal coalesced into
 * it. Called by the dialog's confirm / cancel handlers; a no-op when the queue
 * is empty (a double-click on Confirm must not answer the *next* operator's
 * question). "Start anyway" on a named lane also gives that lane a grant
 * ({@link ResourceGuardGrant}); an `"unknown"` lane never gets one, because a
 * grant scoped to a lane nobody named is not scoped at all.
 */
export function resolvePendingResourceBlock(startAnyway: boolean): void {
  const head = groups.shift();
  if (head && startAnyway && head.metric !== "unknown") {
    giveGrant(head.metric, head.blocks[0].source?.label ?? null);
  }
  notify();
  for (const block of head?.blocks ?? []) block.decide(startAnyway);
}

/** Queue a refusal (joining its lane's waiting group) and resolve once it is answered. */
function askOperator(
  refusal: ResourceGuardRefusal,
  source: ResourceGuardSource | null,
): Promise<boolean> {
  return new Promise<boolean>((resolve) => {
    const block: QueuedBlock = { message: refusal.message, source, decide: resolve };
    const group =
      refusal.metric === "unknown" ? undefined : groups.find((g) => g.metric === refusal.metric);
    if (group) group.blocks.push(block);
    else groups.push({ metric: refusal.metric, blocks: [block] });
    notify();
  });
}

/**
 * Run a spawn through the resource gate's attended path.
 *
 * `attempt` is invoked with `false` first. If it rejects with anything other
 * than a typed CRITICAL refusal the error propagates untouched — this wrapper
 * must never convert a genuine spawn failure into a "start anyway?" prompt.
 * On a typed refusal a live grant for that lane is spent and the spawn
 * re-invoked with `true` without asking; otherwise the operator is asked, and
 * "Start anyway" re-invokes with `true`. Declining re-throws the ORIGINAL
 * refusal so the caller's existing error handling still runs and the spawn is
 * not silently reported as a success.
 *
 * The first attempt is always made WITHOUT the override, grant or no grant: a
 * spawn the guard would admit anyway must not spend one of the grant's starts,
 * and the guard — not this module — decides whether the box is over its limit.
 *
 * `source` names the caller for the dialog ({@link ResourceGuardSource}).
 *
 * Every attended spawn surface should call through this rather than invoking
 * directly, so the override handling exists in exactly one place.
 */
export async function spawnWithResourceGuard<T>(
  attempt: (resourceOverride: boolean) => Promise<T>,
  source?: ResourceGuardSource,
): Promise<T> {
  try {
    return await attempt(false);
  } catch (err) {
    const refusal = parseResourceGuardRefusal(err);
    if (refusal === null) throw err;
    if (consumeGrant(refusal.metric)) return attempt(true);
    const startAnyway = await askOperator(refusal, source ?? null);
    if (!startAnyway) throw err;
    return attempt(true);
  }
}
