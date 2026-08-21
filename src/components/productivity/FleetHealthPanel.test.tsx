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
import {
  COORD_SOURCE_NO_ACCOUNT,
  COORD_SOURCE_SETTINGS_UNREADABLE,
  deriveCoordGating,
} from "@/contexts/CoordModeContext";

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

/**
 * §6.4 — isolated-mode gating. Plan
 * `2026-08-18-runner-embedded-pg-parity-and-coord-http-migration`.
 *
 * Fleet Health is coord-backed outright: `get_fleet_health` is a thin proxy
 * to coord's `/coord/fleet/health` + `/coord/alerts`. On an isolated runner
 * there is nothing behind it, so it renders disabled with a stated reason
 * rather than polling a phantom base once a second.
 */
describe("deriveFleetView — coord-mode gating (§6.4)", () => {
  const connected = deriveCoordGating({
    data: { mode: "connected", base: "https://coord.qontinui.io", source: "tier_default" },
    error: null,
    loading: false,
  });
  const isolatedNoAccount = deriveCoordGating({
    data: { mode: "isolated", base: null, source: COORD_SOURCE_NO_ACCOUNT },
    error: null,
    loading: false,
  });
  const isolatedUnreadable = deriveCoordGating({
    data: { mode: "isolated", base: null, source: COORD_SOURCE_SETTINGS_UNREADABLE },
    error: null,
    loading: false,
  });
  const unknown = deriveCoordGating({
    data: null,
    error: "Command get_coord_mode not found",
    loading: false,
  });

  it("connected → live: polling on, controls on, fleet data rendered", () => {
    const v = deriveFleetView(okPayload(), connected);
    expect(v.coordDisabled).toBe(false);
    expect(v.pollingEnabled).toBe(true);
    expect(v.controlsEnabled).toBe(true);
    expect(v.machines).toHaveLength(1);
    expect(v.alerts).toHaveLength(1);
  });

  it("isolated → disabled: notice instead of data, no poll, controls inert", () => {
    const v = deriveFleetView(okPayload(), isolatedNoAccount);
    expect(v.coordDisabled).toBe(true);
    expect(v.pollingEnabled).toBe(false);
    expect(v.controlsEnabled).toBe(false);
    // A stale frame must never render as if it were current fleet state.
    expect(v.machines).toEqual([]);
    expect(v.alerts).toEqual([]);
    expect(v.rollup).toEqual({ critical: 0, warning: 0, info: 0 });
  });

  it("isolated via unreadable settings.json → disabled the same way", () => {
    const v = deriveFleetView(okPayload(), isolatedUnreadable);
    expect(v.coordDisabled).toBe(true);
    expect(v.pollingEnabled).toBe(false);
    // The reason text differs — that split is pinned in
    // CoordConnectionRequired.test.ts — but the gate itself is the same.
    expect(isolatedUnreadable.source).toBe(COORD_SOURCE_SETTINGS_UNREADABLE);
  });

  it("unknown mode (invoke rejected) → stays ENABLED, never falsely disabled", () => {
    const v = deriveFleetView(okPayload(), unknown);
    expect(v.coordDisabled).toBe(false);
    expect(v.pollingEnabled).toBe(true);
    expect(v.controlsEnabled).toBe(true);
    expect(v.machines).toHaveLength(1);
  });

  it("no gating argument at all → unchanged pre-§6.4 behaviour (fails open)", () => {
    const v = deriveFleetView(okPayload());
    expect(v.coordDisabled).toBe(false);
    expect(v.pollingEnabled).toBe(true);
    expect(v.machines).toHaveLength(1);
  });
});
