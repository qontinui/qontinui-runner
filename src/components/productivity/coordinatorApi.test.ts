/**
 * Pure-logic tests for `coordinatorApi.spawnFromPlan`.
 *
 * Plan `2026-05-19-coordinator-production-readiness.md` Phase 4 (Wave 4).
 *
 * The runner's vitest config is `environment: "node"` (no jsdom).
 * `spawnFromPlan` routes the spawn through the authenticated Rust Tauri
 * command `spawn_from_plan`: production coord gates `POST /agents/spawn` on
 * operator SSO, and the operator's Cognito bearer lives ONLY in the Rust
 * backend, so the frontend must not POST directly. We mock
 * `@tauri-apps/api/core`'s `invoke` and assert the command name + argument
 * contract (Tauri camelCases the snake_case Rust params).
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

import { invoke } from "@tauri-apps/api/core";
import { extractApiErrorMessage, spawnFromPlan } from "./coordinatorApi";

const mockInvoke = invoke as ReturnType<typeof vi.fn>;

describe("spawnFromPlan", () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("invokes the spawn_from_plan command with the coord wire contract", async () => {
    mockInvoke.mockResolvedValue({
      agentId: "agent-deadbeef",
      agentSessionId: "00000000-0000-0000-0000-000000000abc",
      status: "spawned",
    });

    const result = await spawnFromPlan(
      "2026-05-19-coordinator-production-readiness",
      "Phase 4",
      ["qontinui-web", "qontinui-runner"],
      "spawn-from-plan demo",
      "You are Wave 4. Confirm receipt.",
      ["backend/app/api/v1/endpoints/operations.py"],
    );

    expect(result.agentId).toBe("agent-deadbeef");

    expect(mockInvoke).toHaveBeenCalledTimes(1);
    const [command, args] = mockInvoke.mock.calls[0];
    expect(command).toBe("spawn_from_plan");
    // Tauri auto-converts the snake_case Rust params to camelCase JS keys.
    // `workUnitSlug` (NOT `planSlug`) — coord's post-rename wire key; the
    // exact-shape `toEqual` is what keeps the legacy key from creeping back.
    expect(args).toEqual({
      workUnitSlug: "2026-05-19-coordinator-production-readiness",
      planPhase: "Phase 4",
      repos: ["qontinui-web", "qontinui-runner"],
      intent: "spawn-from-plan demo",
      initialPrompt: "You are Wave 4. Confirm receipt.",
      declaredOverlapPaths: ["backend/app/api/v1/endpoints/operations.py"],
    });
  });

  it("defaults declaredOverlapPaths to [] when omitted", async () => {
    mockInvoke.mockResolvedValue({ agentId: "x" });

    await spawnFromPlan("plan-x", "phase-y", ["qontinui-coord"], "intent", "prompt");

    const [, args] = mockInvoke.mock.calls[0];
    expect((args as { declaredOverlapPaths: string[] }).declaredOverlapPaths).toEqual([]);
  });

  it("propagates the backend error on failure", async () => {
    mockInvoke.mockRejectedValue(
      new Error('HTTP 400: {"error":"unknown plan_slug"}'),
    );

    await expect(
      spawnFromPlan("bogus", "phase", ["repo"], "intent", "prompt"),
    ).rejects.toThrow(/HTTP 400/);
  });
});

describe("extractApiErrorMessage", () => {
  it("renders the envelope's error only — not the wire JSON", () => {
    // THE DEFECT: the whole 500 body was spliced into the thrown message and
    // rendered verbatim in the Workers panel.
    const body = '{"success":false,"data":null,"error":"db error","code":"PG_QUERY"}';
    const msg = extractApiErrorMessage(body, "Internal Server Error");
    expect(msg).toBe("db error");
    expect(msg).not.toContain('{"success":false');
    expect(msg).not.toContain('"code":');
  });

  it("appends the envelope hint when the runner supplies one", () => {
    const body = '{"success":false,"error":"relation does not exist","hint":"run migrations"}';
    expect(extractApiErrorMessage(body, "")).toBe("relation does not exist (run migrations)");
  });

  it("reads a nested {error:{message}} shape", () => {
    expect(extractApiErrorMessage('{"error":{"message":"nope"}}', "")).toBe("nope");
  });

  it("falls back to the raw body for a non-envelope response", () => {
    // Losing the message is worse than showing it ugly — an HTML 502 from a
    // proxy still has to reach the operator.
    expect(extractApiErrorMessage("<html>502 Bad Gateway</html>", "Bad Gateway")).toBe(
      "<html>502 Bad Gateway</html>",
    );
  });

  it("falls back to the status text for an empty body", () => {
    expect(extractApiErrorMessage("", "Service Unavailable")).toBe("Service Unavailable");
  });

  it("keeps an unrecognised JSON object visible rather than dropping it", () => {
    expect(extractApiErrorMessage('{"weird":1}', "x")).toBe('{"weird":1}');
  });
});
