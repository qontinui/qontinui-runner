/**
 * Pure-helper tests for FleetHealthPanel's auth-state derivation
 * (fleet-auth P2).
 *
 * The runner's vitest config is `environment: "node"` (no jsdom — see
 * FileActivityPanel.test.tsx / CompletionReportSections.test.tsx for the
 * same constraint), so we can't render the panel and assert on its DOM.
 * The load-bearing logic — mapping coord's structured `auth` state onto
 * the panel's view-state (which banner/empty state shows, and whether
 * the stale machine grid is suppressed) — lives in the pure exported
 * `deriveFleetView` helper, tested here. The JSX merely renders the
 * flags this helper returns.
 */

import { describe, expect, it } from "vitest";

import { deriveFleetView } from "./FleetHealthPanel";
import type { FleetHealth, FleetMachineSnapshot } from "./coordinatorApi";

const machine: FleetMachineSnapshot = {
  machine_id: "m-1",
  hostname: "alpha",
  state: "healthy",
  state_changed_at: "2026-05-31T00:00:00Z",
  last_probe_at: "2026-05-31T00:00:00Z",
  last_probe_ok: true,
  consecutive_failures: 0,
  agents_active: 2,
  updated_at: "2026-05-31T00:00:00Z",
};

function okPayload(overrides: Partial<FleetHealth> = {}): FleetHealth {
  return {
    health: {
      machines: [machine],
      count: 1,
      by_state: { healthy: 1 },
      alerts: { critical: 1, warning: 2, info: 0 },
      kv_bucket: "fleet-health",
      as_of: "2026-05-31T00:00:00Z",
    },
    alerts: [
      {
        id: 1,
        alert_key: "k",
        severity: "critical",
        kind: "partition",
        machine_id: "m-1",
        summary: "down",
        detail: {},
        first_seen_at: "2026-05-31T00:00:00Z",
        last_seen_at: "2026-05-31T00:00:00Z",
        occurrences: 1,
        resolved_at: null,
        page_due_at: null,
      },
    ],
    coordBase: "http://localhost:9870",
    auth: { state: "ok" },
    ...overrides,
  };
}

describe("deriveFleetView — fleet-auth P2 auth-state mapping", () => {
  it("state 'ok' → not auth-blocked, machines + alerts pass through", () => {
    const v = deriveFleetView(okPayload());
    expect(v.authState).toBe("ok");
    expect(v.isAuthBlocked).toBe(false);
    expect(v.isUnauthorized).toBe(false);
    expect(v.isUnpaired).toBe(false);
    expect(v.machines).toHaveLength(1);
    expect(v.alerts).toHaveLength(1);
    expect(v.rollup).toEqual({ critical: 1, warning: 2, info: 0 });
  });

  it("absent auth (older backend) → treated as 'ok' (back-compat)", () => {
    const v = deriveFleetView(okPayload({ auth: undefined }));
    expect(v.authState).toBe("ok");
    expect(v.isAuthBlocked).toBe(false);
    expect(v.machines).toHaveLength(1);
  });

  it("null data (pre-first-load) → 'ok', empty grid/alerts, no crash", () => {
    const v = deriveFleetView(null);
    expect(v.authState).toBe("ok");
    expect(v.isAuthBlocked).toBe(false);
    expect(v.machines).toEqual([]);
    expect(v.alerts).toEqual([]);
    expect(v.rollup).toEqual({ critical: 0, warning: 0, info: 0 });
  });

  it("state 'unauthorized' → auth-blocked; stale machine grid + alerts suppressed", () => {
    // health is null on a 401/403 from the backend; even if a stale frame
    // lingered, the auth-blocked branch must drop it so it never renders
    // as current.
    const v = deriveFleetView(okPayload({ auth: { state: "unauthorized" } }));
    expect(v.authState).toBe("unauthorized");
    expect(v.isUnauthorized).toBe(true);
    expect(v.isUnpaired).toBe(false);
    expect(v.isAuthBlocked).toBe(true);
    expect(v.machines).toEqual([]);
    expect(v.alerts).toEqual([]);
  });

  it("state 'unpaired' → auth-blocked; stale machine grid + alerts suppressed", () => {
    const v = deriveFleetView(okPayload({ auth: { state: "unpaired" } }));
    expect(v.authState).toBe("unpaired");
    expect(v.isUnpaired).toBe(true);
    expect(v.isUnauthorized).toBe(false);
    expect(v.isAuthBlocked).toBe(true);
    expect(v.machines).toEqual([]);
    expect(v.alerts).toEqual([]);
  });

  it("auth-blocked with health:null does not throw on the rollup fallback", () => {
    const v = deriveFleetView({
      health: null,
      alerts: [],
      coordBase: "http://localhost:9870",
      auth: { state: "unauthorized" },
    });
    expect(v.isAuthBlocked).toBe(true);
    expect(v.rollup).toEqual({ critical: 0, warning: 0, info: 0 });
  });
});
