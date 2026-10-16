/**
 * useSpecSync — behaviour of the AI-response parsing step the sync effect
 * runs on each finished turn (`parseSpecSyncResponse`, which goes through the
 * shared `extractJsonBlock`).
 *
 * The runner's vitest config is `environment: "node"` (no jsdom / RTL), so
 * the effect itself is not rendered; this is the seam it delegates to.
 */

import { describe, it, expect } from "vitest";

import { parseSpecSyncResponse } from "./useSpecSync";

describe("parseSpecSyncResponse", () => {
  it("parses a ```json block into the spec config", () => {
    const ai = 'Merged spec:\n\n```json\n{ "id": "settings", "groups": [] }\n```\n';
    expect(parseSpecSyncResponse(ai)).toEqual({
      kind: "parsed",
      config: { id: "settings", groups: [] },
    });
  });

  it("parses a bare ``` fence holding the spec object (no language tag)", () => {
    const ai =
      'Here you go:\n\n```\n{\n  "id": "runs",\n  "stateMachine": { "states": [] }\n}\n```';
    expect(parseSpecSyncResponse(ai)).toEqual({
      kind: "parsed",
      config: { id: "runs", stateMachine: { states: [] } },
    });
  });

  it("reports a turn with no JSON block", () => {
    expect(parseSpecSyncResponse("I could not produce a spec for this page.")).toEqual({
      kind: "no-json-block",
    });
  });
});
