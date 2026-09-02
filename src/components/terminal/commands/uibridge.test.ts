/**
 * Tests for the UI Bridge ↔ registry delegation helper.
 *
 * Two arms, one path. {@link runRegistryAction} is the un-inverted contract —
 * every failure comes back as a `CommandResult`, including "no such action"
 * and a thrown handler. `callRegistry` is the arm that converts that to a
 * throw, which is a WIRE contract its callers depend on
 * (`TerminalTabBar.tsx`, `ZoneLayoutPicker.tsx`, `SuggestionChip.tsx`,
 * `TerminalPage.tsx`'s `no-account` rewrite) rather than an internal
 * convenience, so it stays.
 *
 * The half these tests did not previously cover at all: a direct caller's arg
 * bag used to reach the handler VERBATIM — no coercion, no arity gate.
 * `SuggestionChip.tsx` passes rule-authored args, so "whatever a rule wrote"
 * was the validation. It now goes through `bind.ts::bindDirect`, the same
 * coercion and the same gate the CommandBar applies.
 */

import { afterEach, describe, expect, it, vi } from "vitest";

import { __resetForTest, register } from "./registry";
import { bindCommand, bindSchemaBag } from "./bind";
import { callRegistry, runRegistryAction } from "./uibridge";

afterEach(() => {
  __resetForTest();
});

describe("callRegistry", () => {
  it("throws on unknown action id", async () => {
    await expect(callRegistry("ghost.action", {})).rejects.toThrow(
      /Registry action "ghost.action" not found/,
    );
  });

  it("returns the registry handler's value on success", async () => {
    register({
      id: "test.echo",
      slash: "/echo",
      label: "Echo",
      description: "test",
      paramSchema: { a: "number" },
      handler: async (args) => ({ ok: true, value: args }),
    });
    const result = await callRegistry<{ a: number }>("test.echo", { a: 7 });
    expect(result).toEqual({ a: 7 });
  });

  it("throws with `message` on a failure result", async () => {
    register({
      id: "test.fail",
      slash: "/fail",
      label: "Fail",
      description: "test",
      handler: async () => ({
        ok: false,
        code: "out-of-range",
        message: "zone 99 does not exist",
      }),
    });
    await expect(callRegistry("test.fail", {})).rejects.toThrow(/zone 99 does not exist/);
  });

  it("falls back to `code` when `message` is absent", async () => {
    register({
      id: "test.fail-code-only",
      slash: "/fail",
      label: "Fail",
      description: "test",
      handler: async () => ({ ok: false, code: "not-restartable" }),
    });
    await expect(callRegistry("test.fail-code-only", {})).rejects.toThrow(/not-restartable/);
  });

  it("passes source=uibridge in the ResolverContext", async () => {
    const handler = vi.fn(async () => ({ ok: true as const, value: "x" }));
    register({
      id: "test.context",
      slash: "/context",
      label: "Context",
      description: "test",
      paramSchema: { foo: "number" },
      handler,
    });
    await callRegistry("test.context", { foo: 1 });
    expect(handler).toHaveBeenCalledTimes(1);
    const [args, ctx] = handler.mock.calls[0];
    expect(args).toEqual({ foo: 1 });
    expect(ctx.source).toBe("uibridge");
  });
});

