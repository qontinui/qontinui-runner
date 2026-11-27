/**
 * Shared builders for the session-info tests: a full `SessionInfoBody`, its
 * PR ledger, and the multi-bound tenancy block the Tenant row renders from.
 *
 * Both `useSessionInfo.test.ts` (the pure row helpers) and
 * `SessionInfoDropdown.render.test.tsx` (the real dropdown in jsdom) build
 * their input here, so the render test reads the SAME tenancy that the
 * row-title unit test pins. A builder copied into each file could drift, and
 * the render test would then pin a title no unit test asserts (plan
 * `2026-11-12-session-info-row-title-is-unreachable-through-the-ui-bridge`,
 * Phase 2).
 *
 * Test-only. The `.testkit.ts` suffix is the repo's test-support marker:
 * `isFixtureFile` (`src/lib/ui-bridge/actionSurfaces.ts`) classifies the file,
 * so the action-surface scan counts its surfaces without judging them, and
 * `actionSurfaces.enforcement.test.ts` fails if an app module names it in a
 * static `import … from`.
 */

import type { SessionInfoBody, SessionPrs, SessionTenancy, TenantDisplay } from "./useSessionInfo";

export function prs(overrides: Partial<SessionPrs> = {}): SessionPrs {
  const base: SessionPrs = {
    status: "ok",
    reason: null,
    opened: [],
    landed: [],
    unknown: [],
    openCount: 0,
    landedCount: 0,
    unknownCount: 0,
    // Default to SCANNED. Only the `Repos scanned` row and the panel's
    // empty-PR-list line depend on `scanned`, and the not-scanned case has
    // its own tests in useSessionInfo.test.ts.
    scanned: true,
    scannedRepos: ["D:/qontinui-root/qontinui-runner"],
  };
  const merged = { ...base, ...overrides };
  return {
    ...merged,
    openCount: overrides.openCount ?? merged.opened.length,
    landedCount: overrides.landedCount ?? merged.landed.length,
    unknownCount: overrides.unknownCount ?? merged.unknown.length,
  };
}

export function body(overrides: Partial<SessionInfoBody> = {}): SessionInfoBody {
  return {
    identity: {
      claudeSessionId: "8b45800b-4b65-4c15-afd4-d0feaa0cb0d4",
      terminalId: "term-1",
      fleetSessionHandle: "fsh_abc",
      tenantId: null,
      taskRunId: null,
    },
    name: { value: "runner dropdown", source: "operator" },
    account: { label: "paktis", wrapper: "clp", configDir: "C:/Users/x/.claude-paktis" },
    placement: { pageId: "default", zoneIndex: 3, workingDir: "C:/qontinui-root/qontinui-runner" },
    lifecycle: {
      state: "open",
      provider: "claude",
      origin: "authoritative",
      openedAt: 1_700_000_000_000,
      lastSeenAt: 1_700_000_100_000,
      closedAt: null,
      closeReason: null,
      confirmed: true,
      transcriptExists: true,
      restorable: true,
      restoredFromBootAt: null,
      restoreTier: null,
      bypassPermissions: false,
    },
    prs: prs(),
    tenancy: null,
    ...overrides,
  };
}

export const QONTINUI = "c231d9da-0ca8-4fe4-bd81-0e3d6c20339a";
export const MERYTS = "11111111-1111-4111-8111-111111111111";
export const BAUHAUS = "22222222-2222-4222-8222-222222222222";
export const PIZZERIA = "33333333-3333-4333-8333-333333333333";

export function display(overrides: Partial<TenantDisplay> = {}): TenantDisplay {
  return {
    status: "resolved",
    tenantId: QONTINUI,
    tenantSlug: null,
    source: "credential",
    reason: null,
    conflicts: [],
    conflictDetail: null,
    bound: { status: "measured", tenantIds: [MERYTS, QONTINUI, BAUHAUS, PIZZERIA], reason: null },
    multiBound: true,
    actingInBoundSet: true,
    ...overrides,
  };
}

/** A tenancy block with no spawn stamp — the ordinary multi-bound spawn. */
export function tenancyWith(
  d: TenantDisplay | null,
  rowTenant: string | null = null,
): SessionTenancy {
  return {
    row: {
      tenantId: rowTenant,
      spawnDeviceDefaultTenantId: QONTINUI,
      spawnDeviceDefaultStatus: "recorded",
      spawnDeviceDefaultReason: null,
      currentDeviceDefaultTenantId: QONTINUI,
    },
    dataPlane: { status: "owned", tenantId: QONTINUI, reason: null },
    credential: {
      status: "resolved",
      tenantId: QONTINUI,
      slot: "tenant",
      reason: null,
      posture: { status: "observed", value: "live", canAnswer: true, reason: null },
    },
    repoExpected: null,
    coordRow: { status: "registered", tenantId: QONTINUI, source: "registrar", reason: null },
    display: d,
    diverged: d?.status === "mismatch",
    divergence: d?.status === "mismatch" ? "diverged" : "agree",
  };
}
