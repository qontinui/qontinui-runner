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
});
