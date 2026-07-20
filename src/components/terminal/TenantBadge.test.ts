/**
 * F1/F2 — tenant badge visibility + spawn-tenant precedence.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom), so these
 * exercise the exported pure helpers rather than rendering — the same split
 * `UnzonedChip` / `LaunchMenu` use.
 */

import { describe, expect, it } from "vitest";

import { tenantBadgeLabel } from "./TenantBadge";
import { resolveSpawnTenant, shortTenantId } from "./SpawnTenantPicker";

const A = "6b1f4b0e-1111-4000-8000-000000000001";
const B = "91ffaa20-2222-4000-8000-000000000002";

describe("tenantBadgeLabel", () => {
  it("renders nothing on a single-tenant device (D12 no-clutter rule)", () => {
    expect(tenantBadgeLabel(A, false)).toBeNull();
  });

  it("renders nothing when the session has no recorded tenant", () => {
    // A tab restored from a pre-F2 durable record. Showing the DEVICE's
    // active tenant here would be a lie — the session may have been spawned
    // under a different one, and its tenant is immortal.
    expect(tenantBadgeLabel(undefined, true)).toBeNull();
    expect(tenantBadgeLabel(null, true)).toBeNull();
    expect(tenantBadgeLabel("   ", true)).toBeNull();
  });

  it("renders the uuid stem with the full id in the tooltip", () => {
    const label = tenantBadgeLabel(A, true);
    expect(label?.text).toBe("6b1f4b0e");
    expect(label?.title).toContain(A);
  });

  it("says the tenant is fixed at spawn, so the badge is never read as a switch", () => {
    expect(tenantBadgeLabel(A, true)?.title).toContain("fixed at spawn");
  });

  it("leaves a short (non-uuid) id intact", () => {
    expect(tenantBadgeLabel("acme", true)?.text).toBe("acme");
  });
});

describe("shortTenantId", () => {
  it("truncates a uuid to its discriminating stem", () => {
    expect(shortTenantId(A)).toBe("6b1f4b0e");
  });

  it("leaves anything already short alone", () => {
    expect(shortTenantId("acme")).toBe("acme");
  });
});

describe("resolveSpawnTenant", () => {
  const candidates = [A, B];

  it("prefers the operator's explicit override", () => {
    expect(resolveSpawnTenant({ override: B, inferred: A, active: A, candidates })).toBe(B);
  });

  it("falls back to the repo→tenant inference when there is no override", () => {
    expect(resolveSpawnTenant({ override: null, inferred: B, active: A, candidates })).toBe(B);
  });

  it("falls back to the active pin when inference found nothing", () => {
    expect(resolveSpawnTenant({ inferred: null, active: A, candidates })).toBe(A);
  });

  it("DISCARDS an inferred tenant the device is not bound to", () => {
    // coord can know a repo belongs to a tenant this device has no binding
    // (and therefore no credential) for. Pre-selecting it would yield a
    // session that cannot authenticate.
    const unbound = "ffffffff-0000-4000-8000-00000000ffff";
    expect(resolveSpawnTenant({ inferred: unbound, active: A, candidates })).toBe(A);
  });

  it("DISCARDS an override the device is not bound to", () => {
    const unbound = "ffffffff-0000-4000-8000-00000000ffff";
    expect(resolveSpawnTenant({ override: unbound, active: A, candidates })).toBe(A);
  });

  it("returns undefined on an unpaired device so the caller omits tenant_id", () => {
    expect(resolveSpawnTenant({ active: null, candidates: [] })).toBeUndefined();
  });
});
