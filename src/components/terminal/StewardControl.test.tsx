/**
 * Tests for `StewardControl`'s pure helper.
 *
 * The runner's vitest config uses `environment: "node"` — no jsdom and no
 * `@testing-library/react` (see `LaunchMenu.test.tsx` for precedent). The
 * component's fetch/poll/render logic is exercised manually via UI Bridge
 * (it lives in `SessionManagerPanel`, mounted in the live component tree);
 * here we lock down the wire-shape helper.
 */

import { describe, it, expect } from "vitest";
import { buildStewardUrl } from "./StewardControl";

describe("buildStewardUrl", () => {
  it("targets the runner's local /steward/status endpoint", () => {
    expect(buildStewardUrl(9876, "status")).toBe("http://127.0.0.1:9876/steward/status");
  });

  it("targets the runner's local /steward/start endpoint", () => {
    expect(buildStewardUrl(9876, "start")).toBe("http://127.0.0.1:9876/steward/start");
  });

  it("targets the runner's local /steward/stop endpoint", () => {
    expect(buildStewardUrl(9876, "stop")).toBe("http://127.0.0.1:9876/steward/stop");
  });
});
