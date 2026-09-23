/**
 * The RUNTIME half of the "an unvalidated bag reaches an effect" gate.
 *
 * `actionSurfaces.enforcement.test.ts` proves every action surface in the tree
 * routes its handler through `guardedHandler` (or takes no parameter). This
 * proves the guard refuses — with the effect spied, so "refused BEFORE any
 * effect" is a counted zero rather than an inference from a thrown error. A
 * handler that threw AFTER creating a PTY is exactly what `create-ai-session`
 * used to do, and it threw too.
 *
 * Ported from qontinui-runner#1301's `guardedAction.test.ts`; the guard now
 * wraps the handler rather than the whole action def, so the registration
 * stays a plain literal the `effect` walk can read.
 */

import { describe, it, expect, vi } from "vitest";
import { ACTION_PARAMS_INVALID, guardedHandler } from "./guardedHandler";

/**
 * Bags no action can accept, whatever it declares.
 *
 * The first five are the shape defect: `Object.entries(5)` is `[]`, so a
 * non-object bag used to LAUNDER into an empty one and the action ran bare.
 * The rest are the per-key and per-value defects.
 */
const MALFORMED: Array<[string, unknown]> = [
  ["a number", 5],
  ["a string", "zz"],
  ["an empty list", []],
  ["a populated list", [1, 2]],
  ["true", true],
  ["an undeclared key", { zzz: "x" }],
  ["a declared key holding an object", { count: {} }],
  ["a declared key holding a list", { count: [] }],
  ["a declared key holding true", { count: true }],
  ["a declared key alongside an undeclared one", { count: 2, zzz: "x" }],
];

