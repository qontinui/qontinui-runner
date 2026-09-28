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

import { useEffect, useState } from "react";
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

export function useSupervisorObservation(
  pollMs: number = SUPERVISOR_OBSERVATION_POLL_MS,
): SupervisorObservationState {
  const [state, setState] = useState<SupervisorObservationState>({ kind: "loading" });

  useEffect(() => {
    let cancelled = false;
    const read = async () => {
      try {
        const observation = await fetchSupervisorObservation();
        if (!cancelled) setState({ kind: "read", observation });
      } catch (err) {
        if (!cancelled) {
          // A failed RE-read keeps the last actual observation: unmounting a
          // panel mid-build because one poll of the runner timed out would
          // abort the operator's work over a hop that says nothing about the
          // supervisor. Only a read that answers moves the verdict.
          const message = err instanceof Error ? err.message : String(err);
          setState((prev) => (prev.kind === "read" ? prev : { kind: "error", message }));
        }
      }
    };
    void read();
    const id = window.setInterval(() => void read(), pollMs);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [pollMs]);

  return state;
}
