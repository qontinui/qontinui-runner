/**
 * F3 — `/spawn-ai --tenant` argument handling.
 *
 * Runs under vitest's `environment: "node"` (no jsdom), so these exercise the
 * exported pure helpers rather than the hook — same split the rest of the
 * `commands/` specs use.
 */

import { describe, expect, it } from "vitest";

import { resolveTenantArg, splitTenantFlag } from "./useTerminalCommands";

const A = "6b1f4b0e-0000-4000-8000-000000000001";
const B = "6b1f4b0e-0000-4000-8000-000000000002";
const C = "91ffaa20-0000-4000-8000-000000000003";

describe("splitTenantFlag", () => {
  it("returns nothing for an absent context", () => {
    expect(splitTenantFlag(undefined)).toEqual({});
  });

  it("passes a flagless context through untouched", () => {
    expect(splitTenantFlag("fix the failing test")).toEqual({ context: "fix the failing test" });
  });

  it("extracts a leading `--tenant value` and strips it from the prompt", () => {
    expect(splitTenantFlag("--tenant pizzeria fix the bug")).toEqual({
      tenant: "pizzeria",
      context: "fix the bug",
    });
  });

  it("extracts a trailing `--tenant=value`", () => {
    expect(splitTenantFlag("fix the bug --tenant=pizzeria")).toEqual({
      tenant: "pizzeria",
      context: "fix the bug",
    });
  });

  it("drops the context entirely when the flag was the whole of it", () => {
    expect(splitTenantFlag("--tenant pizzeria")).toEqual({
      tenant: "pizzeria",
      context: undefined,
    });
  });

  it("leaves a bare `--tenant` with no value alone", () => {
    expect(splitTenantFlag("--tenant")).toEqual({ context: "--tenant" });
  });
});

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
