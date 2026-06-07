/**
 * useUIBridgeInvokeHandler tests — exactly-once invoke dispatch.
 *
 * Focus: the `request_id` dedupe backstop guarantees one invoke() + one
 * response even when N listeners deliver the same event (the PR #473
 * round-2 fan-out). The Tauri `listen`/`invoke`/`emit` plumbing is mocked
 * away; we test the extracted `handleInvokeRequest` with injected deps.
 */

import { describe, it, expect, beforeEach, vi } from "vitest";

// The hook imports @tauri-apps/api/{event,core} at module load. Stub them
// so importing the module under test doesn't touch a real Tauri runtime.
vi.mock("@tauri-apps/api/event", () => ({ emit: vi.fn(), listen: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { handleInvokeRequest, type InvokeHandlerDeps } from "./useUIBridgeInvokeHandler";
import { invokeRequestDedupe } from "./ui-bridge-events/request-dedupe";

function makeDeps(): InvokeHandlerDeps & {
  invoke: ReturnType<typeof vi.fn>;
  emit: ReturnType<typeof vi.fn>;
} {
  return {
    invoke: vi.fn().mockResolvedValue({ ok: 1 }),
    emit: vi.fn().mockResolvedValue(undefined),
  };
}

beforeEach(() => {
  invokeRequestDedupe.reset();
});

describe("handleInvokeRequest", () => {
  it("invokes the command and emits a success response", async () => {
    const deps = makeDeps();
    const handled = await handleInvokeRequest(
      { request_id: "r1", command: "create_ai_session", args: { a: 1 } },
      deps,
    );
    expect(handled).toBe(true);
    expect(deps.invoke).toHaveBeenCalledWith("create_ai_session", { a: 1 });
    expect(deps.emit).toHaveBeenCalledWith("ui-bridge:invoke-response", {
      request_id: "r1",
      ok: true,
      result: { ok: 1 },
    });
  });

  it("dedupes N deliveries of one request_id → one invoke + one response", async () => {
    const deps = makeDeps();
    // Simulate 4 accumulated listeners all delivering the same event.
    const results = await Promise.all(
      Array.from({ length: 4 }, () =>
        handleInvokeRequest({ request_id: "storm", command: "create_ai_session", args: {} }, deps),
      ),
    );
    expect(results.filter(Boolean)).toHaveLength(1);
    expect(deps.invoke).toHaveBeenCalledTimes(1);
    expect(deps.emit).toHaveBeenCalledTimes(1);
  });

  it("drops a request with no request_id (cannot route response)", async () => {
    const deps = makeDeps();
    const handled = await handleInvokeRequest(
      // @ts-expect-error intentionally missing request_id
      { command: "noop", args: {} },
      deps,
    );
    expect(handled).toBe(false);
    expect(deps.invoke).not.toHaveBeenCalled();
    expect(deps.emit).not.toHaveBeenCalled();
  });

  it("forwards a serialized error response on invoke rejection", async () => {
    const deps = makeDeps();
    deps.invoke.mockRejectedValueOnce({ kind: "Boom", message: "kaboom" });
    const handled = await handleInvokeRequest({ request_id: "e1", command: "bad", args: {} }, deps);
    expect(handled).toBe(true);
    expect(deps.emit).toHaveBeenCalledWith("ui-bridge:invoke-response", {
      request_id: "e1",
      ok: false,
      error: JSON.stringify({ kind: "Boom", message: "kaboom" }),
    });
  });
});
