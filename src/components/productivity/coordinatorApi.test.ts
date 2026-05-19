/**
 * Pure-logic tests for `coordinatorApi.spawnFromPlan`.
 *
 * Plan `2026-05-19-coordinator-production-readiness.md` Phase 4 (Wave 4).
 *
 * The runner's vitest config is `environment: "node"` (no jsdom). The
 * function under test is a thin fetch wrapper — we mock global `fetch`
 * and `@tauri-apps/api/core` and assert that the request body matches
 * the coord wire contract.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Mock the Tauri core API import — coordinatorApi.ts has `invoke` at
// module scope, so the import has to resolve in node env even though
// `spawnFromPlan` itself never uses it.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

import { spawnFromPlan } from "./coordinatorApi";

describe("spawnFromPlan", () => {
  const originalFetch = global.fetch;

  beforeEach(() => {
    global.fetch = vi.fn();
  });

  afterEach(() => {
    global.fetch = originalFetch;
    vi.restoreAllMocks();
  });

  it("posts the coord wire contract to /agents/spawn", async () => {
    const mockResponse = {
      ok: true,
      status: 200,
      json: async () => ({
        agentId: "agent-deadbeef",
        agentSessionId: "00000000-0000-0000-0000-000000000abc",
        status: "spawned",
      }),
      text: async () => "",
    };
    (global.fetch as ReturnType<typeof vi.fn>).mockResolvedValue(mockResponse);

    const result = await spawnFromPlan(
      "2026-05-19-coordinator-production-readiness",
      "Phase 4",
      ["qontinui-web", "qontinui-runner"],
      "spawn-from-plan demo",
      "You are Wave 4. Confirm receipt.",
      ["backend/app/api/v1/endpoints/operations.py"],
      "http://localhost:9870",
    );

    expect(result.agentId).toBe("agent-deadbeef");

    // Assert the call site:
    expect(global.fetch).toHaveBeenCalledTimes(1);
    const callArgs = (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    const calledUrl = callArgs[0] as string;
    const calledInit = callArgs[1] as RequestInit;

    expect(calledUrl).toBe("http://localhost:9870/agents/spawn");
    expect(calledInit.method).toBe("POST");
    expect((calledInit.headers as Record<string, string>)["Content-Type"]).toBe(
      "application/json",
    );

    const body = JSON.parse(calledInit.body as string);
    expect(body).toEqual({
      plan_slug: "2026-05-19-coordinator-production-readiness",
      plan_phase: "Phase 4",
      repos: ["qontinui-web", "qontinui-runner"],
      intent: "spawn-from-plan demo",
      initial_prompt: "You are Wave 4. Confirm receipt.",
      declared_overlap_paths: [
        "backend/app/api/v1/endpoints/operations.py",
      ],
    });
  });

  it("defaults declared_overlap_paths to [] when omitted", async () => {
    const mockResponse = {
      ok: true,
      status: 200,
      json: async () => ({ agentId: "x" }),
      text: async () => "",
    };
    (global.fetch as ReturnType<typeof vi.fn>).mockResolvedValue(mockResponse);

    await spawnFromPlan(
      "plan-x",
      "phase-y",
      ["qontinui-coord"],
      "intent",
      "prompt",
      undefined,
      "http://localhost:9870",
    );

    const body = JSON.parse(
      (global.fetch as ReturnType<typeof vi.fn>).mock.calls[0][1].body as string,
    );
    expect(body.declared_overlap_paths).toEqual([]);
  });

  it("throws with the response body on non-2xx", async () => {
    const mockResponse = {
      ok: false,
      status: 400,
      statusText: "Bad Request",
      text: async () => '{"error":"unknown plan_slug"}',
      json: async () => ({}),
    };
    (global.fetch as ReturnType<typeof vi.fn>).mockResolvedValue(mockResponse);

    await expect(
      spawnFromPlan(
        "bogus",
        "phase",
        ["repo"],
        "intent",
        "prompt",
        undefined,
        "http://localhost:9870",
      ),
    ).rejects.toThrow(/HTTP 400/);
  });
});
