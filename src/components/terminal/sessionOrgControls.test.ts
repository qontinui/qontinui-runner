import { describe, expect, it } from "vitest";

import { GROUP_HEADER_ACTIONS, SESSION_ORG_CONTROLS } from "./sessionOrgControls";

describe("SESSION_ORG_CONTROLS", () => {
  it("declares every close path destructive (D7)", () => {
    for (const name of [
      "groupMenuCloseGroup",
      "sessionMenuClose",
      "closeConfirm",
      "forceCloseConfirm",
      "bulkClose",
      "readyToCloseReview",
    ] as const) {
      expect(SESSION_ORG_CONTROLS[name].effect, name).toBe("destructive");
    }
  });
});

describe("GROUP_HEADER_ACTIONS", () => {
  // The control route refuses any action an element does not advertise, and
  // a div's inferred set has neither: the group menu and inline rename were
  // unreachable over the UI Bridge until the header declared them.
  it("advertises right-click (group menu) and double-click (rename)", () => {
    expect(GROUP_HEADER_ACTIONS).toContain("rightClick");
    expect(GROUP_HEADER_ACTIONS).toContain("doubleClick");
  });
});
