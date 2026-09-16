/**
 * `overlappingIntentsApi` — the Tauri command contract and the error
 * describer that moved here with it when `coordinatorApi.ts` was deleted
 * (plan `2026-09-12-consolidate-local-orchestration-onto-conductor` Phase 4).
 *
 * The runner's vitest config is `environment: "node"` (no jsdom), so
 * `@tauri-apps/api/core`'s `invoke` is mocked and the assertions are on the
 * command name + argument shape and on the pure describer.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

import { invoke } from "@tauri-apps/api/core";
import { describeThrown, listOverlappingIntents } from "./overlappingIntentsApi";

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

describe("describeThrown", () => {
  it("THE REGRESSION: a plain-string rejection (what invoke() throws) keeps its text", () => {
    expect(
      describeThrown("coord unreachable: connection refused", "Failed to load overlap pairs"),
    ).toBe("coord unreachable: connection refused");
  });

  it("an Error still yields its message", () => {
    expect(describeThrown(new Error("boom"), "fallback")).toBe("boom");
  });

  it("a status + body object serializes BOTH halves", () => {
    expect(describeThrown({ status: 500, error: "list_overlapping_intents failed" }, "fb")).toBe(
      "HTTP 500: list_overlapping_intents failed",
    );
  });

  it("a status-only or body-only object still surfaces what it has", () => {
    expect(describeThrown({ status: 404 }, "fb")).toBe("HTTP 404");
    expect(describeThrown({ message: "no such command" }, "fb")).toBe("no such command");
    expect(describeThrown({ code: "E_NOENT" }, "fb")).toBe("E_NOENT");
  });

  it("an unrecognised object is DUMPED alongside the fallback rather than dropped", () => {
    expect(describeThrown({ weird: 1 }, "Failed to load overlap pairs")).toBe(
      'Failed to load overlap pairs ({"weird":1})',
    );
  });

  it("only a genuinely empty value falls back to the bare constant", () => {
    expect(describeThrown(null, "fb")).toBe("fb");
    expect(describeThrown(undefined, "fb")).toBe("fb");
    expect(describeThrown("   ", "fb")).toBe("fb");
    expect(describeThrown({}, "fb")).toBe("fb");
  });

  it("survives a circular object instead of throwing inside the error path", () => {
    const circular: Record<string, unknown> = {};
    circular.self = circular;
    expect(describeThrown(circular, "fb")).toBe("fb");
  });
});
