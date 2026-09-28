/**
 * useSupervisorObservation — the runner's ONE supervisor probe, read.
 *
 * `GET /supervisor/observation` answers `{ observed, probed_at, port,
 * base_url }` from the same TCP probe the session briefing already trusts
 * (`mcp/auto_continue.rs` `check_supervisor_available`). A published runner
 * has no supervisor, so the dev-only settings panels that talk to one render
 * only when this reports `observed: true`, and take the supervisor's address
 * from here rather than from a port literal.
 *
 * Tri-state on the wire: `observed: null` means the runner's configured
 * supervisor address did not parse, so nothing was probed — UNKNOWN, which
 * the panels treat like "not observed" (they render nothing supervisor
 * related) but which is kept distinct here so it is never reported as absent.
 *
 * Not observed is not absent forever: the read is re-polled, so a supervisor
 * started after the panel opened appears on the next tick.
 *
 * Plan 2026-09-20-the-published-product-works-without-knowing-a-development-environment-exists, B2.
 */

import { useSyncExternalStore } from "react";
import { getApiBase, tracedFetch } from "@/lib/runner-api";

/** Wire shape of `GET /supervisor/observation` (inside the `ApiResponse` envelope). */
export interface SupervisorObservation {
  observed: boolean | null;
  probed_at: string;
  port: number | null;
  base_url: string | null;
}

/**
 * What a consumer renders from. `loading` until the first read settles;
 * `error` when the read itself failed (runner unreachable, older build with no
 * such route) — also UNKNOWN, never "observed".
 */
export type SupervisorObservationState =
  | { kind: "loading" }
  | { kind: "error"; message: string }
  | { kind: "read"; observation: SupervisorObservation };

/** How often the observation is re-read. Each read is a ≤500 ms probe server-side. */
export const SUPERVISOR_OBSERVATION_POLL_MS = 15_000;

/**
 * PURE: the supervisor base URL a panel may call, or `null` when it must
 * render nothing supervisor-related. Only an OBSERVED listener with a known
 * address qualifies.
 */
export function observedSupervisorBase(state: SupervisorObservationState): string | null {
  if (state.kind !== "read") return null;
  const { observed, base_url } = state.observation;
  if (observed !== true || !base_url) return null;
  return base_url.replace(/\/+$/, "");
}

/**
 * PURE: the supervisor's port when it was OBSERVED listening, else `null`.
 * Mirrors the runner's discovery scan, which adds this port only then.
 */
export function observedSupervisorPort(state: SupervisorObservationState): number | null {
  if (state.kind !== "read") return null;
  const { observed, port } = state.observation;
  return observed === true && port !== null ? port : null;
}

/** PURE: ordered, de-duplicated union of `defaults` and an optional extra port. */
export function withObservedPort(defaults: readonly number[], port: number | null): number[] {
  return port === null || defaults.includes(port) ? [...defaults] : [...defaults, port];
}

interface Envelope<T> {
  success: boolean;
  data?: T;
  error?: string;
}

export async function fetchSupervisorObservation(): Promise<SupervisorObservation> {
  const res = await tracedFetch(`${getApiBase()}/supervisor/observation`);
  if (!res.ok) {
    throw new Error(`HTTP ${res.status}`);
  }
  const body = (await res.json()) as Envelope<SupervisorObservation>;
  if (!body.success || !body.data) {
    throw new Error(body.error || "supervisor observation read failed");
  }
  return body.data;
}

// ---------------------------------------------------------------------------
// One shared poller. The sidebar, the Settings sub-nav and the panel itself
// all read this; each mounting its own interval would triple the runner-side
// probes for one answer. The interval runs while at least one subscriber is
// mounted and stops when the last one leaves.
// ---------------------------------------------------------------------------

let sharedState: SupervisorObservationState = { kind: "loading" };
const listeners = new Set<() => void>();
let timer: number | null = null;

function publish(next: SupervisorObservationState) {
  sharedState = next;
  for (const l of listeners) l();
}

/**
 * The transition rule, PURE. A failed RE-read keeps the last actual
 * observation: unmounting a panel mid-build (or dropping its nav entry)
 * because one poll of the runner timed out would act on a hop that says
 * nothing about the supervisor. Only a read that answers moves the verdict.
 */
export function nextObservationState(
  prev: SupervisorObservationState,
  outcome: { ok: true; observation: SupervisorObservation } | { ok: false; message: string },
): SupervisorObservationState {
  if (outcome.ok) return { kind: "read", observation: outcome.observation };
  return prev.kind === "read" ? prev : { kind: "error", message: outcome.message };
}

async function readOnce() {
  try {
    const observation = await fetchSupervisorObservation();
    publish(nextObservationState(sharedState, { ok: true, observation }));
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    publish(nextObservationState(sharedState, { ok: false, message }));
  }
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  if (timer === null) {
    void readOnce();
    timer = window.setInterval(() => void readOnce(), SUPERVISOR_OBSERVATION_POLL_MS);
  }
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0 && timer !== null) {
      window.clearInterval(timer);
      timer = null;
    }
  };
}

export function useSupervisorObservation(): SupervisorObservationState {
  return useSyncExternalStore(
    subscribe,
    () => sharedState,
    () => sharedState,
  );
}

/** Convenience for nav gating: `true` only for an OBSERVED supervisor. */
export function useSupervisorObserved(): boolean {
  return observedSupervisorBase(useSupervisorObservation()) !== null;
}
