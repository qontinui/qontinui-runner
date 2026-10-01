/**
 * SupervisorGate — renders a dev-only panel only when a supervisor is OBSERVED.
 *
 * The CI-runner and "Test My Change" panels are whole development-environment
 * surfaces: every control on them calls the dev supervisor. On a published
 * runner there is no supervisor, and a Start button wired to a port nothing
 * listens on is a surface that lies. So the gate renders the panel only when
 * the runner's own probe (`GET /supervisor/observation`) reports
 * `observed: true`, and hands it the supervisor's base URL from that read —
 * no port literal in the frontend.
 *
 * Every other state (loading, not observed, unknown, read failed) renders a
 * neutral line that names nothing supervisor-related.
 */

import type { ReactNode } from "react";
import {
  observedSupervisorBase,
  useSupervisorObservation,
  type SupervisorObservationState,
} from "@/hooks/useSupervisorObservation";

export const NOT_AVAILABLE_TEXT = "This page is not available on this machine.";

interface SupervisorGateViewProps {
  state: SupervisorObservationState;
  /** The panel, given the observed supervisor's base URL. */
  children: (supervisorBase: string) => ReactNode;
}

/** PURE presentational half — what the tests render. */
export function SupervisorGateView({ state, children }: SupervisorGateViewProps) {
  if (state.kind === "loading") {
    return (
      <div className="flex items-center justify-center h-32" data-testid="dev-surface-gate-loading">
        <div className="text-muted-foreground text-sm">Loading…</div>
      </div>
    );
  }
  const base = observedSupervisorBase(state);
  if (base === null) {
    return (
      <div
        className="p-3 rounded-lg border border-border bg-muted/40 text-xs text-muted-foreground"
        data-testid="dev-surface-gate-unavailable"
      >
        {NOT_AVAILABLE_TEXT}
      </div>
    );
  }
  return <>{children(base)}</>;
}

export function SupervisorGate({ children }: Pick<SupervisorGateViewProps, "children">) {
  const state = useSupervisorObservation();
  return <SupervisorGateView state={state}>{children}</SupervisorGateView>;
}
