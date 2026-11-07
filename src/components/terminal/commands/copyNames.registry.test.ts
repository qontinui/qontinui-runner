/**
 * `/copy-names` entered where the UI Bridge and the suggestion chips enter
 * every registered command: `callRegistry`, over the REAL registry
 * `useTerminalCommands` builds (`realRegistry.testkit`).
 *
 * The contract pinned: the copied text is the runner's session ROSTER — the
 * sessions a restart would bring back, grouped by account — each with its
 * shared `cd … && CLAUDE_CONFIG_DIR=… claude --resume <id>` line, a session
 * with no resolvable account or directory said so rather than guessed, and
 * the finished ones skipped.
 */
import { beforeAll, describe, expect, it } from "vitest";

import { loadRealRegistry, type RealRegistryHarness } from "./realRegistry.testkit";
import { callRegistry } from "./uibridge";

let h: RealRegistryHarness;

beforeAll(async () => {
  h = await loadRealRegistry();
}, 60_000);

describe("/copy-names through the registry door", () => {
  it("copies the roster's unfinished sessions with their resume lines", async () => {
    h.resetArms();
    h.setArms({ roster: "two", clipboard: "written" });
    h.reset();
    try {
      await callRegistry("terminal.copy-names", {});
      const writes = h.calls.filter((c) => c.name === "writeClipboard");
      expect(writes).toHaveLength(1);
      const text = String(writes[0].args[0]);
      expect(text).toContain("# gmail (1)");
      expect(text).toContain(
        'cd "D:/repo" && CLAUDE_CONFIG_DIR="C:/claude/.claude-gmail" claude --resume sess-1',
      );
      expect(text).toContain("# hotmail (1)");
      expect(text).toContain("no resume line");
      expect(text).toContain("# 1 finished session skipped");
      expect(text).not.toContain("sess-3");
    } finally {
      h.resetArms();
    }
  });
});
