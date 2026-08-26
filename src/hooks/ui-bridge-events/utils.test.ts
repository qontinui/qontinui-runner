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
  describeEvaluateResult,
  isThenable,
  isElementActionAllowed,
  resolveEvaluateTimeoutMs,
  PAGE_EVALUATE_MAX_TIMEOUT_MS,
  PAGE_EVALUATE_PROMISE_TIMEOUT_MS,
  describeEvaluateBudget,
  evaluateTimeoutMessage,
  PAGE_EVALUATE_MIN_TIMEOUT_MS,
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

  it.each([
    ["var q = 1 + 1; q", 2],
    ["var q = 1 + 1; q;", 2],
    ["var q = 1 + 1;\nq", 2],
    ["let a = 1; let b = 2; a + b", 3],
    ["const o = {a: {b: 7}}; o.a.b", 7],
  ])("returns the completion value of the statement list %j", (expression, expected) => {
    // THE ARM-3 GAP. A function body has no completion value, so
    // `new Function("var q = 1 + 1; q")()` is `undefined` — the caller got
    // `success: true` with an empty result for input whose intent is
    // unambiguous. Arms 1 and 2 are both SyntaxErrors here, so before the
    // completion-value rewrite every one of these landed on the raw-body arm.
    expect(compileEvaluateExpression(expression as string)()).toBe(expected);
  });

  it.each([
    ["1+1; 2+2", 4],
    ["10; 20; 30", 30],
    ["function f(){return 9}; f()", 9],
    ["'x'; 'y'", "y"],
  ])(
    "returns the LAST statement's value, not the first, for %j",
    (expression, expected) => {
      // THE ARM-2 WRONG-VALUE BUG. `new Function("return 1+1; 2+2")()` compiles
      // and returns 2 — the FIRST statement — so the unguarded arm 2 won ahead
      // of the completion-value arm and the caller got a confidently wrong
      // number under `success: true`. Measured on the shipped build:
      // `1+1; 2+2`->2, `10; 20; 30`->10, `function f(){return 9}; f()`->the
      // function object. A `var` prefix masks it entirely (`return var q = …`
      // is a SyntaxError), which is why every earlier probe missed it.
      expect(compileEvaluateExpression(expression as string)()).toBe(expected);
    },
  );

  it("returns the LAST statement's value for a leading object literal", () => {
    // Same bug, object-valued: arm 2 returned `{a:1}` for input whose last
    // statement is `7`.
    expect(compileEvaluateExpression("({a:1}); 7")()).toBe(7);
  });

  it("skips arm 2 unconditionally, not merely after arm 3", () => {
    // Reordering the arms is NOT equivalent to skipping. `1; if(true){2}` has
    // an arm-3 candidate (`return (if(true){2})`) that does not compile, so a
    // reorder would fall back to arm 2 and return `1` — wrong. The skip sends
    // it to the raw body instead, which is `undefined`: the value is lost, not
    // misreported.
    expect(compileEvaluateExpression("1; if (true) { 2 }")()).toBeUndefined();
  });

  it("keeps arm 2 for a semicolon-terminated single expression", () => {
    // The skip is gated on a NON-BLANK tail after the last top-level break, so
    // arm 2's actual job — `document.title;`, which arm 1 cannot parenthesise —
    // is untouched.
    expect(compileEvaluateExpression("2 + 2;")()).toBe(4);
    expect(compileEvaluateExpression("({a:1});")()).toEqual({ a: 1 });
  });

  it.each([
    // A final BLOCK statement: `return (if(x){f()})` is a hard SyntaxError, so
    // the guarded transform must be DISCARDED and the raw body kept. Wrong is
    // worse than undefined, which is why the candidate is compiled before it
    // is trusted.
    "var hit = false; if (1) { hit = true; }",
    // Same, with a loop.
    "var n = 0; for (var i = 0; i < 2; i++) { n++; }",
    // No `var` prefix: before the arm-2 skip this compiled as
    // `return 1; if (true) { 2 }` and returned 1.
    "1; if (true) { 2 }",
    // A `;` inside a regex literal is the ONE literal kind the scanner does not
    // track, so the only split it finds is inside the literal and the candidate
    // is an unterminated regex. Previously arm 2 answered `1` here.
    "1; /a;b/",
    // ASI: the trailing fragment is a statement, not an expression, so
    // `return (return …)` cannot compile.
    "1; do { } while (false)",
  ])("falls back to the raw body when the rewrite cannot compile: %j", (expression) => {
    // Not a throw and not a wrong value — exactly today's behaviour.
    expect(compileEvaluateExpression(expression)()).toBeUndefined();
  });

  it("still returns the completion value when a trailing line comment follows it", () => {
    // The `\n` before the closing paren is what keeps `// done` from
    // commenting the `)` out.
    expect(compileEvaluateExpression("var q = 1 + 1; q // done")()).toBe(2);
  });

  it("does not mis-split on a `;` inside a regex literal", () => {
    // The scanner does not track regex literals, so a split AT a regex-internal
    // `;` produces an unterminated-regex head — a SyntaxError, which is
    // discarded. It can never produce a different, compiling program.
    expect(compileEvaluateExpression('var r = /a;b/; r.test("a;b")')()).toBe(true);
    // Regex at the very end: the only split point IS inside the literal, so the
    // rewrite is rejected and the raw body's `undefined` stands.
    expect(compileEvaluateExpression("var ok = true; /a;b/")()).toBeUndefined();
  });

  it("never executes the statements twice when the rewrite is tried", () => {
    // Compilation is separated from invocation, so a rejected candidate costs
    // a `new Function` parse and nothing else.
    let calls = 0;
    (globalThis as Record<string, unknown>).__sideEffect = () => {
      calls += 1;
      return calls;
    };
    try {
      const fn = compileEvaluateExpression("var v = globalThis.__sideEffect(); v");
      expect(calls).toBe(0);
      expect(fn()).toBe(1);
      expect(calls).toBe(1);
    } finally {
      delete (globalThis as Record<string, unknown>).__sideEffect;
    }
  });

  it.each([
    ['const s = "a;b"; s', "a;b"],
    ["const t = `x${1};y`; t", "x1;y"],
    ["const f = (a, b) => { return a; }; f(1, 2)", 1],
  ])("ignores a %j break that is inside a string, template or bracket", (expression, expected) => {
    // The split scanner must not treat a `;` inside a string/template/bracket
    // as a statement break — splitting there produces a head that is either a
    // SyntaxError (benign, falls through) or, worse, a different program.
    expect(compileEvaluateExpression(expression as string)()).toBe(expected);
  });

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

