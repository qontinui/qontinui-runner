/**
 * `create-plain-terminal` UI Bridge action wiring (terminal-page surface).
 *
 * Runner vitest config is `environment: "node"` (no jsdom / no React tree),
 * so we exercise the load-bearing wiring via the exported pure factory —
 * same precedent as `aiLaunchCommand.test.ts` / `LaunchMenu.test.tsx`.
 */

import { describe, it, expect, vi } from "vitest";
import {
  buildCreatePlainTerminalAction,
  CREATE_PLAIN_TERMINAL_ACTION_ID,
} from "./createPlainTerminalAction";

describe("buildCreatePlainTerminalAction", () => {
  it("exposes a stable, discoverable action id + label", () => {
    const action = buildCreatePlainTerminalAction(async () => "tab-1");
    expect(action.id).toBe(CREATE_PLAIN_TERMINAL_ACTION_ID);
    expect(action.id).toBe("create-plain-terminal");
    expect(action.label).toBe("Create Plain Terminal");
    expect(action.description).toMatch(/plain/i);
  });

  it("invokes the createAndAssignTerminal (mount) path and reports the new tab id", async () => {
    const createAndAssign = vi.fn(async () => "tab-42");
    const action = buildCreatePlainTerminalAction(createAndAssign);

    const result = await action.handler();

    expect(createAndAssign).toHaveBeenCalledTimes(1);
    expect(result).toEqual({ success: true, tab_id: "tab-42" });
  });

  it("each invocation creates exactly one new plain terminal (idempotent per call)", async () => {
    const createAndAssign = vi.fn(async () => "tab-x");
    const action = buildCreatePlainTerminalAction(createAndAssign);

    await action.handler();
    await action.handler();

    expect(createAndAssign).toHaveBeenCalledTimes(2);
  });

  it("reports success:false with a null tab_id when the backend declines to create one", async () => {
    const action = buildCreatePlainTerminalAction(async () => null);
    const result = await action.handler();
    expect(result).toEqual({ success: false, tab_id: null });
  });

  // ── The arity-0 residual, at its sharpest ────────────────────────────────
  //
  // Measured on the page: `create-plain-terminal({zzz: "x"})` answered
  // `success: true` and SPAWNED A PTY. The handler takes no parameter, so
  // nothing the caller sent reached the effect — and a process still started
  // for a bag nobody checked, reported as success.
  //
  // These assert the refusal where it has to happen: BEFORE the spawn. Each
  // one checks the spy was never called, because "it threw" and "it threw
  // after spawning a terminal" are different outcomes and only the first is
  // the fix.
  describe("refuses an argument it does not declare, before the PTY exists", () => {
    // The refusal is SYNCHRONOUS. `guardHandler` is a plain function even when
    // `run` is `async`, so a refused call throws before a promise is ever
    // constructed — there is no pending microtask during which a racing caller
    // could observe a half-started spawn. Asserted with `expect(() => …)`
    // rather than `rejects`, which would silently also pass for an async throw
    // and so would not be evidence of the stronger property.
    it("refuses an UNDECLARED key without creating a terminal", () => {
      const createAndAssign = vi.fn(async () => "tab-1");
      const action = buildCreatePlainTerminalAction(createAndAssign);

      expect(() => action.handler({ zzz: "x" })).toThrow(/zzz/);
      expect(createAndAssign).not.toHaveBeenCalled();
    });

    // `Object.entries(5)` is `[]`, so a non-object bag used to destructure to
    // the defaults and run the action bare — the laundering `bindSchemaBag`
    // exists to stop. `[]` is here because an array IS an object to `typeof`.
    it.each([[5], ["zz"], [[]], [true]])(
      "refuses the non-object bag %p without creating a terminal",
      (bag) => {
        const createAndAssign = vi.fn(async () => "tab-1");
        const action = buildCreatePlainTerminalAction(createAndAssign);

        expect(() => action.handler(bag)).toThrow(/must be an object/);
        expect(createAndAssign).not.toHaveBeenCalled();
      },
    );

    // The other half of the contract: guarding must not break the no-argument
    // call, which is how every existing caller invokes this action. `null` and
    // `undefined` are the two spellings the SDK actually sends.
    it.each([[undefined], [null], [{}]])("still spawns for the empty bag %p", async (bag) => {
      const createAndAssign = vi.fn(async () => "tab-1");
      const action = buildCreatePlainTerminalAction(createAndAssign);

      await expect(action.handler(bag)).resolves.toEqual({ success: true, tab_id: "tab-1" });
      expect(createAndAssign).toHaveBeenCalledTimes(1);
    });
  });
});
