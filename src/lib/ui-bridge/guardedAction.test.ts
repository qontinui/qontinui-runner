/**
 * The RUNTIME half of the "an unvalidated bag reaches an effect" gate.
 *
 * `actionSurfaces.enforcement.test.ts` proves every action surface in the tree
 * routes through these two wrappers. This proves the wrappers refuse — with
 * the effect spied, so "refused BEFORE any effect" is a counted zero rather
 * than an inference from a thrown error. A handler that threw AFTER creating a
 * PTY is exactly what `create-ai-session` used to do, and it threw too.
 */

import { describe, it, expect, vi } from "vitest";
import { guardedAction, guardedCustomAction } from "./guardedAction";

/**
 * Bags no action can accept, whatever it declares.
 *
 * The first four are the shape defect: `Object.entries(5)` is `[]`, so a
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

describe("guardedAction", () => {
  it("refuses every malformed bag with ZERO effects", () => {
    for (const [label, bag] of MALFORMED) {
      const effect = vi.fn();
      const action = guardedAction({
        id: "probe",
        paramSchema: { count: "number" },
        run: (args) => {
          effect(args);
          return "ran";
        },
      });
      expect(() => action.handler(bag), label).toThrow();
      expect(effect, `${label} reached the effect`).not.toHaveBeenCalled();
    }
  });

  it("names the surface and the reason, never a minified variable", () => {
    const action = guardedAction({
      id: "create-ai-session",
      paramSchema: { context: "string" },
      run: () => "ran",
    });
    // The three sentences an operator can actually act on.
    expect(() => action.handler(5)).toThrow(
      "create-ai-session: arguments must be an object (got number)",
    );
    expect(() => action.handler({ zzz: "x" })).toThrow(
      'create-ai-session: takes no argument named "zzz"',
    );
    expect(() => action.handler({ context: {} })).toThrow(
      'create-ai-session: "context" must be text or a number (got an object)',
    );
    // …and not `od.replace is not a function`, which is what the operator saw
    // before, 750 lines and one PTY later.
    expect(() => action.handler({ context: {} })).not.toThrow(/is not a function/);
  });

  it("runs on a well-formed bag, and the effect sees the BOUND args", () => {
    const effect = vi.fn();
    const action = guardedAction({
      id: "probe",
      paramSchema: { count: "number", name: "string" },
      run: (args) => {
        effect(args);
        return "ran";
      },
    });
    // `"2"` coerces to the number 2 — the same reading Tier 1 gives typed
    // text, which is why `run` reads text fields through `textArg`.
    expect(action.handler({ count: "2", name: "alpha" })).toBe("ran");
    expect(effect).toHaveBeenCalledWith({ count: 2, name: "alpha" });
  });

  it("an EMPTY schema means takes-no-arguments, enforced", () => {
    const effect = vi.fn();
    const action = guardedAction({
      id: "mute",
      paramSchema: {},
      run: () => effect(),
    });
    action.handler({});
    expect(effect).toHaveBeenCalledTimes(1);
    expect(() => action.handler({ anything: 1 })).toThrow(
      'mute: takes no arguments (got "anything")',
    );
    expect(effect).toHaveBeenCalledTimes(1);
  });

  it("a nullish bag is the empty bag; a non-object bag is NOT", () => {
    const effect = vi.fn();
    const action = guardedAction({ id: "probe", paramSchema: {}, run: () => effect() });
    action.handler(undefined);
    action.handler(null);
    expect(effect).toHaveBeenCalledTimes(2);
    // The distinction the old `(params ?? {})` collapsed.
    expect(() => action.handler(0)).toThrow("arguments must be an object (got number)");
    expect(() => action.handler("")).toThrow("arguments must be an object (got string)");
  });

  it("`__proto__` is refused as an undeclared key, not silently dropped", () => {
    const action = guardedAction({ id: "probe", paramSchema: { count: "number" }, run: () => 1 });
    expect(() => action.handler(JSON.parse('{"__proto__": "x"}'))).toThrow(
      'probe: takes no argument named "__proto__"',
    );
  });

  it("publishes the declared paramSchema on the action it builds", () => {
    // The wire contract an agent reads. A guard that validated against a
    // schema nobody could see would be the same drift, one level down.
    const schema = { count: "number (>= 1)" };
    expect(guardedAction({ id: "p", paramSchema: schema, run: () => 1 }).paramSchema).toBe(schema);
    expect(guardedCustomAction({ id: "p", paramSchema: schema, run: () => 1 }).paramSchema).toBe(
      schema,
    );
  });
});

describe("guardedCustomAction", () => {
  it("refuses every malformed bag with ZERO effects", () => {
    for (const [label, bag] of MALFORMED) {
      const effect = vi.fn();
      const action = guardedCustomAction({
        id: "probe",
        paramSchema: { count: "number" },
        run: (args) => effect(args),
      });
      expect(() => action.handler(bag), label).toThrow();
      expect(effect, `${label} reached the effect`).not.toHaveBeenCalled();
    }
  });

  describe("structuredParams — the one sanctioned exemption", () => {
    const build = (effect: (a: Record<string, unknown>) => unknown) =>
      guardedCustomAction({
        id: "sendKeys",
        paramSchema: { keys: "string | string[] | descriptors" },
        structuredParams: ["keys"],
        run: effect,
      });

    it("lets the SDK's two array grammars through un-coerced", () => {
      const effect = vi.fn();
      const action = build(effect);
      action.handler({ keys: ["Enter"] });
      action.handler({ keys: [{ key: "c", modifiers: { ctrl: true } }] });
      action.handler({ keys: "ls\r" });
      expect(effect.mock.calls.map((c) => c[0].keys)).toEqual([
        ["Enter"],
        [{ key: "c", modifiers: { ctrl: true } }],
        "ls\r",
      ]);
    });

    it("does NOT exempt the bag's shape or its key set", () => {
      // The exemption is per-FIELD and per-VALUE only. Widening it to the bag
      // would hand back exactly the hole it is carved out of.
      const effect = vi.fn();
      const action = build(effect);
      expect(() => action.handler(5)).toThrow("arguments must be an object");
      expect(() => action.handler(["Enter"])).toThrow("arguments must be an object (got a list)");
      expect(() => action.handler({ keys: "a", zzz: "x" })).toThrow(
        'sendKeys: takes no argument named "zzz"',
      );
      expect(effect).not.toHaveBeenCalled();
    });

    it("cannot widen an action past its own schema", () => {
      // A field named in `structuredParams` that the schema does not declare
      // is ignored, and then refused as undeclared — otherwise the exemption
      // list would be a second, quieter schema.
      const effect = vi.fn();
      const action = guardedCustomAction({
        id: "probe",
        paramSchema: { text: "string" },
        structuredParams: ["payload"],
        run: (args) => effect(args),
      });
      // Undeclared ⇒ not exempted ⇒ coerced like any other value, so the
      // VALUE refusal fires before the undeclared one. Both are refusals with
      // zero effects; the sentence differs by which gate got there first.
      expect(() => action.handler({ payload: { a: 1 } })).toThrow(
        'probe: "payload" must be text or a number (got an object)',
      );
      // With a scalar value it reaches the undeclared gate, which is the one
      // that says the exemption bought nothing.
      expect(() => action.handler({ payload: "x" })).toThrow(
        'probe: takes no argument named "payload"',
      );
      expect(effect).not.toHaveBeenCalled();
    });
  });
});
