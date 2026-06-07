/**
 * useUIBridgeEvaluateHandler tests — exactly-once evaluate dispatch.
 *
 * The expression is executed for SIDE EFFECTS (it can call
 * `window.__TAURI__.core.invoke(...)`), so duplicate execution is the bug
 * the PR #473 round-2 falsification surfaced (one /page/evaluate ran a
 * command 4×). These tests assert the `request_id` dedupe backstop caps
 * execution at one even when N listeners deliver the same event.
 */

import { describe, it, expect, beforeEach, vi } from "vitest";

vi.mock("@tauri-apps/api/event", () => ({ emit: vi.fn(), listen: vi.fn() }));

import { handleEvaluateRequest, type EvaluateHandlerDeps } from "./useUIBridgeEvaluateHandler";
import { evaluateRequestDedupe } from "./ui-bridge-events/request-dedupe";

function makeDeps(): EvaluateHandlerDeps & { emit: ReturnType<typeof vi.fn> } {
  return { emit: vi.fn().mockResolvedValue(undefined) };
}

beforeEach(() => {
  evaluateRequestDedupe.reset();
});

describe("handleEvaluateRequest", () => {
  it("evaluates an expression and emits a success response", async () => {
    const deps = makeDeps();
    const handled = await handleEvaluateRequest({ request_id: "r1", expression: "1 + 2" }, deps);
    expect(handled).toBe(true);
    expect(deps.emit).toHaveBeenCalledWith("ui-bridge:evaluate-response", {
      request_id: "r1",
      ok: true,
      result: { success: true, result: { value: 3 } },
    });
  });

  it("executes the expression EXACTLY ONCE across N deliveries", async () => {
    const deps = makeDeps();
    // Side-effecting expression: each execution bumps a global counter.
    (globalThis as Record<string, unknown>).__evalCount = 0;
    const expr = "(globalThis.__evalCount = (globalThis.__evalCount || 0) + 1)";

    const results = await Promise.all(
      Array.from({ length: 4 }, () =>
        handleEvaluateRequest({ request_id: "storm", expression: expr }, deps),
      ),
    );

    expect(results.filter(Boolean)).toHaveLength(1);
    expect((globalThis as Record<string, unknown>).__evalCount).toBe(1);
    expect(deps.emit).toHaveBeenCalledTimes(1);
    delete (globalThis as Record<string, unknown>).__evalCount;
  });

  it("drops a request with no request_id", async () => {
    const deps = makeDeps();
    const handled = await handleEvaluateRequest(
      // @ts-expect-error intentionally missing request_id
      { expression: "1" },
      deps,
    );
    expect(handled).toBe(false);
    expect(deps.emit).not.toHaveBeenCalled();
  });

  it("rejects a dangerous expression with an error response", async () => {
    const deps = makeDeps();
    const handled = await handleEvaluateRequest(
      { request_id: "danger", expression: "eval('x')" },
      deps,
    );
    expect(handled).toBe(true);
    const [, payload] = deps.emit.mock.calls[0];
    expect(payload).toMatchObject({ request_id: "danger", ok: false });
    expect((payload as { error: string }).error).toMatch(/prohibited pattern/);
  });

  it("emits the discriminated {value,type} shape when unwrap=true", async () => {
    const deps = makeDeps();
    await handleEvaluateRequest({ request_id: "u1", expression: "42", unwrap: true }, deps);
    expect(deps.emit).toHaveBeenCalledWith("ui-bridge:evaluate-response", {
      request_id: "u1",
      ok: true,
      result: { value: 42, type: "scalar" },
    });
  });
});
