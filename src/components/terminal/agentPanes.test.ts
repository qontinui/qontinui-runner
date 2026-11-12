/**
 * `probeAgentPanes` — the IPC wrapper over `terminal_agent_panes`. A failure
 * must read as UNKNOWN (`null`), never as an empty "no agents" map.
 */
import { describe, expect, it } from "vitest";

import { probeAgentPanes } from "./agentPanes";

describe("probeAgentPanes", () => {
  it("returns the per-pane readings and passes ids and fresh through", async () => {
    let seen: Record<string, unknown> | undefined;
    const out = await probeAgentPanes(["a", "b"], true, async (cmd, args) => {
      expect(cmd).toBe("terminal_agent_panes");
      seen = args;
      return { data: { panes: { a: "pi", b: "absent", c: 3 } } };
    });
    expect(seen).toEqual({ terminalIds: ["a", "b"], fresh: true });
    expect(out).toEqual({ a: "pi", b: "absent" });
  });

  it("is null on an IPC failure or a malformed answer", async () => {
    expect(
      await probeAgentPanes(["a"], false, async () => {
        throw new Error("boom");
      }),
    ).toBeNull();
    expect(await probeAgentPanes(["a"], false, async () => ({ data: {} }))).toBeNull();
  });

  it("asks nothing for no ids", async () => {
    expect(
      await probeAgentPanes([], false, async () => {
        throw new Error("must not be called");
      }),
    ).toEqual({});
  });
});
