/**
 * F3 — `/spawn-ai --tenant` argument handling.
 *
 * The `splitTenantFlag` block that used to sit here is gone with the helper
 * it covered: recovering a declared flag out of a free-form `context` was a
 * per-flag patch for a per-route defect. Flag extraction is route-independent
 * now (`parse.ts::applyDeclaredFlags`), and its contract is covered in
 * `resolve.test.ts` for every declared flag rather than for this one.
 *
 * Runs under vitest's `environment: "node"` (no jsdom), so these exercise the
 * exported pure helpers rather than the hook — same split the rest of the
 * `commands/` specs use.
 */

import { describe, expect, it } from "vitest";

import { resolveTenantArg } from "./useTerminalCommands";

const A = "6b1f4b0e-0000-4000-8000-000000000001";
const B = "6b1f4b0e-0000-4000-8000-000000000002";
const C = "91ffaa20-0000-4000-8000-000000000003";

describe("resolveTenantArg", () => {
  const candidates = [A, B, C];

  it("treats an absent/blank value as 'no tenant requested', not an error", () => {
    expect(resolveTenantArg(undefined, candidates)).toEqual({});
    expect(resolveTenantArg("   ", candidates)).toEqual({});
  });

  it("accepts an exact tenant id", () => {
    expect(resolveTenantArg(A, candidates)).toEqual({ tenantId: A });
  });

  it("accepts a unique prefix — the short form the badge and picker display", () => {
    expect(resolveTenantArg("91ffaa20", candidates)).toEqual({ tenantId: C });
  });

  it("is case-insensitive on the prefix", () => {
    expect(resolveTenantArg("91FFAA20", candidates)).toEqual({ tenantId: C });
  });

  it("REFUSES an ambiguous prefix rather than guessing a binding", () => {
    const { tenantId, error } = resolveTenantArg("6b1f4b0e", candidates);
    expect(tenantId).toBeUndefined();
    expect(error).toContain("ambiguous");
  });

  it("REFUSES an unknown value rather than falling back to the active pin", () => {
    const { tenantId, error } = resolveTenantArg("pizzeria", candidates);
    expect(tenantId).toBeUndefined();
    expect(error).toContain("no tenant binding matches");
  });

  it("refuses any value when the device has no bindings", () => {
    expect(resolveTenantArg(A, []).error).toBeTruthy();
  });
});
