/**
 * Phase 1 of plan 2026-10-22-runner-zone-header-total-output-seed-and-narrow-zone-chips:
 * every `terminal_list` reader seeds the zone header's "total output" figure
 * from `TerminalInfo.totalBytesProduced`.
 *
 * (i) the pure helper's contract on a fresh store, and (ii) a source assertion
 * that all three readers call it — seeding from one reader alone loses to the
 * `retainTabs` prune race on mount, which the 30 s sweep repairs.
 */

import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import type { TerminalInfo } from "@qontinui/shared-types/tauri-events";
import { TerminalHotStore } from "./terminalHotStore";
import { seedTotalBytesFromList } from "./useTerminalManager";

const listed = (id: string, totalBytesProduced: number): TerminalInfo =>
  ({
    id,
    title: id,
    pid: 1,
    isAlive: true,
    exitCode: null,
    createdAt: "2026-10-22T00:00:00Z",
    workingDir: "/w",
    pageId: "default",
    totalBytesProduced,
  }) as unknown as TerminalInfo;

describe("seedTotalBytesFromList", () => {
  it("a listed total appears on the tab slice", () => {
    const store = new TerminalHotStore();
    seedTotalBytesFromList(store, [listed("a", 1234), listed("b", 0)]);
    expect(store.getTabSlice("a").totalBytes).toBe(1234);
    expect(store.getTabSlice("b").totalBytes).toBe(0);
  });

  it("a lower list total never moves an existing figure back", () => {
    const store = new TerminalHotStore();
    store.noteTabTotalBytes("a", 5000);
    seedTotalBytesFromList(store, [listed("a", 100)]);
    expect(store.getTabSlice("a").totalBytes).toBe(5000);
    seedTotalBytesFromList(store, [listed("a", 7000)]);
    expect(store.getTabSlice("a").totalBytes).toBe(7000);
  });

  it("skips a missing or non-finite total instead of recording it", () => {
    const store = new TerminalHotStore();
    const missing = { ...listed("m", 1) } as Record<string, unknown>;
    delete missing.totalBytesProduced;
    seedTotalBytesFromList(store, [
      missing as unknown as TerminalInfo,
      listed("n", Number.NaN),
      listed("i", Number.POSITIVE_INFINITY),
    ]);
    expect(store.getTabSlice("m").totalBytes).toBeUndefined();
    expect(store.getTabSlice("n").totalBytes).toBeUndefined();
    expect(store.getTabSlice("i").totalBytes).toBeUndefined();
  });
});

describe("every terminal_list reader seeds the total", () => {
  const source = readFileSync(
    fileURLToPath(new URL("./useTerminalManager.ts", import.meta.url)),
    "utf8",
  );

  /** The body of `const <name> = useCallback(...)`, up to its dependency list. */
  const callbackBody = (name: string): string => {
    const start = source.indexOf(`const ${name} = useCallback(`);
    expect(start, `${name} not found`).toBeGreaterThan(-1);
    const end = source.indexOf("\n  }, [", start);
    expect(end, `${name} has no dependency list`).toBeGreaterThan(start);
    return source.slice(start, end);
  };

  it.each(["resyncTabs", "reconnectToExistingSessions", "reconcileClaudeSessionIds"])(
    "%s calls seedTotalBytesFromList",
    (name) => {
      const body = callbackBody(name);
      expect(body).toContain('invoke<CommandResponse>("terminal_list")');
      expect(body).toContain("seedTotalBytesFromList(");
      expect(body).toContain("getTerminalHotStore(pageId)");
    },
  );
});
