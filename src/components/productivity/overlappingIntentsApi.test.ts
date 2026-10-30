/**
 * `overlappingIntentsApi` — the Tauri command contract (moved here when `coordinatorApi.ts` was deleted, plan
 * `2026-09-12-consolidate-local-orchestration-onto-conductor` Phase 4).
 *
 * The runner's vitest config is `environment: "node"` (no jsdom), so
 * `@tauri-apps/api/core`'s `invoke` is mocked and the assertions are on the
 * command name + argument shape.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

import { invoke } from "@tauri-apps/api/core";
import { listOverlappingIntents } from "./overlappingIntentsApi";

const mockInvoke = invoke as ReturnType<typeof vi.fn>;

describe("listOverlappingIntents", () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });
  afterEach(() => {
    vi.clearAllMocks();
  });

  it("invokes list_overlapping_intents with the default limit and returns the rows verbatim", async () => {
    const rows = [
      {
        agentA: "a",
        agentB: "b",
        intentA: "x",
        intentB: null,
        overlappingPaths: ["src/lib/foo.ts"],
      },
    ];
    mockInvoke.mockResolvedValueOnce(rows);
    await expect(listOverlappingIntents()).resolves.toEqual(rows);
    expect(mockInvoke).toHaveBeenCalledWith("list_overlapping_intents", { limit: 200 });
  });

  it("forwards an explicit limit", async () => {
    mockInvoke.mockResolvedValueOnce([]);
    await listOverlappingIntents(5);
    expect(mockInvoke).toHaveBeenCalledWith("list_overlapping_intents", { limit: 5 });
  });

  it("propagates a rejection rather than swallowing it into an empty list", async () => {
    mockInvoke.mockRejectedValueOnce("coord unreachable: connection refused");
    await expect(listOverlappingIntents()).rejects.toBe("coord unreachable: connection refused");
  });
});
