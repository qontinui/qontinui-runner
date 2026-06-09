/**
 * Tests for the pure page-reconciliation helper `reconcilePages` — the logic
 * that unions persisted page tabs with the distinct backend page_ids (from
 * `terminal_list` live terminals AND `terminal_session_list_open` durable
 * restore records) so a backend-spawned continuation on a freshly-minted
 * page_id gets a visible, selectable tab.
 *
 * vitest runs `environment: "node"` with no React Testing Library, so we
 * exercise the exported pure helper directly (same precedent as
 * `fetchOpenRecords` in `useTerminalInitialization.test.ts`). The component
 * effect just feeds this helper the unioned id set, so testing the helper
 * covers the reconciliation behavior without booting React or Tauri.
 */

import { describe, it, expect } from "vitest";

import {
  reconcilePages,
  pageIdsFromTerminals,
  pageIdsFromSessions,
  type TerminalPageConfig,
} from "./useTerminalPages";

const DEFAULT: TerminalPageConfig = { id: "default", name: "Terminal", createdAt: 0 };

describe("pageIdsFromTerminals (wire-field contract)", () => {
  it("reads the camelCase `pageId` field TerminalInfo actually serializes", () => {
    // Regression guard: TerminalInfo is #[serde(rename_all = "camelCase")], so
    // the live wire field is `pageId`. Reading `page_id` here silently lands
    // every terminal on "default" — the bug caught by temp-runner verification.
    expect(pageIdsFromTerminals([{ pageId: "minted-1" }, { pageId: "minted-2" }])).toEqual([
      "minted-1",
      "minted-2",
    ]);
  });

  it("falls back to legacy snake_case `page_id` and to 'default' when absent", () => {
    expect(pageIdsFromTerminals([{ page_id: "legacy" }])).toEqual(["legacy"]);
    expect(pageIdsFromTerminals([{}])).toEqual(["default"]);
    expect(pageIdsFromTerminals([{ pageId: "" }])).toEqual(["default"]);
  });
});

describe("pageIdsFromSessions (wire-field contract)", () => {
  it("reads the camelCase `pageId` field on durable restore records", () => {
    expect(pageIdsFromSessions([{ pageId: "cold-1" }, {}])).toEqual(["cold-1", "default"]);
  });
});

describe("reconcilePages", () => {
  it("appends a synthesized tab for a backend id absent from persisted (terminal_list source)", () => {
    // terminal_list yields a live terminal's page_id not in the tab list.
    const out = reconcilePages([DEFAULT], ["default", "minted-1"]);
    expect(out.map((p) => p.id)).toEqual(["default", "minted-1"]);
    const synth = out.find((p) => p.id === "minted-1")!;
    expect(synth.name).toBe("Page 1");
    expect(synth.createdAt).toBeGreaterThan(0);
  });

  it("reconstructs the tab from the durable restore source with terminal_list empty (cold restart)", () => {
    // On a cold restart terminal_list is empty; the only signal is the durable
    // pageId from terminal_session_list_open. The unioned set still contains it.
    const out = reconcilePages([DEFAULT], ["minted-cold"]);
    expect(out.map((p) => p.id)).toEqual(["default", "minted-cold"]);
    expect(out.find((p) => p.id === "minted-cold")!.name).toBe("Page 1");
  });

  it("preserves operator pages + names and never duplicates 'default'", () => {
    const persisted: TerminalPageConfig[] = [
      DEFAULT,
      { id: "op-a", name: "My Work", createdAt: 100 },
      { id: "op-b", name: "Logs", createdAt: 200 },
    ];
    // Backend reports default again plus a new minted id.
    const out = reconcilePages(persisted, ["default", "op-a", "minted-x"]);
    // Existing pages + names untouched, default not duplicated, "default" never synthesized.
    expect(out.slice(0, 3)).toEqual(persisted);
    expect(out.filter((p) => p.id === "default")).toHaveLength(1);
    const synth = out.find((p) => p.id === "minted-x")!;
    // N = count of existing non-default pages (2) + 1 = 3.
    expect(synth.name).toBe("Page 3");
  });

  it("returns the SAME array reference when every backend id is already persisted", () => {
    const persisted: TerminalPageConfig[] = [
      DEFAULT,
      { id: "op-a", name: "My Work", createdAt: 100 },
    ];
    const out = reconcilePages(persisted, ["default", "op-a"]);
    expect(out).toBe(persisted);
  });

  it("never synthesizes a tab for 'default' even when only the durable source reports it", () => {
    const persisted = [DEFAULT];
    const out = reconcilePages(persisted, ["default", "default"]);
    // Nothing missing → same reference returned, single default tab.
    expect(out).toBe(persisted);
    expect(out.map((p) => p.id)).toEqual(["default"]);
  });

  it("normalizes an empty/falsy backend id to 'default' (no spurious tab)", () => {
    const out = reconcilePages([DEFAULT], [""]);
    expect(out.map((p) => p.id)).toEqual(["default"]);
  });

  it("dedupes the same minted id appearing in both backend sources into one tab", () => {
    // Simulates the unioned set already collapsing duplicates, plus a guard
    // against the same id arriving twice in the iterable.
    const out = reconcilePages([DEFAULT], ["minted-dup", "minted-dup"]);
    expect(out.map((p) => p.id)).toEqual(["default", "minted-dup"]);
  });

  it("numbers multiple new pages sequentially", () => {
    const out = reconcilePages([DEFAULT], ["a", "b"]);
    const names = out.filter((p) => p.id !== "default").map((p) => p.name);
    expect(names).toEqual(["Page 1", "Page 2"]);
  });
});
