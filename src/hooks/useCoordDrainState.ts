/**
 * useCoordDrainState — coord's device drain, as this runner reads it.
 *
 * Plan `2026-09-13-drained-runner-never-reaches-idle`, Phase 3. The Rust
 * `coord_drain_state` module caches the drain from the 30 s heartbeat and
 * `GET /coord/devices/me/drain`, emits `coord-drain-state-changed` on every
 * change, and serves the same snapshot from the `coord_drain_state_get` command.
 *
 * Module singleton (the `useSessionRecovery` pattern): one listener, one
 * getter read at first subscribe, and a slow poll — the backend flips a
 * heartbeat-starved state to `unknown` on read, so a poll is what surfaces a
 * wedged heartbeat when no event can fire.
 */

import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/** Mirrors `COORD_DRAIN_STATE_EVENT` in `coord_drain_state.rs`. */
export const COORD_DRAIN_STATE_EVENT = "coord-drain-state-changed";

/** Mirrors the Rust getter command name. */
export const COORD_DRAIN_STATE_GET_COMMAND = "coord_drain_state_get";

/** `clear` | `drained` | `unknown` | `not_enrolled` (a runner that is no coord device). */
export type CoordDrainStateLabel = "clear" | "drained" | "unknown" | "not_enrolled";

/** Mirrors the Rust `CoordDrainSnapshot` (camelCase). */
export interface CoordDrainSnapshot {
  state: CoordDrainStateLabel;
  /** Whether autonomous spawns run right now. */
  autonomousSpawnsAllowed: boolean;
  until: string | null;
  reason: string | null;
  /** For `unknown`: since when. */
  since: string | null;
  /** For `unknown`: why; for `not_enrolled`: what is missing. */
  cause: string | null;
  /** Distinct autonomous work items deferred since the drain began. */
  deferredCount: number;
  /**
   * `true` once the backend's `MAX_DEFERRED_KEYS` cap was hit, which makes
   * `deferredCount` a FLOOR rather than a total.
   *
   * Optional because a runner predating the field sends nothing. The banner
   * renders an absent value AS `false` (`?? false`), and that is deliberate
   * rather than a collapse of unknown into no: a build with no such field also
   * has no such cap, so its count really is a total. The default is only ever
   * read on a runner where the question does not arise.
   */
  deferredCapped?: boolean;
  deferredByOrigin: Record<string, number>;
  lastReadAt: string;
  /** True while the boot read is still in flight and nothing has been read. */
  bootReadPending: boolean;
}

const STATES: readonly string[] = ["clear", "drained", "unknown", "not_enrolled"];

/** Runtime guard for a payload crossing the Tauri boundary. */
export function isCoordDrainSnapshot(value: unknown): value is CoordDrainSnapshot {
  if (!value || typeof value !== "object") return false;
  const v = value as Record<string, unknown>;
  return (
    typeof v.state === "string" &&
    STATES.includes(v.state) &&
    typeof v.autonomousSpawnsAllowed === "boolean" &&
    typeof v.deferredCount === "number"
  );
}

const POLL_MS = 30_000;

let cached: CoordDrainSnapshot | null = null;
const subscribers = new Set<(s: CoordDrainSnapshot) => void>();
let started = false;

function publish(payload: unknown) {
  if (!isCoordDrainSnapshot(payload)) return;
  cached = payload;
  for (const fn of subscribers) fn(payload);
}

async function readNow() {
  try {
    publish(await invoke<unknown>(COORD_DRAIN_STATE_GET_COMMAND));
  } catch (err) {
    console.warn("[coord-drain] coord_drain_state_get failed:", err);
  }
}

function ensureStarted() {
  if (started) return;
  started = true;
  void listen<unknown>(COORD_DRAIN_STATE_EVENT, (event) => publish(event.payload));
  void readNow();
  setInterval(() => void readNow(), POLL_MS);
}

/**
 * Subscribe outside React (restore paths). The callback receives every
 * snapshot, starting with the cached one when there is one. Returns the
 * unsubscribe function.
 */
export function subscribeCoordDrainState(fn: (s: CoordDrainSnapshot) => void): () => void {
  ensureStarted();
  subscribers.add(fn);
  if (cached) fn(cached);
  return () => {
    subscribers.delete(fn);
  };
}

/**
 * Call `onResume` each time autonomous spawns become allowed after they were
 * not (a drain lifting, or an unknown state being read). Pure edge detector
 * over a snapshot stream, exported for tests.
 *
 * ## Why the seed comes from the DEFERRAL, not the stream (review N1)
 *
 * `last` starts at `null` and used to be seeded by whichever snapshot arrived
 * first. That misses the edge whenever the drain lifts between the deferral and
 * the first snapshot this detector sees — which is the ordinary case, because
 * the subscription is set up in an effect that runs after the deferred call
 * returned. The first snapshot then reads `allowed: true`, `last` was never
 * `false`, no resume fires, and the deferred restore waits for a drain cycle
 * that may never come again.
 *
 * `hasDeferredWork` is the caller's own record that something WAS deferred, and
 * it is the honest seed: work parked by the drain is by definition work that
 * saw `allowed: false`, whatever the stream later shows. When it reports
 * `true` and `last` is still unseeded, `last` is seeded `false`, so the first
 * allowing snapshot is a rising edge.
 */
export function autonomousResumeDetector(
  onResume: () => void,
  hasDeferredWork: () => boolean = () => false,
): (s: CoordDrainSnapshot) => void {
  let last: boolean | null = null;
  return (s) => {
    const allowed = s.autonomousSpawnsAllowed;
    if (last === null && hasDeferredWork()) last = false;
    if (last === false && allowed) onResume();
    last = allowed;
  };
}

/** The latest snapshot, or `null` before the first read lands. */
export function useCoordDrainState(): CoordDrainSnapshot | null {
  const [snapshot, setSnapshot] = useState<CoordDrainSnapshot | null>(cached);
  useEffect(() => subscribeCoordDrainState(setSnapshot), []);
  return snapshot;
}