describe("runRegistryAction — the same binding the CommandBar applies", () => {
  const spy = () => vi.fn(async () => ({ ok: true as const, value: "ran" }));

  /**
   * The `SuggestionChip.tsx` shape. A rule authors `chip.args` and nothing
   * checked that the keys are ones the action declares — so a rule naming a
   * field the action does not have reached the handler with a key it never
   * reads, and reported `✓`.
   */
  it("refuses an argument the action does not declare, without calling the handler", async () => {
    const handler = spy();
    register({
      id: "test.zone",
      slash: "/zone",
      label: "Zone",
      description: "test",
      paramSchema: { zone: "number" },
      handler,
    });
    const result = await runRegistryAction("test.zone", { zoen: 1 });
    expect(result.ok).toBe(false);
    expect(result.ok === false && result.message).toMatch(/takes no argument named "zoen"/);
    expect(handler).not.toHaveBeenCalled();
  });

  it("refuses ANY argument to a command that declares none", async () => {
    const handler = spy();
    register({
      id: "test.mute",
      slash: "/mute",
      label: "Mute",
      description: "test",
      paramSchema: {},
      handler,
    });
    const result = await runRegistryAction("test.mute", { count: 1 });
    expect(result.ok === false && result.message).toBe('/mute: takes no arguments (got "count")');
    expect(handler).not.toHaveBeenCalled();
  });

  it("refuses a value that is not text or a number", async () => {
    const handler = spy();
    register({
      id: "test.count",
      slash: "/count",
      label: "Count",
      description: "test",
      paramSchema: { count: "number" },
      handler,
    });
    for (const [value, shape] of [
      [true, "true/false"],
      [{}, "an object"],
      [[], "a list"],
    ] as const) {
      const result = await runRegistryAction("test.count", { count: value });
      expect(result.ok === false && result.message).toBe(
        `/count: "count" must be text or a number (got ${shape})`,
      );
    }
    expect(handler).not.toHaveBeenCalled();
  });

  it("coerces a numeric string the way every other route does", async () => {
    const handler = vi.fn(async (args: Record<string, unknown>) => ({
      ok: true as const,
      value: args,
    }));
    register({
      id: "test.coerce",
      slash: "/coerce",
      label: "Coerce",
      description: "test",
      paramSchema: { count: "number" },
      handler,
    });
    const result = await runRegistryAction("test.coerce", { count: "3" });
    expect(result.ok && result.value).toEqual({ count: 3 });
  });

  it("drops a null the way an optional regex group that did not participate is dropped", async () => {
    const handler = vi.fn(async (args: Record<string, unknown>) => ({
      ok: true as const,
      value: args,
    }));
    register({
      id: "test.null",
      slash: "/null",
      label: "Null",
      description: "test",
      paramSchema: { zone: "number" },
      handler,
    });
    const result = await runRegistryAction("test.null", { zone: null });
    expect(result.ok && result.value).toEqual({});
  });

  it("reports an unknown id as a value, not a throw", async () => {
    const result = await runRegistryAction("ghost.action", {});
    expect(result.ok).toBe(false);
    expect(result.ok === false && result.code).toBe("unknown-action");
  });

  it("reports a thrown handler as a value, not a throw", async () => {
    register({
      id: "test.boom",
      slash: "/boom",
      label: "Boom",
      description: "test",
      paramSchema: {},
      handler: async () => {
        throw new Error("kaboom");
      },
    });
    const result = await runRegistryAction("test.boom", {});
    expect(result.ok === false && result.code).toBe("handler-threw");
    expect(result.ok === false && result.message).toBe("kaboom");
  });

  it("carries the caller's own source through to the handler", async () => {
    const handler = vi.fn(async () => ({ ok: true as const, value: "x" }));
    register({
      id: "test.hotkey",
      slash: "/hotkey",
      label: "Hotkey",
      description: "test",
      paramSchema: {},
      handler,
    });
    await runRegistryAction("test.hotkey", {}, "hotkey");
    expect(handler.mock.calls[0][1].source).toBe("hotkey");
  });
});

/* ── the three gate holes iteration 11 measured ──────────────────────── */

