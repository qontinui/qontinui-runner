/**
 * Registry-backed terminal spawn actions (shared by `terminal-launch-menu`
 * and the always-mounted `terminal-page` surface).
 *
 * Runner vitest config is `environment: "node"` (no jsdom / no React tree), so
 * we exercise the load-bearing wiring via the exported pure factory — same
 * precedent as `createPlainTerminalAction.test.ts`.
 */

import { describe, it, expect, vi } from "vitest";
import { buildRegistrySpawnActions } from "./registrySpawnActions";

describe("buildRegistrySpawnActions", () => {
  it("exposes the spawn action ids the NL planner relies on", () => {
    const actions = buildRegistrySpawnActions(async () => []);
    const ids = actions.map((a) => a.id);
    expect(ids).toEqual(["create-plain", "create-best-account", "create-with-command"]);
  });

  it("create-best-account delegates to terminal.spawn-ai with account:'best'", async () => {
    const callRegistry = vi.fn(async () => ["tab-1", "tab-2"]);
    const best = buildRegistrySpawnActions(callRegistry as never).find(
      (a) => a.id === "create-best-account",
    )!;

    const result = await best.handler({ count: 2, context: "open a session" });

    expect(callRegistry).toHaveBeenCalledWith("terminal.spawn-ai", {
      count: 2,
      account: "best",
      context: "open a session",
    });
    expect(result).toEqual({
      success: true,
      tab_ids: ["tab-1", "tab-2"],
      task_run_ids: [null, null],
    });
  });

  it("create-best-account defaults count to 1 and works with no params (headless spawn)", async () => {
    const callRegistry = vi.fn(async () => ["tab-1"]);
    const best = buildRegistrySpawnActions(callRegistry as never).find(
      (a) => a.id === "create-best-account",
    )!;

    await best.handler();

    expect(callRegistry).toHaveBeenCalledWith("terminal.spawn-ai", {
      count: 1,
      account: "best",
      context: undefined,
    });
  });

  it("create-best-account rewrites a no-account registry error to the canonical message", async () => {
    const callRegistry = vi.fn(async () => {
      throw new Error("no-account: nothing configured");
    });
    const best = buildRegistrySpawnActions(callRegistry as never).find(
      (a) => a.id === "create-best-account",
    )!;

    await expect(best.handler({ count: 1 })).rejects.toThrow("No AI accounts available");
  });

  it("create-best-account rejects a non-positive count", async () => {
    const best = buildRegistrySpawnActions(async () => []).find(
      (a) => a.id === "create-best-account",
    )!;
    await expect(best.handler({ count: 0 })).rejects.toThrow(/positive number/);
  });

  it("create-plain delegates to terminal.spawn", async () => {
    const callRegistry = vi.fn(async () => ["tab-9"]);
    const plain = buildRegistrySpawnActions(callRegistry as never).find(
      (a) => a.id === "create-plain",
    )!;
    const result = await plain.handler({ count: 1 });
    expect(callRegistry).toHaveBeenCalledWith("terminal.spawn", { count: 1 });
    expect(result).toEqual({ success: true, tab_ids: ["tab-9"], task_run_ids: [] });
  });

  it("create-with-command requires a command", async () => {
    const withCmd = buildRegistrySpawnActions(async () => []).find(
      (a) => a.id === "create-with-command",
    )!;
    await expect(withCmd.handler({ count: 1 })).rejects.toThrow(/requires/);
  });
});
