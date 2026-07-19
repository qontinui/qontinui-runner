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
  computeVisiblePages,
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

  it("does not duplicate 'default' when it is already persisted (both backend sources report it)", () => {
    const persisted = [DEFAULT];
    const out = reconcilePages(persisted, ["default", "default"]);
    // Already known → nothing missing → same reference returned, single default tab.
    expect(out).toBe(persisted);
    expect(out.map((p) => p.id)).toEqual(["default"]);
  });

  it("synthesizes a 'Terminal' tab for 'default' when the operator layout dropped it but the backend still parks terminals there", () => {
    // The orphaning bug: once the operator has created pages, `loadPages` no
    // longer injects the default page, so it is ABSENT from `persisted`. A
    // terminal created via `POST /terminals` with no pageId lands on "default"
    // and must still get a visible, selectable tab (regression for the
    // 2026-07-18 fan-out, where API-spawned terminals vanished onto "default").
    const persisted: TerminalPageConfig[] = [
      { id: "op-a", name: "My Work", createdAt: 100 },
      { id: "op-b", name: "Logs", createdAt: 200 },
    ];
    const out = reconcilePages(persisted, ["default", "op-a"]);
    expect(out.map((p) => p.id)).toEqual(["op-a", "op-b", "default"]);
    // "default" keeps its canonical name, not a sequential "Page N".
    expect(out.find((p) => p.id === "default")!.name).toBe("Terminal");
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

describe("computeVisiblePages (default-tab visibility)", () => {
  const OP_A: TerminalPageConfig = { id: "op-a", name: "My Work", createdAt: 100 };
  const OP_B: TerminalPageConfig = { id: "op-b", name: "Logs", createdAt: 200 };

  it("hides the empty 'default' page from the tab strip when other pages exist", () => {
    // The P6 boot bug: default persisted but holds no live/restorable session.
    const out = computeVisiblePages([OP_A, DEFAULT], new Set());
    expect(out.map((p) => p.id)).toEqual(["op-a"]);
  });

  it("shows the 'default' page as soon as it hosts ≥1 session (681e1112f home preserved)", () => {
    // An API-created terminal (POST /terminals, no pageId) lands on "default";
    // reconcile puts "default" in occupiedPageIds → the tab must reappear.
    const out = computeVisiblePages([OP_A, DEFAULT], new Set(["default"]));
    expect(out.map((p) => p.id)).toEqual(["op-a", "default"]);
  });

  it("always shows a USER page even when it hosts zero sessions", () => {
    // Only "default" is special-cased; an operator-created empty page still
    // renders its tab (they made it on purpose — predictability).
    const out = computeVisiblePages([OP_A, OP_B, DEFAULT], new Set(["op-a"]));
    // op-a occupied, op-b empty (still shown), default empty (hidden).
    expect(out.map((p) => p.id)).toEqual(["op-a", "op-b"]);
  });

  it("shows a solo empty 'default' rather than rendering an empty tab strip (first boot)", () => {
    // Never leave the active page without a visible tab: if hiding the empty
    // default would leave nothing, show it anyway.
    const out = computeVisiblePages([DEFAULT], new Set());
    expect(out.map((p) => p.id)).toEqual(["default"]);
  });

  it("active-page fallback: hiding an active empty default leaves a visible page to activate", () => {
    // The hook's normalize-active effect activates visiblePages[0] when the
    // active id has no visible tab. With default active+hidden, the first
    // visible page is a real, selectable tab — never the hidden default.
    const visible = computeVisiblePages([DEFAULT, OP_A], new Set(["op-a"]));
    expect(visible.map((p) => p.id)).toEqual(["op-a"]);
    expect(visible.some((p) => p.id === "default")).toBe(false);
    expect(visible.length).toBeGreaterThan(0);
    expect(visible[0].id).toBe("op-a");
  });

  it("returns the input array (same order) untouched when nothing needs hiding", () => {
    const pages = [OP_A, DEFAULT];
    const out = computeVisiblePages(pages, new Set(["op-a", "default"]));
    expect(out.map((p) => p.id)).toEqual(["op-a", "default"]);
  });
});
