/**
 * F1/F2 — tenant badge visibility + spawn-tenant precedence.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom), so these
 * exercise the exported pure helpers rather than rendering — the same split
 * `UnzonedChip` / `LaunchMenu` use.
 */

import { describe, expect, it } from "vitest";

import { tenantBadgeLabel } from "./TenantBadge";
import type { SessionTenancy } from "./useSessionInfo";
import {
  explicitSpawnTenant,
  pickSpawnTenant,
  resolveSpawnTenant,
  shortTenantId,
  spawnTenantPickerModel,
} from "./SpawnTenantPicker";

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

/** A tenancy block as the runner serves it (Rust `SessionTenancy`). */
function tenancy(overrides: {
  row?: string | null;
  dataPlane?: string | null;
  credential?: string | null;
  credentialStatus?: "resolved" | "unknown";
  credentialReason?: string | null;
  diverged: boolean;
  divergence?: SessionTenancy["divergence"];
  spawnDefault?: { status: string; tenantId?: string | null; reason?: string | null };
}): SessionTenancy {
  return {
    row: {
      tenantId: overrides.row ?? null,
      spawnDeviceDefaultTenantId: overrides.spawnDefault?.tenantId ?? null,
      spawnDeviceDefaultStatus: overrides.spawnDefault?.status ?? "unknown",
      spawnDeviceDefaultReason: overrides.spawnDefault?.reason ?? "not_recorded",
      currentDeviceDefaultTenantId: null,
    },
    dataPlane: {
      status: overrides.dataPlane ? "owned" : "unknown",
      tenantId: overrides.dataPlane ?? null,
      reason: overrides.dataPlane ? null : "no_coord_session",
    },
    credential: {
      status: overrides.credentialStatus ?? "resolved",
      tenantId: overrides.credential ?? null,
      slot: overrides.credential ? "tenant" : null,
      reason: overrides.credentialReason ?? null,
      posture: { status: "unknown", value: null, canAnswer: null, reason: "no_posture_published" },
    },
    diverged: overrides.diverged,
    divergence: overrides.divergence ?? (overrides.diverged ? "diverged" : "agree"),
  };
}

describe("tenantBadgeLabel — the session's tenancy (plan 2026-09-10 P0)", () => {
  it("renders a divergence as a mismatch naming all three tenants, never as the stamped label", () => {
    const label = tenantBadgeLabel(
      B,
      true,
      tenancy({ row: B, dataPlane: B, credential: A, diverged: true }),
    );
    expect(label?.diverged).toBe(true);
    expect(label?.text).toBe("91ffaa20≠6b1f4b0e");
    expect(label?.title).toContain("TENANT MISMATCH");
    expect(label?.title).toContain(`Spawned for: ${B}`);
    expect(label?.title).toContain(`coord-mcp writes (memory, prompt documents, gates): ${A}`);
  });

  it("still renders a divergence when the tab carries no stamped tenant", () => {
    const label = tenantBadgeLabel(
      undefined,
      true,
      tenancy({ dataPlane: B, credential: A, diverged: true }),
    );
    expect(label?.diverged).toBe(true);
    expect(label?.text).toBe("?≠6b1f4b0e");
  });

  it("spells an unknown credential tenant as unknown rather than implying the label", () => {
    const label = tenantBadgeLabel(
      B,
      true,
      tenancy({
        row: B,
        credentialStatus: "unknown",
        credentialReason: "no_session_nonce",
        diverged: false,
      }),
    );
    expect(label?.diverged).toBe(false);
    expect(label?.credentialUnknown).toBe(true);
    expect(label?.text).toBe("91ffaa20?");
    expect(label?.title).toContain("UNKNOWN (no_session_nonce)");
  });

  it("keeps the plain label when all three agree", () => {
    const label = tenantBadgeLabel(
      B,
      true,
      tenancy({ row: B, dataPlane: B, credential: B, diverged: false }),
    );
    expect(label).toMatchObject({ text: "91ffaa20", diverged: false, credentialUnknown: false });
  });

  it("names an unrecorded spawn default as not recorded, never as a device default (W-B)", () => {
    const label = tenantBadgeLabel(
      undefined,
      true,
      tenancy({ credential: A, dataPlane: B, diverged: true }),
    );
    expect(label?.title).toContain("device default at spawn not recorded: not_recorded");
    const recorded = tenantBadgeLabel(
      undefined,
      true,
      tenancy({
        credential: A,
        dataPlane: B,
        diverged: true,
        spawnDefault: { status: "recorded", tenantId: B, reason: null },
      }),
    );
    expect(recorded?.title).toContain(`device default at spawn: ${B}`);
  });

  it("does not present an unknown comparison as confirmed agreement (W-B)", () => {
    const label = tenantBadgeLabel(
      B,
      true,
      tenancy({ row: B, credential: B, diverged: false, divergence: "unknown" }),
    );
    expect(label?.diverged).toBe(false);
    expect(label?.title).toContain("could not be compared");
    const agree = tenantBadgeLabel(B, true, tenancy({ row: B, credential: B, diverged: false }));
    expect(agree?.title).not.toContain("could not be compared");
  });

  it("stays hidden on a single-tenant device even when the halves disagree (D12)", () => {
    expect(
      tenantBadgeLabel(B, false, tenancy({ row: B, credential: A, diverged: true })),
    ).toBeNull();
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
    expect(
      resolveSpawnTenant({ override: B, inferred: A, defaultForNewSessions: A, candidates }),
    ).toBe(B);
  });

  it("falls back to the repo→tenant inference when there is no override", () => {
    expect(
      resolveSpawnTenant({ override: null, inferred: B, defaultForNewSessions: A, candidates }),
    ).toBe(B);
  });

  it("falls back to the device default for new sessions when inference found nothing", () => {
    expect(resolveSpawnTenant({ inferred: null, defaultForNewSessions: A, candidates })).toBe(A);
  });

  it("DISCARDS an inferred tenant the device is not bound to", () => {
    // coord can know a repo belongs to a tenant this device has no binding
    // (and therefore no credential) for. Pre-selecting it would yield a
    // session that cannot authenticate.
    const unbound = "ffffffff-0000-4000-8000-00000000ffff";
    expect(resolveSpawnTenant({ inferred: unbound, defaultForNewSessions: A, candidates })).toBe(A);
  });

  it("DISCARDS an override the device is not bound to", () => {
    const unbound = "ffffffff-0000-4000-8000-00000000ffff";
    expect(resolveSpawnTenant({ override: unbound, defaultForNewSessions: A, candidates })).toBe(A);
  });

  it("returns undefined on an unpaired device so the caller omits tenant_id", () => {
    expect(resolveSpawnTenant({ defaultForNewSessions: null, candidates: [] })).toBeUndefined();
  });
});

