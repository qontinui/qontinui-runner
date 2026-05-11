/**
 * Tests for `useRegistryAwareness` — the per-tab registry-awareness
 * overlap-count hook.
 *
 * The runner's vitest config is `environment: "node"` (no jsdom).
 * Following the precedent of `useFileLockTracking.test.ts`, we test
 * the pure helper `computeOverlapCount` directly rather than rendering
 * the hook through React.
 *
 * Phase 2 of `pty-launched-ai-tabs-warning-plan.md`. Contract:
 *   - Count files in `liveHoldings` whose path is under `tab.workingDir`.
 *   - Exclude files held by the tab itself (`holder_name === tab.title`).
 *   - Return 0 when `workingDir` is undefined or empty.
 *   - Prefix-match honors path boundaries — `/repository` is NOT under
 *     `/repo`.
 */

import { describe, it, expect, vi } from "vitest";
import { computeOverlapCount } from "./useRegistryAwareness";
import type { FileRegistryInfoEntry } from "./useSessionManager";

/**
 * Convenience factory for a {@link FileRegistryInfoEntry} with the
 * fields the helper actually reads + sane defaults for the rest.
 */
function holding(
  file_path: string,
  holder_name: string,
  extras: Partial<FileRegistryInfoEntry> = {},
): FileRegistryInfoEntry {
  return {
    file_path,
    holder_name,
    holder_task_run_id: extras.holder_task_run_id ?? `task-${holder_name}`,
    registered_at: extras.registered_at ?? 0,
    worktree_id: extras.worktree_id ?? null,
    ...extras,
  };
}

describe("computeOverlapCount", () => {
  it("counts files held by other sessions under cwd", () => {
    const tab = { id: "t1", title: "A", workingDir: "/repo" };
    const holdings = [
      holding("/repo/src/foo.rs", "B"),
      holding("/repo/src/bar.rs", "C"),
    ];
    expect(computeOverlapCount(holdings, tab)).toBe(2);
  });

  it("excludes own holdings", () => {
    const tab = { id: "t1", title: "A", workingDir: "/repo" };
    const holdings = [holding("/repo/src/foo.rs", "A")];
    expect(computeOverlapCount(holdings, tab)).toBe(0);
  });

  it("excludes files outside cwd", () => {
    const tab = { id: "t1", title: "A", workingDir: "/repo" };
    const holdings = [holding("/other-repo/foo.rs", "B")];
    expect(computeOverlapCount(holdings, tab)).toBe(0);
  });

  it("returns 0 when workingDir is undefined", () => {
    const tab = { id: "t1", title: "A", workingDir: undefined };
    const holdings = [
      holding("/repo/src/foo.rs", "B"),
      holding("/anywhere/else.rs", "C"),
    ];
    expect(computeOverlapCount(holdings, tab)).toBe(0);
  });

  it("returns 0 when workingDir is an empty string", () => {
    // Otherwise plain prefix-match would treat every path as a match
    // since every string starts with "".
    const tab = { id: "t1", title: "A", workingDir: "" };
    const holdings = [holding("/repo/src/foo.rs", "B")];
    expect(computeOverlapCount(holdings, tab)).toBe(0);
  });

  it("prefix-match honors path boundaries", () => {
    // Plain `startsWith` would return true for "/repository".startsWith("/repo")
    // — the helper must distinguish sibling directories.
    const tab = { id: "t1", title: "A", workingDir: "/repo" };
    const holdings = [holding("/repository/foo.rs", "B")];
    expect(computeOverlapCount(holdings, tab)).toBe(0);
  });

  // Regression: Windows spawn-cwd format mismatch.
  //
  // The backend (`executor/file_registry.rs::normalize_path`) stores
  // file paths as `d:/qontinui-root/cargo.toml` — `\` → `/` and
  // lowercased on Windows. Tauri hands `tab.workingDir` to the frontend
  // verbatim as `D:\qontinui-root` (uppercase drive, backslashes).
  // Without symmetric normalization in the helper the prefix-match
  // silently fails on every Windows tab, the badge never renders, and
  // the feature is invisible to users. Caught during manual smoke
  // (2026-05-11): registered Cargo.toml on a real registry, hook
  // received the holding, but `"d:/...".startsWith("D:\\...")` was
  // false.
  it("normalizes case + slashes for Windows spawn-cwd format", () => {
    // Simulate the Windows webview where backslash + uppercase
    // workingDir collides with lowercase forward-slash backend paths.
    vi.stubGlobal("navigator", { platform: "Win32" } as Navigator);
    try {
      const tab = { id: "t1", title: "A", workingDir: "D:\\qontinui-root" };
      const holdings = [
        holding("d:/qontinui-root/cargo.toml", "manual-test-A"),
        holding("d:/qontinui-root/src/lib.rs", "manual-test-A"),
      ];
      expect(computeOverlapCount(holdings, tab)).toBe(2);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("preserves case on non-Windows platforms", () => {
    // On macOS/Linux file systems are case-sensitive; lowercasing would
    // produce false positives across `/Repo` vs `/repo`.
    vi.stubGlobal("navigator", { platform: "Linux x86_64" } as Navigator);
    try {
      const tab = { id: "t1", title: "A", workingDir: "/Repo" };
      const holdings = [holding("/repo/foo.rs", "B")];
      expect(computeOverlapCount(holdings, tab)).toBe(0);
    } finally {
      vi.unstubAllGlobals();
    }
  });
});
