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
      result: { value: 3, type: "scalar" },
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

  it.each(["location.reload()", "window.location.reload()", "history.go(0)"])(
    "rejects %s (reload does via evaluate what page_refresh refuses)",
    async (expression) => {
      const deps = makeDeps();
      const handled = await handleEvaluateRequest(
        { request_id: `reload:${expression}`, expression },
        deps,
      );
      expect(handled).toBe(true);
      const [, payload] = deps.emit.mock.calls[0];
      expect(payload).toMatchObject({ ok: false });
      expect((payload as { error: string }).error).toMatch(/prohibited pattern/);
    },
  );

  // The identity check that the two `page_evaluate` entry points gate on ONE
  // shared blocklist lives in `ui-bridge-events/usePageEvents.test.ts`, which
  // already imports that module statically. The tests here cover the same
  // sharing behaviourally, through the tagged handler.
  it("relaxes ONLY the network blocks under allow_network_requests", async () => {
    const deps = makeDeps();
    // Blocked by default…
    await handleEvaluateRequest({ request_id: "net-off", expression: "fetch('/x')" }, deps);
    expect(deps.emit.mock.calls[0][1]).toMatchObject({ ok: false });

    // …allowed with the explicit opt-in (the expression itself still throws
    // in jsdom for want of a real `fetch`, which is fine — what matters is
    // that the gate no longer rejects it up front).
    const deps2 = makeDeps();
    await handleEvaluateRequest(
      { request_id: "net-on", expression: "fetch('/x')", allow_network_requests: true },
      deps2,
    );
    const [, netOnPayload] = deps2.emit.mock.calls[0];
    expect((netOnPayload as { error?: string }).error ?? "").not.toMatch(/prohibited pattern/);

    // …while a structural block stays in force regardless.
    const deps3 = makeDeps();
    await handleEvaluateRequest(
      { request_id: "struct-on", expression: "eval('x')", allow_network_requests: true },
      deps3,
    );
    const [, structPayload] = deps3.emit.mock.calls[0];
    expect(structPayload).toMatchObject({ ok: false });
    expect((structPayload as { error: string }).error).toMatch(/prohibited pattern/);
  });

  it("still allows location READS like location.pathname", async () => {
    const deps = makeDeps();
    // node env has no `location`; provide one for the evaluated expression.
    (globalThis as Record<string, unknown>).location = { pathname: "/probe" };
    try {
      const handled = await handleEvaluateRequest(
        { request_id: "read", expression: "location.pathname" },
        deps,
      );
      expect(handled).toBe(true);
      expect(deps.emit).toHaveBeenCalledWith("ui-bridge:evaluate-response", {
        request_id: "read",
        ok: true,
        result: { value: "/probe", type: "scalar" },
      });
    } finally {
      delete (globalThis as Record<string, unknown>).location;
    }
  });

  it("round-trips a LEADING-NEWLINE expression instead of emitting an empty result", async () => {
    // Regression: `new Function("return " + expression)` on `"\n({a:1})"`
    // compiled to `return\n({a:1})`, ASI closed the `return`, and the handler
    // emitted `success: true` with `result: {}` — a SILENT FALSE GREEN, since
    // no SyntaxError was thrown to trigger the statement-style fallback. Every
    // driver that writes a multi-line expression the natural way (heredoc,
    // triple-quoted string, template literal opening on the next line) hit it.
    const deps = makeDeps();
    const handled = await handleEvaluateRequest(
      { request_id: "nl", expression: "\n({a:1})" },
      deps,
    );
    expect(handled).toBe(true);
    // Pre-fix this emitted `result: {}` under `ok: true`, indistinguishable
    // from an expression that legitimately returned an empty object. The
    // discriminated shape is now the ONLY shape, so the `type` tag also
    // separates a real `undefined` from a shape surprise.
    expect(deps.emit).toHaveBeenCalledWith("ui-bridge:evaluate-response", {
      request_id: "nl",
      ok: true,
      result: { value: { a: 1 }, type: "object" },
    });
  });

  it("round-trips a multi-line leading-newline expression with statements inside an IIFE", async () => {
    const deps = makeDeps();
    const expression = "\n(() => {\n  const a = 1;\n  return { a };\n})()\n";
    await handleEvaluateRequest({ request_id: "nl-multi", expression }, deps);
    const [, payload] = deps.emit.mock.calls[0];
    expect(payload).toEqual({
      request_id: "nl-multi",
      ok: true,
      result: { value: { a: 1 }, type: "object" },
    });
  });

  it("awaits a slow Promise for the caller's timeout_ms, not the 30s default cap", async () => {
    // THE GAP. `timeout_ms` was forwarded on every `ui-bridge:evaluate-request`
    // (already clamped to [1000, 600000] by the Rust handler) and never read
    // here — every await was capped at a fixed 30s. A caller who asked for a
    // longer budget to cover a genuinely slow async expression got
    // `ok: false, "did not resolve within 30.0s"` at 30s while the Rust side
    // was still waiting out the rest of its own timeout.
    vi.useFakeTimers();
    try {
      const deps = makeDeps();
      (globalThis as Record<string, unknown>).__slowProbe = () =>
        new Promise((resolve) => setTimeout(() => resolve({ done: true }), 45_000));

      const pending = handleEvaluateRequest(
        { request_id: "slow", expression: "globalThis.__slowProbe()", timeout_ms: 60_000 },
        deps,
      );

      // Past the old fixed cap: pre-fix this had already rejected.
      await vi.advanceTimersByTimeAsync(31_000);
      expect(deps.emit).not.toHaveBeenCalled();

      await vi.advanceTimersByTimeAsync(15_000);
      await pending;
      expect(deps.emit).toHaveBeenCalledWith("ui-bridge:evaluate-response", {
        request_id: "slow",
        ok: true,
        result: { value: { done: true }, type: "object" },
      });
    } finally {
      delete (globalThis as Record<string, unknown>).__slowProbe;
      vi.useRealTimers();
    }
  });

  it("still times out at the caller's budget when the Promise never settles", async () => {
    vi.useFakeTimers();
    try {
      const deps = makeDeps();
      const pending = handleEvaluateRequest(
        { request_id: "hang", expression: "new Promise(() => {})", timeout_ms: 5_000 },
        deps,
      );
      await vi.advanceTimersByTimeAsync(5_001);
      await pending;
      const [, payload] = deps.emit.mock.calls[0];
      expect(payload).toMatchObject({ request_id: "hang", ok: false });
      // The message now leads with the REQUESTED budget (5.0s) and names the
      // derived await separately, because quoting only the derived number is
      // what made the old "9.8s" read as an undocumented cap. Both appear, so
      // this still proves the caller's budget — not the 30 s default — was
      // honoured.
      const err = (payload as { error: string }).error;
      expect(err).toMatch(/within 5\.0s/);
      expect(err).toMatch(/awaited 4\.8s/);
      expect(err).toContain("came from the `timeoutMs` you sent");
    } finally {
      vi.useRealTimers();
    }
  });

  it("still reaches the statement-style fallback for top-level let + explicit return", async () => {
    const deps = makeDeps();
    await handleEvaluateRequest(
      { request_id: "stmt", expression: "let x = 1; return x + 1;" },
      deps,
    );
    expect(deps.emit).toHaveBeenCalledWith("ui-bridge:evaluate-response", {
      request_id: "stmt",
      ok: true,
      result: { value: 2, type: "scalar" },
    });
  });

  it("emits the discriminated {value,type} shape unconditionally — no opt-in", async () => {
    // The `unwrap` opt-in is gone: the varying legacy envelope was the defect,
    // not a feature, and a caller that still sends `unwrap` gets the same one
    // shape (the field is simply not read any more).
    const deps = makeDeps();
    await handleEvaluateRequest({ request_id: "u1", expression: "42" }, deps);
    expect(deps.emit).toHaveBeenCalledWith("ui-bridge:evaluate-response", {
      request_id: "u1",
      ok: true,
      result: { value: 42, type: "scalar" },
    });
  });

  it("returns the completion value of a statement list instead of undefined", async () => {
    // `var q = 1 + 1; q` is not parenthesisable and not valid after a bare
    // `return`, so it landed on the raw-body arm — which has no completion
    // value and yielded `undefined`, serialised as a success envelope with
    // the `value` key dropped entirely.
    const deps = makeDeps();
    await handleEvaluateRequest({ request_id: "comp", expression: "var q = 1 + 1; q" }, deps);
    expect(deps.emit).toHaveBeenCalledWith("ui-bridge:evaluate-response", {
      request_id: "comp",
      ok: true,
      result: { value: 2, type: "scalar" },
    });
  });

  it("types a genuine undefined instead of emitting an empty success", async () => {
    const deps = makeDeps();
    await handleEvaluateRequest({ request_id: "undef", expression: "void 0" }, deps);
    const [, payload] = deps.emit.mock.calls[0];
    expect(payload).toEqual({
      request_id: "undef",
      ok: true,
      result: { value: undefined, type: "undefined" },
    });
  });
});
