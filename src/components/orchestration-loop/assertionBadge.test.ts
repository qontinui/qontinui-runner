import { describe, expect, it } from "vitest";

import { assertionBadge } from "./assertionBadge";

describe("assertionBadge", () => {
  it("paints a passing verdict green, labelled by assertion and query", () => {
    const badge = assertionBadge({ passed: true, assertion: "exists", query: "#ok" }, 0);
    expect(badge.tone).toBe("pass");
    expect(badge.label).toBe("exists #ok");
    expect(badge.mark).toBe(" ✓");
  });

  it("paints a failing verdict red", () => {
    const badge = assertionBadge({ passed: false, assertion: "count", query: "li" }, 1);
    expect(badge.tone).toBe("fail");
    expect(badge.label).toBe("count li");
    expect(badge.mark).toBe(" ✗");
  });

  it("renders a no-verdict entry neutral, never red, naming what the route said", () => {
    const badge = assertionBadge(
      {
        evaluated: false,
        error: "Assertion produced no verdict (HTTP 422: unknown field `role`)",
        status: 422,
        code: "INVALID_REQUEST",
        assertion: { type: "visible", role: "tab" },
      },
      2,
    );
    expect(badge.tone).toBe("noVerdict");
    expect(badge.label).toBe("#3");
    expect(badge.title).toBe(
      "No verdict (HTTP 422 INVALID_REQUEST): Assertion produced no verdict (HTTP 422: unknown field `role`)",
    );
  });

  it("labels a no-verdict entry by the request it echoes, and says when there was no status", () => {
    const badge = assertionBadge(
      {
        evaluated: false,
        error: "Failed to run assertion: connection refused",
        status: null,
        code: null,
        assertion: { query: ".status", assertion: "hasText", expected: "Ready" },
      },
      0,
    );
    expect(badge.tone).toBe("noVerdict");
    expect(badge.label).toBe("hasText .status");
    expect(badge.title).toBe("No verdict (no HTTP status): Failed to run assertion: connection refused");
  });
});
