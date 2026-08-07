/**
 * Tests for `awaitWithTimeout` and `isThenable` — the auto-await helper
 * shared by the runner's two `page_evaluate` handlers
 * (`usePageEvents.ts::page_evaluate` legacy IPC branch and
 * `useUIBridgeEvaluateHandler.ts` tagged Tauri-event handler).
 *
 * Covers the four cases the public spec promises for `page/evaluate`:
 *   - sync object passes through unchanged (regression guard)
 *   - top-level Promise resolves to the awaited value
 *   - top-level Promise rejection surfaces as a thrown Error
 *   - hanging Promise rejects with a timeout Error before exhausting the
 *     bridge's response budget
 *
 * Also pins the spec-correct thenable duck-test (cross-realm Promise
 * safety) since `instanceof Promise` would silently miss those.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import {
  awaitWithTimeout,
  compileEvaluateExpression,
  isThenable,
  isElementActionAllowed,
  resolveEvaluateTimeoutMs,
  PAGE_EVALUATE_MAX_TIMEOUT_MS,
  PAGE_EVALUATE_PROMISE_TIMEOUT_MS,
} from "./utils";

describe("isThenable", () => {
  it("accepts native Promise instances", () => {
    expect(isThenable(Promise.resolve(1))).toBe(true);
    expect(isThenable(Promise.reject(new Error("x")).catch(() => {}))).toBe(true);
  });

  it("accepts plain thenable objects (cross-realm safety)", () => {
    const thenable = { then: (onFulfilled: (v: unknown) => void) => onFulfilled(42) };
    expect(isThenable(thenable)).toBe(true);
  });

  it("rejects non-thenable objects", () => {
    expect(isThenable({ a: 1 })).toBe(false);
    expect(isThenable({ then: "not a function" })).toBe(false);
    expect(isThenable([])).toBe(false);
  });

  it("rejects primitives and null/undefined", () => {
    expect(isThenable(null)).toBe(false);
    expect(isThenable(undefined)).toBe(false);
    expect(isThenable(0)).toBe(false);
    expect(isThenable("then")).toBe(false);
    expect(isThenable(true)).toBe(false);
  });
});

describe("awaitWithTimeout", () => {
  it("returns sync values unchanged (no Promise wrap)", async () => {
    expect(await awaitWithTimeout({ a: 1 }, 1000)).toEqual({ a: 1 });
    expect(await awaitWithTimeout(42, 1000)).toBe(42);
    expect(await awaitWithTimeout(null, 1000)).toBeNull();
    expect(await awaitWithTimeout(undefined, 1000)).toBeUndefined();
  });

  it("resolves top-level Promises to their value", async () => {
    expect(await awaitWithTimeout(Promise.resolve({ a: 1 }), 1000)).toEqual({ a: 1 });
    expect(await awaitWithTimeout(Promise.resolve(42), 1000)).toBe(42);
  });

  it("resolves async-IIFE return values (the user-reported bug shape)", async () => {
    // Mirrors `(async () => ({a: 1}))()` — the exact form the bug report
    // calls out as silently returning `{}` before this fix.
    const result = await awaitWithTimeout((async () => ({ a: 1 }))(), 1000);
    expect(result).toEqual({ a: 1 });
  });

  it("propagates rejections from the awaited Promise", async () => {
    await expect(
      awaitWithTimeout(Promise.reject(new Error("boom")), 1000),
    ).rejects.toThrow("boom");
  });

  it("propagates rejections from async IIFE that throws", async () => {
    const failing = (async () => {
      throw new Error("async failure");
    })();
    await expect(awaitWithTimeout(failing, 1000)).rejects.toThrow("async failure");
  });

  it("times out hanging Promises with a descriptive error", async () => {
    vi.useFakeTimers();
    try {
      // Promise that never resolves
      const hanging = new Promise(() => {});
      const pending = awaitWithTimeout(hanging, 5_000);
      // Attach a swallow handler immediately so the unhandled-rejection
      // warning doesn't fire before the assertion runs.
      const observed = pending.catch((err: unknown) => err);
      await vi.advanceTimersByTimeAsync(5_000);
      const err = await observed;
      expect(err).toBeInstanceOf(Error);
      expect((err as Error).message).toMatch(/did not resolve within 5\.0s/);
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not time out when the Promise resolves before the cap", async () => {
    vi.useFakeTimers();
    try {
      const slow = new Promise((resolve) => setTimeout(() => resolve("done"), 1_000));
      const pending = awaitWithTimeout(slow, 5_000);
      await vi.advanceTimersByTimeAsync(1_000);
      expect(await pending).toBe("done");
    } finally {
      vi.useRealTimers();
    }
  });

  it("resolves plain thenables, not just native Promises", async () => {
    const thenable = {
      then(onFulfilled: (v: unknown) => void) {
        onFulfilled("from-thenable");
      },
    };
    expect(await awaitWithTimeout(thenable, 1000)).toBe("from-thenable");
  });
});

describe("PAGE_EVALUATE_PROMISE_TIMEOUT_MS", () => {
  it("matches the documented 30s cap", () => {
    // Pin the constant so a future drift to a smaller cap (which would
    // surprise integration tests doing real network work) is caught.
    expect(PAGE_EVALUATE_PROMISE_TIMEOUT_MS).toBe(30_000);
  });
});

// Suppress the unhandled-rejection bookkeeping that vi.useFakeTimers()
// can leak when a test exits between "rejection scheduled" and
// "rejection observed". Each test uses an explicit `.catch(...)` handler
// so this is just defensive.
beforeEach(() => {
  // No-op: kept for future stub points (e.g. console.warn spy).
});
afterEach(() => {
  vi.useRealTimers();
});

describe("isElementActionAllowed — execute_action per-element gate", () => {
  it("permits any action when the element declares no action set", () => {
    expect(isElementActionAllowed([], "hoverClick")).toBe(true);
    expect(isElementActionAllowed([], "type")).toBe(true);
  });

  it("permits an action that is explicitly advertised", () => {
    expect(isElementActionAllowed(["click", "focus"], "click")).toBe(true);
  });

  it("rejects an action not in a non-empty declared set", () => {
    expect(isElementActionAllowed(["click", "focus"], "type")).toBe(false);
    expect(isElementActionAllowed(["focus", "blur"], "click")).toBe(false);
  });

  it("exempts hoverClick wherever click is advertised (click-variant) — the regression", () => {
    // A hover-gated toolbar button (e.g. ZoneHoverActions "Send to window")
    // advertises click but not hoverClick; hoverClick must still be allowed so
    // it reaches actionExecutor.performHoverClick instead of being rejected
    // pre-dispatch (mirrors the runner Rust is_action_advertised exemption).
    expect(isElementActionAllowed(["focus", "blur", "click", "hover", "middleClick"], "hoverClick")).toBe(
      true,
    );
  });

  it("does NOT exempt hoverClick when click is absent", () => {
    expect(isElementActionAllowed(["focus", "blur", "hover"], "hoverClick")).toBe(false);
  });
});

describe("compileEvaluateExpression — page_evaluate wrapping", () => {
  it("evaluates a LEADING-NEWLINE expression instead of silently returning undefined", () => {
    // THE REGRESSION. `new Function("return " + "\n({a:1})")` produces
    //   return
    //   ({a:1})
    // and ASI terminates the `return`, yielding `undefined` -> serialised as
    // `{}` under `success: true`. No SyntaxError is thrown, so the
    // statement-style fallback never fires: a silent false green. Any caller
    // writing a multi-line expression the natural way (heredoc, triple-quoted
    // string, template literal opening on the next line) hits this.
    expect(compileEvaluateExpression("\n({a:1})")()).toEqual({ a: 1 });
  });

  it.each([
    ["\r\n({a:1})", { a: 1 }],
    ["\n\n  ({a:1})", { a: 1 }],
    ["\n  // leading comment\n  ({a:1})", { a: 1 }],
  ])("evaluates leading-whitespace/comment variant %j", (expression, expected) => {
    expect(compileEvaluateExpression(expression as string)()).toEqual(expected);
  });

  it("evaluates a trailing line comment without swallowing the closing paren", () => {
    // Guards the mirror hazard introduced by the paren wrap itself: without
    // the `\n` before `)`, `1 + 1 // done` would comment out the `)`.
    expect(compileEvaluateExpression("1 + 1 // done")()).toBe(2);
  });

  it("still evaluates a bare object literal", () => {
    expect(compileEvaluateExpression("({a:1})")()).toEqual({ a: 1 });
  });

  it.each(["document_title_placeholder;", "1 + 1;", "\n1 + 1;"])(
    "still returns a value for the semicolon-terminated expression %j",
    (expression) => {
      const expr = expression.replace("document_title_placeholder", "2");
      expect(compileEvaluateExpression(expr)()).toBe(2);
    },
  );

  it("falls back to a raw function body for statement-style input", () => {
    // Not parenthesisable and not valid after a bare `return` either — this
    // is the case the original SyntaxError fallback existed for, and it must
    // keep working after the paren wrap was added ahead of it.
    expect(compileEvaluateExpression("let x = 1; return x + 1;")()).toBe(2);
  });

  it("preserves an expression that legitimately evaluates to undefined", () => {
    // The success/failure contract must not conflate "returned undefined" with
    // "failed": both arms return the value, errors throw.
    expect(compileEvaluateExpression("undefined")()).toBeUndefined();
    expect(compileEvaluateExpression("void 0")()).toBeUndefined();
  });

  it("throws SyntaxError when no wrapping arm can compile the input", () => {
    expect(() => compileEvaluateExpression("function (")).toThrow(SyntaxError);
  });

  it("compiles WITHOUT executing, so a runtime SyntaxError can't double-execute", () => {
    // `JSON.parse("{")` throws a SyntaxError at RUN time. The old
    // compile-and-call-in-one-try shape misread that as a failed wrap and
    // re-ran the expression as a raw body — a second execution of an
    // expression the evaluate handler guarantees runs exactly once.
    let calls = 0;
    (globalThis as Record<string, unknown>).__compileProbe = () => {
      calls += 1;
      return JSON.parse("{");
    };
    try {
      const fn = compileEvaluateExpression("globalThis.__compileProbe()");
      expect(calls).toBe(0); // compilation alone must not run it
      expect(() => fn()).toThrow(SyntaxError);
      expect(calls).toBe(1); // exactly once, no fallback re-execution
    } finally {
      delete (globalThis as Record<string, unknown>).__compileProbe;
    }
  });
});

describe("resolveEvaluateTimeoutMs — caller-supplied page/evaluate budget", () => {
  // The frontend deliberately gives up a small margin ahead of the Rust
  // dispatcher's identical wait so its more specific timeout message wins the
  // race deterministically.
  const MARGIN = 250;

  it("honors a caller timeout above the default cap", () => {
    // THE GAP. The Rust dispatcher forwards the caller's clamped `timeoutMs`
    // and waits exactly that long, but the frontend used to ignore the field
    // and cap every await at 30s — so `timeoutMs: 600000` failed at 30s with
    // "did not resolve within 30.0s" while Rust was still waiting.
    expect(resolveEvaluateTimeoutMs(60_000)).toBe(60_000 - MARGIN);
    expect(resolveEvaluateTimeoutMs(PAGE_EVALUATE_MAX_TIMEOUT_MS)).toBe(
      PAGE_EVALUATE_MAX_TIMEOUT_MS - MARGIN,
    );
  });

  it("honors a caller timeout below the default cap", () => {
    // The Rust default is 10s, so awaiting the full 30s just kept a dead
    // request's expression alive after Rust had already returned 504.
    expect(resolveEvaluateTimeoutMs(10_000)).toBe(10_000 - MARGIN);
    expect(resolveEvaluateTimeoutMs(1_000)).toBe(1_000 - MARGIN);
  });

  it("never lets the margin drive the budget to zero", () => {
    // Below the Rust clamp floor these can only arrive from a hand-rolled
    // emit, but an instant-failure budget would be far worse than a short one.
    expect(resolveEvaluateTimeoutMs(MARGIN)).toBe(MARGIN);
    expect(resolveEvaluateTimeoutMs(1)).toBe(1);
  });

  it("clamps above the ceiling rather than trusting the payload", () => {
    expect(resolveEvaluateTimeoutMs(5_000_000)).toBe(PAGE_EVALUATE_MAX_TIMEOUT_MS - MARGIN);
    expect(resolveEvaluateTimeoutMs(Number.POSITIVE_INFINITY)).toBe(
      PAGE_EVALUATE_PROMISE_TIMEOUT_MS,
    );
  });

  it.each([
    ["absent", undefined],
    ["null", null],
    ["zero", 0],
    ["negative", -1],
    ["NaN", Number.NaN],
    ["a string", "60000"],
  ])("falls back to the default cap for %s", (_label, raw) => {
    expect(resolveEvaluateTimeoutMs(raw)).toBe(PAGE_EVALUATE_PROMISE_TIMEOUT_MS);
  });
});