describe("pickSpawnTenant (only an explicit choice is sent — re-review F1)", () => {
  it("sends NO tenant when nothing was explicitly chosen, whatever the device default", () => {
    // A plain shell tab must never be refused for a tenant it did not ask for.
    expect(pickSpawnTenant({ spawnTenantId: null })).toBeUndefined();
    expect(pickSpawnTenant({})).toBeUndefined();
    // A caller still holding the device default must not get it sent: the
    // function takes no default any more, and one smuggled in is ignored.
    const withDefault = {
      spawnTenantId: null,
      defaultTenantIdForNewSessions: A,
    } as unknown as Parameters<typeof pickSpawnTenant>[0];
    expect(pickSpawnTenant(withDefault)).toBeUndefined();
  });

  it("sends the picker's explicit pick", () => {
    expect(pickSpawnTenant({ spawnTenantId: B })).toBe(B);
  });

  it("prefers an explicit per-invocation tenant (the /spawn-ai --tenant flag) over the pick", () => {
    expect(pickSpawnTenant({ explicit: B, spawnTenantId: A })).toBe(B);
  });

  it("ignores a blank / whitespace explicit tenant and falls through to the pick, then to none", () => {
    expect(pickSpawnTenant({ explicit: "   ", spawnTenantId: A })).toBe(A);
    expect(pickSpawnTenant({ explicit: "   ", spawnTenantId: null })).toBeUndefined();
  });
});

describe("explicitSpawnTenant (what the picker publishes — re-review F1)", () => {
  it("publishes nothing without a pick, even when a default or inference is displayed", () => {
    expect(explicitSpawnTenant({ override: null, candidates: [A, B] })).toBeNull();
    expect(resolveSpawnTenant({ inferred: B, defaultForNewSessions: A, candidates: [A, B] })).toBe(
      B,
    );
  });

  it("publishes the operator's pick when the device is bound to it", () => {
    expect(explicitSpawnTenant({ override: A, candidates: [A, B] })).toBe(A);
  });

  it("publishes nothing for a pick the device is not bound to", () => {
    expect(
      explicitSpawnTenant({ override: "ffffffff-0000-4000-8000-00000000ffff", candidates: [A, B] }),
    ).toBeNull();
  });
});

describe("spawnTenantPickerModel (the picker shows what it sends — re-review P1)", () => {
  // Repo inference B, device default A: the case where the old picker showed B
  // while the spawn stamped A.
  const base = { inferred: B, defaultForNewSessions: A, candidates: [A, B] };

  it("with no pick, shows the device default option and sends no tenant", () => {
    const model = spawnTenantPickerModel({ ...base, override: null });
    expect(model.value).toBe("");
    expect(model.options[0]).toEqual({ value: "", label: "device default (6b1f4b0e)" });
    expect(model.published).toBeNull();
    expect(pickSpawnTenant({ spawnTenantId: model.published })).toBeUndefined();
  });

  it("labels the repo inference as a hint, not a selected value", () => {
    const model = spawnTenantPickerModel({ ...base, override: null });
    expect(model.options.find((o) => o.value === B)?.label).toBe("91ffaa20 (repo)");
    expect(model.value).not.toBe(B);
  });

  it("picking the inferred B publishes B, shows B, and sends B", () => {
    const model = spawnTenantPickerModel({ ...base, override: B });
    expect(model.value).toBe(B);
    expect(model.published).toBe(B);
    expect(pickSpawnTenant({ spawnTenantId: model.published })).toBe(B);
  });
});
