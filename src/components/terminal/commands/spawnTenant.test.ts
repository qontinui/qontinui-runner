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

import { resolveTenantArg, tenantFirstArgs, type TenantSlug } from "./useTerminalCommands";
import { slugsFromWire } from "./tenantSlugs";

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

describe("resolveTenantArg — tenant names", () => {
  const BAUHAUS = "ce1ec20c-9cf1-427a-857d-81d6449cc6e4";
  const QONTINUI = "c231d9da-0ca8-4fe4-bd81-0e3d6c20339a";
  const MERYTS = "b3ecb579-2dea-47c4-b03e-a23e0ce7e3a5";
  const PIZZA = "7ac125b6-391b-4d64-8493-27305b25c5b9";
  const candidates = [BAUHAUS, QONTINUI, MERYTS, PIZZA];
  const slugs: TenantSlug[] = [
    { tenantId: BAUHAUS, slug: "bauhaus-b2b" },
    { tenantId: QONTINUI, slug: "qontinui" },
    { tenantId: MERYTS, slug: "meryts-2-0" },
    { tenantId: PIZZA, slug: "pizzeria" },
  ];

  it("accepts the full slug, case-insensitively", () => {
    expect(resolveTenantArg("Bauhaus-B2B", candidates, slugs)).toEqual({ tenantId: BAUHAUS });
  });

  it("accepts any unique substring of a slug", () => {
    expect(resolveTenantArg("bauhaus", candidates, slugs)).toEqual({ tenantId: BAUHAUS });
    expect(resolveTenantArg("zeri", candidates, slugs)).toEqual({ tenantId: PIZZA });
    expect(resolveTenantArg("2-0", candidates, slugs)).toEqual({ tenantId: MERYTS });
  });

  it("still accepts an id prefix when slugs are known", () => {
    expect(resolveTenantArg("ce1ec20c", candidates, slugs)).toEqual({ tenantId: BAUHAUS });
  });

  it("an exact slug wins over longer slugs that contain it", () => {
    const more = [...slugs, { tenantId: "11111111-0000-4000-8000-000000000001", slug: "qontinui-labs" }];
    const cands = [...candidates, "11111111-0000-4000-8000-000000000001"];
    expect(resolveTenantArg("qontinui", cands, more)).toEqual({ tenantId: QONTINUI });
  });

  it("REFUSES a substring that matches two slugs, naming both", () => {
    const { tenantId, error } = resolveTenantArg("u", candidates, slugs);
    expect(tenantId).toBeUndefined();
    expect(error).toContain("ambiguous");
    expect(error).toContain("bauhaus-b2b");
    expect(error).toContain("qontinui");
  });

  it("REFUSES a value that is an id prefix of one tenant and a slug substring of another", () => {
    // `b3ec` prefixes MERYTS's id; a slug containing it belongs to another tenant.
    const tricky = [
      { tenantId: MERYTS, slug: "meryts-2-0" },
      { tenantId: BAUHAUS, slug: "xb3ecx" },
    ];
    expect(resolveTenantArg("b3ec", candidates, tricky).error).toContain("ambiguous");
  });

  it("ignores the slug of a tenant this device is not bound to", () => {
    const foreign = [{ tenantId: "99999999-0000-4000-8000-000000000009", slug: "stranger" }];
    expect(resolveTenantArg("stranger", candidates, foreign).error).toContain("no tenant binding");
  });

  it("an unknown name still refuses without slugs (names unread)", () => {
    expect(resolveTenantArg("bauhaus", candidates).error).toContain("no tenant binding");
  });
});

describe("slugsFromWire", () => {
  it("maps rows and drops malformed or blank ones", () => {
    expect(
      slugsFromWire([
        { tenant_id: "a", slug: " x " },
        { tenant_id: "b", slug: "" },
        { tenant_id: 3, slug: "y" },
        null,
      ]),
    ).toEqual([{ tenantId: "a", slug: "x" }]);
    expect(slugsFromWire(undefined)).toEqual([]);
  });
});

describe("tenantFirstArgs — `/spawn-ai bau`", () => {
  const labels = ["gmail", "hotmail"];

  it("reads a lone non-numeric word as the tenant", () => {
    expect(tenantFirstArgs({ count: "bau" }, labels)).toEqual({ tenant: "bau" });
  });

  it("keeps count, account and context after the tenant", () => {
    expect(tenantFirstArgs({ count: "bau", account: "3", context: "gmail fix the build" }, labels)).toEqual({
      tenant: "bau",
      count: "3",
      account: "gmail",
      context: "fix the build",
    });
  });

  it("takes an account only when it is a known label or best", () => {
    expect(tenantFirstArgs({ count: "bau", account: "best" }, labels)).toEqual({ tenant: "bau", account: "best" });
    expect(tenantFirstArgs({ count: "bau", account: "fix the build" }, labels)).toEqual({
      tenant: "bau",
      context: "fix the build",
    });
  });

  it("does not apply to a numeric first word, an absent one, or when --tenant is given", () => {
    expect(tenantFirstArgs({ count: "3", account: "gmail" }, labels)).toBeNull();
    expect(tenantFirstArgs({}, labels)).toBeNull();
    expect(tenantFirstArgs({ count: "bau", tenant: "x" }, labels)).toBeNull();
  });
});

describe("review fixes", () => {
  const BAUHAUS = "ce1ec20c-9cf1-427a-857d-81d6449cc6e4";
  const PIZZA = "7ac125b6-391b-4d64-8493-27305b25c5b9";
  const A = "b3ecb579-2dea-47c4-b03e-a23e0ce7e3a5";

  it("an exact slug that is also another tenant's id prefix is ambiguous, not silently chosen", () => {
    const r = resolveTenantArg("b3ec", [A, BAUHAUS], [{ tenantId: BAUHAUS, slug: "b3ec" }]);
    expect(r.error).toContain("ambiguous");
  });

  it("matches ids case-insensitively against slugs and returns the candidate's own spelling", () => {
    const r = resolveTenantArg("bauhaus", [BAUHAUS], [{ tenantId: BAUHAUS.toUpperCase(), slug: "bauhaus-b2b" }]);
    expect(r).toEqual({ tenantId: BAUHAUS });
  });

  it("tenantFirstArgs leaves number-like and very short first words alone", () => {
    expect(tenantFirstArgs({ count: "2.5" }, [])).toBeNull();
    expect(tenantFirstArgs({ count: "ba" }, [])).toBeNull();
    expect(tenantFirstArgs({ count: "bau" }, [])).toEqual({ tenant: "bau" });
  });
});