describe("describeEvaluateResult — unwrap:true discriminated shape", () => {
  it.each([
    [null, { value: null, type: "null" }],
    [undefined, { value: undefined, type: "undefined" }],
    [3, { value: 3, type: "scalar" }],
    ["s", { value: "s", type: "scalar" }],
    [{ a: 1 }, { value: { a: 1 }, type: "object" }],
    [[1], { value: [1], type: "object" }],
  ])("discriminates %s", (input, expected) => {
    expect(describeEvaluateResult(input)).toEqual(expected);
  });

  it("surfaces a function's name, since functions aren't clone-safe", () => {
    expect(describeEvaluateResult(function named() {})).toEqual({
      value: "named",
      type: "function",
    });
    expect(describeEvaluateResult(Object.defineProperty(() => {}, "name", { value: "" }))).toEqual({
      value: "<anonymous>",
      type: "function",
    });
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


describe("describeEvaluateBudget + evaluateTimeoutMessage (U1: the 9.8s that read as a cap)", () => {
  it("reports the DEFAULT as a default, not as a cap", () => {
    const budget = describeEvaluateBudget(undefined);
    expect(budget.fromDefault).toBe(true);
    expect(budget.requestedMs).toBe(PAGE_EVALUATE_PROMISE_TIMEOUT_MS);

    const msg = evaluateTimeoutMessage(budget.awaitMs, budget);
    // the requested budget, not the derived await, leads the sentence
    expect(msg).toContain("did not resolve within 30.0s");
    expect(msg).toContain("DEFAULT budget, not a cap");
    expect(msg).toContain("timeoutMs");
    expect(msg).toContain(`${PAGE_EVALUATE_MIN_TIMEOUT_MS}-${PAGE_EVALUATE_MAX_TIMEOUT_MS}ms`);
  });

  it("attributes a caller-supplied budget to the caller", () => {
    const budget = describeEvaluateBudget(60_000);
    expect(budget.fromDefault).toBe(false);
    expect(budget.requestedMs).toBe(60_000);

    const msg = evaluateTimeoutMessage(budget.awaitMs, budget);
    expect(msg).toContain("did not resolve within 60.0s");
    expect(msg).toContain("came from the `timeoutMs` you sent");
    expect(msg).not.toContain("DEFAULT budget");
  });

  it("names the reporting margin rather than silently shortening the budget", () => {
    // THE ORIGINAL DEFECT: the message quoted 10000-250 = "9.8s" with no
    // explanation, so a caller read an arbitrary number as a hard ceiling.
    const budget = describeEvaluateBudget(10_000);
    const msg = evaluateTimeoutMessage(budget.awaitMs, budget);
    expect(msg).toContain("did not resolve within 10.0s");
    expect(msg).toContain("awaited 9.8s");
    expect(msg).toContain("250ms is reserved");
  });

  it("stays byte-compatible with resolveEvaluateTimeoutMs", () => {
    for (const raw of [undefined, null, 0, -1, NaN, "60000", 1, 1_000, 10_000, 600_000, 5_000_000]) {
      expect(describeEvaluateBudget(raw).awaitMs).toBe(resolveEvaluateTimeoutMs(raw));
    }
  });

  it("keeps the frontend-vs-Rust discriminator verbatim", () => {
    // This leading clause is how a caller tells a frontend timeout from the
    // Rust side's generic "UI Bridge page_evaluate timed out after Xms".
    expect(evaluateTimeoutMessage(5_000)).toBe(
      "page_evaluate: Promise did not resolve within 5.0s",
    );
  });
});