describe("the argument gate refuses what it used to drop", () => {
  it("refuses a NON-OBJECT bag instead of laundering it into an empty one — D4", async () => {
    let ran = 0;
    register({
      id: "test.bare",
      slash: "/bare",
      label: "Bare",
      description: "test",
      paramSchema: {},
      handler: async () => {
        ran++;
        return { ok: true, value: null };
      },
    });
    // `Object.entries(5)` is `[]`, so a scalar bag used to bind `{}` and the
    // action RAN — measured as `/close` closing the focused session — while
    // `"zz"` was refused for its character indices. One class, one answer.
    for (const bag of [5, "zz", true, [], null] as unknown[]) {
      const r = await runRegistryAction("test.bare", bag as Record<string, unknown>);
      expect(r.ok, JSON.stringify(bag)).toBe(false);
      if (bag !== null) {
        expect((r as { message?: string }).message, JSON.stringify(bag)).toMatch(
          /arguments must be an object/,
        );
      }
    }
    expect(ran, "the handler must not have run for ANY malformed bag").toBe(0);
    // Control: a real bag still runs, or the refusals above prove nothing.
    expect((await runRegistryAction("test.bare", {})).ok).toBe(true);
    expect(ran).toBe(1);
  });

  it("REPORTS a `__proto__` key instead of silently dropping it — D5", async () => {
    register({
      id: "test.proto",
      slash: "/proto",
      label: "Proto",
      description: "test",
      paramSchema: {},
      handler: async () => ({ ok: true, value: null }),
    });
    // `args[key] = …` for `__proto__` hits `Object.prototype`'s setter, so the
    // key never reached `Object.keys` and the undeclared check could not see
    // it. JSON.parse, not a literal: a literal's `__proto__` sets the
    // prototype and creates no own key, so a literal fixture tests nothing.
    const bag = JSON.parse('{"__proto__": "x"}') as Record<string, unknown>;
    expect(Object.keys(bag)).toEqual(["__proto__"]);
    const r = await runRegistryAction("test.proto", bag);
    expect(r.ok).toBe(false);
    expect((r as { message?: string }).message).toBe(
      '/proto: takes no arguments (got "__proto__")',
    );
    // …and nothing was polluted on the way to saying so.
    expect(({} as Record<string, unknown>).x).toBeUndefined();
  });

  it("refuses positional residue on a FLAGS-ONLY schema — D6", () => {
    // `unboundTokens` returns real residue here (`fieldOrder` is empty), but
    // the residue check only ran when `declared.size === 0` — false, because
    // the bare flag name IS declared. Net: `/flagged --tenant x please stop`
    // rendered `✓` over two discarded tokens. No live action is shaped this
    // way today, which is exactly why the gate would not have caught the
    // first one added.
    const action = {
      id: "test.flagged",
      slash: "/flagged",
      label: "Flagged",
      description: "test",
      paramSchema: { "--tenant": "string" },
      handler: async () => ({ ok: true, value: null }),
    };
    register(action);
    const refused = bindCommand(
      { kind: "slash", action, literal: true },
      "/flagged --tenant x please stop",
    );
    expect(refused?.refusal).toBe('/flagged: takes no positional arguments (got "please stop")');
    // The flag ALONE still binds — the fix must not refuse the supported form.
    const clean = bindCommand({ kind: "slash", action, literal: true }, "/flagged --tenant x");
    expect(clean?.refusal).toBeNull();
    expect(clean?.args).toEqual({ tenant: "x" });
  });
});

describe("bindSchemaBag — the gate for a surface with no registry action", () => {
  // `create-ai-session` is the live caller: a UI Bridge wire contract that
  // predates the registry and takes a raw `configDir`, so there is no
  // `CommandAction` to bind against and it reached the spawn closure — and a
  // PTY — with the caller's JSON verbatim.
  const schema = { count: "number", configDir: "string", context: "string" };

  it("refuses a value no argument can hold, naming the FIELD", () => {
    expect(bindSchemaBag("create-ai-session", schema, { count: 1, context: {} }).refusal).toBe(
      'create-ai-session: "context" must be text or a number (got an object)',
    );
    expect(bindSchemaBag("create-ai-session", schema, { context: ["x"] }).refusal).toBe(
      'create-ai-session: "context" must be text or a number (got a list)',
    );
  });

  it("refuses a non-object bag and an undeclared key", () => {
    expect(bindSchemaBag("create-ai-session", schema, 5).refusal).toBe(
      "create-ai-session: arguments must be an object (got number)",
    );
    expect(bindSchemaBag("create-ai-session", schema, { nonsense: "y" }).refusal).toBe(
      'create-ai-session: takes no argument named "nonsense"',
    );
    expect(
      bindSchemaBag("create-ai-session", schema, JSON.parse('{"__proto__": "x"}')).refusal,
    ).toBe('create-ai-session: takes no argument named "__proto__"');
  });

  it("binds a well-formed bag with the same coercion every other route uses", () => {
    const bound = bindSchemaBag("create-ai-session", schema, {
      count: "2",
      configDir: "C:/claude/.claude-gmail",
      context: "fix the bug",
    });
    expect(bound.refusal).toBeNull();
    expect(bound.args).toEqual({
      count: 2,
      configDir: "C:/claude/.claude-gmail",
      context: "fix the bug",
    });
  });

  it("says `takes no arguments` when the surface declares none", () => {
    expect(bindSchemaBag("list-runner-windows", {}, { a: 1 }).refusal).toBe(
      'list-runner-windows: takes no arguments (got "a")',
    );
  });
});
