// @vitest-environment jsdom
/**
 * Parity: the relay `design_run_audit` answers through the SDK's shared
 * `designAuditOf` (what the web and native transports run), not a local
 * copy. With no guide it is the heuristic audit and carries
 * `heuristicCoverage` and the checked / unreadable accounting; with a guide it is `style-guide`.
 */

import { readFileSync } from "node:fs";
import { describe, it, expect, afterEach } from "vitest";
import { designAuditOf } from "@qontinui/ui-bridge";

afterEach(() => {
  document.body.innerHTML = "";
});

function source() {
  const el = document.createElement("button");
  el.id = "b";
  document.body.appendChild(el);
  return [{ id: "b", element: el, type: "button", label: "B" }];
}

describe("design_run_audit parity with the SDK audit", () => {
  it("heuristic mode reports coverage like web/native", () => {
    const report = designAuditOf(source(), {}, null) as unknown as Record<string, unknown>;
    expect(report.mode).toBe("heuristic");
    expect(report.heuristicCoverage).toBeDefined();
    expect(report).toHaveProperty("checkedElements");
    expect(report).toHaveProperty("unreadableElementCount");
  });

  it("style-guide mode when a guide is loaded", () => {
    const guide = { name: "g", rules: [] } as never;
    const report = designAuditOf(source(), {}, guide) as unknown as Record<string, unknown>;
    expect(report.mode).toBe("style-guide");
  });

  it("the handler no longer carries a local audit copy", () => {
    const src = readFileSync("src/hooks/ui-bridge-events/useDesignEvents.ts", "utf8");
    const audit = src.slice(src.indexOf('case "design_run_audit"'), src.indexOf('case "design_load_style_guide"'));
    expect(audit).toContain("designAuditOf(");
    expect(audit).not.toContain("runStyleAudit(");
    expect(audit).not.toContain("collectDesignData(");
  });
});
