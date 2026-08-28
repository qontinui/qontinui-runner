/**
 * Tests for `StewardControl`'s pure helpers.
 *
 * The runner's vitest config uses `environment: "node"` — no jsdom and no
 * `@testing-library/react` (see `LaunchMenu.test.tsx` for precedent). The
 * component's fetch/poll/render logic is exercised manually via UI Bridge
 * (it lives in `SessionManagerPanel`, mounted in the live component tree);
 * here we lock down the wire-shape helpers.
 */

import { describe, it, expect } from "vitest";
import { buildStewardUrl, buildStewardsUrl } from "./StewardControl";

describe("buildStewardsUrl", () => {
  it("targets the runner's local /stewards roster endpoint", () => {
    expect(buildStewardsUrl(9876)).toBe("http://127.0.0.1:9876/stewards");
  });
});

describe("buildStewardUrl", () => {
  it("targets the runner's local per-kind /status endpoint", () => {
    expect(buildStewardUrl(9876, "merge-train", "status")).toBe(
      "http://127.0.0.1:9876/steward/merge-train/status",
    );
  });

  it("targets the runner's local per-kind /start endpoint", () => {
    expect(buildStewardUrl(9876, "dev-ops", "start")).toBe(
      "http://127.0.0.1:9876/steward/dev-ops/start",
    );
  });

  it("targets the runner's local per-kind /stop endpoint", () => {
    expect(buildStewardUrl(9876, "cleanup", "stop")).toBe(
      "http://127.0.0.1:9876/steward/cleanup/stop",
    );
  });

  it("keeps each kind on its own path, so one steward's button cannot hit another", () => {
    // The kind must actually reach the URL. A helper that ignored its `kind`
    // argument — or interpolated a constant — would collapse these to one
    // string and let a second steward's button act on the first.
    const start = (kind: string) => buildStewardUrl(9876, kind, "start");
    expect(start("merge-train")).not.toBe(start("dev-ops"));
    expect(start("dev-ops")).not.toBe(start("cleanup"));
  });

  it("distinguishes the three actions for a single kind", () => {
    // Likewise `path` must reach the URL: a helper that dropped it would
    // make Stop call Start.
    const urls = new Set([
      buildStewardUrl(9876, "dev-ops", "status"),
      buildStewardUrl(9876, "dev-ops", "start"),
      buildStewardUrl(9876, "dev-ops", "stop"),
    ]);
    expect(urls.size).toBe(3);
  });
});
