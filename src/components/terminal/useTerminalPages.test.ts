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

import { describe, it, expect, beforeEach } from "vitest";

import { instanceStorage } from "@/lib/instance-storage";
import { resolveSpawnWorkingDir } from "./useTerminalManager";
import {
  reconcilePages,
  computeVisiblePages,
  pageIdsFromTerminals,
  pageIdsFromSessions,
  applyPageDefaultWorkingDir,
  resolveProjectPage,
  loadPages,
  PAGES_STORAGE_KEY,
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

// ---------------------------------------------------------------------------
// Projects dashboard §7.2 — `defaultWorkingDir` + dangling-page-id tolerance.
// ---------------------------------------------------------------------------

describe("applyPageDefaultWorkingDir", () => {
  const PAGES: TerminalPageConfig[] = [DEFAULT, { id: "op-a", name: "My Work", createdAt: 100 }];
  const ROOT = "D:\\projects\\pizzeria";

  it("sets the page default and leaves every other page untouched", () => {
    const out = applyPageDefaultWorkingDir(PAGES, "op-a", ROOT);
    expect(out).not.toBe(PAGES);
    expect(out.find((p) => p.id === "op-a")!.defaultWorkingDir).toBe(ROOT);
    expect(out.find((p) => p.id === "default")).toBe(DEFAULT);
  });

  it("trims, and treats a blank value as CLEARING the field", () => {
    expect(applyPageDefaultWorkingDir(PAGES, "op-a", `  ${ROOT}  `)[1].defaultWorkingDir).toBe(
      ROOT,
    );
    const set = applyPageDefaultWorkingDir(PAGES, "op-a", ROOT);
    const cleared = applyPageDefaultWorkingDir(set, "op-a", "   ");
    // Cleared DELETES the key, so a cleared page serializes byte-identically
    // to one that never carried the field.
    expect("defaultWorkingDir" in cleared[1]).toBe(false);
  });

  it("returns the SAME array reference when nothing changed (no persist, no re-render)", () => {
    expect(applyPageDefaultWorkingDir(PAGES, "op-a", undefined)).toBe(PAGES);
    const set = applyPageDefaultWorkingDir(PAGES, "op-a", ROOT);
    expect(applyPageDefaultWorkingDir(set, "op-a", ROOT)).toBe(set);
    // Unknown page id is a no-op, never a throw.
    expect(applyPageDefaultWorkingDir(PAGES, "ghost", ROOT)).toBe(PAGES);
  });
});

describe("resolveProjectPage (dangling terminal_page_id tolerance)", () => {
  // The binding lives in Rust `settings.json`; the pages live in
  // `instanceStorage` (localStorage, port-namespaced) which Rust cannot read.
  // A bound id naming a page that does not exist in THIS window is therefore
  // NORMAL (plan §4/§12) and must never fail an activation.
  const PAGES: TerminalPageConfig[] = [DEFAULT, { id: "bound", name: "Pizzeria", createdAt: 100 }];
  const ROOT = "D:\\projects\\pizzeria";
  const mint = () => "minted-uuid";

  it("returns the existing page when the bound id resolves", () => {
    const out = resolveProjectPage(PAGES, "bound", "Papa's Pizzeria", undefined, mint);
    expect(out).toMatchObject({ pageId: "bound", created: false });
    expect(out.pages).toBe(PAGES); // nothing to persist
  });

  it("updates the existing page's defaultWorkingDir without renaming it", () => {
    const out = resolveProjectPage(PAGES, "bound", "Papa's Pizzeria", ROOT, mint);
    expect(out.created).toBe(false);
    const page = out.pages.find((p) => p.id === "bound")!;
    expect(page.defaultWorkingDir).toBe(ROOT);
    // The operator's own page name is never overwritten by the project name.
    expect(page.name).toBe("Pizzeria");
  });

  it("never CLEARS an existing page default when no root is supplied", () => {
    // "no root given" means "leave it alone" — resolving a project page must
    // not silently unpin a page whose cwd is already set.
    const pinned = resolveProjectPage(PAGES, "bound", "Papa's Pizzeria", ROOT, mint).pages;
    const out = resolveProjectPage(pinned, "bound", "Papa's Pizzeria", undefined, mint);
    expect(out.pages).toBe(pinned);
    expect(out.pages.find((p) => p.id === "bound")!.defaultWorkingDir).toBe(ROOT);
  });

  it("re-creates a DANGLING bound id under the same id, named after the project", () => {
    const out = resolveProjectPage(PAGES, "gone-from-this-window", "Papa's Pizzeria", ROOT, mint);
    // Same id → the binding stays stable across windows instead of forking.
    expect(out.pageId).toBe("gone-from-this-window");
    expect(out.created).toBe(true);
    const page = out.pages.find((p) => p.id === "gone-from-this-window")!;
    expect(page.name).toBe("Papa's Pizzeria");
    expect(page.defaultWorkingDir).toBe(ROOT);
    // Existing pages survive.
    expect(out.pages.slice(0, 2)).toEqual(PAGES);
  });

  it("mints a fresh id when the project has no binding yet", () => {
    for (const bound of [undefined, null, "", "   "]) {
      const out = resolveProjectPage(PAGES, bound, "Papa's Pizzeria", ROOT, mint);
      expect(out).toMatchObject({ pageId: "minted-uuid", created: true });
    }
  });

  it("is idempotent when re-resolved with the id it just reported", () => {
    // This is exactly what `ensureProjectPage` does: resolve against the ref,
    // then re-resolve inside the `setPages` updater pinned to the reported id.
    const first = resolveProjectPage(PAGES, null, "Papa's Pizzeria", ROOT, mint);
    const second = resolveProjectPage(first.pages, first.pageId, "Papa's Pizzeria", ROOT, mint);
    expect(second.pageId).toBe(first.pageId);
    expect(second.created).toBe(false);
    expect(second.pages).toBe(first.pages);
  });

  it("falls back to a generic page name when the project has none", () => {
    const out = resolveProjectPage(PAGES, null, "   ", undefined, mint);
    expect(out.pages.find((p) => p.id === "minted-uuid")!.name).toBe("Project");
  });

  it("omits defaultWorkingDir entirely when no root is supplied", () => {
    const out = resolveProjectPage(PAGES, null, "Papa's Pizzeria", undefined, mint);
    expect("defaultWorkingDir" in out.pages.find((p) => p.id === "minted-uuid")!).toBe(false);
  });
});

describe("page persistence round-trip (defaultWorkingDir)", () => {
  // `loadPages`/`savePages` go through `instanceStorage` → JSON in
  // localStorage. vitest's environment is "node", so stub the global the same
  // way the webview provides it.
  beforeEach(() => {
    const store = new Map<string, string>();
    (globalThis as Record<string, unknown>).localStorage = {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => void store.set(k, v),
      removeItem: (k: string) => void store.delete(k),
      key: (i: number) => [...store.keys()][i] ?? null,
      get length() {
        return store.size;
      },
      clear: () => store.clear(),
    };
  });

  it("round-trips defaultWorkingDir through save → load unchanged", () => {
    const saved: TerminalPageConfig[] = [
      { id: "op-a", name: "Pizzeria", createdAt: 100, defaultWorkingDir: "D:\\projects\\pizzeria" },
    ];
    instanceStorage.setJSON(PAGES_STORAGE_KEY, saved);
    expect(loadPages()).toEqual(saved);
  });

  it("loads a page persisted WITHOUT the field (pre-projects layout) unchanged", () => {
    const legacy = [{ id: "op-a", name: "My Work", createdAt: 100 }];
    instanceStorage.setJSON(PAGES_STORAGE_KEY, legacy);
    const loaded = loadPages();
    expect(loaded).toEqual(legacy);
    expect(loaded[0].defaultWorkingDir).toBeUndefined();
    // …and a spawn on such a page behaves exactly as before the field existed.
    expect(resolveSpawnWorkingDir(undefined, loaded[0].defaultWorkingDir)).toBeNull();
  });

  it("still injects the implicit default page when nothing is persisted", () => {
    expect(loadPages()).toEqual([{ id: "default", name: "Terminal", createdAt: 0 }]);
  });
});
