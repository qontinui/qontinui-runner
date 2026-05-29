/**
 * Tests for the UI Bridge ↔ registry delegation helper.
 *
 * Covers:
 *   1. unknown id throws with a descriptive error
 *   2. successful handler unwraps to `result.value`
 *   3. failed handler throws with `message` when present
 *   4. failed handler falls back to `code` when no `message`
 *   5. handler invocation signature carries `source: "uibridge"`
 */

import { afterEach, describe, expect, it, vi } from "vitest";

import { __resetForTest, register } from "./registry";
import { callRegistry } from "./uibridge";

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
    await expect(callRegistry("test.fail-code-only", {})).rejects.toThrow(
      /not-restartable/,
    );
  });

  it("passes source=uibridge in the ResolverContext", async () => {
    const handler = vi.fn(async () => ({ ok: true as const, value: "x" }));
    register({
      id: "test.context",
      slash: "/context",
      label: "Context",
      description: "test",
      handler,
    });
    await callRegistry("test.context", { foo: 1 });
    expect(handler).toHaveBeenCalledTimes(1);
    const [args, ctx] = handler.mock.calls[0];
    expect(args).toEqual({ foo: 1 });
    expect(ctx.source).toBe("uibridge");
  });
});
