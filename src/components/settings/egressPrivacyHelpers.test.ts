/**
 * Tests for the Privacy section's outbound-flow helpers. Node environment (no
 * jsdom), so the honesty rules are exercised through the pure helpers — the
 * `sessionBriefingHelpers.test.ts` precedent.
 */

import { describe, it, expect } from "vitest";

import {
  buildEgressRows,
  describeEgressSource,
  EGRESS_FLOWS,
  egressScope,
  isUpdateEgressOff,
  tenantPolicyLink,
} from "./egressPrivacyHelpers";

const report = (over: Record<string, unknown> = {}) => ({
  allowed: true,
  source: "product_default",
  domain: "egress_x",
  applies_at_next_start: false,
  refused: 0,
  ...over,
});

describe("buildEgressRows", () => {
  it("always renders the six flows in the runner's order", () => {
    const rows = buildEgressRows(undefined);
    expect(rows.map((r) => r.key)).toEqual([
      "transcript_sync",
      "code_mirror",
      "terminal_stream",
      "telemetry",
      "update_check",
      "skill_mirror",
    ]);
    expect(EGRESS_FLOWS).toHaveLength(6);
  });

  it("renders a failed read as UNKNOWN, never as on", () => {
    for (const row of buildEgressRows(undefined)) {
      expect(row.state).toBe("unknown");
    }
    const partial = buildEgressRows({ code_mirror: report({ allowed: false }) });
    expect(partial.find((r) => r.key === "code_mirror")?.state).toBe("off");
    expect(partial.find((r) => r.key === "telemetry")?.state).toBe("unknown");
    // A malformed entry is not a report.
    const malformed = buildEgressRows({ telemetry: { allowed: "yes" } });
    expect(malformed.find((r) => r.key === "telemetry")?.state).toBe("unknown");
  });

  it("names the rung that decided each state", () => {
    const rows = buildEgressRows({
      code_mirror: report({ allowed: false, source: "coord", refused: 3 }),
      skill_mirror: report({ source: "profile" }),
    });
    const mirror = rows.find((r) => r.key === "code_mirror")!;
    expect(mirror.state).toBe("off");
    expect(mirror.sourceText).toBe(describeEgressSource("coord"));
    const decided = buildEgressRows({
      code_mirror: report({ source: "coord", decided_by: "tenant_row" }),
      update_check: report({ source: "persisted", decided_by: "deployment_profile" }),
    });
    expect(decided.find((r) => r.key === "code_mirror")!.sourceText).toBe(
      "set for this project in the web console",
    );
    expect(decided.find((r) => r.key === "update_check")!.sourceText).toContain(
      "this deployment's default",
    );
    expect(mirror.refused).toBe(3);
    expect(rows.find((r) => r.key === "skill_mirror")!.sourceText).toBe(
      "this machine's profile default",
    );
  });

  it("says 'applies at next start' where the runner says so, and what is in effect", () => {
    const rows = buildEgressRows({
      telemetry: report({ allowed: false, applies_at_next_start: true, in_effect: true }),
      update_check: report(),
    });
    expect(rows.find((r) => r.key === "telemetry")!.nextStartNote).toBe(
      "applies at next start (currently on)",
    );
    expect(rows.find((r) => r.key === "update_check")!.nextStartNote).toBeNull();
    const settled = buildEgressRows({
      telemetry: report({ applies_at_next_start: true, in_effect: null }),
    });
    expect(settled.find((r) => r.key === "telemetry")!.nextStartNote).toBe("applies at next start");
  });
});

describe("unknown and legacy sources", () => {
  it("says an unanswered project is refused, not on", () => {
    const rows = buildEgressRows({
      code_mirror: report({ allowed: false, source: "unknown", decided_by: null }),
    });
    const row = rows.find((r) => r.key === "code_mirror")!;
    expect(row.state).toBe("off");
    expect(row.sourceText).toContain("refused until it does");
  });
  it("names a migrated answer", () => {
    expect(describeEgressSource("persisted", "legacy_store")).toContain("earlier runner");
  });
});

describe("tenantPolicyLink", () => {
  it("deep-links into the web tenant-policy panel", () => {
    expect(tenantPolicyLink("https://example.test/", "code_mirror")).toBe(
      "https://example.test/admin/coord/tenant-policy#egress-code_mirror",
    );
  });
  it("is null without a web origin", () => {
    expect(tenantPolicyLink(null, "telemetry")).toBeNull();
  });
});

describe("isUpdateEgressOff", () => {
  it("recognises the runner's egress refusal and nothing else", () => {
    expect(isUpdateEgressOff({ status: "egress_off", available: false })).toBe(true);
    expect(isUpdateEgressOff({ available: false })).toBe(false);
    expect(isUpdateEgressOff(undefined)).toBe(false);
  });
});

describe("egressScope", () => {
  it("reads the polled scope and its note, and nothing from a failed read", () => {
    expect(egressScope(undefined)).toBeNull();
    expect(
      egressScope({ scope: { coord_base: "x", tenant_id: "t-1", note: "default tenant only" } }),
    ).toEqual({ tenantId: "t-1", note: "default tenant only" });
  });

  it("does not count the scope as a seventh flow", () => {
    const rows = buildEgressRows({ scope: { note: "n" } });
    expect(rows).toHaveLength(6);
  });

  it("says AI session output rides the terminal streaming switch", () => {
    const flow = EGRESS_FLOWS.find((f) => f.key === "terminal_stream")!;
    expect(flow.description).toContain("AI session output");
  });
});