describe("guardedHandler", () => {
  it("refuses every malformed bag with ZERO effects", () => {
    for (const [label, bag] of MALFORMED) {
      const effect = vi.fn();
      const handler = guardedHandler("probe", { count: "number" }, (args) => {
        effect(args);
        return "ran";
      });
      expect(() => handler(bag), label).toThrow();
      expect(effect, `${label} reached the effect`).not.toHaveBeenCalled();
    }
  });

  it("names the surface and the reason, never a minified variable", () => {
    const handler = guardedHandler("create-ai-session", { context: "string" }, () => "ran");
    expect(() => handler(5)).toThrow("create-ai-session: arguments must be an object (got number)");
    expect(() => handler({ zzz: "x" })).toThrow('create-ai-session: takes no argument named "zzz"');
    expect(() => handler({ context: {} })).toThrow(
      'create-ai-session: "context" must be text or a number (got an object)',
    );
    // …and not `od.replace is not a function`, which is what the operator saw
    // before, 750 lines and one PTY later.
    expect(() => handler({ context: {} })).not.toThrow(/is not a function/);
  });

  it("carries a machine-readable .code, the shape the SDK hoists", () => {
    const handler = guardedHandler("probe", {}, () => 1);
    let caught: (Error & { code?: string }) | null = null;
    try {
      handler({ zzz: 1 });
    } catch (err) {
      caught = err as Error & { code?: string };
    }
    expect(caught?.code).toBe(ACTION_PARAMS_INVALID);
    expect(caught?.message.startsWith(`${ACTION_PARAMS_INVALID}: `)).toBe(true);
  });

  it("runs on a well-formed bag, and the effect sees the BOUND args", () => {
    const effect = vi.fn();
    const handler = guardedHandler("probe", { count: "number", name: "string" }, (args) => {
      effect(args);
      return "ran";
    });
    // `"2"` coerces to the number 2 — the same reading Tier 1 gives typed
    // text, which is why `run` reads text fields through `textArg`.
    expect(handler({ count: "2", name: "alpha" })).toBe("ran");
    expect(effect).toHaveBeenCalledWith({ count: 2, name: "alpha" });
  });

  it("an EMPTY schema means takes-no-arguments, enforced", () => {
    const effect = vi.fn();
    const handler = guardedHandler("mute", {}, () => effect());
    handler({});
    expect(effect).toHaveBeenCalledTimes(1);
    expect(() => handler({ anything: 1 })).toThrow('mute: takes no arguments (got "anything")');
    expect(effect).toHaveBeenCalledTimes(1);
  });

  it("a nullish bag is the empty bag; a non-object bag is NOT", () => {
    const effect = vi.fn();
    const handler = guardedHandler("probe", {}, () => effect());
    handler(undefined);
    handler(null);
    expect(effect).toHaveBeenCalledTimes(2);
    // The distinction the old `(params ?? {})` collapsed.
    expect(() => handler(0)).toThrow("arguments must be an object (got number)");
    expect(() => handler("")).toThrow("arguments must be an object (got string)");
  });

  it("a TEXT field keeps the caller's exact string; a number field still coerces", () => {
    // `coerceToken` + `textArg` is lossy: "007" -> 7 -> "7". A profile named
    // "01" would have been saved as "1"; a command "1.10" typed as "1.1".
    const effect = vi.fn();
    const handler = guardedHandler("probe", { name: "string", count: "number (>= 1)" }, (args) =>
      effect(args),
    );
    handler({ name: "007", count: "2" });
    handler({ name: "1.10" });
    handler({ name: "12345678901234567890" });
    expect(effect.mock.calls.map((c) => c[0])).toEqual([
      { name: "007", count: 2 },
      { name: "1.10" },
      { name: "12345678901234567890" },
    ]);
  });

  it("an undeclared key is refused even when its value is null", () => {
    // Null drops a DECLARED key as absent; it must not launder an undeclared
    // one past the gate.
    const effect = vi.fn();
    const handler = guardedHandler("probe", { count: "number" }, () => effect());
    expect(() => handler({ zzz: null })).toThrow('probe: takes no argument named "zzz"');
    expect(() => handler(JSON.parse('{"__proto__": null}'))).toThrow(
      'probe: takes no argument named "__proto__"',
    );
    handler({ count: null });
    expect(effect).toHaveBeenCalledTimes(1);
  });

  it("`__proto__` is refused as an undeclared key, not silently dropped", () => {
    const handler = guardedHandler("probe", { count: "number" }, () => 1);
    expect(() => handler(JSON.parse('{"__proto__": "x"}'))).toThrow(
      'probe: takes no argument named "__proto__"',
    );
  });

  describe("structuredParams — a per-field exemption from value coercion", () => {
    const build = (effect: (a: Record<string, unknown>) => unknown) =>
      guardedHandler("sendKeys", { keys: "string | string[] | descriptors" }, effect, {
        structuredParams: ["keys"],
      });

    it("lets the SDK's two array grammars through un-coerced", () => {
      const effect = vi.fn();
      const handler = build(effect);
      handler({ keys: ["Enter"] });
      handler({ keys: [{ key: "c", modifiers: { ctrl: true } }] });
      handler({ keys: "ls\r" });
      expect(effect.mock.calls.map((c) => c[0].keys)).toEqual([
        ["Enter"],
        [{ key: "c", modifiers: { ctrl: true } }],
        "ls\r",
      ]);
    });

    it("does NOT exempt the bag's shape or its key set", () => {
      const effect = vi.fn();
      const handler = build(effect);
      expect(() => handler(5)).toThrow("arguments must be an object");
      expect(() => handler(["Enter"])).toThrow("arguments must be an object (got a list)");
      expect(() => handler({ keys: "a", zzz: "x" })).toThrow(
        'sendKeys: takes no argument named "zzz"',
      );
      expect(effect).not.toHaveBeenCalled();
    });

    it("cannot widen an action past its own schema", () => {
      const effect = vi.fn();
      const handler = guardedHandler("probe", { text: "string" }, (args) => effect(args), {
        structuredParams: ["payload"],
      });
      // Undeclared ⇒ not exempted ⇒ coerced like any other value, so the
      // VALUE refusal fires before the undeclared one.
      expect(() => handler({ payload: { a: 1 } })).toThrow(
        'probe: "payload" must be text or a number (got an object)',
      );
      expect(() => handler({ payload: "x" })).toThrow('probe: takes no argument named "payload"');
      expect(effect).not.toHaveBeenCalled();
    });
  });

  describe('valuesCheckedBy: "handler" — the handler owns per-value validation', () => {
    const build = (effect: (a: Record<string, unknown>) => unknown) =>
      guardedHandler("writeToTerminal", { text: "string" }, effect, {
        valuesCheckedBy: "handler",
      });

    it("passes every declared value through UN-coerced", () => {
      // `{text: "5"}` must stay the string "5": a typed validator downstream
      // would refuse the number 5 as WRITE_TEXT_INVALID.
      const effect = vi.fn();
      const handler = build(effect);
      handler({ text: "5" });
      handler({ text: 42 });
      handler({ text: { a: 1 } });
      handler({ text: null });
      expect(effect.mock.calls.map((c) => c[0].text)).toEqual(["5", 42, { a: 1 }, null]);
      // An explicit null is PRESENT, not absent — the validator decides.
      expect("text" in effect.mock.calls[3][0]).toBe(true);
    });

    it("still refuses a non-object bag and an undeclared key", () => {
      const effect = vi.fn();
      const handler = build(effect);
      expect(() => handler(5)).toThrow("arguments must be an object (got number)");
      expect(() => handler({ text: "x", zzz: 1 })).toThrow(
        'writeToTerminal: takes no argument named "zzz"',
      );
      expect(() => handler(JSON.parse('{"__proto__": "x"}'))).toThrow(
        'writeToTerminal: takes no argument named "__proto__"',
      );
      expect(effect).not.toHaveBeenCalled();
    });
  });
});
